import type { Terminal } from '@xterm/xterm';
import type { SplitDirection, SplitNode, Tab, Pane, Workspace, WorkspaceNote, EditorFileInfo, DiffContext } from '$lib/tauri/types';
import type { AgentRuntime } from '$lib/agents/types';
import * as commands from '$lib/tauri/commands';
import { terminalsStore } from '$lib/stores/terminals.svelte';
import { preferencesStore } from '$lib/stores/preferences.svelte';
import { activityStore } from '$lib/stores/activity.svelte';
import { getCompiledPatterns } from '$lib/utils/promptPattern';
import { error as logError } from '@tauri-apps/plugin-log';
import { pendingResumePanes } from '$lib/stores/resumeGate.svelte';
import { getVariables } from '$lib/stores/triggers.svelte';
import { CLAUDE_RESUME_COMMAND } from '$lib/triggers/defaults';
import { disableBridge } from '$lib/stores/sshMcpBridge.svelte';

/** Sidebar width bounds + default. The minimum reserves room for the footer's
 *  three agent-status indicators (working / waiting / finished) alongside the
 *  corner buttons while keeping workspace names readable — see WorkspaceSidebar's
 *  footer layout. Keep in sync with `default_sidebar_width()` in Rust. */
const SIDEBAR_MIN_WIDTH = 215;
const SIDEBAR_MAX_WIDTH = 400;
const SIDEBAR_DEFAULT_WIDTH = 215;

/**
 * Extract the remote cwd from the terminal prompt using user-configured patterns.
 * Patterns are defined in preferences and compiled to regexes at runtime.
 */
function extractRemoteCwd(terminal: Terminal): string | null {
  const buffer = terminal.buffer.active;
  const cursorLine = buffer.baseY + buffer.cursorY;
  const patterns = getCompiledPatterns(preferencesStore.promptPatterns);
  if (patterns.length === 0) return null;

  for (let i = cursorLine; i >= Math.max(0, cursorLine - 5); i--) {
    const line = buffer.getLine(i);
    if (!line) continue;
    const text = line.translateToString(true).trim();
    if (!text) continue;

    for (const re of patterns) {
      const match = text.match(re);
      if (match?.[1]) return match[1];
    }
  }

  return null;
}

function updateRatioInTree(node: SplitNode, splitId: string, ratio: number): SplitNode {
  if (node.type === 'leaf') return node;
  if (node.id === splitId) return { ...node, ratio };
  return {
    ...node,
    children: [
      updateRatioInTree(node.children[0], splitId, ratio),
      updateRatioInTree(node.children[1], splitId, ratio),
    ],
  };
}

/**
 * Compute a deduplicated name for a duplicated custom-named tab.
 * Strips leading "N " to get the base, finds the highest existing index
 * among all tab names in the workspace, and returns "N+1 base".
 */
function nextDuplicateName(sourceName: string, existingNames: string[]): string {
  const baseMatch = sourceName.match(/^(\d+)\s+(.+)$/);
  const baseName = baseMatch ? baseMatch[2] : sourceName;

  let maxIndex = 0;
  for (const name of existingNames) {
    if (name === baseName) {
      maxIndex = Math.max(maxIndex, 1);
    } else {
      const m = name.match(/^(\d+)\s+(.+)$/);
      if (m && m[2] === baseName) {
        maxIndex = Math.max(maxIndex, parseInt(m[1], 10));
      }
    }
  }

  if (maxIndex === 0) return sourceName;
  return `${maxIndex + 1} ${baseName}`;
}

/** Collect all tab names across all panes in a workspace. */
function allTabNames(ws: Workspace): string[] {
  return ws.panes.flatMap(p => p.tabs.map(t => t.name));
}

/**
 * Pick the next active tab after closing/archiving a tab.
 * When groupActiveTabs is on, prefers non-suspended (live terminal) tabs.
 * Falls back to adjacent tab by index if no live tabs remain.
 */
function pickNextActiveTab(tabs: Tab[], closedIndex: number): string | null {
  if (tabs.length === 0) return null;
  if (preferencesStore.groupActiveTabs) {
    // Prefer live (non-suspended) terminal tabs, searching outward from closedIndex
    const liveTabs = tabs.filter(t => {
      const isTerminal = t.tab_type === 'terminal' || !t.tab_type;
      return !isTerminal || terminalsStore.get(t.id);
    });
    if (liveTabs.length > 0) {
      // Pick the live tab closest to the closed position
      let best = liveTabs[0];
      let bestDist = Infinity;
      for (const t of liveTabs) {
        const idx = tabs.indexOf(t);
        const dist = Math.abs(idx - closedIndex);
        if (dist < bestDist) { bestDist = dist; best = t; }
      }
      return best.id;
    }
  }
  // Default: adjacent tab by index
  const newIndex = closedIndex > 0 ? closedIndex - 1 : 0;
  return tabs[newIndex]?.id ?? null;
}

const RECENT_WINDOW_MS = 30 * 60 * 1000; // 30 minutes


