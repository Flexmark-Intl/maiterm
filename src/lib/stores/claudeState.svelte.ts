import { countedListen as listen } from '$lib/utils/listenCounter';
import { info as logInfo } from '@tauri-apps/plugin-log';
import { setVariable, handleEnableAutoResume } from './triggers.svelte';
import { terminalsStore } from './terminals.svelte';
import { workspacesStore } from './workspaces.svelte';
import { activityStore } from './activity.svelte';
import { dispatch } from './notificationDispatch';
import { CLAUDE_RESUME_COMMAND } from '$lib/triggers/defaults';

/**
 * Claude Code session state per tab, driven by hook events.
 *
 * State machine:
 *   SessionStart → active (thinking)
 *   UserPromptSubmit → active (thinking)
 *   PreToolUse → active (tool_name set)
 *   PostToolUse → active (tool_name cleared)
 *   Stop → idle (waiting for user input)
 *   Notification(idle_prompt) → idle
 *   Notification(permission_prompt) → permission
 *   SessionEnd → (removed)
 */

export type ClaudeState = 'active' | 'idle' | 'permission';

export interface ClaudeTabSession {
  sessionId: string;
  state: ClaudeState;
  /** Current tool being executed (set by PreToolUse, cleared by PostToolUse/Stop) */
  toolName?: string;
  /** Human-readable summary of what the tool is doing */
  toolDetail?: string;
  /** Only meaningful while idle: false once Claude finishes (unread), true after
   *  the user has viewed the tab. Reset on each fresh transition into idle. */
  read?: boolean;
}

/** Workspace-level rollup of Claude state across a set of tabs. `idle` is split
 *  into unread/read so the sidebar can show a filled vs hollow "done" dot. */
export type WorkspaceClaudeState = 'permission' | 'active' | 'idle-unread' | 'idle-read';

/** Build a human-readable action string from tool name + input. */
function buildActionString(toolName: string, toolInput: Record<string, unknown> | null): string {
  const detail = summarizeToolDetail(toolName, toolInput);
  const displayName = toolName.startsWith('mcp__') ? toolName.split('__').slice(2).join('__') : toolName;
  return detail ? `${displayName}: ${detail}` : displayName;
}

/** Extract a short detail from tool_input for display. */
function summarizeToolDetail(toolName: string, toolInput: Record<string, unknown> | null): string | undefined {
  if (!toolInput) return undefined;
  switch (toolName) {
    case 'Bash': {
      const cmd = toolInput.command as string | undefined;
      if (!cmd) return undefined;
      return cmd.length > 50 ? cmd.slice(0, 47) + '...' : cmd;
    }
    case 'Edit':
    case 'Write':
    case 'Read': {
      const fp = toolInput.file_path as string | undefined;
      if (!fp) return undefined;
      return fp.split('/').pop() || fp;
    }
    case 'Glob':
    case 'Grep':
      return toolInput.pattern as string | undefined;
    case 'Agent':
      return toolInput.description as string | undefined;
    case 'WebFetch':
    case 'WebSearch':
      return (toolInput.query ?? toolInput.url) as string | undefined;
    default:
      return undefined;
  }
}

/** How long (ms) to keep stale tool state before auto-clearing.
 *  If no PreToolUse/PostToolUse arrives within this window, we assume
 *  Claude was interrupted and clear the tool indicator. */
const TOOL_STALE_TIMEOUT = 15_000;

