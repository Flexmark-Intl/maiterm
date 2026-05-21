import { check, type Update } from '@tauri-apps/plugin-updater';
import { relaunch } from '@tauri-apps/plugin-process';
import { getVersion } from '@tauri-apps/api/app';
import { invoke } from '@tauri-apps/api/core';
import { toastStore } from './toasts.svelte';
import { terminalsStore } from './terminals.svelte';
import * as commands from '$lib/tauri/commands';
import { info as logInfo, error as logError } from '@tauri-apps/plugin-log';
import type { ChangelogEntry } from '$lib/components/ChangelogModal.svelte';

interface GitHubRelease {
  tag_name: string;
  body: string | null;
}

/** Compare semver strings. Returns true if a > b. */
function isNewerVersion(a: string, b: string): boolean {
  const pa = a.replace(/^v/, '').split('.').map(Number);
  const pb = b.replace(/^v/, '').split('.').map(Number);
  for (let i = 0; i < 3; i++) {
    if ((pa[i] ?? 0) > (pb[i] ?? 0)) return true;
    if ((pa[i] ?? 0) < (pb[i] ?? 0)) return false;
  }
  return false;
}

/** Parse a GitHub release body into changelog items. */
function parseReleaseBody(body: string): string[] {
  return body.split('\n')
    .map(line => line.match(/^- (.+)/))
    .filter((m): m is RegExpMatchArray => m !== null)
    .map(m => m[1].replace(/`([^`]+)`/g, '$1'));
}

function createUpdaterStore() {
  let checking = $state(false);
  let downloading = $state(false);
  let installed = $state(false);
  let currentUpdate = $state<Update | null>(null);
  let dismissed = $state(false);
  let releaseNotes = $state<ChangelogEntry[]>([]);
  let loadingNotes = $state(false);
  let showWhatsNewRequested = $state(false);

  async function checkForUpdates(silent = false): Promise<Update | null> {
    if (checking || downloading) return null;
    checking = true;
    try {
      const update = await check();
      if (update) {
        currentUpdate = update;
        dismissed = false;
        logInfo(`Update available: v${update.version}`);
        if (!silent) {
          toastStore.addToast(
            'Update Available',
            `v${update.version} is ready — click to review`,
            'info',
            undefined,
            undefined,
            () => { showWhatsNewRequested = true; },
          );
        }
      } else if (!silent) {
        toastStore.addToast('Up to Date', 'You are running the latest version.', 'success');
      }
      return update;
    } catch (e) {
      const msg = e instanceof Error ? e.message : String(e);
      logError(`Update check failed: ${msg}`);
      if (!silent) {
        toastStore.addToast('Update Check Failed', msg, 'error');
      }
      return null;
    } finally {
      checking = false;
    }
  }

  async function fetchReleaseNotes(): Promise<ChangelogEntry[]> {
    loadingNotes = true;
    try {
      const currentVersion = await getVersion();
      const res = await fetch('https://api.github.com/repos/Flexmark-Intl/aiterm/releases');
      if (!res.ok) throw new Error(`GitHub API: ${res.status}`);
      const releases: GitHubRelease[] = await res.json();

      const entries: ChangelogEntry[] = releases
        .filter(r => isNewerVersion(r.tag_name, currentVersion) && r.body)
        .map(r => ({
          version: r.tag_name.replace(/^v/, ''),
          items: parseReleaseBody(r.body!),
        }))
        .filter(e => e.items.length > 0);

      releaseNotes = entries;
      return entries;
    } catch (e) {
      const msg = e instanceof Error ? e.message : String(e);
      logError(`Failed to fetch release notes: ${msg}`);
      return [];
    } finally {
      loadingNotes = false;
    }
  }

  /** Re-check for updates and return the newer update if one exists beyond currentUpdate */
  async function recheckForNewer(): Promise<Update | null> {
    if (!currentUpdate) return null;
    try {
      const freshUpdate = await check();
      if (freshUpdate && isNewerVersion(freshUpdate.version, currentUpdate.version)) {
        return freshUpdate;
      }
    } catch (e) {
      logError(`Re-check failed: ${e instanceof Error ? e.message : String(e)}`);
    }
    return null;
  }

  /** Switch currentUpdate to a different update object (e.g. a newer one found during re-check) */
  function switchToUpdate(update: Update) {
    currentUpdate = update;
  }

  async function downloadAndInstall() {
    if (!currentUpdate || downloading) return;
    downloading = true;
    try {
      await currentUpdate.downloadAndInstall();
      installed = true;
    } catch (e) {
      const msg = e instanceof Error ? e.message : String(e);
      logError(`Update install failed: ${msg}`);
      toastStore.addToast('Update Failed', msg, 'error');
    } finally {
      downloading = false;
    }
  }

  function dismiss() {
    dismissed = true;
  }

  /**
   * Flush state to disk, then relaunch. relaunch() hard-kills the process
   * without firing onCloseRequested/quit-requested, so the normal shutdown
   * save path never runs — we must mirror it here or recently-changed state
   * (tab names, scrollback, geometry) is lost across the update.
   */
  async function restart() {
    try {
      const monitorCount = await commands.getMonitorCount().catch(() => 1);
      await commands.saveWindowGeometry(monitorCount).catch(() => {});
      await terminalsStore.saveAllScrollback();
      await invoke('sync_state');
    } catch (e) {
      logError(`Pre-relaunch state flush failed: ${e instanceof Error ? e.message : String(e)}`);
    }
    relaunch();
  }

  return {
    get checking() { return checking; },
    get downloading() { return downloading; },
    get installed() { return installed; },
    get currentUpdate() { return currentUpdate; },
    get dismissed() { return dismissed; },
    get releaseNotes() { return releaseNotes; },
    get loadingNotes() { return loadingNotes; },
    /** True when the banner should be visible */
    get showBanner() { return (currentUpdate !== null || installed) && !dismissed; },
    /** True when a toast click requested showing the What's New modal */
    get showWhatsNewRequested() { return showWhatsNewRequested; },
    checkForUpdates,
    recheckForNewer,
    switchToUpdate,
    downloadAndInstall,
    fetchReleaseNotes,
    dismiss,
    restart,
    requestShowWhatsNew() { showWhatsNewRequested = true; },
    clearShowWhatsNewRequest() { showWhatsNewRequested = false; },
  };
}

export const updaterStore = createUpdaterStore();
