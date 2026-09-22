<script lang="ts">
  import '../app.css';
  import { onMount } from 'svelte';
  import { invoke } from '@tauri-apps/api/core';
  import { getCurrentWindow } from '@tauri-apps/api/window';
  import { countedListen as listen } from '$lib/utils/listenCounter';
  import { workspacesStore, navigateToTab } from '$lib/stores/workspaces.svelte';
  import { wakeTab, type WakeAction } from '$lib/agents/wake';
  import { terminalsStore } from '$lib/stores/terminals.svelte';
  import { retryDownBridgesNow } from '$lib/stores/sshMcpBridge.svelte';
  import ImportPreviewModal from '$lib/components/ImportPreviewModal.svelte';
  import ShareImportWizard from '$lib/components/share/ShareImportWizard.svelte';
  import { SHARE_EXTENSION } from '$lib/share/share';
  import Toast from '$lib/components/Toast.svelte';
  import { pruneHiddenDefaultTriggers, seedDefaultTriggers } from '$lib/triggers/defaults';
  import { preferencesStore } from '$lib/stores/preferences.svelte';
  import { getTheme, applyUiTheme } from '$lib/themes';
  import { error as logError, info as logInfo } from '@tauri-apps/plugin-log';
  import { attachConsole } from '@tauri-apps/plugin-log';
  import { onAction as onNotificationAction } from '@tauri-apps/plugin-notification';
  import * as commands from '$lib/tauri/commands';
  import type { ClaudeCodeToolRequest, Preferences, Tab, CommsBinding } from '$lib/tauri/types';
  import type { ImportPreview } from '$lib/tauri/commands';
  import { claudeCodeStore } from '$lib/stores/claudeCode.svelte';
  import { claudeStateStore } from '$lib/stores/agentState.svelte';
  import { stackStore } from '$lib/stores/stack.svelte';
  import { agentBridgeStore } from '$lib/stores/agentBridge.svelte';
  import { agentMeshStore } from '$lib/stores/agentMesh.svelte';
  import { agentDelivery } from '$lib/stores/agentDeliveryLive';
  import { toastStore } from '$lib/stores/toasts.svelte';
  import { navHistoryStore } from '$lib/stores/navHistory.svelte';
  import { pendingResumePanes } from '$lib/stores/resumeGate.svelte';
  import { isModKey, isMac, modSymbol } from '$lib/utils/platform';
  import { open as dialogOpen, save as dialogSave } from '@tauri-apps/plugin-dialog';
  import { openFileFromTerminal } from '$lib/utils/openFile';
  import { installGlobalSmartQuoteFix } from '$lib/utils/smartQuotes';
  import QuickOpen from '$lib/components/QuickOpen.svelte';
  import AgentBridgePicker from '$lib/components/AgentBridgePicker.svelte';
  import CommsMonitorModal from '$lib/components/CommsMonitorModal.svelte';
  import MeshCockpit from '$lib/components/MeshCockpit.svelte';
  import MeshSetupModal from '$lib/components/MeshSetupModal.svelte';
  import OverlordRuleChangeModal from '$lib/components/overlord/OverlordRuleChangeModal.svelte';
  import { detectLanguageFromPath, isImageFile, isPdfFile } from '$lib/utils/languageDetect';
  import { readFile } from '$lib/tauri/commands';
  import type { EditorFileInfo } from '$lib/tauri/types';
  // Side-effect import: subscribes to activity store for OS notifications
  import '$lib/stores/notifications.svelte';
  import { updaterStore } from '$lib/stores/updater.svelte';

  interface Props {
    children: import('svelte').Snippet;
  }

  let { children }: Props = $props();
  let showImportPreview = $state(false);
  /** Shared workspace files waiting for the import wizard, one at a time (docs/workspace-share.md §4). */
  /** Each entry gets its own id: the same file opened twice is two imports, and a `{#key}` on
   *  the path alone would keep the finished wizard up for the second. */
  let shareImportQueue = $state<{ id: number; path: string }[]>([]);
  let shareImportSeq = 0;
  const queueShareImports = (paths: string[]) => {
    shareImportQueue = [...shareImportQueue, ...paths.map(path => ({ id: ++shareImportSeq, path }))];
  };
  let importPreview = $state<ImportPreview | null>(null);
  let importFilePath = $state('');
  let showQuickOpen = $state(false);
  let showAgentBridgePicker = $state(false);
  let commsMonitorTarget = $state<{ workspaceId: string; paneId: string; tabId: string } | null>(null);
  let showMeshCockpit = $state(false);
  let meshSetupWorkspaceId = $state<string | null>(null);
  let agentBridgeCallerTabId = $state<string | null>(null);

  // Cmd+W two-press confirmation: first press arms closeConfirmTabId for 2s,
  // a second press while armed (on the same tab) actually closes.
  let closeConfirmTabId = $state<string | null>(null);
  let closeConfirmTimer: ReturnType<typeof setTimeout> | null = null;

  function clearCloseConfirm() {
    if (closeConfirmTimer) {
      clearTimeout(closeConfirmTimer);
      closeConfirmTimer = null;
    }
    closeConfirmTabId = null;
  }

  function armCloseConfirm(tabId: string) {
    if (closeConfirmTimer) clearTimeout(closeConfirmTimer);
    closeConfirmTabId = tabId;
    closeConfirmTimer = setTimeout(() => {
      closeConfirmTabId = null;
      closeConfirmTimer = null;
    }, 2000);
  }

  // Apply UI theme reactively (runs outside onMount so it reacts to changes)
  $effect(() => {
    const t = getTheme(preferencesStore.theme, preferencesStore.customThemes);
    applyUiTheme(t.ui);
  });

  // Apply UI font size reactively
  $effect(() => {
    document.documentElement.style.setProperty('--ui-font-size', `${preferencesStore.uiFontSize}px`);
  });

  // Update OS-level window title (Mission Control, Cmd+Tab, etc.) — the window's own name
  // when it has one, else the active workspace, same fallback as the in-app titlebar.
  $effect(() => {
    const name = workspacesStore.windowName ?? workspacesStore.activeWorkspace?.name;
    if (!name) return;
    const suffix = import.meta.env.DEV ? ' (Dev)' : '';
    getCurrentWindow().setTitle(`maiTerm | ${name}${suffix}`);
  });

  // Scheduled backup timer lives in Rust now (commands/scheduler.rs) so it
  // keeps firing even when this webview hangs. Manual "Backup now" buttons
  // and the Claude Code MCP createBackup tool still go through the existing
  // run_scheduled_backup / trim_old_backups commands.

  onMount(() => {
    // Attach console for dev mode (Rust logs appear in browser devtools)
    let detachConsole: (() => void) | undefined;
    attachConsole().then(detach => { detachConsole = detach; });

    // Global webview error capture — these route through tauri-plugin-log
    // so they land in aiterm.log alongside Rust errors. Tagged [WEBVIEW_ERROR]
    // for grep, since a JS error often immediately precedes a WebKit
    // renderer crash and is otherwise invisible once the window dies.
    function formatErrorPayload(label: string, detail: unknown, stack?: string): string {
      let message: string;
      if (detail instanceof Error) {
        message = `${detail.name}: ${detail.message}`;
        stack = stack ?? detail.stack;
      } else if (typeof detail === 'string') {
        message = detail;
      } else {
        try { message = JSON.stringify(detail); } catch { message = String(detail); }
      }
      const stackLine = stack ? `\n${stack}` : '';
      return `[WEBVIEW_ERROR] ${label}: ${message}${stackLine}`;
    }

    const onWindowError = (e: ErrorEvent) => {
      const where = e.filename ? ` @ ${e.filename}:${e.lineno}:${e.colno}` : '';
      logError(formatErrorPayload(`onerror${where}`, e.error ?? e.message, e.error?.stack))
        .catch(() => {});
    };
    // A save that can't reach disk makes every button look broken — the click fires,
    // the command rejects, and nothing visibly happens. Say so, instead of leaving the
    // user to conclude the app has locked up.
    //
    // Deliberately NOT dispatch(): that honours notification_mode, so 'disabled' (or
    // 'native' with OS permission denied) would show nothing at all. This is a
    // state-loss condition, not a routine notification. Re-armed on a cooldown rather
    // than shown once, because the toast auto-dismisses while the failures continue.
    const STATE_SAVE_WARN_COOLDOWN_MS = 60_000;
    let lastStateSaveWarnAt = 0;
    const onUnhandledRejection = (e: PromiseRejectionEvent) => {
      const reason = e.reason as unknown;
      const stack = (reason && typeof reason === 'object' && 'stack' in reason)
        ? String((reason as { stack?: unknown }).stack ?? '')
        : undefined;
      logError(formatErrorPayload('unhandledrejection', reason, stack)).catch(() => {});

      const message = reason instanceof Error ? reason.message : String(reason);
      if (
        message.includes('State conflict detected') &&
        Date.now() - lastStateSaveWarnAt > STATE_SAVE_WARN_COOLDOWN_MS
      ) {
        lastStateSaveWarnAt = Date.now();
        toastStore.addToast(
          'Changes are not being saved',
          'Another maiTerm is writing the same state file, so this window has stopped saving. Quit the other one, then restart this window.',
          'error'
        );
      }
    };
    window.addEventListener('error', onWindowError);
    window.addEventListener('unhandledrejection', onUnhandledRejection);

    // Disable default browser context menu globally, except in notes panel
    // where native cut/copy/paste is useful.
    // To re-enable in dev for Inspect Element, change to: if (!import.meta.env.DEV)
    document.addEventListener('contextmenu', (e) => {
      if ((e.target as Element)?.closest?.('.notes-panel')) return;
      e.preventDefault();
    }, true);

    // Strip macOS smart-quote substitution from all text inputs/textareas
    // app-wide, so straight quotes typed into search/rename/notes/preferences
    // fields aren't silently turned into curly quotes that break code search.
    const cleanupSmartQuotes = installGlobalSmartQuoteFix();

    // Load preferences and clean up stale default triggers
    preferencesStore.load().then(() => {
      const seeded = seedDefaultTriggers(preferencesStore.triggers, preferencesStore.hiddenDefaultTriggers);
      if (seeded) preferencesStore.setTriggers(seeded);
      // Forget deletions of templates that have since been retired — they agree, and the
      // record is what is stale now (see pruneHiddenDefaultTriggers).
      const prunedHidden = pruneHiddenDefaultTriggers(preferencesStore.hiddenDefaultTriggers);
      if (prunedHidden) preferencesStore.setHiddenDefaultTriggers(prunedHidden);

      // Auto-check for updates on startup (silent — only shows toast if update found)
      if (preferencesStore.autoCheckUpdates) {
        updaterStore.checkForUpdates(true).catch(() => {});
      }
    }).catch((e: unknown) => logError(`Failed to load preferences: ${e}`));

    // Listen for cross-window preference changes
    let unlistenPrefs: (() => void) | undefined;
    listen<Preferences>('preferences-changed', (event) => {
      preferencesStore.applyFromBackend(event.payload);
    }).then(unlisten => { unlistenPrefs = unlisten; });

    const appWindow = getCurrentWindow();

    // Non-terminal windows (e.g. preferences) skip terminal lifecycle and shortcuts
    if (appWindow.label === 'preferences' || appWindow.label === 'help') {
      return () => {
        window.removeEventListener('error', onWindowError);
        window.removeEventListener('unhandledrejection', onUnhandledRejection);
        cleanupSmartQuotes();
        unlistenPrefs?.();
        detachConsole?.();
      };
    }

    // Listen for app-wide quit (Cmd+Q / Quit menu).
    // All windows save scrollback, then exit — no window data is removed.
    let unlistenQuit: (() => void) | undefined;
    listen('quit-requested', async () => {
      logInfo('quit-requested — saving scrollback before exit');
      // Save window geometry before exit (don't wait for debounce). Skipped while the
      // displays are asleep — the geometry then isn't the one the user arranged.
      clearTimeout(geometryTimer);
      if (currentMonitorCount !== null && !displaysAsleep) {
        await commands.saveWindowGeometry(currentMonitorCount).catch(() => {});
      }
      await terminalsStore.saveAllScrollback();
      try {
        await invoke('sync_state');
      } catch (e) {
        logError(`sync_state failed: ${e}`);
      }
      await invoke('exit_app');
    }).then(unlisten => { unlistenQuit = unlisten; });

    // File ▸ Duplicate Window. Clicking the menu item produces no keydown, so the
    // Cmd+Shift+N handler below never sees it — Rust emits this to us instead.
    // The target is load-bearing, not decoration: a listener registered without one
    // is EventTarget::Any, which matches every emit_to filter — so leaving it off
    // makes one Duplicate Window click duplicate every open window.
    let unlistenDuplicateWindow: (() => void) | undefined;
    listen('duplicate-window', () => {
      workspacesStore.duplicateWindow();
    }, { target: appWindow.label }).then(unlisten => { unlistenDuplicateWindow = unlisten; });

    // Pause toast timers when window loses focus, resume on focus
    let unlistenFocus: (() => void) | undefined;
    appWindow.onFocusChanged(({ payload: focused }) => {
      toastStore.setWindowFocused(focused);
    }).then(unlisten => { unlistenFocus = unlisten; });

    // Save window geometry per monitor count on resize/move (debounced).
    // Polls for monitor changes to auto-reposition windows when docking/undocking.
    //
    // Zero monitors is NOT a display configuration. macOS reports every screen gone
    // while the displays sleep or the lock screen is up (a Studio Display setup goes
    // straight 2 → 0), and reports them back on wake. Treating that as a dock/undock
    // saved the awake layout under a "0" key and then moved and RESIZED every window
    // to a phantom layout while the webview was occluded — which refits every terminal
    // in it, and an idle tab has nothing to repaint from (xterm here keeps no
    // scrollback of its own, and frames only arrive when the PTY writes), so it came
    // back blank. While the count reads 0 we hold everything: no save, no restore, and
    // no geometry writes from the window's own resize/move events either, so a shuffle
    // by macOS can't overwrite the layout we want back on wake.
    let currentMonitorCount: number | null = null;
    let displaysAsleep = false;
    // The monitor count this window's current rect actually describes — set when we place
    // the window for a count, and when the user's own move/resize is persisted under one.
    // A count we merely READ is not that: the displays can come back one at a time, and
    // between those ticks the window is still sitting wherever a dark launch or macOS left
    // it. Saving that rect under the count we happened to read would file a layout the
    // user never arranged.
    let placedFor: number | null = null;
    let geometryTimer: ReturnType<typeof setTimeout> | undefined;
    let monitorPollTimer: ReturnType<typeof setInterval> | undefined;

    // An IPC failure is an absence, not a count — hold, same as 0.
    const readMonitorCount = () => commands.getMonitorCount().catch(() => 0);

    // Initialize monitor count (0 → leave it unknown until the displays are back)
    readMonitorCount().then(async count => {
      if (count > 0) {
        currentMonitorCount = count;
        // Place the window from our own per-monitor-count map on EVERY start, not just
        // one we can tell was dark. Rust places windows in setup(), but only when it
        // could read a count, and the displays can come back during webview boot — so
        // "did Rust place this window?" is not something a second reading of the monitor
        // count can answer. Asking unconditionally makes it moot: it re-applies what Rust
        // just applied on a lit start, and repairs a dark one. It is also the only thing
        // that ever gives "main" its per-monitor-count geometry — its launch position
        // comes from tauri-plugin-window-state, which is monitor-blind.
        // `false` means this monitor count has no saved layout yet (a first run, a new
        // display setup) — the window is wherever the OS put it, which is nothing we
        // placed, so leave `placedFor` unset until the user arranges it.
        if (await commands.restoreWindowGeometry(count).catch(() => false)) placedFor = count;
      } else {
        displaysAsleep = true;
        logInfo('Started with no monitors (displays asleep or locked) — window geometry deferred until they return');
      }

      // Poll for monitor changes (handles dock/undock)
      monitorPollTimer = setInterval(async () => {
        const count = await readMonitorCount();
        if (count === 0) {
          if (!displaysAsleep) {
            displaysAsleep = true;
            logInfo('Monitor count is 0 (displays asleep or locked) — holding window geometry');
          }
          return;
        }
        const wasAsleep = displaysAsleep;
        displaysAsleep = false;
        // Bridge tunnels reaped while the displays were dark were this machine stalling,
        // not the peers dying — bring their rebuilds forward instead of waiting out backoff.
        if (wasAsleep) retryDownBridgesNow('displays back');
        if (currentMonitorCount === null) {
          currentMonitorCount = count;
          // First displays this window has ever seen: it launched into the dark, so
          // nothing has placed it yet. Do it now, before the debounced save can write the
          // position macOS chose back under this (real) monitor count.
          logInfo(`Displays back (${count}) after starting in the dark — restoring saved geometry`);
          if (await commands.restoreWindowGeometry(count).catch(() => false)) placedFor = count;
          return;
        }
        if (count === currentMonitorCount) {
          // Same displays as before the sleep — but macOS may have shuffled the window
          // around while they were gone, so put it back where this layout had it.
          if (wasAsleep && await commands.restoreWindowGeometry(count).catch(() => false)) {
            placedFor = count;
          }
          return;
        }
        const oldCount = currentMonitorCount;
        currentMonitorCount = count;
        logInfo(`Monitor count changed: ${oldCount} → ${count}, repositioning window`);
        // Save current position under the old monitor count before repositioning —
        // but only when we watched the change happen. If the displays changed while
        // they were asleep (unplugged the external overnight), macOS has already
        // relocated this window onto what's left, so its position now describes the
        // NEW configuration; saving it under the old count would overwrite the
        // arrangement we want back when those displays return.
        // The same applies to a count we only read: macOS brings the displays back one at
        // a time, so an intermediate tick can adopt a count while the window still sits
        // where a dark launch left it. `placedFor` is what makes the difference visible —
        // only a rect we placed, or one the user arranged and we saved, describes a layout
        // worth keeping.
        if (!wasAsleep && placedFor === oldCount) await commands.saveWindowGeometry(oldCount).catch(() => {});
        // Restore saved geometry for the new monitor count (if any)
        if (await commands.restoreWindowGeometry(count).catch(() => false)) placedFor = count;
      }, 2000);
    });

    function saveGeometryDebounced() {
      clearTimeout(geometryTimer);
      geometryTimer = setTimeout(async () => {
        if (currentMonitorCount === null) return;
        // Ask for the count here rather than trusting `displaysAsleep`: the events
        // that bring us here are macOS shuffling the window around the screens it is
        // putting to sleep, and they fire up to a full poll interval before the flag
        // flips — so most of them would slip past a flag check and persist a
        // sleep-time rect as the layout to restore on wake.
        if (await readMonitorCount() === 0) {
          displaysAsleep = true;
          return;
        }
        // A rect the user arranged under a real count describes that layout, whatever put
        // the window there originally.
        placedFor = currentMonitorCount;
        commands.saveWindowGeometry(currentMonitorCount).catch(() => {});
      }, 500);
    }
    let unlistenResize: (() => void) | undefined;
    let unlistenMove: (() => void) | undefined;
    appWindow.onResized(saveGeometryDebounced).then(u => { unlistenResize = u; });
    appWindow.onMoved(saveGeometryDebounced).then(u => { unlistenMove = u; });

    // Listen for reload-tab menu event — duplicate tab with same context, close old
    let unlistenReloadTab: (() => void) | undefined;
    listen('reload-tab', () => {
      const ws = workspacesStore.activeWorkspace;
      const pane = workspacesStore.activePane;
      const tab = workspacesStore.activeTab;
      if (ws && pane && tab) {
        workspacesStore.reloadTab(ws.id, pane.id, tab.id);
      }
    }, { target: appWindow.label }).then(unlisten => { unlistenReloadTab = unlisten; });

    // The Accounts pane switched the active account and the user asked to bring running tabs
    // across. An account is chosen when a tab's shell is exec'd, so the only way to move one is
    // to respawn it — which is what reloadTab does, keeping the tab, its name, cwd and
    // scrollback.
    //
    // Targeted at this window's label: a broadcast would make EVERY window reload its own tabs,
    // so "reload this workspace" would quietly become "reload everything".
    //
    // Only tabs with a live terminal are touched. A suspended or never-opened tab already starts
    // under the active account, so reloading it would spawn shells nobody asked for — and a
    // stack service tab is skipped outright, since restarting the user's dev server has nothing
    // to do with which account is active.
    let unlistenAccountReload: (() => void) | undefined;
    // Serialized ACROSS invocations, not just within one. `listen` does not await its callback,
    // so two requests to the same window — "Reload" on a workspace, then "Reload all" a second
    // later — ran two loops concurrently over overlapping tabs, which is exactly the state the
    // sequential loop below was written to prevent. Chaining onto the previous promise is what
    // makes it a queue rather than a race; an in-flight flag that dropped the second request
    // would silently ignore work the user asked for.
    let accountReloadChain: Promise<void> = Promise.resolve();
    listen<commands.AccountReloadRequest>(commands.ACCOUNT_RELOAD_TABS_EVENT, (event) => {
      accountReloadChain = accountReloadChain.then(() => runAccountReload(event.payload));
    }, { target: appWindow.label }).then(unlisten => { unlistenAccountReload = unlisten; });

    async function runAccountReload(payload: commands.AccountReloadRequest) {
      const wanted = payload.workspace_ids;

      // SNAPSHOT the ids first, then reload them ONE AT A TIME.
      //
      // Both halves are load-bearing, and getting either wrong destroys tabs. `reloadTab` is
      // index-based: it duplicates the tab, re-reads window data, and takes
      // `freshPane.tabs[sourceIndex + 1]` as the replacement. Fired concurrently, the indices
      // shift under each other, so it carries the old tab's state onto the WRONG replacement
      // and then deletes the original — observed losing two of three tabs in a workspace.
      // Iterating the live arrays is the same bug from the other end, since each reload
      // reorders the very list being walked.
      const targets: { wsId: string; paneId: string; tabId: string }[] = [];
      for (const ws of workspacesStore.workspaces) {
        if (wanted && !wanted.includes(ws.id)) continue;
        for (const pane of ws.panes) {
          for (const tab of pane.tabs) {
            // A service tab runs the user's dev server, not an agent — restarting it has
            // nothing to do with which account is active.
            if (tab.service_id) continue;
            // Only live shells need moving. Anything else already starts under the active
            // account, so reloading it would spawn a shell nobody asked for.
            if (!terminalsStore.get(tab.id)) continue;
            targets.push({ wsId: ws.id, paneId: pane.id, tabId: tab.id });
          }
        }
      }

      let reloaded = 0;
      for (const t of targets) {
        try {
          await workspacesStore.reloadTab(t.wsId, t.paneId, t.tabId);
          reloaded++;
        } catch (e) {
          // One failure must not strand the rest, nor the completion report. Logged to the log
          // FILE, not the console — a tab that failed to come back is exactly what someone goes
          // looking for in aiterm.log afterwards.
          logError(`[accounts] reload failed for tab ${t.tabId}: ${e}`);
        }
      }
      try {
        await commands.reportAccountReloadDone(payload.reply_to, payload.request_id, reloaded);
      } catch (e) {
        logError(`[accounts] reporting reload completion failed: ${e}`);
      }
    }

    // A maiLink phone renamed a tab — the backend already persisted it; sync the store so the
    // live tab strip reflects the new title without a reload.
    let unlistenTabRenamed: (() => void) | undefined;
    listen<{ tabId: string; name: string }>('mailink-tab-renamed', (event) => {
      workspacesStore.applyExternalRename(event.payload.tabId, event.payload.name);
    }).then(unlisten => { unlistenTabRenamed = unlisten; });

    // A maiLink phone tapped "Resume workspace" on a suspended workspace. Only the window that
    // owns the workspace acts — resumeWorkspace() no-ops when the id isn't a suspended workspace
    // here — so a global emit reaching every window is safe. This respawns the tabs that were live
    // at suspend (agents auto-resume + re-init), the same path as the desktop resume affordance.
    let unlistenResumeWorkspace: (() => void) | undefined;
    listen<{ workspaceId: string }>('mailink-resume-workspace', (event) => {
      workspacesStore.resumeWorkspace(event.payload.workspaceId);
    }).then(unlisten => { unlistenResumeWorkspace = unlisten; });

    // A maiLink phone tapped "Initialize all" on a mesh workspace. Same global-emit contract:
    // initializeMesh() no-ops unless this window owns a live (non-suspended) mesh with that id.
    let unlistenMeshInit: (() => void) | undefined;
    listen<{ workspaceId: string }>('mailink-mesh-init', (event) => {
      agentMeshStore.initializeMesh(event.payload.workspaceId);
    }).then(unlisten => { unlistenMeshInit = unlisten; });

    // maiLink tab-lifecycle actions. Each is tab-scoped and globally emitted; the store methods
    // no-op in windows that don't own the tab (archive/close resolve the live tab by id; restore
    // targets the archived tab's workspace). Same owning-window contract as resume/mesh-init.
    let unlistenArchiveTab: (() => void) | undefined;
    listen<{ tabId: string }>('mailink-archive-tab', (event) => {
      workspacesStore.archiveTabById(event.payload.tabId);
    }).then(unlisten => { unlistenArchiveTab = unlisten; });

    // A maiLink phone tapped "New conversation" on a thread. Light clone of WHERE the source
    // runs (SSH host + cwd) with a fresh agent session — no scrollback, notes or session id.
    // Same owning-window contract: newConversationFrom no-ops when this window lacks the tab.
    let unlistenNewConversation: (() => void) | undefined;
    listen<{ tabId: string }>('mailink-new-conversation', (event) => {
      workspacesStore.newConversationFrom(event.payload.tabId);
    }).then(unlisten => { unlistenNewConversation = unlisten; });

    // maiLink is waking one unregistered tab — either explicitly (its Initialize button) or
    // implicitly, because a message is about to be delivered to it. The backend already probed
    // the process tree and decided the remedy; we only execute it. wakeTab no-ops in windows
    // that don't own the tab, and while a wake for it is already in flight.
    let unlistenWakeTab: (() => void) | undefined;
    listen<{ tabId: string; action: WakeAction; budgetMs: number }>('mailink-wake-tab', (event) => {
      void wakeTab(event.payload.tabId, event.payload.action, event.payload.budgetMs);
    }).then(unlisten => { unlistenWakeTab = unlisten; });

    let unlistenCloseTab: (() => void) | undefined;
    listen<{ tabId: string }>('mailink-close-tab', (event) => {
      workspacesStore.closeTabById(event.payload.tabId);
    }).then(unlisten => { unlistenCloseTab = unlisten; });

    let unlistenRestoreTab: (() => void) | undefined;
    listen<{ workspaceId: string; tabId: string }>('mailink-restore-tab', (event) => {
      workspacesStore.restoreArchivedTab(event.payload.workspaceId, event.payload.tabId);
    }).then(unlisten => { unlistenRestoreTab = unlisten; });

    // Check for updates menu event
    let unlistenCheckUpdates: (() => void) | undefined;
    listen('check-for-updates', () => {
      updaterStore.checkForUpdates(false);
    }, { target: appWindow.label }).then(unlisten => { unlistenCheckUpdates = unlisten; });

    // Periodic silent update check — the startup check only runs once, so a
    // long-running window would otherwise never notice a new release. Re-reads
    // the preference each tick so toggling auto-check off stops further checks.
    const UPDATE_CHECK_INTERVAL_MS = 60 * 60 * 1000;
    const updateCheckTimer = setInterval(() => {
      if (preferencesStore.autoCheckUpdates) {
        updaterStore.checkForUpdates(true).catch(() => {});
      }
    }, UPDATE_CHECK_INTERVAL_MS);

    // Window > Clear Back/Forward History menu event. Every menu-driven listener
    // below names its own label as the target: without one it registers as
    // EventTarget::Any, which matches every emit_to filter, and the menu click
    // lands in all windows at once.
    let unlistenClearNavHistory: (() => void) | undefined;
    listen('clear-nav-history', () => {
      navHistoryStore.clear();
    }, { target: appWindow.label }).then(unlisten => { unlistenClearNavHistory = unlisten; });

    // State backup menu events
    let unlistenExportState: (() => void) | undefined;
    listen('export_state', async () => {
      try {
        const path = await dialogSave({
          defaultPath: commands.backupFilename(),
          filters: [{ name: 'JSON', extensions: ['json'] }],
        });
        if (path) {
          await commands.exportState(path, preferencesStore.backupExcludeScrollback);
          logInfo(`State exported to ${path}`);
        }
      } catch (e) {
        logError(`Export state failed: ${e}`);
      }
    }, { target: appWindow.label }).then(unlisten => { unlistenExportState = unlisten; });

    let unlistenImportState: (() => void) | undefined;
    listen('import_state', async () => {
      try {
        const path = await dialogOpen({
          multiple: false,
          filters: [{ name: 'maiTerm Backup', extensions: ['json', 'gz'] }],
        });
        if (typeof path === 'string') {
          const preview = await commands.previewImport(path);
          importPreview = preview;
          importFilePath = path;
          showImportPreview = true;
        }
      } catch (e) {
        logError(`Import state failed: ${e}`);
      }
    }, { target: appWindow.label }).then(unlisten => { unlistenImportState = unlisten; });

    // Shared workspaces: File ▸ Import Shared Workspace…, and files opened from the OS. The OS
    // path queues in Rust and rings; drain on every ring and once now, since a cold launch
    // queued its file before this listener existed (docs/workspace-share.md §7).
    const drainShareOpens = () => {
      commands.takePendingShareOpens()
        .then(paths => { if (paths.length) queueShareImports(paths); })
        .catch(e => logError(`share: draining opened files failed: ${e}`));
    };
    let unlistenImportWorkspace: (() => void) | undefined;
    listen('import_workspace', async () => {
      try {
        const path = await dialogOpen({
          multiple: false,
          filters: [{ name: 'maiTerm Workspace', extensions: [SHARE_EXTENSION] }],
        });
        if (typeof path === 'string') queueShareImports([path]);
      } catch (e) {
        logError(`Import shared workspace failed: ${e}`);
      }
    }, { target: appWindow.label }).then(unlisten => { unlistenImportWorkspace = unlisten; });
    let unlistenShareOpened: (() => void) | undefined;
    listen('share-file-opened', drainShareOpens, { target: appWindow.label })
      .then(unlisten => { unlistenShareOpened = unlisten; drainShareOpens(); });

    let unlistenStateImported: (() => void) | undefined;
    listen('state-imported', () => {
      window.location.reload();
    }).then(unlisten => { unlistenStateImported = unlisten; });

    // Claude Code IDE integration event listeners.
    // Use appWindow.listen() (not global listen) — global listen catches both
    // window-targeted and global events in Tauri 2, causing duplicate callbacks.
    let unlistenClaudeTool: (() => void) | undefined;
    appWindow.listen<ClaudeCodeToolRequest>('agent-ide-tool', (event) => {
      claudeCodeStore.handleToolRequest(event.payload);
    }).then(unlisten => { unlistenClaudeTool = unlisten; });

    let unlistenClaudeConnection: (() => void) | undefined;
    appWindow.listen<{ connected: boolean }>('agent-ide-connection', (event) => {
      claudeCodeStore.setConnected(event.payload.connected);
    }).then(unlisten => { unlistenClaudeConnection = unlisten; });

    // Comms watcher: a bound thread got @bot replies but no agent session is
    // running to receive them — ring the operator (dispatch scopes to the
    // window owning the tab; clicking the toast deep-links to it).
    let unlistenCommsPending: (() => void) | undefined;
    appWindow.listen<{ tab_id: string; count: number; preview: string; reason?: string }>('comms-reply-pending', async (event) => {
      const { dispatch } = await import('$lib/stores/notificationDispatch');
      const p = event.payload;
      dispatch(
        'Thread reply waiting',
        `${p.count > 1 ? `${p.count} replies` : 'A reply'} arrived on a bound thread ("${p.preview}") but ${p.reason ?? 'no agent session is running in that tab'}. It will be delivered once that clears.`,
        'info',
        { tabId: p.tab_id },
      );
    }).then(unlisten => { unlistenCommsPending = unlisten; });

    // Backend-side binding changes (summon pickup, startCommsThread, bindCommsThread,
    // resolve/unbind) — the store owns its copy of the tab, so the `@` badge would
    // otherwise show whatever was bound at startup.
    let unlistenCommsBindings: (() => void) | undefined;
    appWindow.listen<{ tab_id: string; bindings: CommsBinding[] }>('comms-bindings-changed', (event) => {
      workspacesStore.applyCommsBindings(event.payload.tab_id, event.payload.bindings ?? []);
    }).then(unlisten => { unlistenCommsBindings = unlisten; });

    // maiLink (docs/mailink-protocol.md §13). A phone task edit lands in Rust and is announced
    // here so the tasks store replaces its copy instead of clobbering it on its next persist;
    // an Overlord action has to run in THIS window's engine, so Rust asks and we answer.
    let unlistenMailinkTasks: (() => void) | undefined;
    appWindow.listen<{ workspaceId: string; tasks: import('$lib/tauri/types').Task[]; workstreams: import('$lib/tauri/types').Workstream[] }>('mailink-tasks-changed', async (event) => {
      // App-wide event; only the window that HOLDS the workspace may take it — judged from the
      // workspaces store, not from whether the tasks store already has a key (it won't, for a
      // workspace created after boot, and that is exactly the row that must not be dropped).
      if (!workspacesStore.workspaces.some((w) => w.id === event.payload.workspaceId)) return;
      const { tasksStore } = await import('$lib/stores/tasks.svelte');
      tasksStore.applyFromBackend(event.payload.workspaceId, event.payload.tasks ?? [], event.payload.workstreams ?? []);
    }).then(unlisten => { unlistenMailinkTasks = unlisten; });
    let unlistenMailinkRequests: (() => void) | undefined;
    appWindow.listen<import('$lib/stores/mailinkRequests').MailinkRequest>('mailink-frontend-request', async (event) => {
      const { handleMailinkRequest } = await import('$lib/stores/mailinkRequests');
      await handleMailinkRequest(event.payload);
    }).then(unlisten => { unlistenMailinkRequests = unlisten; });

    // Chat-monitor summon events: pickups, queued summons, unauthorized attempts.
    let unlistenCommsSummon: (() => void) | undefined;
    appWindow.listen<{ tab_id: string; kind: string; channel: string; from: string; preview: string; reason?: string; reason_detail?: string }>('comms-summon', async (event) => {
      const { dispatch } = await import('$lib/stores/notificationDispatch');
      const p = event.payload;
      if (p.kind === 'picked_up') {
        dispatch('Thread picked up', `${p.from} summoned the bot in ${p.channel}: "${p.preview}"`, 'info', { tabId: p.tab_id });
      } else if (p.kind === 'queued') {
        // Two reasons need the OPERATOR to act — waiting achieves nothing: a full tab
        // needs a thread closed, and a tab whose agent is gone needs one started. The
        // rest do resolve on their own once the session is back, so promising "it will
        // be picked up when the tab frees up" is only true for those.
        const atCapacity = p.reason === 'at_capacity';
        const noAgent = p.reason === 'no_agent';
        const title = atCapacity
          ? 'Summon waiting — tab is full'
          : noAgent
            ? 'Summon waiting — no agent in that tab'
            : 'Summon queued';
        const outcome = atCapacity
          ? 'It stays queued until a thread is closed out.'
          : noAgent
            ? 'It stays queued — nothing is delivered into a bare shell — and lands as soon as an agent is running there.'
            : 'It will be picked up when the tab frees up.';
        dispatch(
          title,
          `${p.from} summoned the bot in ${p.channel}: "${p.preview}". ${
            p.reason_detail ? `${p.reason_detail[0].toUpperCase()}${p.reason_detail.slice(1)}.` : 'The monitoring tab is busy or offline.'
          } ${outcome}`,
          'info',
          { tabId: p.tab_id },
        );
      } else if (p.kind === 'unauthorized') {
        dispatch('Summon not allowed', `${p.from} @mentioned the bot in ${p.channel} but isn't on the pickup or authorized list: "${p.preview}". Nothing was posted in the thread.`, 'info', { tabId: p.tab_id });
      }
    }).then(unlisten => { unlistenCommsSummon = unlisten; });

    // Claude Code state tracking (hook events → per-tab Claude state)
    claudeStateStore.init();

    // Workspace stack (docs/stack.md): exit hooks, binding reconciliation, auto-start.
    stackStore.init();

    // Agent Bridge (hook events → cross-agent message delivery)
    agentBridgeStore.init();
    // Mesh Workspace (N:M agent bridging — docs/mesh-workspace.md)
    agentMeshStore.init();

    // OS notification click → deep-link to workspace+tab.
    // NOTE: onAction only fires on mobile (iOS/Android). On desktop (macOS/Linux/Windows),
    // tauri-plugin-notification uses notify_rust which is fire-and-forget with no click
    // callback. The extra.tabId and this listener are prep work for future mobile support.
    let unlistenNotificationAction: { unregister: () => Promise<void> } | undefined;
    onNotificationAction((notification) => {
      const tabId = (notification.extra as Record<string, unknown>)?.tabId;
      if (typeof tabId === 'string') {
        appWindow.setFocus();
        navigateToTab(tabId);
      }
    }).then(listener => { unlistenNotificationAction = listener; });

    // Handle single-window close (traffic light / Cmd+W on last tab+pane).
    let unlistenClose: (() => void) | undefined;

    (async () => {
      unlistenClose = await appWindow.onCloseRequested(async (event) => {
        event.preventDefault();
        logInfo('onCloseRequested fired — closing window');

        const count = await commands.getWindowCount();

        if (count <= 1 && isMac()) {
          // Last window on macOS: kill terminals and show empty state
          // (macOS convention: apps stay open with no windows)
          logInfo('Last window (macOS) — showing empty state');
          await terminalsStore.killAllTerminals();
          await commands.resetWindow();
          workspacesStore.reset();
        } else if (count <= 1) {
          // Last window on Windows/Linux: exit the app
          logInfo('Last window — exiting app');
          await terminalsStore.killAllTerminals();
          await invoke('exit_app');
        } else {
          // Not last window: kill PTYs, remove window data, destroy
          logInfo('Closing window (not last)');
          await terminalsStore.killAllTerminals();
          await commands.closeWindow();
          try {
            await invoke('sync_state');
          } catch (e) {
            logError(`sync_state failed: ${e}`);
          }
          try {
            await appWindow.destroy();
          } catch (e) {
            logError(`destroy() failed: ${e}`);
          }
        }
      });
    })();

    function isSuspendedTerminal(tab: Tab): boolean {
      const isTerminal = tab.tab_type === 'terminal' || !tab.tab_type;
      return isTerminal && !terminalsStore.get(tab.id) && !terminalsStore.isSpawning(tab.id);
    }

    /** What the keyboard can reach: the tabs in the strip, in strip order. Stack service
     *  tabs are not in the strip (docs/stack.md §7), so Cmd+1-9 must not spend a number on
     *  one and cycling must not land on one — there would be nothing to see. */
    function tabCycleList(tabs: Tab[]): Tab[] {
      const visible = tabs.filter(t => !t.service_id);
      if (!preferencesStore.groupActiveTabs) return visible;
      return visible.filter(t => !isSuspendedTerminal(t));
    }

    function cycleActiveTab(dir: 1 | -1) {
      const ws = workspacesStore.activeWorkspace;
      const pane = workspacesStore.activePane;
      if (!ws || !pane) return;
      const list = tabCycleList(pane.tabs);
      if (list.length < 2) return;
      const currentIndex = list.findIndex(t => t.id === pane.active_tab_id);
      let nextIndex: number;
      if (currentIndex === -1) {
        nextIndex = dir === 1 ? 0 : list.length - 1;
      } else {
        nextIndex = (currentIndex + dir + list.length) % list.length;
      }
      const target = list[nextIndex];
      if (isSuspendedTerminal(target)) pendingResumePanes.add(pane.id);
      workspacesStore.setActiveTab(ws.id, pane.id, target.id);
      terminalsStore.focusTerminal(target.id);
    }

    function handleKeydown(e: KeyboardEvent) {
      const isMeta = isModKey(e);
      const activeTabIsEditor = workspacesStore.activeTab?.tab_type === 'editor';
      const activeTabIsDiff = workspacesStore.activeTab?.tab_type === 'diff';

      // When the active tab is an editor or diff tab, let CodeMirror handle all
      // keyboard shortcuts EXCEPT app-level ones that don't conflict with editing.
      // App-level shortcuts that always apply: tab management (Cmd+T/W/1-9/Shift+[/]),
      // workspace/window management (Cmd+N/Shift+N), zoom (Cmd+=/-/0), preferences (Cmd+,),
      // open file (Cmd+O), sidebar (Cmd+B), notes (Cmd+E).
      // Everything else passes through to the editor.
      if (activeTabIsEditor || activeTabIsDiff) {
        if (isMeta) {
          const key = e.key.toLowerCase();
          const isAppShortcut =
            // Tab management
            (!e.shiftKey && !e.altKey && key === 't') ||             // Cmd+T new tab
            (e.shiftKey && key === 't') ||                           // Cmd+Shift+T duplicate tab
            (e.shiftKey && key === 'r') ||                           // Cmd+Shift+R reload tab
            (e.shiftKey && key === 'e') ||                           // Cmd+Shift+E tasks panel
            (key === 'w') ||                                         // Cmd+W close tab
            (!e.shiftKey && e.key >= '1' && e.key <= '9') ||         // Cmd+1-9 switch tab
            (e.shiftKey && (e.key === '[' || e.code === 'BracketLeft')) ||  // Cmd+Shift+[ prev tab
            (e.shiftKey && (e.key === ']' || e.code === 'BracketRight')) || // Cmd+Shift+] next tab
            (!e.shiftKey && (e.key === '[' || e.code === 'BracketLeft')) ||  // Cmd+[ nav back
            (!e.shiftKey && (e.key === ']' || e.code === 'BracketRight')) || // Cmd+] nav forward
            // Window/workspace management
            (!e.shiftKey && !e.altKey && key === 'n') ||             // Cmd+N new window
            (e.shiftKey && !e.altKey && key === 'n') ||              // Cmd+Shift+N duplicate window
            (e.altKey && e.code === 'KeyN') ||                       // Cmd+Opt+N new workspace
            // Zoom
            (e.key === '=' || e.key === '+') ||                      // Cmd+= zoom in
            (e.key === '-') ||                                       // Cmd+- zoom out
            (e.key === '0') ||                                       // Cmd+0 reset zoom
            // Other app-level
            (e.key === ',') ||                                       // Cmd+, preferences
            (!e.shiftKey && key === 'o') ||                          // Cmd+O open file
            (!e.shiftKey && key === 'b') ||                          // Cmd+B toggle sidebar
            (!e.shiftKey && key === 'e') ||                          // Cmd+E toggle notes
            (e.shiftKey && key === 'm');                             // Cmd+Shift+M mesh cockpit
          if (!isAppShortcut) return; // Let CodeMirror handle it
        } else if (e.altKey) {
          // Alt+Arrow keys etc — let editor handle
          return;
        }
      }

      // Cmd+Shift+R - Reload tab
      if (isMeta && e.shiftKey && e.key.toLowerCase() === 'r') {
        e.preventDefault();
        e.stopPropagation();
        const ws = workspacesStore.activeWorkspace;
        const pane = workspacesStore.activePane;
        const tab = workspacesStore.activeTab;
        if (ws && pane && tab) {
          workspacesStore.reloadTab(ws.id, pane.id, tab.id);
        }
        return;
      }

      // Cmd+Shift+T - Duplicate tab
      if (isMeta && e.shiftKey && e.key.toLowerCase() === 't') {
        e.preventDefault();
        e.stopPropagation();
        const ws = workspacesStore.activeWorkspace;
        const pane = workspacesStore.activePane;
        const tab = workspacesStore.activeTab;
        if (ws && pane && tab) {
          workspacesStore.duplicateTab(ws.id, pane.id, tab.id);
        }
        return;
      }

      // Cmd+T - New tab
      if (isMeta && !e.shiftKey && e.key.toLowerCase() === 't') {
        e.preventDefault();
        e.stopPropagation();
        const ws = workspacesStore.activeWorkspace;
        const pane = workspacesStore.activePane;
        if (ws && pane) {
          // Visible tabs only — hidden service tabs would make the next one "Terminal 5"
          // in a strip showing one (docs/stack.md §7).
          const count = pane.tabs.filter(t => !t.service_id).length + 1;
          workspacesStore.createTab(ws.id, pane.id, `Terminal ${count}`);
        }
        return;
      }

      // Cmd+D - Split pane right (horizontal), cloning context
      if (isMeta && !e.shiftKey && e.key.toLowerCase() === 'd') {
        e.preventDefault();
        e.stopPropagation();
        const ws = workspacesStore.activeWorkspace;
        const pane = workspacesStore.activePane;
        const tab = workspacesStore.activeTab;
        if (ws && pane && tab) {
          workspacesStore.splitPaneWithContext(ws.id, pane.id, tab.id, 'horizontal');
        }
        return;
      }

      // Cmd+Shift+D - Split pane down (vertical), cloning context
      if (isMeta && e.shiftKey && e.key.toLowerCase() === 'd') {
        e.preventDefault();
        e.stopPropagation();
        const ws = workspacesStore.activeWorkspace;
        const pane = workspacesStore.activePane;
        const tab = workspacesStore.activeTab;
        if (ws && pane && tab) {
          workspacesStore.splitPaneWithContext(ws.id, pane.id, tab.id, 'vertical');
        }
        return;
      }

      // Cmd+Shift+N - Duplicate window
      if (isMeta && e.shiftKey && e.key.toLowerCase() === 'n') {
        e.preventDefault();
        e.stopPropagation();
        workspacesStore.duplicateWindow();
        return;
      }

      // Cmd+N - New window
      if (isMeta && !e.shiftKey && !e.altKey && e.key.toLowerCase() === 'n') {
        e.preventDefault();
        e.stopPropagation();
        commands.createNewWindow();
        return;
      }

      // Cmd+Opt+Shift+N - Duplicate workspace
      if (isMeta && e.altKey && e.shiftKey && e.code === 'KeyN') {
        e.preventDefault();
        e.stopPropagation();
        const ws = workspacesStore.activeWorkspace;
        if (ws) {
          const idx = workspacesStore.workspaces.findIndex(w => w.id === ws.id);
          workspacesStore.duplicateWorkspace(ws.id, idx + 1);
        }
        return;
      }

      // Cmd+Opt+N - New workspace (use e.code because Opt+N produces ˜ on macOS)
      if (isMeta && e.altKey && e.code === 'KeyN') {
        e.preventDefault();
        e.stopPropagation();
        const count = workspacesStore.workspaces.length + 1;
        workspacesStore.createWorkspace(`Workspace ${count}`);
        return;
      }

      // Cmd+Opt+R - Replay auto-resume (handled in TerminalPane, prevent browser reload)
      // Cmd+R - Auto-resume toggle (handled in TerminalPane, prevent browser reload)
      if (isMeta && !e.shiftKey && e.key.toLowerCase() === 'r') {
        e.preventDefault();
        return;
      }

      // Cmd+P - Quick Open file search
      if (isMeta && !e.shiftKey && e.key.toLowerCase() === 'p') {
        e.preventDefault();
        e.stopPropagation();
        if (!showQuickOpen) showQuickOpen = true;
        return;
      }

      // Cmd+Shift+L - Connect this agent to another (Agent Bridge)
      if (isMeta && e.shiftKey && e.key.toLowerCase() === 'l') {
        e.preventDefault();
        e.stopPropagation();
        const tab = workspacesStore.activeTab;
        agentBridgeCallerTabId = tab?.tab_type === 'terminal' ? tab.id : null;
        if (!showAgentBridgePicker) showAgentBridgePicker = true;
        return;
      }

      // Cmd+O - Open file in editor tab
      if (isMeta && !e.shiftKey && e.key.toLowerCase() === 'o') {
        e.preventDefault();
        e.stopPropagation();
        const ws = workspacesStore.activeWorkspace;
        const pane = workspacesStore.activePane;
        if (ws && pane) {
          // Default to active terminal's local CWD if available
          const activeTab = workspacesStore.activeTab;
          const instance = activeTab && activeTab.tab_type !== 'editor' ? terminalsStore.get(activeTab.id) : null;
          const ptyInfoP = instance ? commands.getPtyInfo(instance.ptyId).catch(() => null) : Promise.resolve(null);
          ptyInfoP.then(ptyInfo => dialogOpen({
            multiple: false,
            directory: false,
            title: 'Open File',
            defaultPath: ptyInfo?.cwd ?? undefined,
          })).then(async (selected) => {
            if (!selected) return;
            const filePath = selected;
            const fileName = filePath.split('/').pop() ?? filePath;
            const language = detectLanguageFromPath(filePath);
            // Validate the file can be read before creating the tab
            // Skip for images and PDFs — they use readFileBase64 in EditorPane
            if (!isImageFile(filePath) && !isPdfFile(filePath)) {
              try {
                await readFile(filePath);
              } catch (err) {
                const { dispatch } = await import('$lib/stores/notificationDispatch');
                dispatch('Cannot open file', String(err), 'error');
                return;
              }
            }
            const fileInfo: EditorFileInfo = {
              file_path: filePath,
              is_remote: false,
              remote_ssh_command: null,
              remote_path: null,
              language,
            };
            workspacesStore.createEditorTab(ws.id, pane.id, fileName, fileInfo);
          });
        }
        return;
      }

      // Cmd+S - Prevent browser save dialog (editor tabs already passed through above)
      if (isMeta && !e.shiftKey && e.key.toLowerCase() === 's') {
        e.preventDefault();
        return;
      }

      // Cmd+W - Close current tab (or pane if last tab). Terminal tabs require
      // two presses within 2s to prevent accidental close; editor/diff tabs close
      // on the first press since their state is recoverable from disk.
      if (isMeta && e.key.toLowerCase() === 'w') {
        e.preventDefault();
        e.stopPropagation();
        const ws = workspacesStore.activeWorkspace;
        const pane = workspacesStore.activePane;
        const tab = workspacesStore.activeTab;
        if (!ws || !pane || !tab) return;
        if (tab.tab_type === 'terminal' && closeConfirmTabId !== tab.id) {
          armCloseConfirm(tab.id);
          return;
        }
        clearCloseConfirm();
        workspacesStore.closeTabOrPane(ws.id, pane.id, tab.id);
        return;
      }

      // Cmd+1-9 - Switch tabs
      if (isMeta && e.key >= '1' && e.key <= '9') {
        e.preventDefault();
        const index = parseInt(e.key) - 1;
        const ws = workspacesStore.activeWorkspace;
        const pane = workspacesStore.activePane;
        if (ws && pane) {
          const list = tabCycleList(pane.tabs);
          const target = list[index];
          if (target) {
            if (isSuspendedTerminal(target)) pendingResumePanes.add(pane.id);
            workspacesStore.setActiveTab(ws.id, pane.id, target.id);
            terminalsStore.focusTerminal(target.id);
          }
        }
        return;
      }

      // Cmd+[ - Navigate back in tab history
      if (isMeta && !e.shiftKey && (e.key === '[' || e.code === 'BracketLeft')) {
        e.preventDefault();
        e.stopPropagation();
        navHistoryStore.goBack();
        return;
      }

      // Cmd+] - Navigate forward in tab history
      if (isMeta && !e.shiftKey && (e.key === ']' || e.code === 'BracketRight')) {
        e.preventDefault();
        e.stopPropagation();
        navHistoryStore.goForward();
        return;
      }

      // Cmd+Shift+[ - Previous tab (no nav history push — tab bar cycling is separate from history)
      if (isMeta && e.shiftKey && (e.key === '[' || e.code === 'BracketLeft')) {
        e.preventDefault();
        e.stopPropagation();
        cycleActiveTab(-1);
        return;
      }

      // Cmd+Shift+] - Next tab (no nav history push — tab bar cycling is separate from history)
      if (isMeta && e.shiftKey && (e.key === ']' || e.code === 'BracketRight')) {
        e.preventDefault();
        e.stopPropagation();
        cycleActiveTab(1);
        return;
      }

      // Cmd+K - Clear terminal and scrollback
      if (isMeta && !e.shiftKey && e.key.toLowerCase() === 'k') {
        e.preventDefault();
        e.stopPropagation();
        const tab = workspacesStore.activeTab;
        if (tab) {
          terminalsStore.clearTerminal(tab.id);
        }
        return;
      }

      // Cmd+F - Find in terminal
      if (isMeta && !e.shiftKey && e.key.toLowerCase() === 'f') {
        e.preventDefault();
        e.stopPropagation();
        const tab = workspacesStore.activeTab;
        if (tab) {
          terminalsStore.toggleSearch(tab.id);
        }
        return;
      }

      // Cmd+= / Cmd++ - Zoom in
      if (isMeta && (e.key === '=' || e.key === '+')) {
        e.preventDefault();
        e.stopPropagation();
        preferencesStore.setFontSize(preferencesStore.fontSize + 1);
        return;
      }

      // Cmd+- - Zoom out
      if (isMeta && e.key === '-') {
        e.preventDefault();
        e.stopPropagation();
        preferencesStore.setFontSize(preferencesStore.fontSize - 1);
        return;
      }

      // Cmd+0 - Reset zoom
      if (isMeta && e.key === '0') {
        e.preventDefault();
        e.stopPropagation();
        preferencesStore.setFontSize(13);
        return;
      }

      // Cmd+/ or Cmd+? - Show help
      if (isMeta && (e.key === '/' || e.key === '?' || e.code === 'Slash')) {
        e.preventDefault();
        e.stopPropagation();
        commands.openHelpWindow();
        return;
      }

      // Cmd+E - Toggle notes panel
      if (isMeta && !e.shiftKey && e.key.toLowerCase() === 'e') {
        e.preventDefault();
        e.stopPropagation();
        const tab = workspacesStore.activeTab;
        if (tab) {
          workspacesStore.toggleNotes(tab.id);
        }
        return;
      }

      // Cmd+Shift+E - Toggle tasks panel (docs/tasks.md §6)
      if (isMeta && e.shiftKey && !e.altKey && e.key.toLowerCase() === 'e') {
        e.preventDefault();
        e.stopPropagation();
        const tab = workspacesStore.activeTab;
        if (tab) {
          workspacesStore.toggleTasks(tab.id);
        }
        return;
      }

      // Cmd+Shift+C - Toggle composer dock
      if (isMeta && e.shiftKey && !e.altKey && e.key.toLowerCase() === 'c') {
        e.preventDefault();
        e.stopPropagation();
        const tab = workspacesStore.activeTab;
        if (tab && tab.tab_type === 'terminal') {
          workspacesStore.toggleComposer(tab.id);
        }
        return;
      }

      // Cmd+B - Toggle sidebar
      if (isMeta && !e.shiftKey && e.key.toLowerCase() === 'b') {
        e.preventDefault();
        e.stopPropagation();
        workspacesStore.toggleSidebar();
        return;
      }

      // Cmd+Shift+M - Toggle the Mesh cockpit drawer
      if (isMeta && e.shiftKey && e.key.toLowerCase() === 'm') {
        e.preventDefault();
        e.stopPropagation();
        showMeshCockpit = !showMeshCockpit;
        return;
      }

      // Cmd+, - Open preferences window
      if (isMeta && e.key === ',') {
        e.preventDefault();
        e.stopPropagation();
        commands.openPreferencesWindow();
        return;
      }
    }

    // Double-Alt detection for Quick Open
    let lastAltUp = 0;
    let altPressClean = true;

    function handleKeydownAlt(e: KeyboardEvent) {
      // If any non-Alt key is pressed while Alt is held, mark as dirty
      if (e.altKey && e.key !== 'Alt') {
        altPressClean = false;
      }
    }

    function handleKeyupAlt(e: KeyboardEvent) {
      if (e.key !== 'Alt') return;
      if (!altPressClean) {
        altPressClean = true;
        return;
      }
      const now = Date.now();
      if (now - lastAltUp < 400) {
        lastAltUp = 0;
        if (!showQuickOpen) showQuickOpen = true;
      } else {
        lastAltUp = now;
      }
      altPressClean = true;
    }

    // Agent Bridge picker opened from the terminal context menu ("Connect to Agent…")
    const onOpenAgentBridgePicker = (e: Event) => {
      const tabId = (e as CustomEvent<{ tabId: string }>).detail?.tabId ?? null;
      agentBridgeCallerTabId = tabId;
      if (!showAgentBridgePicker) showAgentBridgePicker = true;
    };
    window.addEventListener('open-agent-bridge-picker', onOpenAgentBridgePicker);

    // Mesh cockpit opened from the workspace sidebar badge / titlebar button.
    const onOpenMeshCockpit = () => { showMeshCockpit = true; };
    window.addEventListener('open-mesh-cockpit', onOpenMeshCockpit);

    // Chat-monitoring modal opened from the tab context menu.
    const onOpenCommsMonitor = (e: Event) => {
      commsMonitorTarget = (e as CustomEvent<{ workspaceId: string; paneId: string; tabId: string }>).detail ?? null;
    };
    window.addEventListener('open-comms-monitor', onOpenCommsMonitor);

    // Mesh pre-flight setup modal, opened from the cockpit's Enable Mesh button.
    const onOpenMeshSetup = (e: Event) => { meshSetupWorkspaceId = (e as CustomEvent<string>).detail ?? null; };
    window.addEventListener('open-mesh-setup', onOpenMeshSetup);

    window.addEventListener('keydown', handleKeydown, true);
    window.addEventListener('keydown', handleKeydownAlt, true);
    window.addEventListener('keyup', handleKeyupAlt, true);

    return () => {
      window.removeEventListener('open-agent-bridge-picker', onOpenAgentBridgePicker);
      window.removeEventListener('open-mesh-cockpit', onOpenMeshCockpit);
      window.removeEventListener('open-comms-monitor', onOpenCommsMonitor);
      window.removeEventListener('open-mesh-setup', onOpenMeshSetup);
      window.removeEventListener('keydown', handleKeydown, true);
      window.removeEventListener('keydown', handleKeydownAlt, true);
      window.removeEventListener('keyup', handleKeyupAlt, true);
      unlistenClose?.();
      unlistenQuit?.();
      unlistenDuplicateWindow?.();
      unlistenReloadTab?.();
      unlistenAccountReload?.();
      unlistenTabRenamed?.();
      unlistenResumeWorkspace?.();
      unlistenMeshInit?.();
      unlistenArchiveTab?.();
      unlistenNewConversation?.();
      unlistenWakeTab?.();
      unlistenCloseTab?.();
      unlistenRestoreTab?.();
      unlistenExportState?.();
      unlistenImportState?.();
      unlistenImportWorkspace?.();
      unlistenShareOpened?.();
      unlistenStateImported?.();
      unlistenCheckUpdates?.();
      unlistenClearNavHistory?.();
      unlistenClaudeTool?.();
      unlistenClaudeConnection?.();
      unlistenCommsPending?.();
      unlistenCommsSummon?.();
      unlistenCommsBindings?.();
      unlistenMailinkTasks?.();
      unlistenMailinkRequests?.();
      claudeStateStore.destroy();
      stackStore.destroy();
      agentBridgeStore.destroy();
      agentMeshStore.destroy();
      agentDelivery.destroy(); // the mailbox both of them share
      unlistenNotificationAction?.unregister();
      unlistenFocus?.();
      unlistenResize?.();
      unlistenMove?.();
      clearTimeout(geometryTimer);
      clearInterval(monitorPollTimer);
      clearInterval(updateCheckTimer);
      if (closeConfirmTimer) clearTimeout(closeConfirmTimer);
      window.removeEventListener('error', onWindowError);
      window.removeEventListener('unhandledrejection', onUnhandledRejection);
      cleanupSmartQuotes();
      unlistenPrefs?.();
      detachConsole?.();
    };
  });
