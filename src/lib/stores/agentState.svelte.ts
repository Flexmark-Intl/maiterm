import { countedListen as listen } from '$lib/utils/listenCounter';
import { info as logInfo } from '@tauri-apps/plugin-log';
import { setVariable, handleEnableAutoResume } from './triggers.svelte';
import { terminalsStore } from './terminals.svelte';
import { workspacesStore } from './workspaces.svelte';
import { activityStore } from './activity.svelte';
import { dispatch } from './notificationDispatch';
import { getResumeCommand } from '$lib/agents/resume';
import { getDescriptor } from '$lib/agents/descriptor';
import type { AgentRuntime, AgentState, WorkspaceAgentState } from '$lib/agents/types';

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

export interface AgentTabSession {
  /** Which agent runtime owns this session. */
  runtime: AgentRuntime;
  sessionId: string;
  state: AgentState;
  /** Current tool being executed (set by PreToolUse, cleared by PostToolUse/Stop) */
  toolName?: string;
  /** Human-readable summary of what the tool is doing */
  toolDetail?: string;
  /** Only meaningful while idle: false once Claude finishes (unread), true after
   *  the user has viewed the tab. Reset on each fresh transition into idle. */
  read?: boolean;
  /** Wall-clock ms of the last state change for this tab — drives recency sorting
   *  (e.g. the Agent Bridge picker lists most-recently-active agents first). */
  updatedAt: number;
}

/** Validate a hook payload's runtime field, defaulting to Claude. */
function runtimeOf(payload: { runtime?: string }): AgentRuntime {
  const r = payload.runtime;
  return r === 'codex' || r === 'gemini' ? r : 'claude';
}

/** Build a human-readable action string from tool name + input, using the
 *  runtime's tool summarizer for the detail portion. */
function buildActionString(runtime: AgentRuntime, toolName: string, toolInput: Record<string, unknown> | null): string {
  const detail = getDescriptor(runtime).summarizeTool(toolName, toolInput);
  const displayName = toolName.startsWith('mcp__') ? toolName.split('__').slice(2).join('__') : toolName;
  return detail ? `${displayName}: ${detail}` : displayName;
}

