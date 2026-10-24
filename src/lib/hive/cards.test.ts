import { describe, expect, it } from 'vitest';
import { cardAge, cardState, isRetired, latestNote, onScreenAgent, retiredLine, sortCards, stateLine, STATE_ORDER, waves } from './cards';
import type { HiveWorkerCard, TaskBlocker } from '$lib/tauri/types';

const worker = (over: Partial<HiveWorkerCard> = {}): HiveWorkerCard => ({
  badge: 1,
  worker: 'w',
  title: 'W',
  task_id: 't1',
  tab_id: 'tab1',
  role: 'backend',
  phase: 'running',
  wait_reason: null,
  depends_on: [],
  depends_on_labels: [],
  folders: [],
  model: 'sonnet',
  effort: 'high',
  last_report: null,
  created_at: '2026-10-24T00:00:00Z',
  launched_at: '2026-10-24T00:00:01Z',
  ...over,
});

const report = (status: string, summary: string) => ({ status, summary, receipts: { commits: [], files: [], assets: [] }, at: 'now' });
const blocker = (kind: TaskBlocker['kind'], question = 'Which DB?'): TaskBlocker => ({ kind, question, asked_at: 'now', asked_by: 'agent' });
const active = { state: 'active' as const };
const idle = { state: 'idle' as const };

describe('cardState', () => {
  it('a task blocker is the human’s whatever the phase, red for an action', () => {
    const s = cardState(worker(), { status: 'blocked', blocker: blocker('decision') }, active, null);
    expect([s.state, s.sub, s.urgent, s.waiting]).toEqual(['needs_you', 'Which DB?', false, 1]);
    expect(cardState(worker({ phase: 'stopped' }), { status: 'blocked', blocker: blocker('action') }, null, null).urgent).toBe(true);
  });

  it('a Needs-you card shows its waiting count, even when it is one', () => {
    expect(stateLine(cardState(worker(), null, { state: 'permission' }, null))).toBe('Needs you · 1 waiting');
    expect(stateLine(cardState(worker(), { status: 'blocked', blocker: blocker('external') }, { state: 'permission' }, null))).toBe('Needs you · 2 waiting');
    expect(stateLine(cardState(worker(), null, active, null))).toBe('Working');
  });

  it('a blocker only counts while the task is in Blocked', () => {
    expect(cardState(worker(), { status: 'active', blocker: blocker('decision') }, active, null).state).toBe('working');
  });

  it('a prompt on screen needs you, and adds to a blocker’s count', () => {
    expect(cardState(worker(), { status: 'active' }, { state: 'permission' }, null)).toMatchObject({ state: 'needs_you', waiting: 1 });
    expect(cardState(worker(), null, { state: 'active', toolName: 'AskUserQuestion' }, null)).toMatchObject({ state: 'needs_you', sub: 'Asking a question' });
    expect(cardState(worker(), { status: 'blocked', blocker: blocker('external') }, { state: 'permission' }, null).waiting).toBe(2);
  });

  it('a stale prompt on a stopped, failed or suspended worker is not a prompt', () => {
    expect(cardState(worker({ phase: 'stopped' }), null, { state: 'permission' }, null).state).toBe('stopped');
    expect(cardState(worker({ phase: 'failed' }), null, { state: 'permission' }, null).state).toBe('failed');
    expect(cardState(worker(), null, { state: 'permission' }, { suspended: true }).state).toBe('suspended');
  });

  it('a worker reported blocked waits on its coordinator, not the human', () => {
    const s = cardState(worker({ phase: 'blocked', last_report: report('blocked', 'Needs the schema') }), null, idle, null);
    expect([s.state, s.sub]).toEqual(['blocked', 'Needs the schema']);
  });

  it('a busy agent reads Working, even after a report', () => {
    expect(cardState(worker(), null, active, null).state).toBe('working');
    expect(cardState(worker({ phase: 'reported' }), null, active, null).state).toBe('working');
    expect(cardState(worker({ phase: 'reported', last_report: report('done', 'Shipped') }), null, idle, null)).toMatchObject({ state: 'reported', sub: 'Shipped' });
  });

  it('starting: launching with no session yet, and a waiter with its reason', () => {
    expect(cardState(worker({ phase: 'launching' }), null, null, null).state).toBe('starting');
    expect(cardState(worker(), null, idle, null).state).toBe('idle');
    const w = cardState(worker({ phase: 'waiting', tab_id: null, wait_reason: 'waits on #1 schema' }), null, null, null);
    expect([w.state, w.sub]).toEqual(['starting', 'waits on #1 schema']);
    expect(cardState(worker({ phase: 'waiting', wait_reason: 'blocked: #1 schema failed' }), null, null, null).sub).toBe('blocked: #1 schema failed');
    expect(cardState(worker({ phase: 'waiting', depends_on_labels: ['#1 a', '#2 b'] }), null, null, null).sub).toBe('waits on #1 a, #2 b');
  });

  it('a launching worker whose session started reads Idle once its kickoff turn ends', () => {
    // The board may still hold `launching` from the spawn when the agent's hooks arrive.
    expect(cardState(worker({ phase: 'launching' }), null, idle, null).state).toBe('idle');
    expect(cardState(worker({ phase: 'launching' }), null, active, null).state).toBe('working');
  });

  it('a running worker with no hook state is Idle, never a launch in progress', () => {
    // A webview reload empties the agent store, and an idle agent sends no hook to refill it.
    expect(cardState(worker(), null, null, null)).toMatchObject({ state: 'idle', sub: null });
  });

  it('failed shows a failed report’s summary; a crash has none', () => {
    expect(cardState(worker({ phase: 'failed', last_report: report('failed', 'Tests never pass') }), null, null, null).sub).toBe('Tests never pass');
    expect(cardState(worker({ phase: 'failed', last_report: report('done', 'old') }), null, null, null).sub).toBeNull();
  });

  it('a phase from a newer build shows as itself', () => {
    expect(cardState(worker({ phase: 'reviewing' }), null, idle, null)).toMatchObject({ state: 'idle', sub: 'reviewing' });
  });

  it('landing reads Landing even while its agent works, and still needs you for a prompt', () => {
    expect(cardState(worker({ phase: 'landing' }), null, active, null)).toMatchObject({ state: 'landing', order: 7 });
    expect(cardState(worker({ phase: 'landing' }), null, { state: 'permission' }, null).state).toBe('needs_you');
  });

  it('landed and retired: no agent read, the retirement reason and the undone qualifier shown', () => {
    const retired = { reason: 'Wrong approach', at: 'now', kept: [] };
    const r = cardState(worker({ phase: 'retired', tab_id: null, retired }), null, { state: 'permission' }, null);
    expect([r.state, r.order, r.sub]).toEqual(['retired', 11, 'Wrong approach']);
    const l = cardState(worker({ phase: 'landed', tab_id: null, undone: true }), null, active, null);
    expect([l.state, l.order, stateLine(l)]).toEqual(['landed', 10, 'Landed · undone']);
    expect(stateLine(cardState(worker({ phase: 'landed' }), null, null, null))).toBe('Landed');
    expect(cardState(worker({ phase: 'retired' }), { status: 'blocked', blocker: blocker('action') }, null, null).state).toBe('needs_you');
  });
});

