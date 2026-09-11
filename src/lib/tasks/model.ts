/** Pure task helpers (docs/tasks.md). No runes here — the reactive surface lives in
 *  `stores/tasks.svelte.ts`, so this stays unit-testable and importable from anywhere. */

import type { Task, TaskNote, TaskStatus, TaskOrigin, Workstream } from '$lib/tauri/types';

/** A task tagged with the workspace it came from. A *view* type only — `workspace_id` is
 *  never persisted, since the workspace already owns the list it is nested in. Used where
 *  a surface spans workspaces (the Overlord board groups the whole window by project). */
export interface TaskRow extends Task {
  workspace_id: string;
}

/** The lanes work FLOWS through, in board order. `backlog` sits LEFTMOST even though new
 *  tasks start in `todo`, because parking something is a move backwards out of the active
 *  flow — which is also what makes dragging a card left to shelve it read correctly.
 *
 *  This is the sequence the panel's ‹ › steppers walk. `dropped` is deliberately NOT in it:
 *  see `TASK_STATUSES`. */
export const FLOW_STATUSES: TaskStatus[] = ['backlog', 'todo', 'active', 'blocked', 'review', 'done'];

/** Every lane, flow plus `dropped`. Board columns and validation read this one.
 *
 *  `dropped` is a lane rather than a stepper stop on purpose. It has to be REACHABLE — an
 *  agent that filed work it had misread previously had only `done` (a lie that also
 *  satisfies dependents legitimately waiting on it) or `backlog` (a lie that hides the row
 *  from every staleness check). And it has to be REVERSIBLE — a card the human can drag
 *  back out, which is what keeps "an agent may retract" from becoming "an agent may
 *  disappear work". But it is not a step in the flow: putting it in the cycle would make
 *  one click past DONE mean "this never should have existed", which is the single worst
 *  adjacency in the vocabulary. */
export const TASK_STATUSES: TaskStatus[] = [...FLOW_STATUSES, 'dropped'];

/** The parking lot (docs/tasks.md §3): next month, future ideas, low-priority.
 *
 *  Exempt from every "is this work in flight" question — staleness rules, the untracked
 *  check, the scan's classification. A parked item is *supposed* to sit untouched for
 *  months; treating it as a stalled task would turn the backlog into a source of
 *  interruptions, which is the opposite of what it is for. */
export function isParked(status: TaskStatus): boolean {
  return status === 'backlog';
}

/** Retracted (docs/tasks.md §3): filed by mistake, superseded, or decided against.
 *
 *  NOT a synonym for done, and the difference is load-bearing in two places. It does not
 *  satisfy a dependent — dropping "migrate the schema" does not migrate the schema, so
 *  anything waiting on it stays blocked and says so, rather than quietly becoming ready
 *  work. And it is not parked — a dropped row is not coming back on its own, so it ages
 *  out of the board like a finished one instead of living in the parking lot forever. */
export function isDropped(status: TaskStatus): boolean {
  return status === 'dropped';
}

/** Off the board for good, whichever way it left: finished or retracted. What the
 *  retention sweep ages out, and where `effectiveStatus` stops deriving a lane. */
export function isRetired(status: TaskStatus): boolean {
  return status === 'done' || isDropped(status);
}

/** Work that is neither retired nor deliberately shelved — what "in flight" means. */
export function isInFlight(task: Task): boolean {
  return !isRetired(task.status) && !isParked(task.status);
}

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

/** Does this task have an unmet dependency?
 *
 *  A `blocked_by` id that no longer resolves is treated as met — a deleted prerequisite must
 *  not wedge its dependents forever.
 *
 *  `parked` is the exception that proves it: ids that still exist but are off the list,
 *  because their tab was archived. Those are NOT deleted, so treating them as met silently
 *  unblocked every dependent — a task waiting on "migrate schema" jumped into To-do, and
 *  `listTasks` reported it to agents as ready work, the moment the tab holding the migration
 *  was archived. Unresolvable-and-unknown means gone; unresolvable-but-parked means waiting.
 *
 *  A `dropped` prerequisite is UNMET — it falls out of "only done is met", and that is the
 *  behaviour we want. Retracting a prerequisite is not doing it, so the dependent stays
 *  blocked and the human is shown a real question ("this waits on something nobody intends
 *  to do") rather than the work quietly becoming ready. It is also what stops `dropped`
 *  from being a back door: an agent cannot unblock its own task by dropping the one it is
 *  waiting on. `resolveBlockers` names the offending row and its lane so the reason is on
 *  screen instead of inferred from an id. */