function createAgentStateStore() {
  // tabId → session info
  let sessions = $state<Map<string, AgentTabSession>>(new Map());
  const unlisteners: (() => void)[] = [];
  // tabId → timeout handle for stale tool detection
  const staleTimers = new Map<string, ReturnType<typeof setTimeout>>();

  function setState(tabId: string, sessionId: string, state: AgentState, toolName?: string, toolDetail?: string, runtime: AgentRuntime = 'claude') {
    const current = sessions.get(tabId);
    if (current?.sessionId === sessionId && current?.state === state && current?.toolName === toolName) return;
    // Entering idle fresh = unread; staying idle preserves whatever read flag we had.
    const read = state === 'idle' ? (current?.state === 'idle' ? current.read : false) : undefined;
    sessions = new Map(sessions);
    sessions.set(tabId, { runtime, sessionId, state, toolName, toolDetail, read, updatedAt: Date.now() });

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
      }, getDescriptor(runtime).toolStaleTimeoutMs));
    }
  }

  function clearStaleTimer(tabId: string) {
    const timer = staleTimers.get(tabId);
    if (timer) {
      clearTimeout(timer);
      staleTimers.delete(tabId);
    }
  }

  function removeSession(tabId: string, expectedSessionId?: string) {
    const was = sessions.get(tabId);
    if (!was) return;
    // A synthetic/stale SessionEnd may name a session already superseded in this
    // tab (e.g. Codex exited and auto-resume started a NEW session before the
    // dormancy reaper's end for the OLD one landed). Only clear when the end
    // refers to the session we're currently showing.
    if (expectedSessionId && was.sessionId !== expectedSessionId) return;
    clearStaleTimer(tabId);
    sessions = new Map(sessions);
    sessions.delete(tabId);
    // Clean up tab state if session ended while in permission state
    if (was.state === 'permission') {
      activityStore.clearTabState(tabId);
    }
  }

  /** Bucket every in-scope Claude session by state. Idle is split into
   *  unread/read. Shared by the dominant-state rollup and the per-category
   *  footer breakdown so the priority/bucketing logic lives in one place. */
  function computeBreakdown(scope?: ReadonlySet<string>): { permission: string[]; active: string[]; idleUnread: string[]; idleRead: string[] } {
    const permission: string[] = [], active: string[] = [];
    const idleUnread: string[] = [], idleRead: string[] = [];
    for (const [tabId, s] of sessions) {
      if (scope && !scope.has(tabId)) continue;
      if (s.state === 'permission') permission.push(tabId);
      else if (s.state === 'active') active.push(tabId);
      else if (s.state === 'idle') (s.read ? idleRead : idleUnread).push(tabId);
    }
    return { permission, active, idleUnread, idleRead };
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
    getState(tabId: string): AgentTabSession | undefined {
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
    getWorkspaceClaudeState(tabIds: string[]): WorkspaceAgentState | null {
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

    /** Highest-priority rollup across Claude sessions, optionally scoped to a set
     *  of tab IDs. Returns the dominant state, the ordered list of every tab in
     *  that state (so the footer dot can cycle through them on repeated clicks),
     *  and how many agents are in that state. `tabId` is the first/representative
     *  tab. Priority: permission > active > idle-unread > idle-read. Returns null
     *  when no matching Claude sessions exist. Powers the always-visible "global
     *  agent" dot in the sidebar footer.
     *
     *  Hook events broadcast to every window (`app_handle.emit`), so each window's
     *  session map holds agents from ALL windows. Pass `scope` — the current
     *  window's tab IDs — to keep the footer dot window-scoped and independent;
     *  without it the dot would count foreign-window agents and clicks couldn't
     *  navigate to them (`navigateToTab` only searches this window's workspaces). */
    getGlobalClaudeState(scope?: ReadonlySet<string>): { state: WorkspaceAgentState; tabId: string; tabIds: string[]; count: number } | null {
      const { permission, active, idleUnread, idleRead } = computeBreakdown(scope);
      if (permission.length) return { state: 'permission', tabId: permission[0], tabIds: permission, count: permission.length };
      if (active.length) return { state: 'active', tabId: active[0], tabIds: active, count: active.length };
      if (idleUnread.length) return { state: 'idle-unread', tabId: idleUnread[0], tabIds: idleUnread, count: idleUnread.length };
      if (idleRead.length) return { state: 'idle-read', tabId: idleRead[0], tabIds: idleRead, count: idleRead.length };
      return null;
    },

    /** Per-state breakdown of Claude sessions in scope — every category at once,
     *  unlike getGlobalClaudeState which collapses to the single dominant state.
     *  Lets the sidebar footer show independent working / waiting / finished dots.
     *  Each bucket is the ordered list of tab IDs in that state. See
     *  getGlobalClaudeState for why `scope` matters (cross-window broadcast). */
    getClaudeStateBreakdown(scope?: ReadonlySet<string>): { permission: string[]; active: string[]; idleUnread: string[]; idleRead: string[] } {
      return computeBreakdown(scope);
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
      const u1 = await listen<{ session_id: string; tab_id: string; source?: string; runtime?: string }>('agent-hook-session-start', (e) => {
        const { session_id, tab_id, source } = e.payload;
        if (!tab_id) return;
        const runtime = runtimeOf(e.payload);
        setState(tab_id, session_id, 'active', undefined, undefined, runtime);
        // Tag the tab's runtime live so getTabRuntime (bridge adapter, resume) is right
        // even when the agent never calls initSession (e.g. Codex via the hook path).
        workspacesStore.setTabRuntimeLocal(tab_id, runtime);
        if (source === 'compact') {
          dispatch(getDescriptor(runtime).displayName, 'Compaction complete', 'info', { tabId: tab_id });
        }
        logInfo(`Claude state: session ${session_id.slice(0, 8)} started (${source ?? 'unknown'}) → tab ${tab_id.slice(0, 8)} = active`);
      });
      unlisteners.push(u1);

      const u2 = await listen<{ session_id: string; tab_id: string | null }>('agent-hook-session-end', (e) => {
        const { session_id, tab_id } = e.payload;
        if (!tab_id) return;
        removeSession(tab_id, session_id);
        setVariable(tab_id, 'claudeAction', '');
        logInfo(`Claude state: session ended → tab ${tab_id.slice(0, 8)} removed`);
      });
      unlisteners.push(u2);

      const u3 = await listen<{ session_id: string; tab_id: string | null; runtime?: string }>('agent-hook-stop', (e) => {
        const { session_id, tab_id } = e.payload;
        if (!tab_id) return;
        setState(tab_id, session_id, 'idle', undefined, undefined, runtimeOf(e.payload));
        setVariable(tab_id, 'claudeAction', '');
      });
      unlisteners.push(u3);

      const u4 = await listen<{ session_id: string; tab_id: string | null; runtime?: string }>('agent-hook-user-prompt', (e) => {
        const { session_id, tab_id } = e.payload;
        if (!tab_id) return;
        // Clear tool state — new prompt means previous operation ended (possibly interrupted)
        setState(tab_id, session_id, 'active', undefined, undefined, runtimeOf(e.payload));
        setVariable(tab_id, 'claudeAction', '');
      });
      unlisteners.push(u4);

      const u5 = await listen<{ session_id: string; tab_id: string | null; notification_type: string; runtime?: string }>('agent-hook-notification', (e) => {
        const { session_id, tab_id, notification_type } = e.payload;
        if (!tab_id) return;
        const runtime = runtimeOf(e.payload);
        if (notification_type === 'permission_prompt') {
          setState(tab_id, session_id, 'permission', undefined, undefined, runtime);
          dispatch(getDescriptor(runtime).displayName, 'Needs permission approval', 'info', { tabId: tab_id });
        } else if (notification_type === 'idle_prompt') {
          setState(tab_id, session_id, 'idle', undefined, undefined, runtime);
          // Notification disabled — the Stop hook already notifies when Claude finishes,
          // and this fires at awkward moments (e.g. between tool calls). Re-enable if we
          // find a case where idle_prompt provides value beyond what Stop covers.
          // dispatch(getDescriptor(runtime).displayName, 'Waiting for input', 'info', { tabId: tab_id });
        }
      });
      unlisteners.push(u5);

      // initSession sets claudeSessionId trigger variable and enables auto-resume directly
      const u6 = await listen<{ tab_id: string; session_id: string; runtime?: string }>('agent-init-session', (e) => {
        const { tab_id, session_id } = e.payload;
        if (!tab_id || !session_id) return;
        // Update the tab's runtime live (backend already persists it) so
        // getTabRuntime/getResumeCommand below resolve the correct runtime.
        if (e.payload.runtime) {
          workspacesStore.setTabRuntimeLocal(tab_id, runtimeOf(e.payload));
        }
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

        handleEnableAutoResume(tab_id, getResumeCommand(workspacesStore.getTabRuntime(tab_id)));
        logInfo(`Claude init: set claudeSessionId for tab ${tab_id.slice(0, 8)} = ${session_id.slice(0, 8)}`);
      });
      unlisteners.push(u6);

      // PreToolUse: track which tool Claude is about to use + set %claudeAction variable
      const u7 = await listen<{ session_id: string; tab_id: string | null; tool_name: string; tool_input: Record<string, unknown> | null; runtime?: string }>('agent-hook-pre-tool-use', (e) => {
        const { session_id, tab_id, tool_name, tool_input } = e.payload;
        if (!tab_id) return;
        const runtime = runtimeOf(e.payload);
        const action = buildActionString(runtime, tool_name, tool_input);
        const detail = getDescriptor(runtime).summarizeTool(tool_name, tool_input);
        setState(tab_id, session_id, 'active', tool_name, detail, runtime);
        setVariable(tab_id, 'claudeAction', action);
      });
      unlisteners.push(u7);

      // PostToolUse: tool finished, clear tool info (still active/thinking)
      const u8 = await listen<{ session_id: string; tab_id: string | null; tool_name: string; runtime?: string }>('agent-hook-post-tool-use', (e) => {
        const { session_id, tab_id } = e.payload;
        if (!tab_id) return;
        setState(tab_id, session_id, 'active', undefined, undefined, runtimeOf(e.payload));
        setVariable(tab_id, 'claudeAction', '');
      });
      unlisteners.push(u8);

      // PreCompact: context compaction starting
      const u9 = await listen<{ session_id: string; tab_id: string | null; trigger: string; runtime?: string }>('agent-hook-pre-compact', (e) => {
        const { tab_id, trigger } = e.payload;
        if (!tab_id) return;
        const runtime = runtimeOf(e.payload);
        dispatch(getDescriptor(runtime).displayName, `Compacting conversation (${trigger})...`, 'info', { tabId: tab_id });
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

export const agentStateStore = createAgentStateStore();

// Backward-compat aliases — existing consumers still import the old names; these
// resolve to the renamed runtime-neutral store/types until they're migrated.
export const claudeStateStore = agentStateStore;
export type ClaudeState = AgentState;
export type ClaudeTabSession = AgentTabSession;
export type WorkspaceClaudeState = WorkspaceAgentState;
