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
 *  idempotent and a compacted agent duplicates its own list. */
export function normalizeTitle(title: string): string {
  return title
    .trim()
    .replace(/[.,;:]+$/, '')
    .toLowerCase()
    .split(/\s+/)
    .filter(Boolean)
    .join(' ');
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

/** Find the existing row an incoming item refers to: same normalized title, same tab.
 *
 *  Scoped to the tab rather than the workspace on purpose — two agents in one project
 *  legitimately both have a "write the tests" task, and collapsing those would hide one
 *  agent's work behind another's. */
export function findDuplicate(list: Task[], title: string, tabId: string | null | undefined): Task | undefined {
  const key = normalizeTitle(title);
  const tab = tabId ?? null;
  return list.find((t) => (t.tab_id ?? null) === tab && (t.normalized_title || normalizeTitle(t.title)) === key);
}