export function hasUnmetDeps(task: Task, all: Task[], parked?: ReadonlySet<string>): boolean {
  if (!task.blocked_by?.length) return false;
  return task.blocked_by.some((id) => {
    const dep = all.find((t) => t.id === id);
    if (dep) return dep.status !== 'done';
    return parked?.has(id) ?? false;
  });
}

/** Status as the UI should render it: an unfinished task with unmet dependencies shows as
 *  blocked regardless of its stored status, so a dependency chain is visible without
 *  anyone having to restate it. The stored value is left alone — this is a view concern.
 *
 *  Retired rows short-circuit, `dropped` as well as `done`: a lane is derived for work that
 *  is still going to happen, and a retracted task with a half-finished prerequisite is not.
 *  Without that, dropping a task moved its card into BLOCKED — the one lane that means the
 *  opposite of retired — and it would sit there being counted as waiting on something. */
export function effectiveStatus(task: Task, all: Task[], parked?: ReadonlySet<string>): TaskStatus {
  if (!isRetired(task.status) && hasUnmetDeps(task, all, parked)) return 'blocked';
  return task.status;
}

/** A `blocked_by` id resolved for display: what it is, where it is, and whether it is
 *  actually holding this task up.
 *
 *  Agents were handed raw ids, which meant scanning the whole list to learn what they were
 *  waiting on — and on a `scope: 'tab'` list the prerequisite frequently wasn't in the
 *  payload at all. `parked` distinguishes the two ways an id fails to resolve: gone
 *  (deleted, no longer blocking) from off-list (parked with an archived tab, still
 *  blocking). */
export interface ResolvedBlocker {
  id: string;
  title: string | null;
  status: TaskStatus | null;
  /** Why this one is or isn't holding the dependent up. */
  state: 'met' | 'waiting' | 'parked' | 'gone';
  /** For a `parked` blocker: the archived tab holding it, so the reader can restore it
   *  (`restoreArchivedTab`) rather than only being told something invisible is in the way. */
  parked_with?: { tab_id: string; tab_name: string };
}

/** Names a parked blocker. Optional — the lane logic only needs `parked` (does it block),
 *  but nothing that has to EXPLAIN a blocked row can work from an id alone. */
export type ParkedLookup = (id: string) => { title: string; status: TaskStatus; tab_id: string; tab_name: string } | undefined;

export function resolveBlockers(
  task: Task,
  all: Task[],
  parked?: ReadonlySet<string>,
  parkedLookup?: ParkedLookup,
): ResolvedBlocker[] {
  return (task.blocked_by ?? []).map((id) => {
    const dep = all.find((t) => t.id === id);
    if (dep) {
      return {
        id,
        title: dep.title,
        status: dep.status,
        state: dep.status === 'done' ? ('met' as const) : ('waiting' as const),
      };
    }
    // Unresolvable: parked with an archived tab (still blocks) or genuinely deleted (does
    // not). `hasUnmetDeps` draws the same line — keep the two in step.
    if (!parked?.has(id)) return { id, title: null, status: null, state: 'gone' as const };
    const held = parkedLookup?.(id);
    return {
      id,
      title: held?.title ?? null,
      status: held?.status ?? null,
      state: 'parked' as const,
      ...(held ? { parked_with: { tab_id: held.tab_id, tab_name: held.tab_name } } : {}),
    };
  });
}

/** The reverse edge: tasks that are waiting on this one. Nothing answered it, and it is
 *  what an agent finishing a task needs before it goes idle — "who did I just unblock". */
export function blocking(task: Task, all: Task[]): Task[] {
  return all.filter((t) => t.id !== task.id && t.blocked_by?.includes(task.id));
}

