/** Argument validation & normalization for the four task MCP tools (docs/tasks.md §5).
 *
 *  The MCP layer is hand-rolled JSON-RPC with no runtime schema validator, so an unknown key
 *  sent by an agent (e.g. `notes` instead of `detail`, or `workstream` inside a task item
 *  before per-task workstreams existed) was silently ignored and the caller thought it had
 *  been saved. These pure helpers report ignored keys as `warnings` — warning rather than
 *  refusing so older callers aren't broken while live agents immediately see what was dropped.
 *
 *  Allowed key sets MUST stay in lockstep with `src-tauri/src/claude_code/protocol.rs`.
 */

/** Resolve the effective workstream name for one `createTasks` item.
 *
 *  Per-task `workstream` wins over the call-level `workstream`:
 *  - omitted (`undefined`): inherits `callWorkstream`
 *  - non-empty string: uses that workstream name
 *  - empty/whitespace string or `null`: explicitly loose (overrides `callWorkstream`) */
export function resolveCreateTaskWorkstream(
  itemWorkstream: unknown,
  callWorkstream: unknown,
): string | null {
  const raw = itemWorkstream !== undefined ? itemWorkstream : callWorkstream;
  if (typeof raw !== 'string') return null;
  const trimmed = raw.trim();
  return trimmed || null;
}

export const LIST_TASKS_KEYS = new Set(['tabId', 'scope', 'status', 'ready', 'limit']);
export const CREATE_TASKS_KEYS = new Set(['tabId', 'workstream', 'tasks']);
export const CREATE_TASK_ITEM_KEYS = new Set([
  'title',
  'detail',
  'status',
  'blocked_by',
  'assign_to_me',
  'blocker',
  'workstream',
]);
export const UPDATE_TASKS_KEYS = new Set(['tabId', 'updates']);
export const UPDATE_TASK_ITEM_KEYS = new Set([
  'id',
  'status',
  'title',
  'detail',
  'note',
  'workstream',
  'blocked_by',
  'block_on',
  'unblock_from',
  'blocker',
  'assign_to',
]);
export const DELETE_TASKS_KEYS = new Set(['tabId', 'ids', 'workstreams']);
export const BLOCKER_KEYS = new Set(['kind', 'question', 'context', 'options', 'pick', 'command', 'run_in']);
export const BLOCKER_OPTION_KEYS = new Set(['label', 'detail', 'recommended']);

/** `tabId` is auto-injected by maiTerm and callers are told to omit it — valid when present,
 *  but never suggested as a fuzzy match for a mistyped business key. */
const NO_FUZZY_TOP_LEVEL = new Set(['tabId']);

const LIST_TASKS_HINTS: Record<string, string> = {
  workstream: 'listTasks returns all workstreams grouped; it does not filter by workstream',
  workstreams: 'listTasks returns all workstreams grouped; it does not filter by workstream',
  group: 'listTasks returns all workstreams grouped; it does not filter by workstream',
  statuses: 'did you mean "status"?',
  lane: 'did you mean "status"?',
  lanes: 'did you mean "status"?',
  state: 'did you mean "status"?',
  states: 'did you mean "status"?',
  tab: 'did you mean "scope": "tab"?',
  tab_id: 'did you mean "scope": "tab"?',
  assignee: 'did you mean "scope": "tab"?',
  mine: 'did you mean "scope": "tab"?',
  max: 'did you mean "limit"?',
  count: 'did you mean "limit"?',
  top: 'did you mean "limit"?',
  page_size: 'did you mean "limit"?',
  filter: 'did you mean "status" or "ready"?',
};

const CREATE_TASKS_TOP_HINTS: Record<string, string> = {
  task: 'did you mean "tasks"?',
  items: 'did you mean "tasks"?',
  list: 'did you mean "tasks"?',
  todos: 'did you mean "tasks"?',
  group: 'did you mean "workstream"?',
  stream: 'did you mean "workstream"?',
  workstream_name: 'did you mean "workstream"?',
  project: 'did you mean "workstream"?',
  title: 'belongs inside each item of "tasks"',
  detail: 'belongs inside each item of "tasks"',
  notes: 'belongs inside each item of "tasks" as "detail"',
  note: 'belongs inside each item of "tasks" as "detail"',
  status: 'belongs inside each item of "tasks"',
  blocker: 'belongs inside each item of "tasks"',
  blocked_by: 'belongs inside each item of "tasks"',
  assign_to_me: 'belongs inside each item of "tasks"',
};

