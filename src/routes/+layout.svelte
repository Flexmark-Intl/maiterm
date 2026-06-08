<script lang="ts">
  import '../app.css';
  import { onMount } from 'svelte';
  import { invoke } from '@tauri-apps/api/core';
  import { getCurrentWindow } from '@tauri-apps/api/window';
  import { countedListen as listen } from '$lib/utils/listenCounter';
  import { workspacesStore, navigateToTab } from '$lib/stores/workspaces.svelte';
  import { terminalsStore } from '$lib/stores/terminals.svelte';
  import ImportPreviewModal from '$lib/components/ImportPreviewModal.svelte';
  import Toast from '$lib/components/Toast.svelte';
  import { seedDefaultTriggers } from '$lib/triggers/defaults';
  import { preferencesStore } from '$lib/stores/preferences.svelte';
  import { getTheme, applyUiTheme } from '$lib/themes';
  import { error as logError, info as logInfo } from '@tauri-apps/plugin-log';
  import { attachConsole } from '@tauri-apps/plugin-log';
  import { onAction as onNotificationAction } from '@tauri-apps/plugin-notification';
  import * as commands from '$lib/tauri/commands';
  import type { ClaudeCodeToolRequest, Preferences, Tab } from '$lib/tauri/types';
  import type { ImportPreview } from '$lib/tauri/commands';
  import { claudeCodeStore } from '$lib/stores/claudeCode.svelte';
  import { claudeStateStore } from '$lib/stores/claudeState.svelte';
  import { agentBridgeStore } from '$lib/stores/agentBridge.svelte';
  import { toastStore } from '$lib/stores/toasts.svelte';
  import { navHistoryStore } from '$lib/stores/navHistory.svelte';
  import { pendingResumePanes } from '$lib/stores/resumeGate.svelte';
  import { isModKey, isMac, modSymbol } from '$lib/utils/platform';
  import { open as dialogOpen, save as dialogSave } from '@tauri-apps/plugin-dialog';
  import { openFileFromTerminal } from '$lib/utils/openFile';
  import { installGlobalSmartQuoteFix } from '$lib/utils/smartQuotes';
  import QuickOpen from '$lib/components/QuickOpen.svelte';
  import AgentBridgePicker from '$lib/components/AgentBridgePicker.svelte';
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
  let importPreview = $state<ImportPreview | null>(null);
  let importFilePath = $state('');
  let showQuickOpen = $state(false);
  let showAgentBridgePicker = $state(false);
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

  // Update OS-level window title (Mission Control, Cmd+Tab, etc.)
  $effect(() => {
    const ws = workspacesStore.activeWorkspace;
    if (!ws) return;
    const suffix = import.meta.env.DEV ? ' (Dev)' : '';
    getCurrentWindow().setTitle(`maiTerm | ${ws.name}${suffix}`);
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
    const onUnhandledRejection = (e: PromiseRejectionEvent) => {
      const reason = e.reason as unknown;
      const stack = (reason && typeof reason === 'object' && 'stack' in reason)
        ? String((reason as { stack?: unknown }).stack ?? '')
        : undefined;
      logError(formatErrorPayload('unhandledrejection', reason, stack)).catch(() => {});
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
      // Save window geometry before exit (don't wait for debounce)
      clearTimeout(geometryTimer);
      await commands.saveWindowGeometry(currentMonitorCount).catch(() => {});
      await terminalsStore.saveAllScrollback();
      try {
        await invoke('sync_state');
      } catch (e) {
        logError(`sync_state failed: ${e}`);
      }
      await invoke('exit_app');
    }).then(unlisten => { unlistenQuit = unlisten; });

    // Pause toast timers when window loses focus, resume on focus
    let unlistenFocus: (() => void) | undefined;
    appWindow.onFocusChanged(({ payload: focused }) => {
      toastStore.setWindowFocused(focused);
    }).then(unlisten => { unlistenFocus = unlisten; });

    // Save window geometry per monitor count on resize/move (debounced).
    // Polls for monitor changes to auto-reposition windows when docking/undocking.
    let currentMonitorCount = 1;
    let geometryTimer: ReturnType<typeof setTimeout> | undefined;
    let monitorPollTimer: ReturnType<typeof setInterval> | undefined;

    // Initialize monitor count
    commands.getMonitorCount().then(count => {
      currentMonitorCount = count;

      // Poll for monitor changes (handles dock/undock)
      monitorPollTimer = setInterval(async () => {
        const count = await commands.getMonitorCount().catch(() => currentMonitorCount);
        if (count !== currentMonitorCount) {
          const oldCount = currentMonitorCount;
          currentMonitorCount = count;
          logInfo(`Monitor count changed: ${oldCount} → ${count}, repositioning window`);
          // Save current position under old monitor count before repositioning
          await commands.saveWindowGeometry(oldCount).catch(() => {});
          // Restore saved geometry for the new monitor count (if any)
          await commands.restoreWindowGeometry(count).catch(() => {});
        }
      }, 2000);
    });

    function saveGeometryDebounced() {
      clearTimeout(geometryTimer);
      geometryTimer = setTimeout(() => {
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
    }).then(unlisten => { unlistenReloadTab = unlisten; });

    // Check for updates menu event
    let unlistenCheckUpdates: (() => void) | undefined;
    listen('check-for-updates', () => {
      updaterStore.checkForUpdates(false);
    }).then(unlisten => { unlistenCheckUpdates = unlisten; });

    // Periodic silent update check — the startup check only runs once, so a
    // long-running window would otherwise never notice a new release. Re-reads
    // the preference each tick so toggling auto-check off stops further checks.
    const UPDATE_CHECK_INTERVAL_MS = 60 * 60 * 1000;
    const updateCheckTimer = setInterval(() => {
      if (preferencesStore.autoCheckUpdates) {
        updaterStore.checkForUpdates(true).catch(() => {});
      }
    }, UPDATE_CHECK_INTERVAL_MS);

    // Window > Clear Back/Forward History menu event
    let unlistenClearNavHistory: (() => void) | undefined;
    listen('clear-nav-history', () => {
      navHistoryStore.clear();
    }).then(unlisten => { unlistenClearNavHistory = unlisten; });

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
    }).then(unlisten => { unlistenExportState = unlisten; });

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
    }).then(unlisten => { unlistenImportState = unlisten; });

    let unlistenStateImported: (() => void) | undefined;
    listen('state-imported', () => {
      window.location.reload();
    }).then(unlisten => { unlistenStateImported = unlisten; });

    // Claude Code IDE integration event listeners.
    // Use appWindow.listen() (not global listen) — global listen catches both
    // window-targeted and global events in Tauri 2, causing duplicate callbacks.
    let unlistenClaudeTool: (() => void) | undefined;
    appWindow.listen<ClaudeCodeToolRequest>('claude-code-tool', (event) => {
      claudeCodeStore.handleToolRequest(event.payload);
    }).then(unlisten => { unlistenClaudeTool = unlisten; });

    let unlistenClaudeConnection: (() => void) | undefined;
    appWindow.listen<{ connected: boolean }>('claude-code-connection', (event) => {
      claudeCodeStore.setConnected(event.payload.connected);
    }).then(unlisten => { unlistenClaudeConnection = unlisten; });

    // Claude Code state tracking (hook events → per-tab Claude state)
    claudeStateStore.init();

    // Agent Bridge (hook events → cross-agent message delivery)
    agentBridgeStore.init();

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

    function tabCycleList(tabs: Tab[]): Tab[] {
      if (!preferencesStore.groupActiveTabs) return tabs;
      return tabs.filter(t => !isSuspendedTerminal(t));
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
            (!e.shiftKey && key === 'e');                             // Cmd+E toggle notes
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
          const count = pane.tabs.length + 1;
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
        if (pane.tabs.length > 1) {
          workspacesStore.deleteTab(ws.id, pane.id, tab.id);
        } else if (ws.panes.length > 1) {
          workspacesStore.deletePane(ws.id, pane.id);
        } else {
          // Last tab in last pane — close tab, pane shows empty state
          workspacesStore.deleteTab(ws.id, pane.id, tab.id);
        }
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

      // Cmd+B - Toggle sidebar
      if (isMeta && !e.shiftKey && e.key.toLowerCase() === 'b') {
        e.preventDefault();
        e.stopPropagation();
        workspacesStore.toggleSidebar();
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

    window.addEventListener('keydown', handleKeydown, true);
    window.addEventListener('keydown', handleKeydownAlt, true);
    window.addEventListener('keyup', handleKeyupAlt, true);

    return () => {
      window.removeEventListener('open-agent-bridge-picker', onOpenAgentBridgePicker);
      window.removeEventListener('keydown', handleKeydown, true);
      window.removeEventListener('keydown', handleKeydownAlt, true);
      window.removeEventListener('keyup', handleKeyupAlt, true);
      unlistenClose?.();
      unlistenQuit?.();
      unlistenReloadTab?.();
      unlistenExportState?.();
      unlistenImportState?.();
      unlistenStateImported?.();
      unlistenCheckUpdates?.();
      unlistenClearNavHistory?.();
      unlistenClaudeTool?.();
      unlistenClaudeConnection?.();
      claudeStateStore.destroy();
      agentBridgeStore.destroy();
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
<Toast />

{#if closeConfirmTabId && closeConfirmTabId === workspacesStore.activeTab?.id}
  <div class="close-confirm-backdrop" role="status" aria-live="polite">
    <div class="close-confirm-card">
      Press <kbd>{modSymbol}W</kbd> again to close this tab
    </div>
  </div>
{/if}

<style>
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