/**
 * Did a call hand this row to someone else? Compares the owner the row had when the call
 * BEGAN against the owner it has now.
 *
 * Stated over the net effect rather than per-write on purpose. A batch may assign a row
 * twice, assign then release it, or assign it away and back again, and only the net result
 * is true of the stored board — so announcing per-write raised cards for delegations that
 * had been undone, and made the batch and single-update paths disagree about identical
 * final state.
 *
 * Three things are not a delegation: no change of owner, a release (`to` is null — the row
 * goes to the unclaimed pile, which is nobody to notify), and claiming it for yourself,
 * which would raise a card every time an agent picked up its own work.
 */
export function isDelegation(from: string | null, to: string | null, callerTabId: string): to is string {
  return !!to && to !== callerTabId && to !== from;
}

/** One update's dependency edits (`updateTasks`). `blocked_by` replaces the whole set;
 *  `block_on` and `unblock_from` add and remove single edges on top of it. */
export interface EdgeEdit {
  blocked_by?: string[];
  block_on?: string[];
  unblock_from?: string[];
}

export type EdgeResult =
  | { ok: true; edges: string[] }
  | { ok: false; reason: 'self_dependency' | 'unknown_blocker'; bad: string[] };

/**
 * Apply one update's dependency edits, refusing the two edges that would lie.
 *
 * **Validation covers every id the update ASSERTS** — `block_on`, and `blocked_by` too,
 * since a whole-array replace asserts each of its members. Checking only `block_on` left
 * the refusal bypassable through its sibling field: `blocked_by: [self]` recorded the exact
 * edge the refusal exists to stop, and that one cannot be undone from any human surface —
 * both steppers, park and "Do it" are disabled on a dependency-blocked row, and a drag
 * writes the stored status while the effective one stays `blocked`.
 *
 * **Edges merely CARRIED OVER are not re-validated**, and must not be. A row that already
 * holds a bad edge is repaired with `unblock_from`; refusing on the pre-existing edge would
 * refuse the repair too and wedge the row permanently.
 */
export function resolveEdges(
  taskId: string,
  current: readonly string[],
  edit: EdgeEdit,
  exists: (id: string) => boolean,
): EdgeResult {
  const asserted = [...new Set([...(edit.blocked_by ?? []), ...(edit.block_on ?? [])])];
  if (asserted.includes(taskId)) {
    return { ok: false, reason: 'self_dependency', bad: [taskId] };
  }
  // An id resolving to nothing counts as MET (`hasUnmetDeps`) — a deleted prerequisite must
  // not wedge its dependents forever — so accepting one would record an edge that silently
  // does nothing while reading back as a real dependency.
  const bad = asserted.filter((b) => !exists(b));
  if (bad.length) return { ok: false, reason: 'unknown_blocker', bad };

  const edges = new Set(edit.blocked_by ?? current);
  for (const b of edit.block_on ?? []) edges.add(b);
  for (const b of edit.unblock_from ?? []) edges.delete(b);
  return { ok: true, edges: [...edges] };
}

/** Newest notes kept per task. MUST match `TASK_NOTE_CAP` in state/workspace.rs, which
 *  re-trims before disk — a log is for the last few things that happened, an agent in a
 *  retry loop appends forever, and the store persists a WHOLE workspace list on every task
 *  write, so an unbounded field is paid for by every unrelated write too. */
export const TASK_NOTE_CAP = 20;

/** Append one line to a task's log, oldest first, trimmed to the cap.
 *
 *  Returns a new array rather than mutating: every writer here commits whole lists, and a
 *  mutated-in-place vector on a `$state` row is exactly the shape that persists from one
 *  surface while another still holds the pre-append copy. */
export function appendNote(task: Task, text: string, by: TaskNote['by'], now = new Date().toISOString()): TaskNote[] {
  const next = [...(task.notes ?? []), { at: now, text: text.trim(), by }];
  return next.length > TASK_NOTE_CAP ? next.slice(next.length - TASK_NOTE_CAP) : next;
}

/** Map a runtime's own vocabulary onto ours (importer + MCP callers, which speak
 *  Claude's pending/in_progress/completed). Anything unrecognized lands in backlog. */
