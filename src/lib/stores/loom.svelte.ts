/** Workstream Loom drawer state (docs/loom.md). Per window, not persisted: which view is open
 *  and what is selected is a glance, not a document. */

export type LoomView = 'loom' | 'decisions' | 'focus';
/** 'workspace': the active workspace. 'window': every workspace in this window, which is what
 *  the Overlord workspace shows by default. */
export type LoomScope = 'workspace' | 'window';

function createLoomStore() {
  let open = $state(false);
  let view = $state<LoomView>('loom');
  let scope = $state<LoomScope>('workspace');
  let selectedTaskId = $state<string | null>(null);
  /** An agent picked in the loom's left column: everything unrelated fades. */
  let focusAgentId = $state<string | null>(null);
  /** The chat open in the Focus view. */
  let focusChatTabId = $state<string | null>(null);

  return {
    get open() { return open; },
    get view() { return view; },
    get scope() { return scope; },
    get selectedTaskId() { return selectedTaskId; },
    get focusAgentId() { return focusAgentId; },
    get focusChatTabId() { return focusChatTabId; },

    toggle() { open = !open; },
    close() { open = false; },
    /** Open on a view, optionally selecting a task (a decision card's "show in loom"). */
    show(v: LoomView, taskId?: string) {
      view = v;
      if (taskId !== undefined) selectedTaskId = taskId;
      open = true;
    },
    setView(v: LoomView) { view = v; },
    setScope(s: LoomScope) { scope = s; },
    select(taskId: string | null) { selectedTaskId = taskId; },
    toggleFocusAgent(tabId: string) { focusAgentId = focusAgentId === tabId ? null : tabId; },
    openChat(tabId: string) { focusChatTabId = tabId; },
  };
}

export const loomStore = createLoomStore();
