/** The Hive board's cards (docs/hive.md §4.3, §4.4), as pure functions over what each store
 *  already holds: the worker from Rust (`hive_workers`), its task from the tasks store, its agent
 *  from the hook state. Nothing here reads a transcript (§4.4: the board never polls one). */

import type { AgentState } from '$lib/agents/types';
import type { HiveWorkerCard, TaskBlocker, TaskStatus } from '$lib/tauri/types';

/** §4.3's states. */
export type CardStateName =
  | 'needs_you' | 'blocked' | 'working' | 'starting' | 'idle' | 'reported' | 'landing' | 'failed' | 'stopped' | 'suspended' | 'landed' | 'retired';

/** §4.3's sort column. */
export const STATE_ORDER: Record<CardStateName, number> = {
  needs_you: 1,
  blocked: 2,
  working: 3,
  starting: 4,
  idle: 5,
  reported: 6,
  landing: 7,
  failed: 8,
  stopped: 8,
  suspended: 9,
  landed: 10,
  retired: 11,
};

export const STATE_LABEL: Record<CardStateName, string> = {
  needs_you: 'Needs you',
  blocked: 'Blocked',
  working: 'Working',
  starting: 'Starting',
  idle: 'Idle',
  reported: 'Reported',
  landing: 'Landing',
  failed: 'Failed',
  stopped: 'Stopped',
  suspended: 'Suspended',
  landed: 'Landed',
  retired: 'Retired',
};

export interface CardState {
  state: CardStateName;
  order: number;
  label: string;
  /** One line under the title: what it waits on, the question, the report. */
  sub: string | null;
  /** Red rather than orange: an `action` blocker only the human can do. */
  urgent: boolean;
  /** What waits on the human (a task blocker, a prompt on screen); 0 unless Needs you. */
  waiting: number;
  /** Landed's qualifier (§4.3): `undone`. */
  qualifier?: string | null;
}

/** The state line: §4.3 has a Needs-you card carry its waiting count, one included, and a Landed
 *  card its qualifier (*undone*). */
export function stateLine(s: Pick<CardState, 'state' | 'label' | 'waiting'> & { qualifier?: string | null }): string {
  if (s.state === 'needs_you' && s.waiting > 0) return `${s.label} · ${s.waiting} waiting`;
  return s.qualifier ? `${s.label} · ${s.qualifier}` : s.label;
}

/** The phases that hold a slot (hive/worker.rs `LIVE_PHASES`): only these have an agent to read. */
const LIVE_PHASES = ['launching', 'running', 'blocked', 'reported', 'landing'];

/** A worker that left for good (§8.4): no tab, its column reads its recorded sessions. */
export const isRetired = (w: Pick<HiveWorkerCard, 'phase'>) => w.phase === 'landed' || w.phase === 'retired';

/** What a retired worker's column says in place of a composer: "Retired · landed a1b2c3d". */
export function retiredLine(w: Pick<HiveWorkerCard, 'phase' | 'retired' | 'undone'>): string {
  if (w.phase === 'landed') {
    const sha = w.retired?.landed?.trim();
    return `Retired · landed${sha ? ` ${sha.slice(0, 7)}` : ''}${w.undone ? ' · undone' : ''}`;
  }
  const why = w.retired?.reason?.trim();
  return why ? `Retired · ${why}` : 'Retired';
}

type WorkerFields = Pick<HiveWorkerCard, 'phase' | 'wait_reason' | 'depends_on_labels' | 'last_report'> & Partial<Pick<HiveWorkerCard, 'retired' | 'undone'>>;
type TaskFields = { status: TaskStatus; blocker?: TaskBlocker | null } | null | undefined;
type AgentFields = { state: AgentState; toolName?: string } | null | undefined;
/** `suspended`: the tab's PTY was parked by a suspend, its own or its workspace's (§11.2). */
type TabFields = { suspended?: boolean } | null | undefined;

function make(state: CardStateName, sub: string | null = null, extra: Partial<CardState> = {}): CardState {
  return { state, order: STATE_ORDER[state], label: STATE_LABEL[state], sub, urgent: false, waiting: 0, ...extra };
}

/** The agent a card may read: only a live phase has one, and a suspended tab's PTY is gone. The
 *  state and the activity line both go through this, so they never disagree about it. */
export function onScreenAgent<A>(phase: string, agent: A | null | undefined, tab: TabFields): A | null {
  return LIVE_PHASES.includes(phase) && !tab?.suspended && agent ? agent : null;
}

/** Which state a worker's card shows (§4.3). A task blocker is the human's whatever the phase;
 *  a prompt counts only while an agent can be holding one. A busy agent reads Working even after
 *  a report: a coordinator's message sets it going again without changing its phase. */
