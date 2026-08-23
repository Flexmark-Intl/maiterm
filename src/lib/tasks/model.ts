/** Pure task helpers (docs/tasks.md). No runes here — the reactive surface lives in
 *  `stores/tasks.svelte.ts`, so this stays unit-testable and importable from anywhere. */

import type { Task, TaskStatus, TaskOrigin } from '$lib/tauri/types';

/** A task tagged with the workspace it came from. A *view* type only — `workspace_id` is
 *  never persisted, since the workspace already owns the list it is nested in. Used where
 *  a surface spans workspaces (the Overlord board groups the whole window by project). */
export interface TaskRow extends Task {
  workspace_id: string;
}

export const TASK_STATUSES: TaskStatus[] = ['backlog', 'active', 'blocked', 'review', 'done'];

/** Case/whitespace-normalized title — the dedup key within a tab.
 *
 *  MUST stay in lockstep with `Task::normalize_title` in state/workspace.rs, which
 *  recomputes it on every persist. If the two ever disagree, re-migration stops being
 *  idempotent and a compacted agent duplicates its own list once per restart.
 *
 *  The whitespace class is spelled out because the defaults do NOT agree: Rust's
 *  `char::is_whitespace` (Unicode White_Space) includes U+0085 NEL but not U+FEFF, while
 *  JS `\s` is the mirror image — it matches U+FEFF but not U+0085. Titles arrive pasted
 *  from terminals and transcripts, so a stray BOM is not hypothetical. Both sides use the
 *  union of the two sets. */
const TITLE_WS = /[\s\u0085\uFEFF]+/g;
/** Trailing separators an agent tacks on when restating an item; stripped after whitespace
 *  collapsing so "guard ." and "guard." land on the same key. */
const TITLE_TRAILING = /[.,;:\s\u0085\uFEFF]+$/;

export function normalizeTitle(title: string): string {
  return title
    .replace(TITLE_WS, ' ')
    .replace(TITLE_TRAILING, '')
    .replace(/^ /, '')
    .toLowerCase();
}

/** Statuses that mean "this is not work in flight". */
export function isFinished(status: TaskStatus): boolean {
  return status === 'done';
}

/** Does this task have an unmet dependency? A `blocked_by` id that no longer resolves is
 *  treated as met — a deleted prerequisite must not wedge its dependents forever. */
export function hasUnmetDeps(task: Task, all: Task[]): boolean {
  if (!task.blocked_by?.length) return false;
  return task.blocked_by.some((id) => {
    const dep = all.find((t) => t.id === id);
    return !!dep && dep.status !== 'done';
  });
}

/** Status as the UI should render it: an unfinished task with unmet dependencies shows as
 *  blocked regardless of its stored status, so a dependency chain is visible without
 *  anyone having to restate it. The stored value is left alone — this is a view concern. */
export function effectiveStatus(task: Task, all: Task[]): TaskStatus {
  if (task.status !== 'done' && hasUnmetDeps(task, all)) return 'blocked';
  return task.status;
}

/** Map a runtime's own vocabulary onto ours (importer + MCP callers, which speak
 *  Claude's pending/in_progress/completed). Anything unrecognized lands in backlog. */
export function statusFromAgent(status: string | undefined, blocked?: boolean): TaskStatus {
  if (status === 'completed' || status === 'done') return 'done';
  if (blocked) return 'blocked';
  if (status === 'in_progress' || status === 'active') return 'active';
  if (status === 'review') return 'review';
  return 'backlog';
}

/** Coerce whatever a caller sent into our vocabulary.
 *
 *  The MCP layer is a hand-rolled JSON-RPC server: `TaskStatus` is a compile-time union
 *  and the declared enum is never enforced at runtime, so an agent carrying its own
 *  vocabulary across (which the migration priming explicitly asks it to do) would
 *  otherwise persist "in_progress"/"completed" verbatim. A status outside the five lanes
 *  is invisible on the board — `tasksFor` matches lane by equality — and permanently
 *  unfinished to `hasUnmetDeps`, which wedges everything blocked on it. */
export function coerceStatus(status: string | undefined): TaskStatus {
  if (!status) return 'backlog';
  return (TASK_STATUSES as string[]).includes(status)
    ? (status as TaskStatus)
    : statusFromAgent(status);
}

export interface TaskInput {
  title: string;
  detail?: string | null;
  status?: TaskStatus;
  tab_id?: string | null;
  blocked_by?: string[];
  origin?: TaskOrigin;
  topic_id?: string | null;
}

/** Build a persistable Task. `normalized_title` is filled in locally so in-memory dedup
 *  works before the round trip; Rust recomputes it authoritatively on persist. */
export function makeTask(input: TaskInput, now = new Date().toISOString()): Task {
  return {
    id: crypto.randomUUID(),
    title: input.title,
    normalized_title: normalizeTitle(input.title),
    detail: input.detail ?? null,
    status: input.status ?? 'backlog',
    tab_id: input.tab_id ?? null,
    blocked_by: input.blocked_by ?? [],
    origin: input.origin ?? 'human',
    created_at: now,
    updated_at: now,
    topic_id: input.topic_id ?? null,
  };
}

/** Find the existing row an incoming item refers to: same normalized title, on the same
 *  tab or sitting unclaimed in the project backlog.
 *
 *  Tab scoping is deliberate — two agents in one project legitimately both have a "write
 *  the tests" task, and collapsing those would hide one agent's work behind another's.
 *
 *  The unassigned fallback is what makes re-migration survive a tab id change. Tab ids are
 *  not stable across the very events the priming fires on: a reload is a duplicate-then-
 *  close, and a fork mints a new id too, so the resumed agent re-sends a list whose rows
 *  are all tagged with an id that no longer exists. Closing a tab releases its tasks back
 *  to the backlog (`tasksStore.releaseTab`), and matching them here lets the new tab
 *  reclaim its own work instead of creating a second copy of every item, forever, once
 *  per reload. */
export function findDuplicate(list: Task[], title: string, tabId: string | null | undefined): Task | undefined {
  const key = normalizeTitle(title);
  const tab = tabId ?? null;
  const sameTitle = (t: Task) => (t.normalized_title || normalizeTitle(t.title)) === key;
  return (
    list.find((t) => (t.tab_id ?? null) === tab && sameTitle(t)) ??
    // Only reclaim unfinished work: a task someone already closed out shouldn't be
    // resurrected and re-owned just because a new tab restated it.
    (tab === null ? undefined : list.find((t) => !t.tab_id && t.status !== 'done' && sameTitle(t)))
  );
}