const CREATE_TASK_ITEM_HINTS: Record<string, string> = {
  notes: 'did you mean "detail"?',
  note: 'did you mean "detail"?',
  description: 'did you mean "detail"?',
  desc: 'did you mean "detail"?',
  body: 'did you mean "detail"?',
  details: 'did you mean "detail"?',
  content: 'did you mean "detail"?',
  summary: 'did you mean "detail"?',
  spec: 'did you mean "detail"?',
  acceptance_criteria: 'did you mean "detail"?',
  name: 'did you mean "title"?',
  subject: 'did you mean "title"?',
  task: 'did you mean "title"?',
  state: 'did you mean "status"?',
  lane: 'did you mean "status"?',
  dependencies: 'did you mean "blocked_by"?',
  deps: 'did you mean "blocked_by"?',
  depends_on: 'did you mean "blocked_by"?',
  block_on: 'did you mean "blocked_by"?',
  prerequisites: 'did you mean "blocked_by"?',
  assignee: 'did you mean "assign_to_me"? (boolean; use updateTasks "assign_to" to assign to another tab)',
  assign_to: 'did you mean "assign_to_me"? (boolean; use updateTasks "assign_to" to assign to another tab)',
  assigned: 'did you mean "assign_to_me"?',
  assigned_to: 'did you mean "assign_to_me"? (boolean; use updateTasks "assign_to" to assign to another tab)',
  unassigned: 'did you mean "assign_to_me": false?',
  owner: 'did you mean "assign_to_me"?',
  tab_id: 'did you mean "assign_to_me"? (boolean; use updateTasks "assign_to" to assign to another tab)',
  group: 'did you mean "workstream"?',
  stream: 'did you mean "workstream"?',
  workstream_name: 'did you mean "workstream"?',
  question: 'did you mean "blocker"?',
  decision: 'did you mean "blocker"?',
  action: 'did you mean "blocker"?',
};

const UPDATE_TASKS_TOP_HINTS: Record<string, string> = {
  update: 'did you mean "updates"?',
  tasks: 'did you mean "updates"?',
  items: 'did you mean "updates"?',
  changes: 'did you mean "updates"?',
  edits: 'did you mean "updates"?',
  id: 'belongs inside each item of "updates"',
  status: 'belongs inside each item of "updates"',
  title: 'belongs inside each item of "updates"',
  detail: 'belongs inside each item of "updates"',
  note: 'belongs inside each item of "updates"',
  notes: 'belongs inside each item of "updates" as "note" (progress log) or "detail" (body)',
  workstream: 'belongs inside each item of "updates"',
  blocked_by: 'belongs inside each item of "updates"',
  block_on: 'belongs inside each item of "updates"',
  unblock_from: 'belongs inside each item of "updates"',
  blocker: 'belongs inside each item of "updates"',
  assign_to: 'belongs inside each item of "updates"',
};

const UPDATE_TASK_ITEM_HINTS: Record<string, string> = {
  task_id: 'did you mean "id"?',
  taskid: 'did you mean "id"?',
  notes: 'did you mean "note" (append to progress log) or "detail" (replace body)?',
  comment: 'did you mean "note"?',
  log: 'did you mean "note"?',
  progress: 'did you mean "note"?',
  message: 'did you mean "note"?',
  description: 'did you mean "detail"?',
  desc: 'did you mean "detail"?',
  body: 'did you mean "detail"?',
  details: 'did you mean "detail"?',
  content: 'did you mean "detail"?',
  summary: 'did you mean "detail"?',
  spec: 'did you mean "detail"?',
  name: 'did you mean "title"?',
  subject: 'did you mean "title"?',
  state: 'did you mean "status"?',
  lane: 'did you mean "status"?',
  assignee: 'did you mean "assign_to"?',
  assigned_to: 'did you mean "assign_to"?',
  assign_to_me: 'did you mean "assign_to": "me"?',
  owner: 'did you mean "assign_to"?',
  tab_id: 'did you mean "assign_to"?',
  group: 'did you mean "workstream"?',
  stream: 'did you mean "workstream"?',
  workstream_name: 'did you mean "workstream"?',
  dependencies: 'did you mean "block_on" or "blocked_by"?',
  deps: 'did you mean "block_on" or "blocked_by"?',
  depends_on: 'did you mean "block_on" or "blocked_by"?',
  add_blocked_by: 'did you mean "block_on"?',
  remove_blocked_by: 'did you mean "unblock_from"?',
  unblock: 'did you mean "unblock_from"?',
  question: 'did you mean "blocker"?',
  decision: 'did you mean "blocker"?',
  action: 'did you mean "blocker"?',
};

