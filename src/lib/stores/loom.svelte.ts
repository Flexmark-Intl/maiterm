/** Workstream Loom state (docs/loom.md). The Loom is the Overlord deck's home view, so this
 *  also holds which deck view is showing: Cmd+Shift+J and "show in loom" links set it from
 *  outside the deck. Per window, not persisted: what is open and selected is a glance, not a
 *  document. */

/** The Overlord deck's views. `loom` is home. */
export type DeckView = 'loom' | 'deck' | 'board' | 'ledger';
/** The Loom's modes: Focus (talk to agents), Weave (the picture), Decisions (the queue). */
export type LoomMode = 'focus' | 'weave' | 'decisions';

function createLoomStore() {
  let deckView = $state<DeckView>('loom');
  let mode = $state<LoomMode>('focus');
  /** One workspace, or null for every workspace in the window. */
  let workspaceFilter = $state<string | null>(null);
  let selectedTaskId = $state<string | null>(null);
  /** An agent picked in the weave's left column: everything unrelated fades. */
  let focusAgentId = $state<string | null>(null);
  /** The chat open in Focus. */
  let focusChatTabId = $state<string | null>(null);

  return {
    get deckView() { return deckView; },
    get mode() { return mode; },
    get workspaceFilter() { return workspaceFilter; },
    get selectedTaskId() { return selectedTaskId; },
    get focusAgentId() { return focusAgentId; },
    get focusChatTabId() { return focusChatTabId; },

    setDeckView(v: DeckView) { deckView = v; },
    setMode(m: LoomMode) { mode = m; },
    setWorkspaceFilter(id: string | null) { workspaceFilter = id; },
    /** Show a mode, optionally selecting a task (a chat's task rule, "show in weave"). */
    show(m: LoomMode, taskId?: string) {
      deckView = 'loom';
      mode = m;
      if (taskId !== undefined) selectedTaskId = taskId;
    },
    select(taskId: string | null) { selectedTaskId = taskId; },
    toggleFocusAgent(tabId: string) { focusAgentId = focusAgentId === tabId ? null : tabId; },
    openChat(tabId: string) { focusChatTabId = tabId; },
  };
}

export const loomStore = createLoomStore();