function createWorkspacesStore() {
  let windowId = $state<string>('');
  let windowLabel = $state<string>('');
  let workspaces = $state<Workspace[]>([]);
  let activeWorkspaceId = $state<string | null>(null);
  let sidebarWidth = $state(SIDEBAR_DEFAULT_WIDTH);
  let sidebarCollapsed = $state(false);
  let lastSwitchedAt = $state<Map<string, number>>(new Map());
  // Frontend-only: set of tab IDs with notes panel visible
  let notesVisible = $state<Set<string>>(new Set());
  // Workspace IDs currently being suspended — guards pty-close from deleting tabs
  const suspendingWorkspaceIds = new Set<string>();
  // Tab IDs currently being suspended — guards pty-close from deleting the tab
  const suspendingTabIds = new Set<string>();
  // Tick counter to force re-evaluation of recentWorkspaces when entries expire
  let _recentTick = $state(0);
  let _recentTimer: ReturnType<typeof setInterval> | null = null;

  /** Find a tab by workspace/pane/tab IDs (helper for direct mutations). */
  function findTab(workspaceId: string, paneId: string, tabId: string) {
    const ws = workspaces.find(w => w.id === workspaceId);
    const pane = ws?.panes.find(p => p.id === paneId);
    const tab = pane?.tabs.find(t => t.id === tabId);
    return { ws, pane, tab };
  }

  const recentWorkspaces = $derived.by(() => {
    void _recentTick; // subscribe to tick for expiry re-evaluation
    const now = Date.now();
    return workspaces.filter(w => {
      if (w.id === activeWorkspaceId) return false;
      if (w.suspended) return false;
      const ts = lastSwitchedAt.get(w.id);
      return ts != null && (now - ts) < RECENT_WINDOW_MS;
    });
  });

  const activeWorkspace = $derived(
    workspaces.find(w => w.id === activeWorkspaceId && !w.suspended) ?? null
  );

  const activePane = $derived.by(() => {
    if (!activeWorkspace) return null;
    return activeWorkspace.panes.find(p => p.id === activeWorkspace.active_pane_id) ?? null;
  });

  const activeTab = $derived.by(() => {
    if (!activePane) return null;
    return activePane.tabs.find(t => t.id === activePane.active_tab_id) ?? null;
  });

  return {
    get windowId() { return windowId; },
    get windowLabel() { return windowLabel; },
    get workspaces() { return workspaces; },
    get activeWorkspaceId() { return activeWorkspaceId; },
    get activeWorkspace() { return activeWorkspace; },
    get activePane() { return activePane; },
    get activeTab() { return activeTab; },
    get sidebarWidth() { return sidebarWidth; },
    get sidebarCollapsed() { return sidebarCollapsed; },
    get recentWorkspaces() { return recentWorkspaces; },
    get lastSwitchedAt() { return lastSwitchedAt; },

    /** Detected agent runtime for a tab, defaulting to 'claude' when none is set. */
    getTabRuntime(tabId: string): AgentRuntime {
      for (const ws of workspaces) for (const pane of ws.panes) {
        const t = pane.tabs.find(x => x.id === tabId);
        if (t) return t.runtime ?? 'claude';
      }
      return 'claude';
    },

    /**
     * Update a tab's detected runtime in local state (the backend already
     * persists it on initSession). Mutates in place for Svelte reactivity so
     * getTabRuntime reflects the change live, without a reload.
     */
    setTabRuntimeLocal(tabId: string, runtime: AgentRuntime) {
      for (const ws of workspaces) for (const pane of ws.panes) {
        const t = pane.tabs.find(x => x.id === tabId);
        if (t) {
          if (t.runtime !== runtime) t.runtime = runtime;
          return;
        }
      }
    },

    /** True while a workspace is being suspended (PTYs being killed). */
    isWorkspaceSuspending(workspaceId: string) { return suspendingWorkspaceIds.has(workspaceId); },

    /** True while a tab is being suspended (PTY being killed intentionally). */
    isTabSuspending(tabId: string) { return suspendingTabIds.has(tabId); },

    reset() {
      workspaces = [];
      activeWorkspaceId = null;
    },

    async load() {
      const data = await commands.getWindowData();
      windowId = data.id;
      windowLabel = data.label;
      workspaces = data.workspaces;
      activeWorkspaceId = data.active_workspace_id;
      sidebarWidth = Math.min(SIDEBAR_MAX_WIDTH, Math.max(SIDEBAR_MIN_WIDTH, data.sidebar_width || SIDEBAR_DEFAULT_WIDTH));
      sidebarCollapsed = data.sidebar_collapsed ?? false;

      // Seed notesVisible from persisted notes_open state
      const seeded = new Set<string>();
      for (const ws of data.workspaces) {
        for (const pane of ws.panes) {
          for (const tab of pane.tabs) {
            if (tab.notes_open) seeded.add(tab.id);
          }
        }
      }
      notesVisible = seeded;

      // Migration: update old auto-resume commands and backfill missing context
      const OLD_RESUME_COMMANDS = [
        'if [ -n "%claudeSessionId" ]; then claude --resume %claudeSessionId; elif [ -n "%claudeResumeCommand" ]; then %claudeResumeCommand; else claude --continue; fi',
        "if [ -n '%claudeSessionId' ]; then claude --resume %claudeSessionId; elif [ -n '%claudeResumeCommand' ]; then eval %claudeResumeCommand; else claude --continue; fi",
        'claude --resume %claudeSessionId "/aiterm init"',
      ];
      // Skeletons of old templates that may have had %variables interpolated
      // to literal values before being saved (older versions had this bug).
      const OLD_RESUME_REGEXES = [
        /^if \[ -n ['"].*['"] \]; then claude --resume .*; elif \[ -n ['"].*['"] \]; then (eval )?.*; else claude --continue; fi$/,
        /^claude --resume \S+ "\/aiterm init"$/,
      ];
      for (const ws of workspaces) {
        for (const pane of ws.panes) {
          for (const tab of pane.tabs) {
            let migrated = false;

            // Update old auto-resume command templates (including legacy
            // interpolated forms) to the current version.
            const cmd = tab.auto_resume_command;
            if (cmd && cmd !== CLAUDE_RESUME_COMMAND
              && (OLD_RESUME_COMMANDS.includes(cmd) || OLD_RESUME_REGEXES.some(re => re.test(cmd)))) {
              tab.auto_resume_command = CLAUDE_RESUME_COMMAND;
              tab.auto_resume_remembered_command = CLAUDE_RESUME_COMMAND;
              migrated = true;
            }

            // Backfill missing SSH/CWD context from restore fields — older
            // versions stored the command but not the connection context.
            if (tab.auto_resume_command && !tab.auto_resume_ssh_command && !tab.auto_resume_cwd) {
              if (tab.restore_ssh_command || tab.restore_cwd) {
                tab.auto_resume_ssh_command = tab.restore_ssh_command;
                tab.auto_resume_cwd = tab.restore_cwd;
                tab.auto_resume_remote_cwd = tab.restore_remote_cwd;
                migrated = true;
              }
            }

            if (migrated) {
              await commands.setTabAutoResumeContext(
                ws.id, pane.id, tab.id,
                tab.auto_resume_cwd, tab.auto_resume_ssh_command,
                tab.auto_resume_remote_cwd, tab.auto_resume_command,
              );
            }
          }
        }
      }

      // On restart, non-active workspaces have no PTYs — mark them as suspended
      // so the UI is consistent (dimmed, click to resume).
      for (const ws of workspaces) {
        if (ws.id !== activeWorkspaceId && !ws.suspended) {
          ws.suspended = true;
          commands.suspendWorkspace(ws.id).catch(() => {});
        }
      }

      // Create default workspace if none exist
      if (workspaces.length === 0) {
        await this.createWorkspace('Default');
      }

      // Start periodic tick to expire recent workspaces
      if (!_recentTimer) {
        _recentTimer = setInterval(() => { _recentTick++; }, 60_000);
      }

      // Keep local tab.last_cwd in sync with live OSC state and persist to backend
      terminalsStore.onOscChange((tabId, osc) => {
        const resolvedCwd = osc.cwd ?? osc.promptCwd;
        if (!resolvedCwd) return;
        for (const ws of workspaces) {
          for (const p of ws.panes) {
            const tab = p.tabs.find(t => t.id === tabId);
            if (tab && tab.last_cwd !== resolvedCwd) {
              tab.last_cwd = resolvedCwd;
              commands.setTabLastCwd(ws.id, p.id, tabId, resolvedCwd).catch(() => {});
              return;
            }
          }
        }
      });
    },

    setSidebarWidth(width: number) {
      sidebarWidth = Math.max(SIDEBAR_MIN_WIDTH, Math.min(SIDEBAR_MAX_WIDTH, width));
    },

    async saveSidebarWidth() {
      await commands.setSidebarWidth(sidebarWidth);
    },

    async toggleSidebar() {
      sidebarCollapsed = !sidebarCollapsed;
      await commands.setSidebarCollapsed(sidebarCollapsed);
    },

    async createWorkspace(name: string) {
      const workspace = await commands.createWorkspace(name);
      // Insert after the currently active workspace, or append if none active
      const activeIdx = workspaces.findIndex(w => w.id === activeWorkspaceId);
      workspaces.splice(activeIdx + 1, 0, workspace);
      activeWorkspaceId = workspace.id;
      await commands.reorderWorkspaces(workspaces.map(w => w.id));
      return workspace;
    },

    async deleteWorkspace(workspaceId: string) {
      const oldIndex = workspaces.findIndex(w => w.id === workspaceId);
      await commands.deleteWorkspace(workspaceId);
      workspaces.splice(oldIndex, 1);
      if (lastSwitchedAt.has(workspaceId)) {
        const updated = new Map(lastSwitchedAt);
        updated.delete(workspaceId);
        lastSwitchedAt = updated;
      }
      if (activeWorkspaceId === workspaceId) {
        // Activate adjacent: prefer previous, fall back to next
        const adjacentIndex = Math.min(oldIndex, workspaces.length - 1);
        activeWorkspaceId = workspaces[adjacentIndex]?.id ?? null;
      }
      import('$lib/stores/navHistory.svelte').then(m => m.navHistoryStore.removeWorkspace(workspaceId));
    },

    async suspendWorkspace(workspaceId: string) {
      const ws = workspaces.find(w => w.id === workspaceId);
      if (!ws || ws.suspended) return;

      // Mark as suspending so pty-close handlers don't delete tabs
      suspendingWorkspaceIds.add(workspaceId);

      // 1. Save scrollback and snapshot restore context for all terminal tabs
      for (const pane of ws.panes) {
        for (const tab of pane.tabs) {
          if (tab.tab_type !== 'terminal') continue;
          const instance = terminalsStore.get(tab.id);
          if (!instance) continue;

          // Save scrollback directly in Rust (no WebView round-trip)
          try {
            await commands.saveTerminalScrollback(instance.ptyId, tab.id);
          } catch {
            // Alternate screen active or terminal gone — skip
          }

          // Snapshot CWD/SSH for restore
          if (tab.pty_id) {
            try {
              const info = await commands.getPtyInfo(tab.pty_id);
              await commands.setTabRestoreContext(
                ws.id, pane.id, tab.id,
                info.cwd ?? null,
                info.foreground_command ?? null,
                null, // remote_cwd — extracted on resume from prompt patterns
              );
            } catch {
              // PTY may already be dead
            }
          }
        }
      }

      // 2. Kill all PTYs in this workspace
      for (const pane of ws.panes) {
        for (const tab of pane.tabs) {
          if (tab.tab_type !== 'terminal') continue;
          const instance = terminalsStore.get(tab.id);
          if (instance) {
            try { await commands.killTerminal(instance.ptyId); } catch { /* already dead */ }
            terminalsStore.unregister(tab.id);
          }
        }
      }

      // 3. Set suspended flag (persisted to backend)
      await commands.suspendWorkspace(workspaceId);
      suspendingWorkspaceIds.delete(workspaceId);
      const wsToSuspend = workspaces.find(w => w.id === workspaceId);
      if (wsToSuspend) wsToSuspend.suspended = true;
      const { navHistoryStore } = await import('$lib/stores/navHistory.svelte');
      navHistoryStore.removeWorkspace(workspaceId);

      // 4. If this was the active workspace, prefer back-history; fall back to first non-suspended
      if (activeWorkspaceId === workspaceId) {
        const backEntry = navHistoryStore.peekMostRecent(e => {
          const w = workspaces.find(ww => ww.id === e.workspaceId);
          return !!w && !w.suspended;
        });
        if (backEntry) {
          await this.setActiveWorkspace(backEntry.workspaceId);
          await this.setActivePane(backEntry.workspaceId, backEntry.paneId);
          await this.setActiveTab(backEntry.workspaceId, backEntry.paneId, backEntry.tabId);
          terminalsStore.focusTerminal(backEntry.tabId);
        } else {
          const next = workspaces.find(w => !w.suspended && w.id !== workspaceId);
          if (next) {
            await this.setActiveWorkspace(next.id);
          } else {
            activeWorkspaceId = null;
          }
        }
      }
    },

    async resumeWorkspace(workspaceId: string) {
      const ws = workspaces.find(w => w.id === workspaceId);
      if (!ws || !ws.suspended) return;

      // Clear suspended flag
      await commands.resumeWorkspace(workspaceId);
      const wsToResume = workspaces.find(w => w.id === workspaceId);
      if (wsToResume) wsToResume.suspended = false;

      // Switch to this workspace — lazy init handles PTY spawning
      await this.setActiveWorkspace(workspaceId);
    },

    /**
     * Tear down all terminal tabs in the active workspace except the currently
     * active tab.  Saves scrollback, snapshots CWD/SSH, kills PTYs.
     * Returns the tab IDs that were torn down so the caller can remove them
     * from activatedTabIds.
     */
    async suspendOtherTabs(): Promise<string[]> {
      const ws = workspaces.find(w => w.id === activeWorkspaceId);
      if (!ws) return [];

      // Guard pty-close handlers from deleting tabs while we kill PTYs
      suspendingWorkspaceIds.add(ws.id);

      const activeTabId = ws.panes.find(p => p.id === ws.active_pane_id)?.active_tab_id;
      const tornDown: string[] = [];

      for (const pane of ws.panes) {
        for (const tab of pane.tabs) {
          if (tab.tab_type !== 'terminal') continue;
          if (tab.id === activeTabId) continue;

          const instance = terminalsStore.get(tab.id);
          if (!instance) continue;

          // Save scrollback directly in Rust (no WebView round-trip)
          try {
            await commands.saveTerminalScrollback(instance.ptyId, tab.id);
          } catch {
            // Alternate screen active or terminal gone — skip
          }

          // Snapshot CWD/SSH
          if (tab.pty_id) {
            try {
              const info = await commands.getPtyInfo(tab.pty_id);
              await commands.setTabRestoreContext(
                ws.id, pane.id, tab.id,
                info.cwd ?? null,
                info.foreground_command ?? null,
                null,
              );
            } catch { /* PTY may already be dead */ }
          }

          // Kill PTY
          try { await commands.killTerminal(instance.ptyId); } catch { /* already dead */ }
          terminalsStore.unregister(tab.id);
          tornDown.push(tab.id);
        }
      }

      suspendingWorkspaceIds.delete(ws.id);
      if (tornDown.length > 0) {
        import('$lib/stores/navHistory.svelte').then(m => {
          for (const tabId of tornDown) {
            m.navHistoryStore.removeTab(tabId);
          }
        });
      }
      return tornDown;
    },

    async suspendAllOtherWorkspaces() {
      const others = workspaces.filter(w => w.id !== activeWorkspaceId && !w.suspended);
      for (const ws of others) {
        await this.suspendWorkspace(ws.id);
      }
    },

    async renameWorkspace(workspaceId: string, name: string) {
      await commands.renameWorkspace(workspaceId, name);
      const ws = workspaces.find(w => w.id === workspaceId);
      if (ws) ws.name = name;
    },

    async setActiveWorkspace(workspaceId: string) {
      // Record the workspace we're leaving as recently active
      if (activeWorkspaceId && activeWorkspaceId !== workspaceId) {
        const updated = new Map(lastSwitchedAt);
        updated.set(activeWorkspaceId, Date.now());
        lastSwitchedAt = updated;
      }
      await commands.setActiveWorkspace(workspaceId);
      activeWorkspaceId = workspaceId;
      // Clear import highlight on activation
      const ws = workspaces.find(w => w.id === workspaceId);
      if (ws?.import_highlight) {
        ws.import_highlight = false;
      }
      // Record the now-visible active tab. push() dedups by tabId, so the
      // sidebar's manual push that runs after this call is a safe no-op.
      const activePane = ws?.active_pane_id ? ws.panes.find(p => p.id === ws.active_pane_id) : undefined;
      if (activePane?.active_tab_id) {
        const { navHistoryStore } = await import('$lib/stores/navHistory.svelte');
        navHistoryStore.push({ workspaceId, paneId: activePane.id, tabId: activePane.active_tab_id });
      }
    },

    async splitPane(workspaceId: string, targetPaneId: string, direction: SplitDirection) {
      const pane = await commands.splitPane(workspaceId, targetPaneId, direction);
      // Reload workspace to get updated split_root from backend
      const data = await commands.getWindowData();
      const freshWs = data.workspaces.find(w => w.id === workspaceId);
      if (freshWs) {
        const idx = workspaces.findIndex(w => w.id === workspaceId);
        if (idx >= 0) workspaces[idx] = freshWs;
      }
      return pane;
    },

    /**
     * Split a pane and boot a *forked* Claude session into the new tab — the core
     * spawn for Agent Bridge. The new tab auto-resumes `claude --resume <sessionId>
     * --fork-session` in the target's cwd (and SSH context if remote), giving an
     * isolated peer with the target session's full context without disturbing the
     * original. Returns { newPaneId, newTabId } so the caller can register the bridge.
     *
     * Ordering matters: the split context + auto-resume command must be set on the
     * backend BEFORE the reactive `workspaces` array updates (which mounts the new
     * TerminalPane and consumes the context). So we use the low-level
     * `commands.splitPane` and refresh the store last — mirroring
     * `splitPaneWithScrollback`.
     */
    async forkSessionIntoSplit(
      workspaceId: string,
      sourcePaneId: string,
      target: { sessionId: string; cwd: string | null; sshCommand: string | null; remoteCwd: string | null },
      tabName: string,
      direction: SplitDirection = 'horizontal',
    ): Promise<{ newPaneId: string; newTabId: string } | null> {
      const newPane = await commands.splitPane(workspaceId, sourcePaneId, direction);
      const newTabId = newPane.tabs[0]?.id;
      if (!newTabId) return null;

      await commands.renameTab(workspaceId, newPane.id, newTabId, tabName, true);

      const forkCommand = `claude --resume ${target.sessionId} --fork-session`;
      await commands.setTabAutoResumeContext(
        workspaceId, newPane.id, newTabId,
        target.cwd, target.sshCommand, target.remoteCwd,
        forkCommand, false,
      );
      // setTabAutoResumeContext stores the command but may not flip the enabled
      // flag; do it explicitly so TerminalPane fires the fork command on mount.
      await commands.setTabAutoResumeEnabled(workspaceId, newPane.id, newTabId, true);

      // Make the new TerminalPane run the fork command when it mounts (the split
      // context's fireAutoResume gate — see TerminalPane onMount).
      terminalsStore.setSplitContext(newTabId, {
        cwd: target.cwd,
        sshCommand: target.sshCommand,
        remoteCwd: target.remoteCwd,
        fireAutoResume: true,
      });

      // Refresh store LAST → mounts the new pane with context already in place.
      const data = await commands.getWindowData();
      const freshWs = data.workspaces.find(w => w.id === workspaceId);
      if (freshWs) {
        const idx = workspaces.findIndex(w => w.id === workspaceId);
        if (idx >= 0) workspaces[idx] = freshWs;
      }
      return { newPaneId: newPane.id, newTabId };
    },

    async splitPaneWithContext(workspaceId: string, sourcePaneId: string, sourceTabId: string, direction: SplitDirection) {
      // Look up source tab to determine its type
      const ws_current = workspaces.find(w => w.id === workspaceId);
      const sourcePane = ws_current?.panes.find(p => p.id === sourcePaneId);
      const sourceTab = sourcePane?.tabs.find(t => t.id === sourceTabId);

      // Editor tab: create a duplicate editor pane (no terminal context needed)
      if (sourceTab?.tab_type === 'editor' && sourceTab.editor_file) {
        const newPane = await commands.splitPane(workspaceId, sourcePaneId, direction, null, sourceTab.editor_file);

        // Copy notes
        const newTabId = newPane.tabs[0]?.id;
        if (newTabId) {
          if (preferencesStore.cloneNotes && sourceTab.notes) {
            await commands.setTabNotes(workspaceId, newPane.id, newTabId, sourceTab.notes);
          }
          if (preferencesStore.cloneNotes && sourceTab.notes_mode) {
            await commands.setTabNotesMode(workspaceId, newPane.id, newTabId, sourceTab.notes_mode);
          }
        }

        // Reload workspace to get updated split_root
        const data = await commands.getWindowData();
        const freshWsEditor = data.workspaces.find(w => w.id === workspaceId);
        if (freshWsEditor) {
          const idx = workspaces.findIndex(w => w.id === workspaceId);
          if (idx >= 0) workspaces[idx] = freshWsEditor;
        }
        return newPane;
      }

      // Terminal tab: gather context from the source terminal
      const instance = terminalsStore.get(sourceTabId);
      let scrollback: string | null = null;
      let cwd: string | null = null;
      let sshCommand: string | null = null;

      if (instance) {
        // Serialize current scrollback
        if (preferencesStore.cloneScrollback) {
          try {
            const bytes = await commands.serializeTerminal(instance.ptyId);
            scrollback = new TextDecoder().decode(new Uint8Array(bytes));
          } catch (e) {
            logError(`Failed to serialize scrollback for split: ${e}`);
          }
        }

        // Get PTY info (cwd + SSH detection)
        if (preferencesStore.cloneCwd || preferencesStore.cloneSsh) {
          try {
            const info = await commands.getPtyInfo(instance.ptyId);
            cwd = preferencesStore.cloneCwd ? info.cwd : null;
            sshCommand = preferencesStore.cloneSsh ? info.foreground_command : null;
          } catch (e) {
            // PTY may already be gone — fall through with null
          }
        }
      }

      // 2. Create split (with scrollback pre-populated on new tab)
      const newPane = await commands.splitPane(workspaceId, sourcePaneId, direction, scrollback);

      // 2b. Name the new pane and tab properly
      const paneCount = (ws_current?.panes.length ?? 0) + 1; // +1 for the newly created pane
      await commands.renamePane(workspaceId, newPane.id, `Pane ${paneCount}`);

      const newTabId = newPane.tabs[0]?.id;
      if (sourceTab && newTabId) {
        const tabName = sourceTab.custom_name && ws_current && preferencesStore.numberDuplicatedTabs
          ? nextDuplicateName(sourceTab.name, allTabNames(ws_current))
          : sourceTab.name;
        await commands.renameTab(workspaceId, newPane.id, newTabId, tabName, sourceTab.custom_name);

        // Copy notes
        if (preferencesStore.cloneNotes && sourceTab.notes) {
          await commands.setTabNotes(workspaceId, newPane.id, newTabId, sourceTab.notes);
        }
        if (preferencesStore.cloneNotes && sourceTab.notes_mode) {
          await commands.setTabNotesMode(workspaceId, newPane.id, newTabId, sourceTab.notes_mode);
        }

        // Copy trigger variables
        if (preferencesStore.cloneVariables) {
          const srcVars = getVariables(sourceTabId);
          if (srcVars && srcVars.size > 0) {
            const plain: Record<string, string> = {};
            for (const [k, v] of srcVars) plain[k] = v;
            await commands.setTabTriggerVariables(workspaceId, newPane.id, newTabId, plain).catch(e =>
              logError(`Failed to copy trigger variables: ${e}`)
            );
          }
        }

        // Copy auto-resume settings
        if (preferencesStore.cloneAutoResume && (sourceTab.auto_resume_cwd || sourceTab.auto_resume_ssh_command || sourceTab.auto_resume_command)) {
          await this.setTabAutoResumeContext(
            workspaceId, newPane.id, newTabId,
            sourceTab.auto_resume_cwd,
            sourceTab.auto_resume_ssh_command,
            sourceTab.auto_resume_remote_cwd,
            sourceTab.auto_resume_command,
            sourceTab.auto_resume_pinned,
          );
        }
      }
      if (newTabId) {
        if (preferencesStore.cloneHistory) {
          try {
            await commands.copyTabHistory(sourceTabId, newTabId);
          } catch (e) {
            logError(`Failed to copy tab history: ${e}`);
          }
        }

        // 4. Store split context for the new TerminalPane to consume on mount
        if (preferencesStore.cloneCwd || preferencesStore.cloneSsh) {
          // OSC 7 gives the most accurate cwd (works for both local and remote shells)
          const oscState = terminalsStore.getOsc(sourceTabId);
          const osc7Cwd = oscState?.cwd ?? null;
          const promptCwd = oscState?.promptCwd ?? null;

          let remoteCwd: string | null = null;
          if (sshCommand) {
            // SSH active: OSC 7 may be stale (from the local shell before SSH started)
            // or updated by the remote shell. Compare with the lsof-reported local cwd:
            // if they match, OSC 7 is stale → fall back to promptCwd then buffer scan.
            const isOsc7Stale = osc7Cwd === cwd;
            const osc7RemoteCwd = (osc7Cwd && !isOsc7Stale) ? osc7Cwd : null;
            remoteCwd = osc7RemoteCwd ?? promptCwd ?? (instance ? extractRemoteCwd(instance.terminal) : null);
          } else if (preferencesStore.cloneCwd) {
            // No SSH: OSC 7 reports local cwd, can supplement lsof
            cwd = cwd ?? osc7Cwd;
          }

          if (cwd || sshCommand) {
            terminalsStore.setSplitContext(newTabId, { cwd, sshCommand, remoteCwd });
          }
        }
      }

      // 5. Reload workspace to get updated split_root
      const data = await commands.getWindowData();
      const freshWsSplit = data.workspaces.find(w => w.id === workspaceId);
      if (freshWsSplit) {
        const idx = workspaces.findIndex(w => w.id === workspaceId);
        if (idx >= 0) workspaces[idx] = freshWsSplit;
      }
      return newPane;
    },

    async deletePane(workspaceId: string, paneId: string) {
      await commands.deletePane(workspaceId, paneId);
      // Reload workspace to get updated split_root from backend
      const data = await commands.getWindowData();
      const freshWsPane = data.workspaces.find(w => w.id === workspaceId);
      if (freshWsPane) {
        const idx = workspaces.findIndex(w => w.id === workspaceId);
        if (idx >= 0) workspaces[idx] = freshWsPane;
      }
    },

    async renamePane(workspaceId: string, paneId: string, name: string) {
      await commands.renamePane(workspaceId, paneId, name);
      const ws = workspaces.find(w => w.id === workspaceId);
      const pane = ws?.panes.find(p => p.id === paneId);
      if (pane) pane.name = name;
    },

    async setActivePane(workspaceId: string, paneId: string) {
      await commands.setActivePane(workspaceId, paneId);
      const ws = workspaces.find(w => w.id === workspaceId);
      if (ws) ws.active_pane_id = paneId;
      // The visible active tab changes when the pane changes — record it
      // so Cmd+[/] can step back through pane switches.
      const pane = ws?.panes.find(p => p.id === paneId);
      if (pane?.active_tab_id) {
        const { navHistoryStore } = await import('$lib/stores/navHistory.svelte');
        navHistoryStore.push({ workspaceId, paneId, tabId: pane.active_tab_id });
      }
    },

    async createTab(workspaceId: string, paneId: string, name: string, options?: { append?: boolean }) {
      const afterTabId = options?.append
        ? undefined
        : workspaces.flatMap(w => w.panes).find(p => p.id === paneId)?.active_tab_id ?? undefined;
      const tab = await commands.createTab(workspaceId, paneId, name, afterTabId);

      // Open new tab at the most common CWD (and SSH setup) among sibling
      // LIVE terminal tabs. On ties, the active tab's setup wins. Suspended
      // tabs are excluded — their persisted restore_* cwd is stale and, in a
      // workspace that has accumulated many suspended tabs, would dominate the
      // tally and pin every new tab to that majority directory regardless of
      // which tab the user is actually on.
      const ws = workspaces.find(w => w.id === workspaceId);
      if (ws) {
        const activePane = ws.panes.find(p => p.id === paneId);
        const activeTabId = activePane?.active_tab_id;

        // Query live PTY info for the active terminal — persisted fields
        // (restore_ssh_command etc.) may not reflect the current state yet
        // if auto-save hasn't fired.
        let liveSsh: string | null = null;
        let liveCwd: string | null = null;
        if (activeTabId) {
          const instance = terminalsStore.instances.get(activeTabId);
          if (instance) {
            try {
              const info = await commands.getPtyInfo(instance.ptyId);
              liveSsh = info.foreground_command;
              liveCwd = info.cwd;
            } catch { /* ignore */ }
          }
        }

        // Build a composite key: "ssh\0command\0remoteCwd" for SSH tabs, or "local\0cwd" for local tabs
        const setupCounts = new Map<string, { count: number; cwd: string | null; sshCommand: string | null; remoteCwd: string | null }>();
        for (const p of ws.panes) {
          for (const t of p.tabs) {
            if (t.tab_type !== 'terminal') continue;
            // Only count tabs with a live PTY. Suspended tabs are unregistered
            // from the terminals store, so their (stale) restore_* cwd never
            // skews the tally.
            if (!terminalsStore.get(t.id)) continue;
            // Use live PTY info for the active tab, persisted fields for others
            let ssh: string | null;
            let remoteCwd: string | null;
            let localCwd: string | null;
            if (t.id === activeTabId && t.auto_resume_enabled && t.auto_resume_ssh_command) {
              // Pinned auto-resume is the source of truth — live PTY state can
              // be misleading (e.g. ssh → sudo -i changes foreground_command to
              // something that won't reconnect correctly).
              ssh = t.auto_resume_ssh_command;
              remoteCwd = t.auto_resume_remote_cwd ?? null;
              localCwd = liveCwd ?? t.last_cwd;
            } else if (t.id === activeTabId && liveSsh) {
              ssh = liveSsh;
              // Get remote cwd from OSC state (promptCwd) since live PTY only gives local cwd
              const oscState = terminalsStore.getOsc(t.id);
              remoteCwd = oscState?.promptCwd ?? t.auto_resume_remote_cwd ?? t.restore_remote_cwd ?? null;
              localCwd = liveCwd;
            } else {
              ssh = t.auto_resume_ssh_command || t.restore_ssh_command || null;
              // For live SSH tabs the real remote cwd lives in OSC promptCwd
              // (the PTY only reports the local cwd); fall back to persisted.
              const oscState = ssh ? terminalsStore.getOsc(t.id) : null;
              remoteCwd = ssh
                ? (oscState?.promptCwd ?? t.auto_resume_remote_cwd ?? t.restore_remote_cwd ?? null)
                : null;
              localCwd = t.last_cwd;
            }
            if (!ssh && !localCwd) continue;
            const key = ssh ? `ssh\0${ssh}\0${remoteCwd ?? ''}` : `local\0${localCwd}`;
            const existing = setupCounts.get(key);
            if (existing) {
              existing.count++;
            } else {
              setupCounts.set(key, { count: 1, cwd: localCwd, sshCommand: ssh, remoteCwd });
            }
          }
        }
        // Find the active tab's setup key to use as tiebreaker
        const activeTab = activePane?.tabs.find(t => t.id === activeTabId);
        let activeKey: string | null = null;
        if (activeTab?.tab_type === 'terminal') {
          if (activeTab.auto_resume_enabled && activeTab.auto_resume_ssh_command) {
            activeKey = `ssh\0${activeTab.auto_resume_ssh_command}\0${activeTab.auto_resume_remote_cwd ?? ''}`;
          } else if (liveSsh) {
            const oscState = terminalsStore.getOsc(activeTab.id);
            const remoteCwd = oscState?.promptCwd ?? activeTab.auto_resume_remote_cwd ?? activeTab.restore_remote_cwd ?? null;
            activeKey = `ssh\0${liveSsh}\0${remoteCwd ?? ''}`;
          } else {
            const localCwd = activeTab.last_cwd ?? liveCwd;
            if (localCwd) activeKey = `local\0${localCwd}`;
          }
        }
        let best: { cwd: string | null; sshCommand: string | null; remoteCwd: string | null } | null = null;
        let bestCount = 0;
        for (const [key, entry] of setupCounts) {
          if (entry.count > bestCount || (entry.count === bestCount && key === activeKey)) {
            best = entry; bestCount = entry.count;
          }
        }
        // Fall back to live PTY info if no tab had any data
        if (!best && (liveCwd || liveSsh)) {
          best = { cwd: liveCwd, sshCommand: liveSsh, remoteCwd: null };
        }
        if (best) {
          terminalsStore.setSplitContext(tab.id, { cwd: best.cwd, sshCommand: best.sshCommand, remoteCwd: best.remoteCwd });
        }
      }

      const wsForTab = workspaces.find(w => w.id === workspaceId);
      const paneForTab = wsForTab?.panes.find(p => p.id === paneId);
      if (paneForTab) {
        const insertIdx = afterTabId
          ? paneForTab.tabs.findIndex(t => t.id === afterTabId) + 1
          : paneForTab.tabs.length;
        paneForTab.tabs.splice(insertIdx >= 0 ? insertIdx : paneForTab.tabs.length, 0, tab);
        const { navHistoryStore } = await import('$lib/stores/navHistory.svelte');
        paneForTab.active_tab_id = tab.id;
        terminalsStore.markSpawning(tab.id);
        navHistoryStore.push({ workspaceId, paneId, tabId: tab.id });
      }
      return tab;
    },

    async createEditorTab(workspaceId: string, paneId: string, name: string, fileInfo: EditorFileInfo, insertAfterTabId?: string) {
      // Insert after specified tab, or after the currently active tab
      const pane = workspaces.flatMap(w => w.panes).find(p => p.id === paneId);
      const afterTabId = insertAfterTabId ?? pane?.active_tab_id ?? undefined;
      const tab = await commands.createEditorTab(workspaceId, paneId, name, fileInfo, afterTabId);
      const wsForEditor = workspaces.find(w => w.id === workspaceId);
      const paneForEditor = wsForEditor?.panes.find(p => p.id === paneId);
      if (paneForEditor) {
        const targetIdx = afterTabId ? paneForEditor.tabs.findIndex(t => t.id === afterTabId) : paneForEditor.tabs.findIndex(t => t.id === paneForEditor.active_tab_id);
        const insertIdx = targetIdx === -1 ? paneForEditor.tabs.length : targetIdx + 1;
        paneForEditor.tabs.splice(insertIdx, 0, tab);
        const { navHistoryStore } = await import('$lib/stores/navHistory.svelte');
        paneForEditor.active_tab_id = tab.id;
        navHistoryStore.push({ workspaceId, paneId, tabId: tab.id });
      }
      return tab;
    },

    async createDiffTab(workspaceId: string, paneId: string, name: string, diffContext: DiffContext, afterTabId?: string | null) {
      const tab = await commands.createDiffTab(workspaceId, paneId, name, diffContext, afterTabId);
      const wsForDiffTab = workspaces.find(w => w.id === workspaceId);
      const paneForDiffTab = wsForDiffTab?.panes.find(p => p.id === paneId);
      if (paneForDiffTab) {
        const activeIdx = paneForDiffTab.tabs.findIndex(t => t.id === (afterTabId ?? paneForDiffTab.active_tab_id));
        const insertIdx = activeIdx === -1 ? paneForDiffTab.tabs.length : activeIdx + 1;
        paneForDiffTab.tabs.splice(insertIdx, 0, tab);
        const { navHistoryStore: navHistory } = await import('$lib/stores/navHistory.svelte');
        paneForDiffTab.active_tab_id = tab.id;
        navHistory.push({ workspaceId, paneId, tabId: tab.id });
      }
      return tab;
    },

    async deleteTab(workspaceId: string, paneId: string, tabId: string) {
      // Tear down any agent bridge on this tab BEFORE it's removed from state, so the
      // surviving partner isn't left "bridged to a ghost" (a dangling bridge to a
      // closed tab would hide the survivor from the Agent Bridge picker).
      //
      // PERMANENT-REMOVAL ONLY: deleteTab is the genuine-close path — Cmd+W, the ×
      // button, or the shell itself exiting (pty-close → deleteTab). It is NOT reached
      // when a tab merely goes inactive, so the bridge correctly survives all of:
      //   • suspend        — suspendTab keeps the tab; pty-close is gated by isTabSuspending
      //   • app quit       — pty-close returns early on terminalsStore.shuttingDown
      //   • restart/resume — tabs reload from persisted state; rehydrate() rebuilds the
      //                      bridge, and a dormant partner just queues until it resumes
      //   • reload / move  — go through commands.deleteTab / atomic move, not this path
      // A session ENDING (Claude exits to its shell) is not a tab delete either — that
      // only suspends delivery; the bridge re-binds when the agent re-inits. We clear
      // the durable pairing here precisely because the tab is gone for good.
      //
      // Dynamic import avoids a static cycle (agentBridge imports this store).
      import('$lib/stores/agentBridge.svelte').then(m => m.agentBridgeStore.handleTabClosed(tabId)).catch(() => {});
      import('$lib/stores/agentMesh.svelte').then(m => m.agentMeshStore.handleTabClosed(tabId)).catch(() => {});

      // If closing a diff tab with a pending Claude request, respond with rejection
      // so Claude Code doesn't hang waiting for accept/reject.
      const wsForDiff = workspaces.find(w => w.id === workspaceId);
      const paneForDiff = wsForDiff?.panes.find(p => p.id === paneId);
      const diffTab = paneForDiff?.tabs.find(t => t.id === tabId);
      if (diffTab?.tab_type === 'diff' && diffTab.diff_context?.request_id) {
        commands.claudeCodeRespond(diffTab.diff_context.request_id, { result: 'DIFF_REJECTED' }).catch(() => {});
      }

      // Migrate tab notes to workspace if enabled
      if (preferencesStore.migrateTabNotes) {
        const ws = workspaces.find(w => w.id === workspaceId);
        const pane = ws?.panes.find(p => p.id === paneId);
        const tab = pane?.tabs.find(t => t.id === tabId);
        if (tab?.notes?.trim()) {
          try {
            const note = await commands.addWorkspaceNote(workspaceId, tab.notes, tab.notes_mode ?? null);
            if (ws) {
              ws.workspace_notes = [...ws.workspace_notes, note];
            }
          } catch (e) {
            logError(`Failed to migrate tab notes: ${e}`);
          }
        }
      }
      await commands.deleteTab(workspaceId, paneId, tabId);
      const wsForDelete = workspaces.find(w => w.id === workspaceId);
      const paneForDelete = wsForDelete?.panes.find(p => p.id === paneId);
      if (paneForDelete) {
        const oldIndex = paneForDelete.tabs.findIndex(t => t.id === tabId);
        paneForDelete.tabs.splice(oldIndex, 1);
        if (paneForDelete.active_tab_id === tabId) {
          // Prefer nav history: go back to the tab you came from
          const { navHistoryStore } = await import('$lib/stores/navHistory.svelte');
          const prev = navHistoryStore.peekBackForClose(tabId, e => !!paneForDelete.tabs.find(t => t.id === e.tabId));
          const newActiveId = prev ? prev.tabId : pickNextActiveTab(paneForDelete.tabs, oldIndex);
          // If the next active tab is a suspended terminal, gate it behind resume prompt
          if (newActiveId && newActiveId !== paneForDelete.active_tab_id) {
            const nextTab = paneForDelete.tabs.find(t => t.id === newActiveId);
            const isNextTerminal = nextTab && (nextTab.tab_type === 'terminal' || !nextTab.tab_type);
            if (isNextTerminal && !terminalsStore.get(newActiveId)) {
              pendingResumePanes.add(paneForDelete.id);
            }
          }
          paneForDelete.active_tab_id = newActiveId;
          if (prev && newActiveId) {
            // newActiveId is in nav history — pin walkIndex there so Cmd+[/]
            // continues from this position instead of resetting to MRU front.
            navHistoryStore.removeTab(tabId, newActiveId);
          } else {
            navHistoryStore.removeTab(tabId);
            if (newActiveId) {
              // Fallback active tab was never visited — record it at MRU front.
              navHistoryStore.push({ workspaceId, paneId, tabId: newActiveId });
            }
          }
          return;
        }
      }
      import('$lib/stores/navHistory.svelte').then(m => m.navHistoryStore.removeTab(tabId));
    },

    async suspendTab(workspaceId: string, paneId: string, tabId: string) {
      const ws = workspaces.find(w => w.id === workspaceId);
      const pane = ws?.panes.find(p => p.id === paneId);
      const tab = pane?.tabs.find(t => t.id === tabId);
      if (!tab) return;

      const instance = terminalsStore.get(tabId);
      if (!instance) return; // Already suspended

      // Gather context before killing
      let cwd: string | null = null;
      let sshCommand: string | null = null;
      let remoteCwd: string | null = null;

      try {
        const info = await commands.getPtyInfo(instance.ptyId);
        cwd = info.cwd;
        sshCommand = info.foreground_command;
      } catch { /* PTY may already be gone */ }

      if (sshCommand) {
        const oscState = terminalsStore.getOsc(tabId);
        const osc7Cwd = oscState?.cwd ?? null;
        const promptCwd = oscState?.promptCwd ?? null;
        const isOsc7Stale = osc7Cwd === cwd;
        const osc7RemoteCwd = (osc7Cwd && !isOsc7Stale) ? osc7Cwd : null;
        remoteCwd = osc7RemoteCwd ?? promptCwd ?? null;
      }

      // Save scrollback, then kill PTY
      try {
        await commands.saveTerminalScrollback(instance.ptyId, tabId);
      } catch { /* best effort */ }

      // Detach SSH MCP bridge — refcounted on the Rust side, so other tabs
      // sharing the tunnel keep it alive; suspended tab is removed from tab_ids.
      await disableBridge(tabId).catch(() => {});

      // Guard the pty-close listener from deleting this tab
      suspendingTabIds.add(tabId);
      try {
        await commands.killTerminal(instance.ptyId).catch(() => {});
      } finally {
        // Clear after a short delay to let the pty-close event fire and be ignored
        setTimeout(() => suspendingTabIds.delete(tabId), 2000);
      }
      terminalsStore.unregister(tabId);

      // Clear pty_id and save restore context on backend
      await commands.suspendTab(workspaceId, paneId, tabId, cwd, sshCommand, remoteCwd);

      // Update local state
      tab.pty_id = null;
      tab.restore_cwd = cwd;
      tab.restore_ssh_command = sshCommand;
      tab.restore_remote_cwd = remoteCwd;
      tab.suspended_at = new Date().toISOString();

      // Show resume prompt in the pane, and destroy the TerminalPane component
      // so it re-mounts (and re-spawns the PTY) when the user clicks Resume.
      pendingResumePanes.add(paneId);
      window.dispatchEvent(new CustomEvent<string[]>('deactivate-tabs', { detail: [tabId] }));
    },

    async archiveTab(workspaceId: string, paneId: string, tabId: string, displayName: string) {
      const ws = workspaces.find(w => w.id === workspaceId);
      const pane = ws?.panes.find(p => p.id === paneId);
      const tab = pane?.tabs.find(t => t.id === tabId);
      if (!tab) return;

      // Gather context (terminal-specific for terminal tabs, null for editor/diff)
      let scrollback: string | null = null;
      let cwd: string | null = null;
      let sshCommand: string | null = null;
      let remoteCwd: string | null = null;

      if (tab.tab_type === 'terminal') {
        const ctx = await this._gatherTabContext(tabId);
        scrollback = ctx.scrollback;
        cwd = ctx.cwd;
        sshCommand = ctx.sshCommand;

        // Detect remote cwd
        if (sshCommand) {
          const oscState = terminalsStore.getOsc(tabId);
          const osc7Cwd = oscState?.cwd ?? null;
          const promptCwd = oscState?.promptCwd ?? null;
          const isOsc7Stale = osc7Cwd === cwd;
          const osc7RemoteCwd = (osc7Cwd && !isOsc7Stale) ? osc7Cwd : null;
          remoteCwd = osc7RemoteCwd ?? promptCwd ?? null;
        }
      }

      // Skip note migration — archived tabs preserve their notes and restore them intact

      await commands.archiveTab(workspaceId, paneId, tabId, displayName, scrollback, cwd, sshCommand, remoteCwd);
      import('$lib/stores/navHistory.svelte').then(m => m.navHistoryStore.removeTab(tabId));

      // Build the archived tab object for local state
      const archivedTab: Tab = {
        ...tab,
        archived_name: displayName,
        pty_id: null,
        scrollback,
        restore_cwd: cwd,
        restore_ssh_command: sshCommand,
        restore_remote_cwd: remoteCwd,
        archived_at: new Date().toISOString(),
      };

      // Update local state
      if (!ws) return;
      ws.archived_tabs.push(archivedTab);
      const archivePane = ws.panes.find(p => p.id === paneId);
      if (archivePane) {
        const oldIndex = archivePane.tabs.findIndex(t => t.id === tabId);
        archivePane.tabs.splice(oldIndex, 1);
        if (archivePane.active_tab_id === tabId) {
          const { navHistoryStore } = await import('$lib/stores/navHistory.svelte');
          const prev = navHistoryStore.peekBackForClose(tabId, e => !!archivePane.tabs.find(t => t.id === e.tabId));
          const newActiveId = prev ? prev.tabId : pickNextActiveTab(archivePane.tabs, oldIndex);
          archivePane.active_tab_id = newActiveId;
          if (prev && newActiveId) {
            navHistoryStore.removeTab(tabId, newActiveId);
          } else {
            navHistoryStore.removeTab(tabId);
            if (newActiveId) {
              navHistoryStore.push({ workspaceId, paneId, tabId: newActiveId });
            }
          }
        }
      }
    },

    async restoreArchivedTab(workspaceId: string, tabId: string) {
      const ws = workspaces.find(w => w.id === workspaceId);
      if (!ws) return;

      // Find active pane
      const pane = ws.panes.find(p => p.id === ws.active_pane_id) ?? ws.panes[0];
      if (!pane) return;

      const tab = await commands.restoreArchivedTab(workspaceId, pane.id, tabId);

      // Migrate old auto-resume command if needed (archived tabs skip the startup migration)
      const OLD_PATTERNS = [
        'if [ -n "%claudeSessionId" ]; then claude --resume %claudeSessionId; elif [ -n "%claudeResumeCommand" ]; then %claudeResumeCommand; else claude --continue; fi',
        "if [ -n '%claudeSessionId' ]; then claude --resume %claudeSessionId; elif [ -n '%claudeResumeCommand' ]; then eval %claudeResumeCommand; else claude --continue; fi",
        'claude --resume %claudeSessionId "/aiterm init"',
      ];
      const OLD_PATTERN_REGEXES = [
        /^if \[ -n ['"].*['"] \]; then claude --resume .*; elif \[ -n ['"].*['"] \]; then (eval )?.*; else claude --continue; fi$/,
        /^claude --resume \S+ "\/aiterm init"$/,
      ];
      const arCmd = tab.auto_resume_command;
      if (arCmd && arCmd !== CLAUDE_RESUME_COMMAND && (
        OLD_PATTERNS.includes(arCmd) || OLD_PATTERN_REGEXES.some(re => re.test(arCmd))
      )) {
        tab.auto_resume_command = CLAUDE_RESUME_COMMAND;
        tab.auto_resume_remembered_command = CLAUDE_RESUME_COMMAND;
        await commands.setTabAutoResumeContext(
          workspaceId, pane.id, tab.id,
          tab.auto_resume_cwd, tab.auto_resume_ssh_command,
          tab.auto_resume_remote_cwd, tab.auto_resume_command,
          tab.auto_resume_pinned,
        );
      }

      // Update local state
      const archIdx = ws.archived_tabs.findIndex(t => t.id === tabId);
      if (archIdx >= 0) ws.archived_tabs.splice(archIdx, 1);
      const activeIdx = pane.active_tab_id
        ? pane.tabs.findIndex(t => t.id === pane.active_tab_id)
        : -1;
      const insertIdx = activeIdx >= 0 ? activeIdx + 1 : 0;
      pane.tabs.splice(insertIdx, 0, tab);
      pane.active_tab_id = tab.id;
      terminalsStore.markSpawning(tab.id);
      // Restore picks this tab's placement (next to the active tab) on purpose;
      // mark it so the active-group promotion effect leaves it there instead of
      // treating it like a resumed-from-suspend tab and moving it to the end.
      terminalsStore.markRestoredFromArchive(tab.id);
      import('$lib/stores/navHistory.svelte').then(m => {
        m.navHistoryStore.push({ workspaceId, paneId: pane.id, tabId });
      });
    },

    async deleteArchivedTab(workspaceId: string, tabId: string) {
      await commands.deleteArchivedTab(workspaceId, tabId);
      const ws = workspaces.find(w => w.id === workspaceId);
      if (ws) {
        const idx = ws.archived_tabs.findIndex(t => t.id === tabId);
        if (idx >= 0) ws.archived_tabs.splice(idx, 1);
      }
    },

    async reorderTabs(workspaceId: string, paneId: string, tabIds: string[]) {
      const ws = workspaces.find(w => w.id === workspaceId);
      const pane = ws?.panes.find(p => p.id === paneId);
      if (pane) {
        const reordered = tabIds
          .map(id => pane.tabs.find(t => t.id === id))
          .filter((t): t is Tab => t !== undefined);
        pane.tabs.splice(0, pane.tabs.length, ...reordered);
      }
      await commands.reorderTabs(workspaceId, paneId, tabIds);
    },

    /**
     * When group-active-tabs is enabled, a just-resumed tab visually jumps into
     * the active group (which renders ahead of suspended tabs) but keeps its old
     * storage position. Move it in storage too, so the visible order is the real
     * order. Placement: when the resume was a click and the tab the user came
     * from (`anchorTabId`) is still in the active group, land right after that
     * anchor — the resumed tab appears next to where the user just was. Otherwise
     * (workspace resume, auto-resume, no live anchor) fall back to the end of the
     * active group, i.e. just before the first still-suspended terminal tab.
     * That makes drag-reordering within the active group meaningful and lets the
     * most-recently-used tabs settle at the front, so once everything is
     * suspended they stay where they were (front/left).
     * No-op when grouping is off (storage order already equals display order).
     */
    promoteResumedTab(workspaceId: string, paneId: string, tabId: string, anchorTabId: string | null = null) {
      if (!preferencesStore.groupActiveTabs) return;
      const ws = workspaces.find(w => w.id === workspaceId);
      const pane = ws?.panes.find(p => p.id === paneId);
      if (!pane) return;
      const tab = pane.tabs.find(t => t.id === tabId);
      const isTerminal = (t: Tab) => t.tab_type === 'terminal' || !t.tab_type;
      if (!tab || !isTerminal(tab)) return;

      // First still-suspended terminal tab marks the active/suspended boundary.
      // (The resumed tab itself is excluded — it has just left the suspended group.)
      const isSuspendedTerminal = (t: Tab) =>
        isTerminal(t) && t.id !== tabId &&
        !terminalsStore.get(t.id) && !terminalsStore.isSpawning(t.id);

      const without = pane.tabs.filter(t => t.id !== tabId);
      let insertAt = -1;
      if (anchorTabId && anchorTabId !== tabId) {
        const anchorIdx = without.findIndex(t => t.id === anchorTabId);
        if (anchorIdx !== -1 && !isSuspendedTerminal(without[anchorIdx])) {
          insertAt = anchorIdx + 1;
        }
      }
      if (insertAt === -1) {
        insertAt = without.findIndex(isSuspendedTerminal);
        if (insertAt === -1) insertAt = without.length;
      }
      const reordered = [...without.slice(0, insertAt), tab, ...without.slice(insertAt)];

      const curIds = pane.tabs.map(t => t.id);
      const newIds = reordered.map(t => t.id);
      if (newIds.every((id, i) => id === curIds[i])) return; // already in place
      this.reorderTabs(workspaceId, paneId, newIds);
    },

    async renameTab(workspaceId: string, paneId: string, tabId: string, name: string, customName?: boolean) {
      await commands.renameTab(workspaceId, paneId, tabId, name, customName);
      const { tab } = findTab(workspaceId, paneId, tabId);
      if (tab) {
        tab.name = name;
        if (customName !== undefined) tab.custom_name = customName;
      }
    },

    async updateEditorTabFile(tabId: string, name: string, fileInfo: EditorFileInfo) {
      await commands.updateEditorTabFile(tabId, name, fileInfo);
      for (const ws of workspaces) {
        for (const pane of ws.panes) {
          const tab = pane.tabs.find(t => t.id === tabId);
          if (tab) {
            tab.name = name;
            tab.editor_file = fileInfo;
            return;
          }
        }
      }
    },

    async setActiveTab(workspaceId: string, paneId: string, tabId: string) {
      await commands.setActiveTab(workspaceId, paneId, tabId);
      const { pane, tab } = findTab(workspaceId, paneId, tabId);
      if (pane) pane.active_tab_id = tabId;
      if (tab?.import_highlight) tab.import_highlight = false;
      // Record in nav history. The store's `navigating` flag no-ops this
      // during a Cmd+[ / Cmd+] walk, so walkIndex stays anchored.
      const { navHistoryStore } = await import('$lib/stores/navHistory.svelte');
      navHistoryStore.push({ workspaceId, paneId, tabId });
    },

    async setTabPtyId(workspaceId: string, paneId: string, tabId: string, ptyId: string) {
      await commands.setTabPtyId(workspaceId, paneId, tabId, ptyId);
      const { tab } = findTab(workspaceId, paneId, tabId);
      if (tab) {
        tab.pty_id = ptyId;
        tab.suspended_at = null; // live again — clear suspended-age
      }
    },

    async setTabAutoResumeContext(workspaceId: string, paneId: string, tabId: string, cwd: string | null, sshCommand: string | null, remoteCwd: string | null, command: string | null = null, pinned?: boolean) {
      await commands.setTabAutoResumeContext(workspaceId, paneId, tabId, cwd, sshCommand, remoteCwd, command, pinned);
      const { tab } = findTab(workspaceId, paneId, tabId);
      if (tab) {
        tab.auto_resume_cwd = cwd;
        tab.auto_resume_ssh_command = sshCommand;
        tab.auto_resume_remote_cwd = remoteCwd;
        tab.auto_resume_command = command;
        tab.auto_resume_enabled = true;
        if (command != null) tab.auto_resume_remembered_command = command;
        if (pinned != null) tab.auto_resume_pinned = pinned;
      }
    },

    async disableAutoResume(workspaceId: string, paneId: string, tabId: string) {
      await commands.setTabAutoResumeEnabled(workspaceId, paneId, tabId, false);
      const { tab } = findTab(workspaceId, paneId, tabId);
      if (tab) tab.auto_resume_enabled = false;
    },

    setSplitRatioLocal(workspaceId: string, splitId: string, ratio: number) {
      const ws = workspaces.find(w => w.id === workspaceId);
      if (ws?.split_root) ws.split_root = updateRatioInTree(ws.split_root, splitId, ratio);
    },

    async persistSplitRatio(workspaceId: string, splitId: string, ratio: number) {
      await commands.setSplitRatio(workspaceId, splitId, ratio);
    },

    /**
     * Gather terminal context (scrollback, cwd, SSH, history) for a source tab.
     * Shared by splitPaneWithContext, moveTabToWorkspace, and copyTabToWorkspace.
     */
    async _gatherTabContext(sourceTabId: string) {
      const instance = terminalsStore.get(sourceTabId);
      let scrollback: string | null = null;
      let cwd: string | null = null;
      let sshCommand: string | null = null;

      if (instance) {
        if (preferencesStore.cloneScrollback) {
          try {
            const bytes = await commands.serializeTerminal(instance.ptyId);
            scrollback = new TextDecoder().decode(new Uint8Array(bytes));
          } catch (e) {
            logError(`Failed to serialize scrollback: ${e}`);
          }
        }

        if (preferencesStore.cloneCwd || preferencesStore.cloneSsh) {
          try {
            const info = await commands.getPtyInfo(instance.ptyId);
            cwd = preferencesStore.cloneCwd ? info.cwd : null;
            sshCommand = preferencesStore.cloneSsh ? info.foreground_command : null;
          } catch (e) {
            // PTY may already be gone
          }
        }
      }

      return { instance, scrollback, cwd, sshCommand };
    },

    /**
     * Store split context (cwd/SSH) for a newly created tab so TerminalPane
     * consumes it on mount.
     */
    _storeSplitContext(sourceTabId: string, newTabId: string, cwd: string | null, sshCommand: string | null, instance: { terminal: import('@xterm/xterm').Terminal } | undefined) {
      if (!preferencesStore.cloneCwd && !preferencesStore.cloneSsh) return;

      const oscState = terminalsStore.getOsc(sourceTabId);
      const osc7Cwd = oscState?.cwd ?? null;
      const promptCwd = oscState?.promptCwd ?? null;

      let remoteCwd: string | null = null;
      if (sshCommand) {
        const isOsc7Stale = osc7Cwd === cwd;
        const osc7RemoteCwd = (osc7Cwd && !isOsc7Stale) ? osc7Cwd : null;
        remoteCwd = osc7RemoteCwd ?? promptCwd ?? (instance ? extractRemoteCwd(instance.terminal) : null);
      } else if (preferencesStore.cloneCwd) {
        cwd = cwd ?? osc7Cwd;
      }

      if (cwd || sshCommand) {
        terminalsStore.setSplitContext(newTabId, { cwd, sshCommand, remoteCwd });
      }
    },

    /**
     * Copy a tab to another workspace (clone with context, keep source).
     */
    async copyTabToWorkspace(sourceWsId: string, sourcePaneId: string, sourceTabId: string, targetWsId: string) {
      const sourceWs = workspaces.find(w => w.id === sourceWsId);
      const sourcePane = sourceWs?.panes.find(p => p.id === sourcePaneId);
      const sourceTab = sourcePane?.tabs.find(t => t.id === sourceTabId);
      if (!sourceTab) return;

      // Gather context from source
      const { instance, scrollback, cwd, sshCommand } = await this._gatherTabContext(sourceTabId);

      // Create tab in target workspace's first pane, preserving original active tab
      const targetWs = workspaces.find(w => w.id === targetWsId);
      if (!targetWs || targetWs.panes.length === 0) return;
      const targetPane = targetWs.panes[0];
      const previousActiveTabId = targetPane.active_tab_id;

      const tabName = sourceTab.custom_name && preferencesStore.numberDuplicatedTabs
        ? nextDuplicateName(sourceTab.name, allTabNames(targetWs))
        : sourceTab.name;
      const newTab = await commands.createTab(targetWsId, targetPane.id, tabName);

      // Restore the previously active tab (createTab sets the new one as active)
      if (previousActiveTabId) {
        await commands.setActiveTab(targetWsId, targetPane.id, previousActiveTabId);
      }

      // Set scrollback on the new tab
      if (scrollback) {
        await commands.setTabScrollback(newTab.id, scrollback);
      }

      // Copy custom name
      if (sourceTab.custom_name) {
        await commands.renameTab(targetWsId, targetPane.id, newTab.id, tabName, true);
      }

      // Copy history
      if (preferencesStore.cloneHistory) {
        try {
          await commands.copyTabHistory(sourceTabId, newTab.id);
        } catch (e) {
          logError(`Failed to copy tab history: ${e}`);
        }
      }

      // Copy notes
      if (preferencesStore.cloneNotes && sourceTab.notes) {
        await commands.setTabNotes(targetWsId, targetPane.id, newTab.id, sourceTab.notes);
      }
      if (preferencesStore.cloneNotes && sourceTab.notes_mode) {
        await commands.setTabNotesMode(targetWsId, targetPane.id, newTab.id, sourceTab.notes_mode);
      }

      // Copy trigger variables
      if (preferencesStore.cloneVariables) {
        const srcVars = getVariables(sourceTabId);
        if (srcVars && srcVars.size > 0) {
          const plain: Record<string, string> = {};
          for (const [k, v] of srcVars) plain[k] = v;
          await commands.setTabTriggerVariables(targetWsId, targetPane.id, newTab.id, plain).catch(e =>
            logError(`Failed to copy trigger variables: ${e}`)
          );
        }
      }

      // Copy auto-resume settings
      if (preferencesStore.cloneAutoResume && (sourceTab.auto_resume_cwd || sourceTab.auto_resume_ssh_command || sourceTab.auto_resume_command)) {
        await this.setTabAutoResumeContext(
          targetWsId, targetPane.id, newTab.id,
          sourceTab.auto_resume_cwd,
          sourceTab.auto_resume_ssh_command,
          sourceTab.auto_resume_remote_cwd,
          sourceTab.auto_resume_command,
          sourceTab.auto_resume_pinned,
        );
      }

      // Store split context for the new terminal
      this._storeSplitContext(sourceTabId, newTab.id, cwd, sshCommand, instance);

      // Mark as unreviewed activity so the tab shows the activity dot
      activityStore.markActive(newTab.id);

      // Reload all workspaces
      const data = await commands.getWindowData();
      workspaces = data.workspaces;
    },

    /**
     * Move a tab to another workspace (delete source, create in target).
     */
    async moveTabToWorkspace(sourceWsId: string, sourcePaneId: string, sourceTabId: string, targetWsId: string) {
      const sourceWs = workspaces.find(w => w.id === sourceWsId);
      const sourcePane = sourceWs?.panes.find(p => p.id === sourcePaneId);
      if (!sourceWs || !sourcePane) return;

      // Snapshot pre-move state so we can re-run the active-tab fallback with
      // groupActiveTabs awareness (backend's tab_pos - 1 pick ignores suspension).
      const movedTabWasActive = sourcePane.active_tab_id === sourceTabId;
      const movedTabIndex = sourcePane.tabs.findIndex(t => t.id === sourceTabId);

      // Mark the PTY as preserved so the old TerminalPane's onDestroy
      // doesn't kill it when Svelte removes it from the source workspace's keyed each block
      const termInstance = terminalsStore.get(sourceTabId);
      if (termInstance) {
        terminalsStore.preservePty(termInstance.ptyId);
      }

      // Move the tab in backend state (preserves PTY, scrollback, everything)
      await commands.moveTabToWorkspaceCmd(sourceWsId, sourcePaneId, sourceTabId, targetWsId);

      // Refresh frontend state from backend
      const data = await commands.getWindowData();

      // Ensure source pane's active_tab_id points to an existing tab
      // (backend handles this, but guard against stale binary during dev)
      const updatedSourceWs = data.workspaces.find(w => w.id === sourceWsId);
      const updatedSourcePane = updatedSourceWs?.panes.find(p => p.id === sourcePaneId);
      if (updatedSourcePane && updatedSourcePane.active_tab_id && !updatedSourcePane.tabs.some(t => t.id === updatedSourcePane.active_tab_id)) {
        const fallback = updatedSourcePane.tabs[updatedSourcePane.tabs.length - 1]?.id ?? null;
        updatedSourcePane.active_tab_id = fallback;
        if (fallback) {
          await commands.setActiveTab(sourceWsId, sourcePaneId, fallback);
        }
      }

      // If we moved the active tab, redo the active-tab pick using the same
      // grouping-aware logic as deleteTab — backend just picks the adjacent
      // tab in storage order, which ignores groupActiveTabs and lands on a
      // suspended tab when a live one was the user's expectation.
      if (movedTabWasActive && updatedSourcePane && updatedSourcePane.tabs.length > 0 && movedTabIndex >= 0) {
        const preferred = pickNextActiveTab(updatedSourcePane.tabs, movedTabIndex);
        if (preferred && preferred !== updatedSourcePane.active_tab_id) {
          updatedSourcePane.active_tab_id = preferred;
          await commands.setActiveTab(sourceWsId, sourcePaneId, preferred);
        }
      }

      // If the source pane is now empty, handle cleanup
      if (updatedSourcePane && updatedSourcePane.tabs.length === 0) {
        if (updatedSourceWs!.panes.length > 1) {
          await commands.deletePane(sourceWsId, sourcePaneId);
        } else {
          await commands.createTab(sourceWsId, sourcePaneId, 'Terminal 1');
        }
        // Re-fetch after cleanup
        const data2 = await commands.getWindowData();
        workspaces = data2.workspaces;
      } else {
        workspaces = data.workspaces;
      }

      // Update the terminal store's workspace/pane references for the moved tab
      const finalTargetWs = workspaces.find(w => w.id === targetWsId);
      const finalTargetPane = finalTargetWs?.panes.find(p => p.tabs.some(t => t.id === sourceTabId));
      if (finalTargetPane) {
        terminalsStore.updateTabLocation(sourceTabId, targetWsId, finalTargetPane.id);
      }
    },

    /**
     * Move a tab to another pane in the same workspace, PTY intact (no clone,
     * no respawn). The backend removes the source pane if this empties it.
     * `insertBeforeTabId` positions the tab within the target pane (appended
     * when omitted).
     */
    async moveTabToPane(workspaceId: string, sourcePaneId: string, tabId: string, targetPaneId: string, insertBeforeTabId?: string | null) {
      if (sourcePaneId === targetPaneId) return;
      const ws = workspaces.find(w => w.id === workspaceId);
      const sourcePane = ws?.panes.find(p => p.id === sourcePaneId);
      if (!ws || !sourcePane) return;

      const movedTabWasActive = sourcePane.active_tab_id === tabId;
      const movedTabIndex = sourcePane.tabs.findIndex(t => t.id === tabId);

      // The tab's component moves between per-pane keyed each blocks in
      // +page.svelte, so Svelte destroys and recreates it — preserve the PTY
      // across the gap, same as moveTabToWorkspace.
      const termInstance = terminalsStore.get(tabId);
      if (termInstance) {
        terminalsStore.preservePty(termInstance.ptyId);
      }

      await commands.moveTabToPaneCmd(workspaceId, sourcePaneId, tabId, targetPaneId, insertBeforeTabId);

      const data = await commands.getWindowData();

      // Backend's active-tab fallback for the source pane ignores
      // groupActiveTabs — redo the pick with grouping awareness.
      const updatedWs = data.workspaces.find(w => w.id === workspaceId);
      const updatedSourcePane = updatedWs?.panes.find(p => p.id === sourcePaneId);
      if (movedTabWasActive && updatedSourcePane && updatedSourcePane.tabs.length > 0 && movedTabIndex >= 0) {
        const preferred = pickNextActiveTab(updatedSourcePane.tabs, movedTabIndex);
        if (preferred && preferred !== updatedSourcePane.active_tab_id) {
          updatedSourcePane.active_tab_id = preferred;
          await commands.setActiveTab(workspaceId, sourcePaneId, preferred);
        }
      }

      workspaces = data.workspaces;
      terminalsStore.updateTabLocation(tabId, workspaceId, targetPaneId);
    },

    /**
     * Move a tab into a brand-new split pane — unlike splitPaneWithContext
     * this moves the existing tab (PTY intact) instead of cloning it. The
     * split is created on `targetPaneId` (which may differ from the source
     * pane, e.g. dropping on another pane's edge); `before` puts the new pane
     * on the left/top side.
     */
    async moveTabToSplit(workspaceId: string, sourcePaneId: string, tabId: string, targetPaneId: string, direction: SplitDirection, before = false) {
      const ws = workspaces.find(w => w.id === workspaceId);
      const sourcePane = ws?.panes.find(p => p.id === sourcePaneId);
      if (!ws || !sourcePane) return;
      // Splitting a pane off with its only tab just churns pane IDs
      if (sourcePaneId === targetPaneId && sourcePane.tabs.length === 1) return;

      const movedTabWasActive = sourcePane.active_tab_id === tabId;
      const movedTabIndex = sourcePane.tabs.findIndex(t => t.id === tabId);

      const termInstance = terminalsStore.get(tabId);
      if (termInstance) {
        terminalsStore.preservePty(termInstance.ptyId);
      }

      const newPane = await commands.moveTabToSplitCmd(workspaceId, sourcePaneId, tabId, targetPaneId, direction, before);

      const data = await commands.getWindowData();

      const updatedWs = data.workspaces.find(w => w.id === workspaceId);
      const updatedSourcePane = updatedWs?.panes.find(p => p.id === sourcePaneId);
      if (movedTabWasActive && updatedSourcePane && updatedSourcePane.tabs.length > 0 && movedTabIndex >= 0) {
        const preferred = pickNextActiveTab(updatedSourcePane.tabs, movedTabIndex);
        if (preferred && preferred !== updatedSourcePane.active_tab_id) {
          updatedSourcePane.active_tab_id = preferred;
          await commands.setActiveTab(workspaceId, sourcePaneId, preferred);
        }
      }

      workspaces = data.workspaces;
      terminalsStore.updateTabLocation(tabId, workspaceId, newPane.id);
    },

    async reorderWorkspaces(workspaceIds: string[]) {
      const reordered = workspaceIds
        .map(id => workspaces.find(w => w.id === id))
        .filter((w): w is Workspace => w !== undefined);
      workspaces = reordered;
      await commands.reorderWorkspaces(workspaceIds);
    },

    async duplicateWorkspace(sourceWorkspaceId: string, insertIndex: number) {
      const ws = workspaces.find(w => w.id === sourceWorkspaceId);
      if (!ws) return;

      // 1. Gather context for all tabs in source workspace
      const tabContexts: commands.TabContext[] = [];
      for (const pane of ws.panes) {
        for (const tab of pane.tabs) {
          const ctx = await this._gatherTabContext(tab.id);

          let remoteCwd: string | null = null;
          if (ctx.sshCommand && ctx.instance) {
            const oscState = terminalsStore.getOsc(tab.id);
            const osc7Cwd = oscState?.cwd ?? null;
            const promptCwd = oscState?.promptCwd ?? null;
            const isOsc7Stale = osc7Cwd === ctx.cwd;
            const osc7RemoteCwd = (osc7Cwd && !isOsc7Stale) ? osc7Cwd : null;
            remoteCwd = osc7RemoteCwd ?? promptCwd ?? extractRemoteCwd(ctx.instance.terminal);
          }

          tabContexts.push({
            tab_id: tab.id,
            scrollback: ctx.scrollback,
            cwd: ctx.cwd,
            ssh_command: ctx.sshCommand,
            remote_cwd: remoteCwd,
          });
        }
      }

      // 2. Duplicate on backend (deep-clones with new IDs, applies scrollback)
      const result = await commands.duplicateWorkspaceCmd(sourceWorkspaceId, insertIndex, tabContexts);

      // 3. Copy shell history for each tab pair
      if (preferencesStore.cloneHistory) {
        for (const [oldTabId, newTabId] of Object.entries(result.tab_id_map)) {
          try {
            await commands.copyTabHistory(oldTabId, newTabId);
          } catch (e) {
            // ignore — history may not exist
          }
        }
      }

      // 4. Rename duplicate workspace
      const dupName = nextDuplicateName(ws.name, workspaces.map(w => w.name));
      await commands.renameWorkspace(result.workspace.id, dupName);

      // 5. Reload all workspaces to get consistent state
      const data = await commands.getWindowData();
      workspaces = data.workspaces;
    },

    async duplicateTab(workspaceId: string, paneId: string, tabId: string, opts?: { shallow?: boolean }) {
      const ws = workspaces.find(w => w.id === workspaceId);
      const pane = ws?.panes.find(p => p.id === paneId);
      const sourceTab = pane?.tabs.find(t => t.id === tabId);
      if (!sourceTab) return;

      const shallow = opts?.shallow ?? false;

      // 1. Gather context from source terminal
      const { instance, scrollback, cwd, sshCommand } = await this._gatherTabContext(tabId);

      // 2. Compute duplicate name with incrementing index for custom names
      const dupName = sourceTab.custom_name && preferencesStore.numberDuplicatedTabs
        ? nextDuplicateName(sourceTab.name, allTabNames(ws!))
        : sourceTab.name;

      // 3. Create new tab (appended at end)
      const newTab = await commands.createTab(workspaceId, paneId, dupName);

      // 4. Copy custom name if source had one
      if (sourceTab.custom_name) {
        await commands.renameTab(workspaceId, paneId, newTab.id, dupName, true);
      }

      // 5. Set scrollback (skip in shallow mode)
      if (!shallow && scrollback) {
        await commands.setTabScrollback(newTab.id, scrollback);
      }

      // 6. Copy history
      if (preferencesStore.cloneHistory) {
        try {
          await commands.copyTabHistory(tabId, newTab.id);
        } catch (e) {
          logError(`Failed to copy tab history: ${e}`);
        }
      }

      // 7. Copy notes (skip in shallow mode)
      if (!shallow && preferencesStore.cloneNotes && sourceTab.notes) {
        await commands.setTabNotes(workspaceId, paneId, newTab.id, sourceTab.notes);
      }
      if (!shallow && preferencesStore.cloneNotes && sourceTab.notes_mode) {
        await commands.setTabNotesMode(workspaceId, paneId, newTab.id, sourceTab.notes_mode);
      }

      // 7c. Copy trigger variables (pref-gated, skip in shallow mode — variables are session-specific)
      if (!shallow && preferencesStore.cloneVariables) {
        const srcVars = getVariables(tabId);
        if (srcVars && srcVars.size > 0) {
          const plain: Record<string, string> = {};
          for (const [k, v] of srcVars) plain[k] = v;
          await commands.setTabTriggerVariables(workspaceId, paneId, newTab.id, plain).catch(e =>
            logError(`Failed to copy trigger variables: ${e}`)
          );
        }
      }

      // 7d. Copy auto-resume settings (skip in shallow mode)
      if (!shallow && preferencesStore.cloneAutoResume && (sourceTab.auto_resume_cwd || sourceTab.auto_resume_ssh_command || sourceTab.auto_resume_command)) {
        await this.setTabAutoResumeContext(
          workspaceId, paneId, newTab.id,
          sourceTab.auto_resume_cwd,
          sourceTab.auto_resume_ssh_command,
          sourceTab.auto_resume_remote_cwd,
          sourceTab.auto_resume_command,
          sourceTab.auto_resume_pinned,
        );
      }

      // 8. Store split context for the new TerminalPane to consume on mount
      terminalsStore.markSpawning(newTab.id);
      this._storeSplitContext(tabId, newTab.id, cwd, sshCommand, instance);

      // 9. Reorder to place new tab right after source
      const currentIds = pane!.tabs.map(t => t.id);
      const sourceIndex = currentIds.indexOf(tabId);
      // newTab.id was appended at end by createTab; move it after source
      const reordered = currentIds.filter(id => id !== newTab.id);
      reordered.splice(sourceIndex + 1, 0, newTab.id);
      await commands.reorderTabs(workspaceId, paneId, reordered);

      // 10. Switch to the new tab
      await commands.setActiveTab(workspaceId, paneId, newTab.id);
      const { navHistoryStore: navHistorySplit } = await import('$lib/stores/navHistory.svelte');
      navHistorySplit.push({ workspaceId, paneId, tabId: newTab.id });

      // 11. Reload workspace state
      const data = await commands.getWindowData();
      const updatedWs = data.workspaces.find(w => w.id === workspaceId);
      if (updatedWs) {
        const idx = workspaces.findIndex(w => w.id === workspaceId);
        if (idx >= 0) workspaces[idx] = updatedWs;
      }
    },

    async reloadTab(workspaceId: string, paneId: string, tabId: string) {
      const ws = workspaces.find(w => w.id === workspaceId);
      const pane = ws?.panes.find(p => p.id === paneId);
      const sourceTab = pane?.tabs.find(t => t.id === tabId);
      if (!ws || !pane || !sourceTab) return;

      // Editor tabs: re-read file from disk
      if (sourceTab.tab_type === 'editor') {
        window.dispatchEvent(new CustomEvent('editor-reload', { detail: { tabId } }));
        return;
      }

      // Diff tabs: nothing to reload (content is ephemeral from Claude)
      if (sourceTab.tab_type === 'diff') return;

      // Terminal tabs: duplicate + delete for full PTY restart
      // Remember exact name and position before duplication
      const tabName = sourceTab.name;
      const isCustom = sourceTab.custom_name;
      const sourceIndex = pane.tabs.findIndex(t => t.id === tabId);

      // Deep duplicate: clones scrollback, CWD, SSH, notes, history, auto-resume, variables
      await this.duplicateTab(workspaceId, paneId, tabId);

      // Reload state to get the new tab
      const freshData = await commands.getWindowData();
      const freshWs = freshData.workspaces.find(w => w.id === workspaceId);
      const freshPane = freshWs?.panes.find(p => p.id === paneId);
      if (!freshWs || !freshPane) return;

      // Find the new tab (duplicateTab places it right after source)
      const newTab = freshPane.tabs[sourceIndex + 1];
      if (!newTab) return;

      // Mark split context so auto-resume command fires on mount (reload = full restore).
      // Reload destination follows the auto_resume_pinned lever:
      //   - Pinned: the saved SSH command + remote CWD are explicit user intent and win
      //     over the live-captured split context (e.g. they pointed auto-resume at a
      //     renamed remote folder — editing the Remote CWD field auto-pins).
      //   - Unpinned: track the live session (reconnect to wherever it currently is);
      //     the saved SSH command is only a fallback when the live SSH has died.
      const splitCtx = terminalsStore.consumeSplitContext(newTab.id);
      if (splitCtx) {
        const ctx = { ...splitCtx, fireAutoResume: true };
        if (sourceTab.auto_resume_ssh_command && (sourceTab.auto_resume_pinned || !ctx.sshCommand)) {
          ctx.sshCommand = sourceTab.auto_resume_ssh_command;
          ctx.remoteCwd = sourceTab.auto_resume_remote_cwd ?? ctx.remoteCwd;
        }
        terminalsStore.setSplitContext(newTab.id, ctx);
      }

      // Restore exact name (duplicateTab may have appended " (2)" for custom names)
      if (isCustom) {
        await commands.renameTab(workspaceId, paneId, newTab.id, tabName, true);
      }

      // Move new tab into the old tab's position and delete the old one
      const currentIds = freshPane.tabs.map(t => t.id);
      const reordered = currentIds.filter(id => id !== newTab.id);
      reordered.splice(sourceIndex, 0, newTab.id);
      reordered.splice(reordered.indexOf(tabId), 1);
      await commands.reorderTabs(workspaceId, paneId, reordered);

      await commands.setActiveTab(workspaceId, paneId, newTab.id);
      await commands.deleteTab(workspaceId, paneId, tabId);

      // Final state reload
      const data = await commands.getWindowData();
      const updatedWs = data.workspaces.find(w => w.id === workspaceId);
      if (updatedWs) {
        const idx = workspaces.findIndex(w => w.id === workspaceId);
        if (idx >= 0) workspaces[idx] = updatedWs;
      }

      // Reload mints a new tab id for the same resumed session — carry any agent bridge
      // across so the partner isn't orphaned (pointing at the now-deleted old tab). Done
      // after the state reload so the new tab resolves for persistence. NB: reload uses
      // commands.deleteTab directly (above), bypassing the store deleteTab + its bridge
      // teardown, so the transfer must be done explicitly here.
      import('$lib/stores/agentBridge.svelte').then(m => m.agentBridgeStore.remapTab(tabId, newTab.id)).catch(() => {});
      import('$lib/stores/agentMesh.svelte').then(m => m.agentMeshStore.remapTab(tabId, newTab.id)).catch(() => {});
    },

    async duplicateWindow() {
      // Gather context for ALL terminals in current window
      const tabContexts: commands.TabContext[] = [];

      for (const ws of workspaces) {
        for (const pane of ws.panes) {
          for (const tab of pane.tabs) {
            const ctx = await this._gatherTabContext(tab.id);

            // Also detect remote cwd
            let remoteCwd: string | null = null;
            if (ctx.sshCommand && ctx.instance) {
              const oscState = terminalsStore.getOsc(tab.id);
              const osc7Cwd = oscState?.cwd ?? null;
              const promptCwd = oscState?.promptCwd ?? null;
              const isOsc7Stale = osc7Cwd === ctx.cwd;
              const osc7RemoteCwd = (osc7Cwd && !isOsc7Stale) ? osc7Cwd : null;
              remoteCwd = osc7RemoteCwd ?? promptCwd ?? extractRemoteCwd(ctx.instance.terminal);
            }

            tabContexts.push({
              tab_id: tab.id,
              scrollback: ctx.scrollback,
              cwd: ctx.cwd,
              ssh_command: ctx.sshCommand,
              remote_cwd: remoteCwd,
            });
          }
        }
      }

      await commands.duplicateWindow(tabContexts);
    },

    toggleNotes(tabId: string) {
      const updated = new Set(notesVisible);
      const isOpen = !updated.has(tabId);
      if (isOpen) {
        updated.add(tabId);
      } else {
        updated.delete(tabId);
      }
      notesVisible = updated;

      // Persist notes_open to backend
      for (const ws of workspaces) {
        for (const pane of ws.panes) {
          const tab = pane.tabs.find(t => t.id === tabId);
          if (tab) {
            tab.notes_open = isOpen;
            commands.setTabNotesOpen(ws.id, pane.id, tabId, isOpen);
            return;
          }
        }
      }
    },

    isNotesVisible(tabId: string) {
      return notesVisible.has(tabId);
    },

    /** Effective composer open state: explicit per-tab value, else the default-open preference. */
    isComposerOpen(tabId: string): boolean {
      for (const ws of workspaces) {
        for (const pane of ws.panes) {
          const tab = pane.tabs.find(t => t.id === tabId);
          if (tab) return tab.composer_open ?? preferencesStore.composerDefaultOpen;
        }
      }
      return false;
    },

    toggleComposer(tabId: string) {
      for (const ws of workspaces) {
        for (const pane of ws.panes) {
          const tab = pane.tabs.find(t => t.id === tabId);
          if (tab) {
            const isOpen = !(tab.composer_open ?? preferencesStore.composerDefaultOpen);
            tab.composer_open = isOpen;
            commands.setTabComposerOpen(ws.id, pane.id, tabId, isOpen);
            return;
          }
        }
      }
    },

    setComposerDraft(tabId: string, draft: string | null) {
      for (const ws of workspaces) {
        for (const pane of ws.panes) {
          const tab = pane.tabs.find(t => t.id === tabId);
          if (tab) {
            tab.composer_draft = draft;
            commands.setTabComposerDraft(ws.id, pane.id, tabId, draft);
            return;
          }
        }
      }
    },


    async setTabNotes(workspaceId: string, paneId: string, tabId: string, notes: string | null) {
      await commands.setTabNotes(workspaceId, paneId, tabId, notes);
      const { tab } = findTab(workspaceId, paneId, tabId);
      if (tab) tab.notes = notes;
    },

    async addWorkspaceNote(workspaceId: string, content: string, mode: string | null): Promise<WorkspaceNote | null> {
      try {
        const note = await commands.addWorkspaceNote(workspaceId, content, mode);
        const ws = workspaces.find(w => w.id === workspaceId);
        if (ws) ws.workspace_notes.push(note);
        return note;
      } catch (e) {
        logError(`Failed to add workspace note: ${e}`);
        return null;
      }
    },

    async updateWorkspaceNote(workspaceId: string, noteId: string, content: string, mode: string | null) {
      await commands.updateWorkspaceNote(workspaceId, noteId, content, mode);
      const ws = workspaces.find(w => w.id === workspaceId);
      const note = ws?.workspace_notes.find(n => n.id === noteId);
      if (note) {
        note.content = content;
        note.mode = mode;
        note.updated_at = new Date().toISOString();
      }
    },

    async deleteWorkspaceNote(workspaceId: string, noteId: string) {
      await commands.deleteWorkspaceNote(workspaceId, noteId);
      const ws = workspaces.find(w => w.id === workspaceId);
      if (ws) {
        const idx = ws.workspace_notes.findIndex(n => n.id === noteId);
        if (idx >= 0) ws.workspace_notes.splice(idx, 1);
      }
    },

    async setTabNotesMode(workspaceId: string, paneId: string, tabId: string, notesMode: string | null) {
      await commands.setTabNotesMode(workspaceId, paneId, tabId, notesMode);
      const { tab } = findTab(workspaceId, paneId, tabId);
      if (tab) tab.notes_mode = notesMode;
    },
  };
}

export const workspacesStore = createWorkspacesStore();

/**
 * Navigate to a specific tab by finding its workspace and pane.
 * Used by toast clicks and OS notification deep-links.
 */
export async function navigateToTab(tabId: string): Promise<void> {
  for (const ws of workspacesStore.workspaces) {
    for (const pane of ws.panes) {
      const tab = pane.tabs.find(t => t.id === tabId);
      if (tab) {
        if (ws.id !== workspacesStore.activeWorkspaceId) {
          await workspacesStore.setActiveWorkspace(ws.id);
        }
        if (pane.active_tab_id !== tabId) {
          await workspacesStore.setActiveTab(ws.id, pane.id, tabId);
        } else {
          const { navHistoryStore } = await import('$lib/stores/navHistory.svelte');
          navHistoryStore.push({ workspaceId: ws.id, paneId: pane.id, tabId });
        }
        return;
      }
    }
  }
}