const DELETE_TASKS_HINTS: Record<string, string> = {
  id: 'did you mean "ids"?',
  task_id: 'did you mean "ids"?',
  task_ids: 'did you mean "ids"?',
  taskids: 'did you mean "ids"?',
  tasks: 'did you mean "ids"?',
  workstream: 'did you mean "workstreams"?',
  workstream_names: 'did you mean "workstreams"?',
  groups: 'did you mean "workstreams"?',
  group: 'did you mean "workstreams"?',
};

const BLOCKER_HINTS: Record<string, string> = {
  type: 'did you mean "kind"?',
  prompt: 'did you mean "question"?',
  title: 'did you mean "question"?',
  headline: 'did you mean "question"?',
  message: 'did you mean "question"?',
  text: 'did you mean "question"?',
  detail: 'did you mean "context"?',
  details: 'did you mean "context"?',
  description: 'did you mean "context"?',
  desc: 'did you mean "context"?',
  body: 'did you mean "context"?',
  explanation: 'did you mean "context"?',
  reason: 'did you mean "context"?',
  notes: 'did you mean "context"?',
  note: 'did you mean "context"?',
  choices: 'did you mean "options"?',
  answers: 'did you mean "options"?',
  multi: 'did you mean "pick"?',
  multiple: 'did you mean "pick"?',
  multi_select: 'did you mean "pick"?',
  select: 'did you mean "pick"?',
  cmd: 'did you mean "command"?',
  shell: 'did you mean "command"?',
  script: 'did you mean "command"?',
  target: 'did you mean "run_in"?',
  peer: 'did you mean "run_in"?',
  tab: 'did you mean "run_in"?',
  tab_id: 'did you mean "run_in"?',
};

const BLOCKER_OPTION_HINTS: Record<string, string> = {
  title: 'did you mean "label"?',
  name: 'did you mean "label"?',
  text: 'did you mean "label"?',
  value: 'did you mean "label"?',
  option: 'did you mean "label"?',
  description: 'did you mean "detail"?',
  desc: 'did you mean "detail"?',
  details: 'did you mean "detail"?',
  note: 'did you mean "detail"?',
  notes: 'did you mean "detail"?',
  explanation: 'did you mean "detail"?',
  default: 'did you mean "recommended"?',
  preferred: 'did you mean "recommended"?',
  selected: 'did you mean "recommended"?',
};

function isPlainObject(v: unknown): v is Record<string, unknown> {
  return typeof v === 'object' && v !== null && !Array.isArray(v);
}

function levenshtein(a: string, b: string): number {
  const m = a.length;
  const n = b.length;
  let prev = Array.from({ length: n + 1 }, (_, j) => j);
  let curr = new Array<number>(n + 1);
  for (let i = 1; i <= m; i++) {
    curr[0] = i;
    for (let j = 1; j <= n; j++) {
      const cost = a[i - 1] === b[j - 1] ? 0 : 1;
      curr[j] = Math.min(prev[j] + 1, curr[j - 1] + 1, prev[j - 1] + cost);
    }
    [prev, curr] = [curr, prev];
  }
  return prev[n];
}

function suggestKey(
  key: string,
  allowed: ReadonlySet<string>,
  hints: Readonly<Record<string, string>>,
  noFuzzy?: ReadonlySet<string>,
): string | null {
  const lower = key.toLowerCase();
  const direct = hints[key] ?? hints[lower];
  if (direct) return direct;

  // Case / separator mismatch (e.g. `blockedBy` -> `blocked_by`, `runIn` -> `run_in`).
  const norm = lower.replace(/[-_\s]/g, '');
  for (const candidate of allowed) {
    if (noFuzzy?.has(candidate)) continue;
    if (candidate.toLowerCase().replace(/[-_\s]/g, '') === norm) {
      return `did you mean "${candidate}"?`;
    }
  }

  // Small typo fallback.
  const maxDist = lower.length >= 5 ? 2 : lower.length >= 3 ? 1 : 0;
  if (maxDist === 0) return null;
  let best: string | null = null;
  let bestDist = maxDist + 1;
  let tied = false;
  for (const candidate of allowed) {
    if (noFuzzy?.has(candidate)) continue;
    const d = levenshtein(lower, candidate.toLowerCase());
    if (d < bestDist) {
      best = candidate;
      bestDist = d;
      tied = false;
    } else if (d === bestDist) {
      tied = true;
    }
  }
  return best && !tied ? `did you mean "${best}"?` : null;
}

