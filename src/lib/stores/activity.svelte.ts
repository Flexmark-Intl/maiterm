import type { TabStateName } from '$lib/tauri/types';

export interface ShellState {
  state: 'prompt' | 'completed';
  exitCode?: number;
}

type CommandStartListener = (tabId: string) => void;
type CommandCompleteListener = (tabId: string, exitCode: number) => void;

function createActivityStore() {
  let active = $state<Set<string>>(new Set());
  let shellStates = $state<Map<string, ShellState>>(new Map());
  let tabStates = $state<Map<string, TabStateName>>(new Map());

  const commandStartListeners = new Set<CommandStartListener>();
  const commandCompleteListeners = new Set<CommandCompleteListener>();
  const commandExitListeners = new Set<CommandCompleteListener>();

  return {
    hasActivity(tabId: string): boolean {
      return active.has(tabId);
    },

    /** Check if any tab in the given list has activity. */
    hasAnyActivity(tabIds: string[]): boolean {
      for (const id of tabIds) {
        if (active.has(id)) return true;
      }
      return false;
    },

    markActive(tabId: string) {
      if (active.has(tabId)) return;
      active = new Set(active);
      active.add(tabId);
    },

    clearActive(tabId: string) {
      if (!active.has(tabId)) return;
      active = new Set(active);
      active.delete(tabId);
    },

    getShellState(tabId: string): ShellState | undefined {
      return shellStates.get(tabId);
    },

    setShellState(tabId: string, state: 'prompt' | 'completed' | null, exitCode?: number) {
      if (state === null) {
        // B/C: command started running
        if (shellStates.has(tabId)) {
          shellStates = new Map(shellStates);
          shellStates.delete(tabId);
        }
        for (const fn of commandStartListeners) fn(tabId);
        return;
      }
      if (state === 'prompt') {
        // Only set prompt if not already showing completed (completed has priority)
        const current = shellStates.get(tabId);
        if (current?.state === 'completed') return;
      }
      shellStates = new Map(shellStates);
      shellStates.set(tabId, { state, exitCode });
      if (state === 'completed') {
        for (const fn of commandCompleteListeners) fn(tabId, exitCode ?? 0);
      }
    },

    clearShellState(tabId: string) {
      if (!shellStates.has(tabId)) return;
      shellStates = new Map(shellStates);
      shellStates.delete(tabId);
    },

    /** Subscribe to command start events (B/C sequence). Returns unsubscribe function. */
    onCommandStart(fn: CommandStartListener): () => void {
      commandStartListeners.add(fn);
      return () => { commandStartListeners.delete(fn); };
    },

    /** Subscribe to command complete events (D sequence). Returns unsubscribe function. */
    onCommandComplete(fn: CommandCompleteListener): () => void {
      commandCompleteListeners.add(fn);
      return () => { commandCompleteListeners.delete(fn); };
    },

    /** EVERY OSC 133 D, unfiltered — no 2s completion floor, no mount-window gate. The
     *  filters above exist for notifications ("this command was worth telling you about");
     *  the stack store needs the opposite: a dev server that dies 900ms after being typed
     *  is exactly the exit it must see. Restored-scrollback replays reach this too, and
     *  consumers must ignore exits for anything they did not start. */
    noteCommandExit(tabId: string, exitCode: number) {
      for (const fn of commandExitListeners) fn(tabId, exitCode);
    },
    onCommandExit(fn: CommandCompleteListener): () => void {
      commandExitListeners.add(fn);
      return () => { commandExitListeners.delete(fn); };
    },

    // Tab state (alert / question) — set by trigger actions, cleared on tab focus

    getTabState(tabId: string): TabStateName | undefined {
      return tabStates.get(tabId);
    },

    setTabState(tabId: string, state: TabStateName) {
      const current = tabStates.get(tabId);
      // Alert overwrites question; same state is a no-op
      if (current === state) return;
      if (current === 'alert' && state === 'question') return;
      tabStates = new Map(tabStates);
      tabStates.set(tabId, state);
    },

    clearTabState(tabId: string) {
      if (!tabStates.has(tabId)) return;
      tabStates = new Map(tabStates);
      tabStates.delete(tabId);
    },

    /** Returns the highest-priority tab state across the given tabs, or null. */
    getWorkspaceTabState(tabIds: string[]): TabStateName | null {
      let hasQuestion = false;
      for (const id of tabIds) {
        const s = tabStates.get(id);
        if (s === 'alert') return 'alert';
        if (s === 'question') hasQuestion = true;
      }
      return hasQuestion ? 'question' : null;
    },

    /** Diagnostic snapshot for getDiagnostics. */
    getInternalSizes() {
      return {
        active_tabs: active.size,
        shell_states: shellStates.size,
        tab_states: tabStates.size,
        command_start_listeners: commandStartListeners.size,
        command_complete_listeners: commandCompleteListeners.size,
      };
    },
  };
}

export const activityStore = createActivityStore();