</script>

{@render children()}

<ImportPreviewModal
  open={showImportPreview}
  preview={importPreview}
  filePath={importFilePath}
  onclose={() => { showImportPreview = false; }}
  onimported={() => { showImportPreview = false; window.location.reload(); }}
/>
{#if shareImportQueue.length > 0}
  {#key shareImportQueue[0].id}
    <ShareImportWizard path={shareImportQueue[0].path} onclose={() => { shareImportQueue = shareImportQueue.slice(1); }} />
  {/key}
{/if}
<QuickOpen
  open={showQuickOpen}
  onclose={() => {
    showQuickOpen = false;
    const tab = workspacesStore.activeTab;
    if (tab?.tab_type === 'terminal') terminalsStore.focusTerminal(tab.id);
  }}
  onselect={(filePath) => {
    showQuickOpen = false;
    const ws = workspacesStore.activeWorkspace;
    const pane = workspacesStore.activePane;
    const tab = workspacesStore.activeTab;
    if (ws && pane && tab && tab.tab_type === 'terminal') {
      openFileFromTerminal(ws.id, pane.id, tab.id, filePath);
    } else if (ws && pane) {
      // Find a terminal tab in pane for context
      const termTab = pane.tabs.find(t => t.tab_type === 'terminal');
      if (termTab) {
        openFileFromTerminal(ws.id, pane.id, termTab.id, filePath);
      }
    }
  }}
/>
<AgentBridgePicker
  open={showAgentBridgePicker}
  callerTabId={agentBridgeCallerTabId}
  onclose={() => {
    showAgentBridgePicker = false;
    const tab = workspacesStore.activeTab;
    if (tab?.tab_type === 'terminal') terminalsStore.focusTerminal(tab.id);
  }}
/>
<CommsMonitorModal
  open={commsMonitorTarget !== null}
  workspaceId={commsMonitorTarget?.workspaceId ?? null}
  paneId={commsMonitorTarget?.paneId ?? null}
  tabId={commsMonitorTarget?.tabId ?? null}
  onclose={() => { commsMonitorTarget = null; }}
/>
<MeshCockpit
  open={showMeshCockpit}
  onclose={() => {
    showMeshCockpit = false;
    const tab = workspacesStore.activeTab;
    if (tab?.tab_type === 'terminal') terminalsStore.focusTerminal(tab.id);
  }}
/>
<MeshSetupModal
  open={meshSetupWorkspaceId !== null}
  workspaceId={meshSetupWorkspaceId}
  onclose={() => { meshSetupWorkspaceId = null; }}
  onEnabled={() => { showMeshCockpit = true; }}
/>
<OverlordRuleChangeModal />
<!-- Right-edge pull-tab: appears when the active workspace is a mesh and the cockpit is closed. -->
{#if workspacesStore.activeWorkspace?.bridge_all && !showMeshCockpit && meshSetupWorkspaceId === null}
  <button
    class="mesh-lip"
    onclick={() => { showMeshCockpit = true; }}
    title="Open mesh cockpit (⌘⇧M)"
    aria-label="Open mesh cockpit"
  >MESH</button>
{/if}

<Toast />

{#if closeConfirmTabId && closeConfirmTabId === workspacesStore.activeTab?.id}
  <div class="close-confirm-backdrop" role="status" aria-live="polite">
    <div class="close-confirm-card">
      Press <kbd>{modSymbol}W</kbd> again to close this tab
    </div>
  </div>
{/if}

<style>
  .mesh-lip {
    position: fixed;
    right: 0;
    top: 50%;
    transform: translateY(-50%);
    z-index: 900; /* below cockpit (1000) + modals, above content */
    background: var(--accent);
    color: var(--bg-dark);
    border: none;
    border-radius: 6px 0 0 6px;
    padding: 14px 3px;
    cursor: pointer;
    writing-mode: vertical-rl;
    text-orientation: mixed;
    font-size: 9px;
    font-weight: 700;
    letter-spacing: 0.12em;
    opacity: 0.55;
    box-shadow: -2px 0 8px rgba(0, 0, 0, 0.3);
    transition: opacity 0.15s ease, padding-right 0.15s ease;
  }
  .mesh-lip:hover { opacity: 1; padding-right: 6px; }

  .close-confirm-backdrop {
    position: fixed;
    inset: 0;
    display: flex;
    align-items: center;
    justify-content: center;
    background: rgba(0, 0, 0, 0.45);
    backdrop-filter: blur(6px);
    -webkit-backdrop-filter: blur(6px);
    z-index: 10000;
    pointer-events: none;
    animation: close-confirm-fade 140ms ease-out;
  }
  .close-confirm-card {
    background: var(--bg-medium);
    color: var(--fg);
    border: 1px solid var(--bg-light);
    border-radius: 10px;
    padding: 19px 29px;
    font-size: 1.14rem;
    font-weight: 500;
    line-height: 1.4;
    text-align: center;
    box-shadow: 0 12px 38px rgba(0, 0, 0, 0.45);
    animation: close-confirm-pop 160ms ease-out;
  }
  .close-confirm-card kbd {
    font-family: var(--font-family, monospace);
    background: var(--bg-dark);
    border: 1px solid var(--bg-light);
    border-radius: 4px;
    padding: 2px 7px;
    font-size: 0.95em;
    margin: 0 2px;
  }
  @keyframes close-confirm-fade {
    from { opacity: 0; }
    to { opacity: 1; }
  }
  @keyframes close-confirm-pop {
    from { opacity: 0; transform: scale(0.94); }
    to { opacity: 1; transform: scale(1); }
  }
</style>