export function statusFromAgent(status: string | undefined, blocked?: boolean): TaskStatus {
  if (status === 'completed' || status === 'done') return 'done';
  // Retraction has more spellings than any other lane because no runtime agrees on one.
  // Mapping them is the difference between a retracted row landing in `dropped` and it
  // falling through to `todo`, where it reappears as live work the agent already decided
  // against — and would then be asked about by the staleness rules.
  if (status === 'dropped' || status === 'cancelled' || status === 'canceled' || status === 'abandoned') {
    return 'dropped';
  }
  if (blocked) return 'blocked';
  if (status === 'in_progress' || status === 'active') return 'active';
  if (status === 'review') return 'review';
  if (status === 'backlog') return 'backlog';
  // Anything else — including Claude's "pending" — is not-started work, which is `todo`.
  // It must NOT land in `backlog`: that is the parking lot, exempt from staleness, so
  // filing live work there would hide it from the board and from Overlord.
  return 'todo';
}

/** Coerce whatever a caller sent into our vocabulary.
 *
 *  The MCP layer is a hand-rolled JSON-RPC server: `TaskStatus` is a compile-time union
 *  and the declared enum is never enforced at runtime, so an agent carrying its own
 *  vocabulary across (which the migration priming explicitly asks it to do) would
 *  otherwise persist "in_progress"/"completed" verbatim. A status outside the lanes is
 *  invisible on the board — lanes match by equality — and permanently unfinished to
 *  `hasUnmetDeps`, which wedges everything blocked on it. */
export function coerceStatus(status: string | undefined): TaskStatus {
  if (!status) return 'todo';
  return (TASK_STATUSES as string[]).includes(status)
    ? (status as TaskStatus)
    : statusFromAgent(status);
}

/** Same normalization as titles — one rule for every human-typed name here, so
 *  "Auth refactor", "auth refactor" and "Auth Refactor." are one workstream, not three.
 *  Mirrors `Workstream::normalize_name` in Rust. */
export function normalizeWorkstreamName(name: string): string {
  return normalizeTitle(name);
}

export function findWorkstream(list: Workstream[], name: string): Workstream | undefined {
  const key = normalizeWorkstreamName(name);
  return list.find((w) => (w.normalized_name || normalizeWorkstreamName(w.name)) === key);
}

export function makeWorkstream(name: string, now = new Date().toISOString()): Workstream {
  return {
    id: crypto.randomUUID(),
    name: name.trim(),
    normalized_name: normalizeWorkstreamName(name),
    created_at: now,
    updated_at: now,
  };
}

export interface TaskInput {
  title: string;
  detail?: string | null;
  status?: TaskStatus;
  tab_id?: string | null;
  blocked_by?: string[];
  origin?: TaskOrigin;
  workstream_id?: string | null;
}

/** Build a persistable Task. `normalized_title` is filled in locally so in-memory dedup
 *  works before the round trip; Rust recomputes it authoritatively on persist. */
