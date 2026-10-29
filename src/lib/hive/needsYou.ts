/** The Hive's Needs you view (docs/hive.md §4.8), pure: everything any of the Hive's agents waits
 *  on the human for, in one queue, so nobody has to find which worker's column holds a question.
 *  A worker's blocker (on its task or a sub-task it made), its open permission or question, its
 *  watch scripts, and the Hive's agent requests (lands, undos, settings). What the coordinator
 *  holds is listed apart and not counted: the human may watch it, and take it over. */

import type { AgentRequest, HivePromptRoute, HiveWorkerCard, Task } from '$lib/tauri/types';
import type { PendingApproval } from '$lib/stores/followUps.svelte';
import { decisionsQueue } from '$lib/loom/model';
import { coordinatorHolds } from './model';

/** Who in the Hive is asking. */
export interface HiveAsker {
  tabId: string;
  /** Null for the coordinator. */
  taskId: string | null;
  badge: number | null;
  /** `#2 Session store`, or `Coordinator`. */
  name: string;
}

export type NeedsYouItem =
  | { kind: 'blocker'; key: string; at: number; asker: HiveAsker | null; task: Task; held: boolean }
  | { kind: 'prompt'; key: string; at: number; asker: HiveAsker; what: string; held: boolean }
  | { kind: 'script'; key: string; at: number; asker: HiveAsker; approval: PendingApproval; held: false }
  | { kind: 'request'; key: string; at: number; asker: HiveAsker | null; request: AgentRequest; held: false };

type AgentFields = { state: string; toolName?: string } | null | undefined;

export interface NeedsYouInput {
  coordinatorTabId: string | null;
  workers: readonly Pick<HiveWorkerCard, 'badge' | 'title' | 'task_id' | 'tab_id' | 'phase'>[];
  tasks: readonly Task[];
  /** The agent's hook state by tab. */
  agent: (tabId: string) => AgentFields;
  /** Rust's routes of this Hive's worker prompts (`hivePrompts.routesFor`). */
  routes: readonly HivePromptRoute[];
  approvals: readonly PendingApproval[];
  requests: readonly AgentRequest[];
}

/** What an agent's state says is open on its screen, as the board's card reads it (cards.ts). */
export function promptOnScreen(agent: AgentFields): string | null {
  if (agent?.state === 'permission') return 'Waiting for permission';
  if (agent?.state === 'active' && agent.toolName === 'AskUserQuestion') return 'Asking a question';
  return null;
}

/** Only a live worker's agent can hold a prompt (cards.ts `LIVE_PHASES`'s idea, kept loose: a
 *  phase a newer build writes still shows what its agent says). */
const GONE_PHASES = ['landed', 'retired', 'stopped', 'failed', 'waiting'];

export function askers(input: Pick<NeedsYouInput, 'coordinatorTabId' | 'workers'>): Map<string, HiveAsker> {
  const out = new Map<string, HiveAsker>();
  if (input.coordinatorTabId) out.set(input.coordinatorTabId, { tabId: input.coordinatorTabId, taskId: null, badge: null, name: 'Queen' });
  for (const w of input.workers) {
    if (w.tab_id) out.set(w.tab_id, { tabId: w.tab_id, taskId: w.task_id, badge: w.badge, name: `#${w.badge} ${w.title}` });
  }
  return out;
}

/** Every item, oldest first; what the coordinator holds after the rest. */
export function needsYouQueue(input: NeedsYouInput): NeedsYouItem[] {
  const who = askers(input);
  const byTask = new Map(input.workers.map((w) => [w.task_id, w]));
  const askerOfRow = (t: Task): HiveAsker | null => {
    const w = byTask.get(t.id);
    if (w?.tab_id) return who.get(w.tab_id) ?? null;
    return t.tab_id ? (who.get(t.tab_id) ?? null) : null;
  };
  const blockerRoute = (t: Task) => input.routes.find((r) => r.kind === 'blocker' && r.key === `b_${t.id}_${t.blocker!.asked_at}`);
  const items: NeedsYouItem[] = [];

  for (const task of decisionsQueue([...input.tasks])) {
    items.push({ kind: 'blocker', key: `b:${task.id}`, at: Date.parse(task.blocker!.asked_at) || 0, asker: askerOfRow(task), task, held: coordinatorHolds(blockerRoute(task)) });
  }
  const live = new Set(input.workers.filter((w) => !GONE_PHASES.includes(w.phase)).map((w) => w.tab_id));
  for (const a of who.values()) {
    if (a.taskId && !live.has(a.tabId)) continue;
    const what = promptOnScreen(input.agent(a.tabId));
    if (!what) continue;
    const route = input.routes.filter((r) => r.tab_id === a.tabId && r.kind !== 'blocker').sort((x, y) => y.opened_ms - x.opened_ms)[0];
    items.push({ kind: 'prompt', key: `p:${a.tabId}`, at: route?.opened_ms ?? 0, asker: a, what, held: coordinatorHolds(route) });
  }
  for (const approval of input.approvals) {
    const asker = who.get(approval.tabId);
    if (!asker || approval.archived) continue;
    items.push({ kind: 'script', key: `s:${approval.followUp.id}`, at: Date.parse(approval.followUp.created_at) || 0, asker, approval, held: false });
  }
  for (const request of input.requests) {
    if (request.status !== 'pending' || !who.has(request.tab_id)) continue;
    items.push({ kind: 'request', key: `r:${request.id}`, at: request.requested_at, asker: who.get(request.tab_id) ?? null, request, held: false });
  }
  return items.sort((a, b) => Number(a.held) - Number(b.held) || a.at - b.at);
}

/** The header's count: what is the human's now. */
export const needsYouCount = (items: readonly NeedsYouItem[]): number => items.filter((i) => !i.held).length;

/** The row a worker's card reads for its state: its own task, or, while that isn't blocked, the
 *  oldest blocked sub-task it made on its own tab, so a question there reads Needs you too. */
export function cardTask(tasks: readonly Task[], taskId: string, tabId: string | null | undefined): Task | null {
  const own = tasks.find((t) => t.id === taskId) ?? null;
  if (own?.status === 'blocked' && own.blocker) return own;
  if (!tabId) return own;
  const sub = decisionsQueue(tasks.filter((t) => t.id !== taskId && t.tab_id === tabId))[0];
  return sub ? { ...(own ?? sub), status: 'blocked', blocker: sub.blocker } : own;
}