export function cardState(worker: WorkerFields, task: TaskFields, agent: AgentFields, tab: TabFields): CardState {
  const live = LIVE_PHASES.includes(worker.phase);
  const suspended = live && !!tab?.suspended;
  const onScreen = onScreenAgent(worker.phase, agent, tab);
  const prompt = onScreen?.state === 'permission' ? 'Waiting for permission'
    : onScreen?.state === 'active' && onScreen.toolName === 'AskUserQuestion' ? 'Asking a question'
    : null;
  const blocker = task?.status === 'blocked' ? task.blocker ?? null : null;
  const report = worker.last_report?.summary?.trim() || null;

  if (blocker) return make('needs_you', blocker.question, { urgent: blocker.kind === 'action', waiting: prompt ? 2 : 1 });
  switch (worker.phase) {
    case 'failed':
      return make('failed', worker.last_report?.status === 'failed' ? report : null);
    case 'stopped':
      return make('stopped');
    case 'landed':
      return make('landed', null, { qualifier: worker.undone ? 'undone' : null });
    case 'retired':
      return make('retired', worker.retired?.reason?.trim() || null);
    case 'waiting': {
      const deps = worker.depends_on_labels;
      return make('starting', worker.wait_reason?.trim() || (deps.length ? `waits on ${deps.join(', ')}` : null));
    }
  }
  if (suspended) return make('suspended');
  if (prompt) return make('needs_you', prompt, { waiting: 1 });
  // Merging, held or waiting on the human: what it is, whatever its agent is doing meanwhile.
  if (worker.phase === 'landing') return make('landing');
  if (onScreen?.state === 'active') return make('working');
  switch (worker.phase) {
    case 'blocked':
      return make('blocked', report);
    case 'reported':
      return make('reported', report);
    case 'launching':
      // Its session may have started before the board heard `running`.
      return onScreen ? make('idle') : make('starting');
    case 'running':
      // Starting is a launch in progress (§4.3), and a running worker's launch is done: no hook
      // state (a webview reload empties it, an idle agent sends none) is Idle, not Starting.
      return make('idle');
    default:
      // A phase a newer build wrote: shown, never guessed at.
      return make('idle', worker.phase || null);
  }
}

type Sortable = { state: Pick<CardState, 'order'>; worker: Pick<HiveWorkerCard, 'badge'> };

/** By state (§4.3's sort column), then by badge, the worker's fixed number (§4.6). */
export function sortCards<T extends Sortable>(cards: readonly T[]): T[] {
  return [...cards].sort((a, b) => a.state.order - b.state.order || a.worker.badge - b.worker.badge);
}

type Waveable = { worker: Pick<HiveWorkerCard, 'task_id' | 'depends_on'> };

/** The board's columns (§4.4): wave 1 depends on no card, wave 2 on wave 1, and so on. A
 *  dependency that isn't a card here (a hand-made task) adds no depth, and a cycle (a hand-edited
 *  task list) is cut where it closes. Each wave keeps the order it was given. */
export function waves<T extends Waveable>(cards: readonly T[]): T[][] {
  const byTask = new Map(cards.map((c) => [c.worker.task_id, c]));
  const depth = new Map<string, number>();
  const visiting = new Set<string>();
  const depthOf = (id: string): number => {
    const known = depth.get(id);
    if (known !== undefined) return known;
    const card = byTask.get(id);
    if (!card || visiting.has(id)) return -1;
    visiting.add(id);
    let d = 0;
    for (const dep of card.worker.depends_on) {
      if (dep !== id) d = Math.max(d, depthOf(dep) + 1);
    }
    visiting.delete(id);
    depth.set(id, d);
    return d;
  };
  const out: T[][] = [];
  for (const c of cards) {
    const d = depthOf(c.worker.task_id);
    (out[d] ??= []).push(c);
  }
  return out.filter((w) => w?.length);
}

/** How long since it launched (or, waiting, since it was registered): 2d, 4h, 12m, now. `now` is
 *  passed in so a board's one coarse tick re-renders every age. */
export function cardAge(worker: Pick<HiveWorkerCard, 'created_at' | 'launched_at'>, now: number): string {
  const ms = now - Date.parse(worker.launched_at || worker.created_at);
  if (!Number.isFinite(ms) || ms < 0) return '';
  const d = Math.floor(ms / 86_400_000);
  if (d > 0) return `${d}d`;
  const h = Math.floor(ms / 3_600_000);
  if (h > 0) return `${h}h`;
  const m = Math.floor(ms / 60_000);
  return m > 0 ? `${m}m` : 'now';
}

/** The newest note on its task: the worker's context update (§4.3). */
export function latestNote(task: { notes?: { text: string }[] } | null | undefined): string | null {
  const n = task?.notes;
  return n?.length ? n[n.length - 1].text.trim() || null : null;
}