export function makeTask(input: TaskInput, now = new Date().toISOString()): Task {
  return {
    id: crypto.randomUUID(),
    title: input.title,
    normalized_title: normalizeTitle(input.title),
    detail: input.detail ?? null,
    status: input.status ?? 'todo',
    tab_id: input.tab_id ?? null,
    blocked_by: input.blocked_by ?? [],
    origin: input.origin ?? 'human',
    workstream_id: input.workstream_id ?? null,
    created_at: now,
    updated_at: now,
    notes: [],
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
export function findDuplicate(
  list: Task[],
  title: string,
  tabId: string | null | undefined,
  workstreamId?: string | null,
): Task | undefined {
  const key = normalizeTitle(title);
  const tab = tabId ?? null;
  const stream = workstreamId ?? null;
  const sameTitle = (t: Task) => (t.normalized_title || normalizeTitle(t.title)) === key;
  const mine = (t: Task) => (t.tab_id ?? null) === tab && sameTitle(t);
  const inStream = (t: Task) => (t.workstream_id ?? null) === stream;

  // 1. Exact match: same tab, same job. Two jobs may each own a task called "write the
  //    tests" — those are different pieces of work and must not collapse.
  const exact = list.find((t) => mine(t) && inStream(t));
  if (exact) return exact;

  // 2. Grouping drift. An agent records its list loose, then after a compact re-sends the
  //    same items under a workstream name (the priming asks it to name its jobs) — or the
  //    reverse. Without this the documented promise that "re-sending your list is safe"
  //    fails on the very flip the priming encourages, and the whole list duplicates.
  //    Only LOOSE rows are adopted: a row already filed under a different job belongs to
  //    that job, and moving it would be a guess.
  const drifted = stream
    ? list.find((t) => mine(t) && !t.workstream_id)
    : // Incoming is loose: reuse a grouped row only when exactly one candidate exists, so
      // an ambiguous title spread across several jobs isn't arbitrarily merged into one.
      ((c) => (c.length === 1 ? c[0] : undefined))(list.filter(mine));
  if (drifted) return drifted;

  // 3. Reclaim work released to the backlog when its previous tab closed (docs/tasks.md
  //    §3). Live rows only: a task that was closed out — finished OR retracted — shouldn't
  //    be resurrected and re-owned because a new tab restated its title. Reclaiming a
  //    dropped row would be the worse of the two, since it puts back exactly the work
  //    somebody decided against.
  if (tab === null) return undefined;
  return list.find((t) => !t.tab_id && !isRetired(t.status) && inStream(t) && sameTitle(t));
}

/** Dedup for the Claude-store importer, which must ignore grouping entirely.
 *
 *  Its source has no workstreams, but a human may since have dragged an imported card into
 *  one. Matching strictly on "loose" would stop recognizing that row and re-import it every
 *  five seconds, forever, leaving the same task on the board twice. */
export function findImportedDuplicate(
  list: Task[],
  title: string,
  tabId: string,
): Task | undefined {
  const key = normalizeTitle(title);
  const sameTitle = (t: Task) => (t.normalized_title || normalizeTitle(t.title)) === key;
  return (
    list.find((t) => t.tab_id === tabId && sameTitle(t)) ??
    list.find((t) => !t.tab_id && !isRetired(t.status) && sameTitle(t))
  );
}

/** One sentence explaining why a row is held, naming the prerequisites and their lanes.
 *
 *  Lives here rather than in a component because the two human surfaces disagreed, and the
 *  board's version was wrong. It read "It moves on its own once that task is done" for
 *  every blocked card — false in precisely the case `dropped` exists for: a RETRACTED
 *  prerequisite is never going to be done, so the card never clears and the reader was
 *  being told to wait for work nobody intends to do. The panel, meanwhile, named the row
 *  but never its lane, so "waiting on Retry queue" looked identical whether that task was
 *  active or retracted.
 *
 *  `listTasks` has shipped `blocked_by[].title` and `.status` to agents all along. The
 *  human was the only one guessing. */
export function explainBlocked(
  task: Task,
  all: Task[],
  parked?: ReadonlySet<string>,
  parkedLookup?: ParkedLookup,
): string | null {
  const unmet = resolveBlockers(task, all, parked, parkedLookup).filter(
    (b) => b.state === 'waiting' || b.state === 'parked',
  );
  if (!unmet.length) return null;

  const name = (b: ResolvedBlocker) => {
    if (!b.title) return 'a task parked with an archived tab';
    const where =
      b.state === 'parked' && b.parked_with
        ? `parked with “${b.parked_with.tab_name}”`
        : b.status
          ? laneName(b.status)
          : null;
    return where ? `“${b.title}” (${where})` : `“${b.title}”`;
  };

  const head = `Waiting on ${unmet.map(name).join(' and ')}.`;
  // A retracted prerequisite is the one that cannot resolve itself. Say so instead of
  // promising a clearance that will never arrive.
  return unmet.some((b) => b.status === 'dropped')
    ? `${head} That work was retracted, so this will not clear on its own — restore it, or retract this one too.`
    : `${head} It clears on its own once that lands.`;
}

/** Lane as it reads in a sentence. `todo` is the only one whose stored value is not the
 *  word a human uses for it. */
export function laneName(status: TaskStatus): string {
  return status === 'todo' ? 'to-do' : status;
}
