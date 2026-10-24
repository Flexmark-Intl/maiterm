/**
 * What is waiting on the human, by the tab whose agent asked: a task's question (a `decision`
 * or `action` blocker, docs/tasks.md §3.1), a watch script to allow (docs/follow-ups.md §5.1),
 * an agent request (docs/agent-requests.md), and the Overlord agent's proposed rule changes
 * (docs/overlord.md §10). The Loom's Decisions view lists them all; this
 * index puts each one on its own terminal, as a corner card (`WaitingCorner.svelte`) and a tab
 * strip badge, so a question is seen where the human already is (docs/loom.md, "On the
 * terminal").
 *
 * The cards are the same components every surface uses (BlockerCard, ScriptApprovalCard,
 * AgentRequestCard, RuleChangeCard), so answering here goes through the same guards and the
 * same verbs.
 */
import { tasksStore } from '$lib/stores/tasks.svelte';
import { followUpsStore, type PendingApproval } from '$lib/stores/followUps.svelte';
import { agentRequestsStore } from '$lib/stores/agentRequests.svelte';
import { overlordStore, type PendingRuleChangeBatch } from '$lib/stores/overlord.svelte';
import { decisionsQueue } from '$lib/loom/model';
import { workspacesStore, navigateToTab } from '$lib/stores/workspaces.svelte';
import { SvelteMap } from 'svelte/reactivity';
import type { AgentRequest, Task } from '$lib/tauri/types';
import { isStripHidden } from '$lib/workspace/hiddenTabs';
import { dispatchHiveOpenColumn, openTargetFor } from '$lib/hive/model';

export type WaitingItem =
  | { kind: 'task'; key: string; at: number; task: Task }
  | { kind: 'script'; key: string; at: number; approval: PendingApproval }
  | { kind: 'request'; key: string; at: number; request: AgentRequest }
  | { kind: 'rules'; key: string; at: number; batch: PendingRuleChangeBatch };

/** How urgent a tab's corner reads, most urgent first: something only the human can do, then a
 *  choice. Scripts, requests and rule changes are choices. */
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
  // The Overlord agent's proposed rule changes, on the Overlord tab that proposed them.
  const batch = overlordStore.pendingRuleChanges;
  if (batch) add(batch.tabId, { kind: 'rules', key: batch.id, at: overlordStore.pendingRuleChangesAt, batch });
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

/** What the human has typed under a question, by task id, with the `asked_at` it was typed
 *  against (BlockerCard drops it when the agent re-asks). Kept here rather than in the card:
 *  the corner card closes when its tab is left (see `open` below), and a draft the card held
 *  died with it. Every surface's BlockerCard reads the same draft. */
export interface BlockerDraft {
  askedAt: string;
  text: string;
}
const drafts = new SvelteMap<string, BlockerDraft>();

export const blockerDrafts = {
  get(taskId: string): BlockerDraft | null {
    return drafts.get(taskId) ?? null;
  },
  set(taskId: string, draft: BlockerDraft) {
    if (draft.text) drafts.set(taskId, draft);
    else drafts.delete(taskId);
  },
  /** Text typed under the question the task is asking now. */
  holds(task: Task): boolean {
    const d = drafts.get(task.id);
    return !!d && d.askedAt === task.blocker?.asked_at && !!d.text.trim();
  },
};

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
    /** A notification's click: go to the tab and open its card there, on `focus`. The tab
     *  first, because a terminal focuses itself when it becomes visible and the card has to
     *  take focus after that (as the tab strip badge does). Only where a corner can mount:
     *  a tab not in a pane (archived, closed), in a suspended workspace, or in a mesh shown
     *  as a stage has none, and an `open` nothing shows would be left to open by itself, unasked,
     *  when one next mounts. Those go to the Loom's Decisions. */
    async openOnTab(tabId: string, focus: string | null = null) {
      // A Hive agent's waiting items are shown in its column of the Hive view (docs/hive.md §5),
      // which scrolls to `focus`; it has no corner and is never made the active tab.
      for (const w of workspacesStore.workspaces) {
        const tab = w.panes.flatMap((p) => p.tabs).find((t) => t.id === tabId);
        if (!tab) continue;
        const target = openTargetFor(tab, w);
        if (target.kind !== 'hive') break;
        if (w.id !== workspacesStore.activeWorkspaceId) await workspacesStore.setActiveWorkspace(w.id);
        dispatchHiveOpenColumn({ workspaceId: target.workspaceId, tabId: target.tabId, focus });
        return;
      }
      const { agentMeshStore } = await import('$lib/stores/agentMesh.svelte');
      const ws = workspacesStore.workspaces.find((w) => w.panes.some((p) => p.tabs.some((t) => t.id === tabId && !isStripHidden(t))));
      if (!ws || ws.suspended || (ws.bridge_all && agentMeshStore.isStageView(ws.id))) {
        window.dispatchEvent(new CustomEvent('open-loom', { detail: { mode: 'decisions' } }));
        return;
      }
      await navigateToTab(tabId);
      this.open(tabId, focus);
    },
    close(tabId: string) {
      if (!(tabId in open)) return;
      const { [tabId]: _, ...rest } = open;
      open = rest;
    },
  };
}

export const waitingStore = createWaitingStore();
