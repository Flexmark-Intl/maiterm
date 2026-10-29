/** The window's side of a Hive's prompt routing (docs/hive.md §4.7, §7.4), pure: who holds each
 *  open worker prompt (Rust's routes, mirrored by `hivePrompts`), what the human is told when Rust
 *  says one needs them, and the blocker answer Rust's decision log takes. Rust decides all of it;
 *  this only reads and forwards. */

import { hiveBlockerAnswered } from '$lib/tauri/commands';
import type { HiveBlockerAnswerArg, HivePromptNotice, HivePromptRoute, TaskBlocker } from '$lib/tauri/types';
import type { BlockerAnswer } from '$lib/tasks/model';
import type { ToastSource } from '$lib/stores/toasts.svelte';
import { HIVE_HUMAN_KIND } from './model';

export type RoutesByWorkspace = ReadonlyMap<string, readonly HivePromptRoute[]>;

/** A blocker's route key (Rust's `prompts::blocker_key`). */
export const blockerKey = (taskId: string, askedAt: string) => `b_${taskId}_${askedAt}`;

/** `hive-prompts-changed` applied: that workspace's routes, replaced whole. */
export function applyRoutes(byWs: RoutesByWorkspace, workspaceId: string, routes: readonly HivePromptRoute[]): Map<string, readonly HivePromptRoute[]> {
  const next = new Map(byWs);
  if (routes.length) next.set(workspaceId, routes);
  else next.delete(workspaceId);
  return next;
}

export function findRoute(byWs: RoutesByWorkspace, tabId: string, key: string): { workspaceId: string; route: HivePromptRoute } | null {
  for (const [workspaceId, routes] of byWs) {
    const route = routes.find((r) => r.tab_id === tabId && r.key === key);
    if (route) return { workspaceId, route };
  }
  return null;
}

/** The route of a worker's question blocker, by its task and the question's `asked_at`. */
export function findBlockerRoute(byWs: RoutesByWorkspace, taskId: string, askedAt: string): { workspaceId: string; route: HivePromptRoute } | null {
  const key = blockerKey(taskId, askedAt);
  for (const [workspaceId, routes] of byWs) {
    const route = routes.find((r) => r.key === key);
    if (route) return { workspaceId, route };
  }
  return null;
}

const WHAT: Record<string, string> = {
  permission: "Workers' permission prompts",
  question: "Workers' questions",
  blocker: "Workers' questions",
};

/** What the human is told: `hive-human`, which the gate never holds back. */
export function noticeFor(n: HivePromptNotice, timeoutSecs: number): { title: string; body: string; source: ToastSource } {
  let body: string;
  if (n.why === 'timeout') body = `Needs your answer: the queen hasn't answered in ${Math.max(1, Math.round(timeoutSecs / 60))} min`;
  else if (n.why === 'ask') body = `Needs your answer: ${WHAT[n.kind] ?? 'this checkpoint'} is set to Ask me`;
  else if (n.why === 'take_over') body = 'Needs your answer: you took it over';
  else body = `Needs you: ${n.why}`;
  return { title: n.title, body, source: { tabId: n.tab_id, kind: HIVE_HUMAN_KIND } };
}

type Dispatch = (title: string, body: string, type: 'info', source: ToastSource) => unknown;

/** `hive-prompt-notify` heard: through the normal dispatch, so a muted tab stays silent. */
export function handleNotice(n: HivePromptNotice, timeoutSecs: number, dispatch: Dispatch): void {
  const { title, body, source } = noticeFor(n, timeoutSecs);
  void dispatch(title, body, 'info', source);
}

/** The arguments for `hive_blocker_answered`: the chosen option's label, and the words. */
export function blockerAnswerArg(blocker: TaskBlocker, answer: BlockerAnswer): HiveBlockerAnswerArg {
  const label = answer.option !== undefined ? (blocker.options?.[answer.option]?.label ?? null) : null;
  return { option: label, text: answer.text?.trim() ?? '' };
}

type Logger = (msg: string) => void;

/** A blocker the human answered, into Rust's decision log when its workspace is a Hive (plan
 *  D16): fire-and-forget, logged on error. Nothing is called for an ordinary workspace. */
export function recordHumanBlockerAnswer(
  ws: { id: string; hive?: unknown } | null | undefined,
  taskId: string,
  blocker: TaskBlocker,
  answer: BlockerAnswer,
  warn: Logger,
  call: typeof hiveBlockerAnswered = hiveBlockerAnswered,
): Promise<string | null> | null {
  if (!ws?.hive) return null;
  return call(ws.id, taskId, blocker, blockerAnswerArg(blocker, answer)).catch((e) => {
    warn(`hive: the human's answer on task ${taskId} wasn't logged: ${e}`);
    return null;
  });
}