describe('retired workers', () => {
  it('only landed and retired read their recorded sessions', () => {
    for (const phase of ['landed', 'retired']) expect(isRetired({ phase })).toBe(true);
    for (const phase of ['stopped', 'failed', 'waiting', 'landing']) expect(isRetired({ phase })).toBe(false);
  });

  it('the column says how it left', () => {
    expect(retiredLine({ phase: 'landed', retired: { reason: 'landed', at: 'now', kept: [], landed: 'a1b2c3d4e5f6' } })).toBe('Retired · landed a1b2c3d');
    expect(retiredLine({ phase: 'landed', retired: null, undone: true })).toBe('Retired · landed · undone');
    expect(retiredLine({ phase: 'retired', retired: { reason: 'Wrong approach', at: 'now', kept: [] } })).toBe('Retired · Wrong approach');
    expect(retiredLine({ phase: 'retired' })).toBe('Retired');
  });
});

describe('onScreenAgent', () => {
  it('only a live, unsuspended worker has an agent to read', () => {
    expect(onScreenAgent('running', active, null)).toBe(active);
    expect(onScreenAgent('reported', active, { suspended: false })).toBe(active);
    expect(onScreenAgent('landing', active, null)).toBe(active);
    expect(onScreenAgent('running', active, { suspended: true })).toBeNull();
    for (const phase of ['waiting', 'stopped', 'failed', 'landed', 'retired']) expect(onScreenAgent(phase, active, null)).toBeNull();
    expect(onScreenAgent('running', undefined, null)).toBeNull();
  });
});

const card = (badge: number, state: keyof typeof STATE_ORDER, task = `t${badge}`, deps: string[] = []) => ({
  state: { order: STATE_ORDER[state] },
  worker: { badge, task_id: task, depends_on: deps },
});

describe('sortCards', () => {
  it('by state, then badge', () => {
    const out = sortCards([card(3, 'idle'), card(2, 'working'), card(5, 'needs_you'), card(1, 'working'), card(4, 'stopped')]);
    expect(out.map((c) => c.worker.badge)).toEqual([5, 1, 2, 3, 4]);
  });
});

describe('waves', () => {
  it('columns by dependency depth, each keeping the given order', () => {
    const a = card(1, 'working', 'a');
    const b = card(2, 'starting', 'b', ['a']);
    const c = card(3, 'working', 'c');
    const d = card(4, 'starting', 'd', ['b', 'c']);
    expect(waves([d, c, b, a]).map((w) => w.map((x) => x.worker.task_id))).toEqual([['c', 'a'], ['b'], ['d']]);
  });

  it('a dependency that is not a card adds no depth', () => {
    expect(waves([card(1, 'starting', 'a', ['hand-made'])]).map((w) => w.length)).toEqual([1]);
  });

  it('a cycle is cut, not looped', () => {
    const out = waves([card(1, 'idle', 'a', ['b']), card(2, 'idle', 'b', ['a']), card(3, 'idle', 'c', ['c'])]);
    expect(out.flat()).toHaveLength(3);
    expect(out.length).toBe(2);
  });

  it('nothing in, nothing out', () => {
    expect(waves([])).toEqual([]);
  });
});

describe('cardAge', () => {
  it('since launch, or since registered while waiting', () => {
    const t0 = Date.parse('2026-10-24T00:00:00Z');
    expect(cardAge(worker({ launched_at: '2026-10-24T00:00:00Z' }), t0 + 4 * 60_000)).toBe('4m');
    expect(cardAge(worker({ launched_at: null, created_at: '2026-10-24T00:00:00Z' }), t0 + 3 * 3_600_000)).toBe('3h');
    expect(cardAge(worker({ launched_at: '2026-10-24T00:00:00Z' }), t0 + 10_000)).toBe('now');
    expect(cardAge(worker({ launched_at: null, created_at: '' }), t0)).toBe('');
  });
});

describe('latestNote', () => {
  it('the newest note, trimmed', () => {
    expect(latestNote({ notes: [{ text: 'old' }, { text: ' Split reads; tests next ' }] })).toBe('Split reads; tests next');
    expect(latestNote({ notes: [] })).toBeNull();
    expect(latestNote(null)).toBeNull();
  });
});
