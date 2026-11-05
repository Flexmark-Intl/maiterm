/**
 * A Hive's approved media ask, in the Hive's window (docs/hive-media.md §8, lib/hive/toolSetup.ts).
 *
 * Rust emits `hive-tool-setup-approved` to the Hive's window when the human approves a
 * `tool_setup` card, on the Loom or the phone. This store holds the opening until the window can
 * show it, then brings the Hive's workspace forward and hands it to the Hive view, which opens Hive
 * settings with the setup sheet (HiveView → HiveSettingsModal → HiveMediaPanel's `setup` seed):
 * - **When**: at once if the window has focus (a desktop approval always has: the click was here),
 *   else on its next focus. A phone approval never pulls the window forward or switches its
 *   workspace while the human is elsewhere.
 * - **Missed**: a window reloading when the event came finds the approval in the request list at
 *   init and at each focus until it has looked once (`missedOpenings`). Every opening the window
 *   is done with (opened, replaced by a newer one for its Hive, or dropped) is kept in
 *   sessionStorage, which survives a reload: each gets one try, and a reload never revives one.
 * - **A phone-started install** (`install` set) ends maybe before the sheet opens: its end is held
 *   on the opening, so the sheet shows how it went rather than waiting for an event that came.
 *
 * Nothing here decides or carries out the request: Rust settled it at approval, and adds the tools
 * once the capability works.
 */
import { getCurrentWindow } from '@tauri-apps/api/window';
import { error as logError, info as logInfo, warn as logWarn } from '@tauri-apps/plugin-log';
import { countedListen } from '$lib/utils/listenCounter';
import * as commands from '$lib/tauri/commands';
import type { HiveMediaInstallFinished } from '$lib/tauri/types';
import { workspacesStore } from '$lib/stores/workspaces.svelte';
import { APPROVED_EVENT, HIVE_OPEN_TOOL_SETUP, createOpeningQueue, missedOpenings, openingFrom, parseApproved, type ToolSetupOpening } from '$lib/hive/toolSetup';

const OPENED_KEY = 'maiterm.hive.toolSetupOpened';
/** How long a flushed opening waits for its Hive view to take it. */
const TAKE_WITHIN_MS = 3000;

function readOpened(): Set<string> {
  try {
    const raw = sessionStorage.getItem(OPENED_KEY);
    const ids = raw ? (JSON.parse(raw) as unknown) : [];
    return new Set(Array.isArray(ids) ? ids.filter((x): x is string => typeof x === 'string') : []);
  } catch {
    return new Set();
  }
}

function writeOpened(ids: Set<string>) {
  try {
    // Rust lists a settled request for an hour: the last few dozen ids are plenty.
    sessionStorage.setItem(OPENED_KEY, JSON.stringify([...ids].slice(-50)));
  } catch {
    // No storage: a reload may offer a recovered sheet once more, nothing worse.
  }
}