function collectUnknownKeys(
  obj: unknown,
  allowed: ReadonlySet<string>,
  hints: Readonly<Record<string, string>>,
  warnings: string[],
  prefix?: string,
  noFuzzy?: ReadonlySet<string>,
) {
  if (!isPlainObject(obj)) return;
  for (const [key, value] of Object.entries(obj)) {
    if (value === undefined || allowed.has(key)) continue;
    const hint = suggestKey(key, allowed, hints, noFuzzy);
    const body = `ignored unknown key "${key}"${hint ? ` (${hint})` : ''}`;
    warnings.push(prefix ? `${prefix}: ${body}` : body);
  }
}

function collectBlockerWarnings(blocker: unknown, prefix: string, warnings: string[]) {
  if (!isPlainObject(blocker)) return;
  collectUnknownKeys(blocker, BLOCKER_KEYS, BLOCKER_HINTS, warnings, prefix);
  if (Array.isArray(blocker.options)) {
    for (let j = 0; j < blocker.options.length; j++) {
      collectUnknownKeys(
        blocker.options[j],
        BLOCKER_OPTION_KEYS,
        BLOCKER_OPTION_HINTS,
        warnings,
        `${prefix}.options[${j}]`,
      );
    }
  }
}

export function checkListTasksArgs(args: unknown): string[] {
  const warnings: string[] = [];
  collectUnknownKeys(args, LIST_TASKS_KEYS, LIST_TASKS_HINTS, warnings, undefined, NO_FUZZY_TOP_LEVEL);
  return warnings;
}

export function checkCreateTasksArgs(
  args: unknown,
  opts?: { reportSkipped?: boolean },
): string[] {
  const warnings: string[] = [];
  if (!isPlainObject(args)) return warnings;
  collectUnknownKeys(args, CREATE_TASKS_KEYS, CREATE_TASKS_TOP_HINTS, warnings, undefined, NO_FUZZY_TOP_LEVEL);
  if (Array.isArray(args.tasks)) {
    for (let i = 0; i < args.tasks.length; i++) {
      const item = args.tasks[i];
      const prefix = `tasks[${i}]`;
      if (!isPlainObject(item)) {
        if (opts?.reportSkipped) warnings.push(`${prefix}: skipped (expected an object with "title")`);
        continue;
      }
      collectUnknownKeys(item, CREATE_TASK_ITEM_KEYS, CREATE_TASK_ITEM_HINTS, warnings, prefix);
      if (item.blocker !== undefined && item.blocker !== null) {
        collectBlockerWarnings(item.blocker, `${prefix}.blocker`, warnings);
      }
      if (opts?.reportSkipped && !(typeof item.title === 'string' && item.title.trim())) {
        warnings.push(`${prefix}: skipped (missing required "title")`);
      }
    }
  }
  return warnings;
}

export function checkUpdateTasksArgs(
  args: unknown,
  opts?: { reportSkipped?: boolean },
): string[] {
  const warnings: string[] = [];
  if (!isPlainObject(args)) return warnings;
  collectUnknownKeys(args, UPDATE_TASKS_KEYS, UPDATE_TASKS_TOP_HINTS, warnings, undefined, NO_FUZZY_TOP_LEVEL);
  if (Array.isArray(args.updates)) {
    for (let i = 0; i < args.updates.length; i++) {
      const item = args.updates[i];
      const prefix = `updates[${i}]`;
      if (!isPlainObject(item)) {
        if (opts?.reportSkipped) warnings.push(`${prefix}: skipped (expected an object with "id")`);
        continue;
      }
      collectUnknownKeys(item, UPDATE_TASK_ITEM_KEYS, UPDATE_TASK_ITEM_HINTS, warnings, prefix);
      if (item.blocker !== undefined && item.blocker !== null) {
        collectBlockerWarnings(item.blocker, `${prefix}.blocker`, warnings);
      }
      if (opts?.reportSkipped && !(typeof item.id === 'string' && item.id.trim())) {
        warnings.push(`${prefix}: skipped (missing required "id")`);
      }
    }
  }
  return warnings;
}

export function checkDeleteTasksArgs(args: unknown): string[] {
  const warnings: string[] = [];
  collectUnknownKeys(args, DELETE_TASKS_KEYS, DELETE_TASKS_HINTS, warnings, undefined, NO_FUZZY_TOP_LEVEL);
  return warnings;
}