function createClaudeStateStore() {
  // tabId → session info
  let sessions = $state<Map<string, ClaudeTabSession>>(new Map());
  const unlisteners: (() => void)[] = [];
  // tabId → timeout handle for stale tool detection
  const staleTimers = new Map<string, ReturnType<typeof setTimeout>>();

  function setState(tabId: string, sessionId: string, state: ClaudeState, toolName?: string, toolDetail?: string) {
    const current = sessions.get(tabId);
    if (current?.sessionId === sessionId && current?.state === state && current?.toolName === toolName) return;
    // Entering idle fresh = unread; staying idle preserves whatever read flag we had.
    const read = state === 'idle' ? (current?.state === 'idle' ? current.read : false) : undefined;
    sessions = new Map(sessions);
    sessions.set(tabId, { sessionId, state, toolName, toolDetail, read });

    // Propagate permission state to activityStore tab state so workspace sidebar shows alert.
    // Clear alert when leaving permission state (but only if we set it).
    if (state === 'permission') {
      activityStore.setTabState(tabId, 'alert');
    } else if (current?.state === 'permission') {
      activityStore.clearTabState(tabId);
    }

    // Manage stale tool timer
    clearStaleTimer(tabId);
    if (toolName) {
      staleTimers.set(tabId, setTimeout(() => {
        const s = sessions.get(tabId);
        if (s?.toolName === toolName) {
          setState(tabId, sessionId, state);
          setVariable(tabId, 'claudeAction', '');
        }
      }, TOOL_STALE_TIMEOUT));
    }
  }

  function clearStaleTimer(tabId: string) {
    const timer = staleTimers.get(tabId);
    if (timer) {
      clearTimeout(timer);
      staleTimers.delete(tabId);
    }
  }

  function removeSession(tabId: string) {
    if (!sessions.has(tabId)) return;
    clearStaleTimer(tabId);
    const was = sessions.get(tabId);
    sessions = new Map(sessions);
    sessions.delete(tabId);
    // Clean up tab state if session ended while in permission state
    if (was?.state === 'permission') {
      activityStore.clearTabState(tabId);
    }
  }

  return {
    /** Diagnostic snapshot for getDiagnostics. */
    getInternalSizes() {
      return {
        sessions: sessions.size,
        stale_timers: staleTimers.size,
        unlisteners: unlisteners.length,
      };
    },

    /** Get Claude state for a tab, if a Claude session is active there. */
    getState(tabId: string): ClaudeTabSession | undefined {
      return sessions.get(tabId);
    },

    /** Check if any tab in the list has a Claude session needing attention. */
    hasAttention(tabIds: string[]): boolean {
      for (const id of tabIds) {
        const s = sessions.get(id);
        if (s?.state === 'permission') return true;
      }
      return false;
    },

    /** Clear Claude session for a tab (e.g. when shell prompt is detected). */
    clearSession(tabId: string) {
      removeSession(tabId);
      setVariable(tabId, 'claudeAction', '');
    },

    /** Check if any tab in the list has an active Claude session. */
    hasActive(tabIds: string[]): boolean {
      for (const id of tabIds) {
        if (sessions.has(id)) return true;
      }
      return false;
    },

    /** Highest-priority Claude state across the given tabs, or null.
     *  Priority: permission > active > idle. Lets the workspace sidebar
     *  mirror the per-tab indicators (blue = working, green = done). Idle is
     *  split into unread/read: an unread "done" outranks a read one, so the
     *  workspace dot stays filled until every finished agent has been seen. */
    getWorkspaceClaudeState(tabIds: string[]): WorkspaceClaudeState | null {
      let hasActiveTab = false;
      let hasIdleUnread = false;
      let hasIdleRead = false;
      for (const id of tabIds) {
        const s = sessions.get(id);
        if (!s) continue;
        if (s.state === 'permission') return 'permission';
        if (s.state === 'active') hasActiveTab = true;
        else if (s.state === 'idle') {
          if (s.read) hasIdleRead = true;
          else hasIdleUnread = true;
        }
      }
      if (hasActiveTab) return 'active';
      if (hasIdleUnread) return 'idle-unread';
      if (hasIdleRead) return 'idle-read';
      return null;
    },

    /** Global rollup across ALL Claude sessions in every workspace. Returns the
     *  highest-priority state, the ordered list of every tab in that state (so the
     *  footer dot can cycle through them on repeated clicks), and how many agents
     *  are in that state. `tabId` is the first/representative tab. Priority:
     *  permission > active > idle-unread > idle-read. Returns null when no Claude
     *  sessions exist anywhere. Powers the always-visible "global agent" dot in
     *  the sidebar footer. */
    getGlobalClaudeState(): { state: WorkspaceClaudeState; tabId: string; tabIds: string[]; count: number } | null {
      const permTabs: string[] = [], activeTabs: string[] = [];
      const unreadTabs: string[] = [], readTabs: string[] = [];
      for (const [tabId, s] of sessions) {
        if (s.state === 'permission') permTabs.push(tabId);
        else if (s.state === 'active') activeTabs.push(tabId);
        else if (s.state === 'idle') (s.read ? readTabs : unreadTabs).push(tabId);
      }
      if (permTabs.length) return { state: 'permission', tabId: permTabs[0], tabIds: permTabs, count: permTabs.length };
      if (activeTabs.length) return { state: 'active', tabId: activeTabs[0], tabIds: activeTabs, count: activeTabs.length };
      if (unreadTabs.length) return { state: 'idle-unread', tabId: unreadTabs[0], tabIds: unreadTabs, count: unreadTabs.length };
      if (readTabs.length) return { state: 'idle-read', tabId: readTabs[0], tabIds: readTabs, count: readTabs.length };
      return null;
    },

    /** Mark a finished (idle) Claude result as read — called when the user
     *  views the tab. No-op unless the tab is currently idle and still unread. */
    markRead(tabId: string) {
      const s = sessions.get(tabId);
      if (!s || s.state !== 'idle' || s.read) return;
      sessions = new Map(sessions);
      sessions.set(tabId, { ...s, read: true });
    },

    async init() {
      const u1 = await listen<{ session_id: string; tab_id: string; source?: string }>('claude-hook-session-start', (e) => {
        const { session_id, tab_id, source } = e.payload;
        if (!tab_id) return;
        setState(tab_id, session_id, 'active');
        if (source === 'compact') {
          dispatch('Claude Code', 'Compaction complete', 'info', { tabId: tab_id });
        }
        logInfo(`Claude state: session ${session_id.slice(0, 8)} started (${source ?? 'unknown'}) → tab ${tab_id.slice(0, 8)} = active`);
      });
      unlisteners.push(u1);

      const u2 = await listen<{ session_id: string; tab_id: string | null }>('claude-hook-session-end', (e) => {
        const { tab_id } = e.payload;
        if (!tab_id) return;
        removeSession(tab_id);
        setVariable(tab_id, 'claudeAction', '');
        logInfo(`Claude state: session ended → tab ${tab_id.slice(0, 8)} removed`);
      });
      unlisteners.push(u2);

      const u3 = await listen<{ session_id: string; tab_id: string | null }>('claude-hook-stop', (e) => {
        const { session_id, tab_id } = e.payload;
        if (!tab_id) return;
        setState(tab_id, session_id, 'idle');
        setVariable(tab_id, 'claudeAction', '');
      });
      unlisteners.push(u3);

      const u4 = await listen<{ session_id: string; tab_id: string | null }>('claude-hook-user-prompt', (e) => {
        const { session_id, tab_id } = e.payload;
        if (!tab_id) return;
        // Clear tool state — new prompt means previous operation ended (possibly interrupted)
        setState(tab_id, session_id, 'active');
        setVariable(tab_id, 'claudeAction', '');
      });
      unlisteners.push(u4);

      const u5 = await listen<{ session_id: string; tab_id: string | null; notification_type: string }>('claude-hook-notification', (e) => {
        const { session_id, tab_id, notification_type } = e.payload;
        if (!tab_id) return;
        if (notification_type === 'permission_prompt') {
          setState(tab_id, session_id, 'permission');
          dispatch('Claude Code', 'Needs permission approval', 'info', { tabId: tab_id });
        } else if (notification_type === 'idle_prompt') {
          setState(tab_id, session_id, 'idle');
          // Notification disabled — the Stop hook already notifies when Claude finishes,
          // and this fires at awkward moments (e.g. between tool calls). Re-enable if we
          // find a case where idle_prompt provides value beyond what Stop covers.
          // dispatch('Claude Code', 'Waiting for input', 'info', { tabId: tab_id });
        }
      });
      unlisteners.push(u5);

      // initSession sets claudeSessionId trigger variable and enables auto-resume directly
      const u6 = await listen<{ tab_id: string; session_id: string }>('claude-init-session', (e) => {
        const { tab_id, session_id } = e.payload;
        if (!tab_id || !session_id) return;
        // Always set the variable so pinned commands can reference %claudeSessionId
        setVariable(tab_id, 'claudeSessionId', session_id);

        // Skip auto-resume setup if the tab has a pinned auto-resume — don't overwrite user's config
        const instance = terminalsStore.get(tab_id);
        if (instance) {
          const ws = workspacesStore.workspaces.find(w => w.id === instance.workspaceId);
          const pane = ws?.panes.find(p => p.id === instance.paneId);
          const tab = pane?.tabs.find(t => t.id === tab_id);
          if (tab?.auto_resume_pinned) {
            logInfo(`Claude init: tab ${tab_id.slice(0, 8)} has pinned auto-resume, skipping`);
            return;
          }
        }

        handleEnableAutoResume(tab_id, CLAUDE_RESUME_COMMAND);
        logInfo(`Claude init: set claudeSessionId for tab ${tab_id.slice(0, 8)} = ${session_id.slice(0, 8)}`);
      });
      unlisteners.push(u6);

      // PreToolUse: track which tool Claude is about to use + set %claudeAction variable
      const u7 = await listen<{ session_id: string; tab_id: string | null; tool_name: string; tool_input: Record<string, unknown> | null }>('claude-hook-pre-tool-use', (e) => {
        const { session_id, tab_id, tool_name, tool_input } = e.payload;
        if (!tab_id) return;
        const action = buildActionString(tool_name, tool_input);
        const detail = summarizeToolDetail(tool_name, tool_input);
        setState(tab_id, session_id, 'active', tool_name, detail);
        setVariable(tab_id, 'claudeAction', action);
      });
      unlisteners.push(u7);

      // PostToolUse: tool finished, clear tool info (still active/thinking)
      const u8 = await listen<{ session_id: string; tab_id: string | null; tool_name: string }>('claude-hook-post-tool-use', (e) => {
        const { session_id, tab_id } = e.payload;
        if (!tab_id) return;
        setState(tab_id, session_id, 'active');
        setVariable(tab_id, 'claudeAction', '');
      });
      unlisteners.push(u8);

      // PreCompact: context compaction starting
      const u9 = await listen<{ session_id: string; tab_id: string | null; trigger: string }>('claude-hook-pre-compact', (e) => {
        const { tab_id, trigger } = e.payload;
        if (!tab_id) return;
        dispatch('Claude Code', `Compacting conversation (${trigger})...`, 'info', { tabId: tab_id });
      });
      unlisteners.push(u9);
    },

    destroy() {
      for (const u of unlisteners) u();
      unlisteners.length = 0;
      for (const timer of staleTimers.values()) clearTimeout(timer);
      staleTimers.clear();
    },
  };
}

export const claudeStateStore = createClaudeStateStore();
