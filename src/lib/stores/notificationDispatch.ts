import { preferencesStore } from './preferences.svelte';
import { toastStore } from './toasts.svelte';
import type { Toast, ToastSource } from './toasts.svelte';
import { workspacesStore } from './workspaces.svelte';
import { getCurrentWindow } from '@tauri-apps/api/window';
import {
  isPermissionGranted,
  requestPermission,
  sendNotification,
} from '@tauri-apps/plugin-notification';
import { info as logInfo } from '@tauri-apps/plugin-log';
import { playSystemSound } from '$lib/tauri/commands';

// NOTE: The `extra` field is passed through to Rust but only used on mobile (iOS/Android).
// On desktop, tauri-plugin-notification uses notify_rust which discards extra data and has
// no click callback. The extra.tabId is prep work for future mobile support.
async function sendOsNotification(title: string, body: string, source?: ToastSource): Promise<void> {
  let granted = await isPermissionGranted();
  if (!granted) {
    const permission = await requestPermission();
    granted = permission === 'granted';
  }
  if (!granted) return;
  sendNotification({
    title,
    body,
    ...(source?.tabId ? { extra: { tabId: source.tabId } } : {}),
  });
}

/** Shared AudioContext — reused across calls to avoid WebKit's context limit. */
let audioCtx: AudioContext | null = null;

function getAudioContext(): AudioContext {
  if (!audioCtx || audioCtx.state === 'closed') {
    audioCtx = new AudioContext();
  }
  // Resume if suspended (e.g. after tab goes idle)
  if (audioCtx.state === 'suspended') {
    audioCtx.resume();
  }
  return audioCtx;
}

/** Play the built-in chirp sound using Web Audio API. */
function playBuiltinChirp() {
  try {
    const ctx = getAudioContext();
    const volume = preferencesStore.notificationVolume / 100;

    // Two-tone chirp: a quick rising pair of tones
    const now = ctx.currentTime;

    // First tone — lower pitch
    const osc1 = ctx.createOscillator();
    const gain1 = ctx.createGain();
    osc1.type = 'sine';
    osc1.frequency.setValueAtTime(800, now);
    gain1.gain.setValueAtTime(volume * 0.3, now);
    gain1.gain.exponentialRampToValueAtTime(0.001, now + 0.08);
    osc1.connect(gain1);
    gain1.connect(ctx.destination);
    osc1.start(now);
    osc1.stop(now + 0.08);

    // Second tone — higher pitch, slightly delayed
    const osc2 = ctx.createOscillator();
    const gain2 = ctx.createGain();
    osc2.type = 'sine';
    osc2.frequency.setValueAtTime(1200, now + 0.06);
    gain2.gain.setValueAtTime(0.001, now);
    gain2.gain.setValueAtTime(volume * 0.3, now + 0.06);
    gain2.gain.exponentialRampToValueAtTime(0.001, now + 0.15);
    osc2.connect(gain2);
    gain2.connect(ctx.destination);
    osc2.start(now + 0.06);
    osc2.stop(now + 0.15);
  } catch {
    // Web Audio not available — silently skip
  }
}

/** Preview the notification sound from the preferences UI. */
export function playNotificationSoundPreview() {
  playNotificationSound();
}

/** Play the configured notification sound (exported for bell, etc.). */
export function playNotificationSound() {
  const sound = preferencesStore.notificationSound;
  if (sound === 'none') return;
  if (sound === 'default') {
    playBuiltinChirp();
  } else {
    // System sound — delegate to Rust backend
    playSystemSound(sound, preferencesStore.notificationVolume).catch(() => {
      // Fallback to built-in if system sound fails
      playBuiltinChirp();
    });
  }
}

/** Check if a tab belongs to the current window's workspaces. */
function tabBelongsToWindow(tabId: string): boolean {
  for (const ws of workspacesStore.workspaces) {
    for (const pane of ws.panes) {
      if (pane.tabs.some(t => t.id === tabId)) return true;
    }
  }
  return false;
}

/**
 * Central notification dispatch. Routes to in-app toast or OS notification
 * based on the user's notification_mode preference and window focus state.
 */
export async function dispatch(
  title: string,
  body: string,
  type: Toast['type'] = 'info',
  source?: ToastSource,
  /** A request that waits for the human (`toastStore.addRequest`): the in-app toast stays until
   *  its owner removes it by `key`, and clicking it runs `action`. */
  request?: { key: string; action?: () => void },
): Promise<void> {
  const mode = preferencesStore.notificationMode;

  if (mode === 'disabled') return;

  // If the notification is scoped to a specific tab, only show it in the window that owns that tab.
  // Tauri events broadcast to all windows, so without this check every window would show the toast.
  if (source?.tabId && !tabBelongsToWindow(source.tabId)) return;

  playNotificationSound();

  if (mode === 'native') {
    logInfo(`Notification (native): ${body}`);
    await sendOsNotification(title, body, source);
    return;
  }

  const toast = (focused?: boolean) => {
    if (request) toastStore.addRequest({ key: request.key, title, body, source, action: request.action });
    else toastStore.addToast(title, body, type, source, focused);
  };

  if (mode === 'in_app') {
    logInfo(`Notification (in-app): ${body}`);
    toast();
    return;
  }

  // mode === 'auto': always toast, additionally OS notification when unfocused
  try {
    const focused = await getCurrentWindow().isFocused();
    toast(focused);
    if (focused) {
      logInfo(`Notification (auto/in-app): ${body}`);
    } else {
      logInfo(`Notification (auto/both): ${body}`);
      await sendOsNotification(title, body, source);
    }
  } catch {
    // Fallback to in-app if focus check fails
    toast();
  }
}