function createHiveToolSetupStore() {
  /** Its `spent` set is what sessionStorage keeps: every opening this window is done with. */
  const queue = createOpeningQueue();
  let unlisteners: (() => void)[] = [];
  let started = false;
  let recovered = false;
  let flushing = false;

  const inThisWindow = (workspaceId: string) => workspacesStore.workspaces.some((w) => w.id === workspaceId && !!w.hive && !w.suspended);
  const persist = () => writeOpened(queue.spent);

  /** Queued; a replaced one is spent by the queue, and that is saved. */
  function queueOpening(o: ToolSetupOpening) {
    queue.add(o);
    persist();
  }

  /** Given up on without opening: spent and saved, so a reload never finds it again. */
  function drop(workspaceId: string) {
    queue.drop(workspaceId);
    persist();
  }

  /** The request list, read once per window life, for an approval the window wasn't listening for. */
  async function recover() {
    if (recovered) return;
    const ids = new Set(workspacesStore.workspaces.filter((w) => !!w.hive).map((w) => w.id));
    // Workspaces not loaded yet: the next focus looks again.
    if (workspacesStore.workspaces.length === 0) return;
    recovered = true;
    if (ids.size === 0) return;
    try {
      const list = await commands.listAgentRequests();
      for (const o of missedOpenings(list, getCurrentWindow().label, ids, queue.spent, Date.now())) {
        logInfo(`hive: tool setup ${o.requestId} approved while this window wasn't listening; opens on focus`);
        queueOpening(o);
      }
    } catch (e) {
      void logWarn(`hive: missed tool setups not read: ${e}`);
    }
  }

  /** Bring the newest waiting opening's Hive forward; its view takes it (`take`). */
  async function flush() {
    if (flushing) return;
    flushing = true;
    try {
      await recover();
      const o = queue.next();
      if (!o) return;
      if (!inThisWindow(o.workspaceId)) {
        // The Hive was deleted, moved or suspended meanwhile: nothing to open here.
        drop(o.workspaceId);
        return;
      }
      if (workspacesStore.activeWorkspaceId !== o.workspaceId) await workspacesStore.setActiveWorkspace(o.workspaceId);
      // A Hive view already showing hears this; a freshly mounted one takes it at mount.
      window.dispatchEvent(new CustomEvent<string>(HIVE_OPEN_TOOL_SETUP, { detail: o.workspaceId }));
      // One try per opening: an opening no view took (the workspace couldn't be shown) is dropped
      // rather than pulling the window to that workspace on every focus. Hive settings → Tools
      // still reaches its sheet, and Rust still adds the tools once it works.
      setTimeout(() => {
        if (queue.peek(o.workspaceId)?.requestId !== o.requestId) return;
        drop(o.workspaceId);
        void logWarn(`hive: tool setup ${o.requestId}'s sheet wasn't opened: its Hive view didn't show`);
      }, TAKE_WITHIN_MS);
    } catch (e) {
      logError(`hive: tool setup sheet not opened: ${e}`);
    } finally {
      flushing = false;
    }
  }

  function onFocus() {
    if (queue.size > 0 || !recovered) void flush();
  }

  return {
    /** The Hive view of `workspaceId`: its waiting opening, taken once and marked shown. */
    take(workspaceId: string): ToolSetupOpening | null {
      const o = queue.take(workspaceId);
      if (o) persist();
      return o;
    },

    /** The end of an install Rust started from the phone, heard after its opening was taken and
     *  before its sheet listened (HiveMediaSetupSheet asks once its own listener is up). */
    installEnd(tabId: string): HiveMediaInstallFinished | null {
      return queue.endOf(tabId);
    },

    async init() {
      if (started) return;
      started = true;
      for (const id of readOpened()) queue.spent.add(id);
      const target = getCurrentWindow().label;
      window.addEventListener('focus', onFocus);
      try {
        // Both are sent to this window only (`emit_to`); the target keeps another window's out.
        unlisteners.push(
          await countedListen<unknown>(APPROVED_EVENT, (e) => {
            const p = parseApproved(e.payload);
            if (!p) {
              void logWarn(`hive: ${APPROVED_EVENT} with a payload this build doesn't read`);
              return;
            }
            if (!inThisWindow(p.workspace_id)) {
              // One of this window's workspaces that can't show it now (a suspended Hive): its one
              // try is spent, so a reload doesn't open it later.
              if (workspacesStore.workspaces.some((w) => w.id === p.workspace_id)) {
                queue.spend(p.request_id);
                persist();
              }
              return;
            }
            queueOpening(openingFrom(p, Date.now()));
            if (p.by === 'desktop' || document.hasFocus()) void flush();
          }, { target }),
          await countedListen<HiveMediaInstallFinished>('hive-media-install-finished', (e) => {
            queue.installEnded(e.payload);
          }, { target }),
        );
      } catch (e) {
        logError(`hive: tool setup listeners not set up: ${e}`);
      }
      if (document.hasFocus()) void flush();
      else void recover();
    },

    destroy() {
      window.removeEventListener('focus', onFocus);
      for (const u of unlisteners.splice(0)) u();
      started = false;
    },
  };
}

export const hiveToolSetup = createHiveToolSetupStore();
