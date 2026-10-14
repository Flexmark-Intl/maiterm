/**
 * What is waiting on the human, by the tab whose agent asked: a task's question (a `decision`
 * or `action` blocker, docs/tasks.md §3.1), a watch script to allow (docs/follow-ups.md §5.1),
 * and an agent request (docs/agent-requests.md). The Loom's Decisions view lists them all; this
 * index puts each one on its own terminal, as a corner card (`WaitingCorner.svelte`) and a tab
 * strip badge, so a question is seen where the human already is (docs/loom.md, "On the
 * terminal").
 *
 * The cards are the same components every surface uses (BlockerCard, ScriptApprovalCard,
 * AgentRequestCard), so answering here goes through the same guards and the same verbs.
 */
import { tasksStore } from '$lib/stores/tasks.svelte';
import { followUpsStore, type PendingApproval } from '$lib/stores/followUps.svelte';
import { agentRequestsStore } from '$lib/stores/agentRequests.svelte';
import { decisionsQueue } from '$lib/loom/model';
import type { AgentRequest, Task } from '$lib/tauri/types';

export type WaitingItem =
  | { kind: 'task'; key: string; at: number; task: Task }
  | { kind: 'script'; key: string; at: number; approval: PendingApproval }
  | { kind: 'request'; key: string; at: number; request: AgentRequest };

/** How urgent a tab's corner reads, most urgent first: something only the human can do, then a
 *  choice. Scripts and requests are choices. */
export type WaitingTone = 'action' | 'decision';

export function waitingTone(items: WaitingItem[]): WaitingTone {
  return items.some((i) => i.kind === 'task' && i.task.blocker?.kind === 'action') ? 'action' : 'decision';
}

/** Every waiting item by the tab that asked, each list oldest first. Tasks with no tab (an
 *  unassigned row) and scripts on archived tabs have no terminal to show on; the Loom still
 *  lists them. */
function byTab(): Map<string, WaitingItem[]> {
  const out = new Map<string, WaitingItem[]>();
  const add = (tabId: string | null | undefined, item: WaitingItem) => {
    if (!tabId) return;
    const list = out.get(tabId);
    if (list) list.push(item);
    else out.set(tabId, [item]);
  };
  for (const ws of tasksStore.workspaceIds) {
    for (const task of decisionsQueue(tasksStore.forWorkspace(ws))) {
      add(task.tab_id, { kind: 'task', key: task.id, at: Date.parse(task.blocker!.asked_at), task });
    }
  }
  for (const approval of followUpsStore.pendingApprovals) {
    if (approval.archived) continue;
    add(approval.tabId, { kind: 'script', key: approval.followUp.id, at: Date.parse(approval.followUp.created_at), approval });
  }
  for (const request of agentRequestsStore.pending) {
    add(request.tab_id, { kind: 'request', key: request.id, at: request.requested_at, request });
  }
  for (const list of out.values()) list.sort((a, b) => a.at - b.at);
  return out;
}

/** An open corner card: which item to bring into view, and a task to show although it isn't
 *  waiting on the human from this tab (a row in this tab's task panel that another tab, or no
 *  tab, carries; or an `external` wait, which is nobody's to answer but can be dismissed). */
export interface OpenCorner {
  focus: string | null;
  extraTaskId: string | null;
  /** Bumped on every open, so a second open of a card already open still scrolls to it. */
  seq: number;
}

function createWaitingStore() {
  /** Open corner cards by tab id. A tab's card shows only while the tab is its pane's active
   *  tab, and leaving the tab closes it (WaitingCorner's onDestroy): reopened on its own, it
   *  would take the keyboard without a click. */
  let open = $state<Record<string, OpenCorner>>({});
  let seq = 0;

  return {
    byTab,
    forTab(tabId: string): WaitingItem[] {
      return byTab().get(tabId) ?? [];
    },
    opened(tabId: string): OpenCorner | null {
      return open[tabId] ?? null;
    },
    /** Open a tab's corner card. Never called by anything but the human's click: a card that
     *  opened by itself would take the keyboard from a terminal they are typing in. */
    open(tabId: string, focus: string | null = null, extraTaskId: string | null = null) {
      open = { ...open, [tabId]: { focus, extraTaskId, seq: ++seq } };
    },
    close(tabId: string) {
      if (!(tabId in open)) return;
      const { [tabId]: _, ...rest } = open;
      open = rest;
    },
  };
}

export const waitingStore = createWaitingStore();
