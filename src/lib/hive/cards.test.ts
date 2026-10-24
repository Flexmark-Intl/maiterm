import { describe, expect, it } from 'vitest';
import { cardAge, cardState, isRetired, landLine, latestNote, onScreenAgent, pushHeld, retiredLine, reviewLine, sortCards, stateLine, STATE_ORDER, waves } from './cards';
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

  it('a Landing card says what its land waits on, and a held one says so', () => {
    const land = (over: Partial<NonNullable<HiveWorkerCard['land']>>) => ({ id: 'L-1', state: 'checking', repos: [], ...over });
    const held = cardState(worker({ phase: 'landing', land: land({ state: 'held', held_reason: 'Landed web; holding api: main keeps moving (3 tries)' }) }), null, idle, null);
    expect(stateLine(held)).toBe('Landing · held');
    expect(held.sub).toBe('Landed web; holding api: main keeps moving (3 tries)');
    expect(cardState(worker({ phase: 'landing', land: land({}) }), null, idle, null).sub).toBe('land check running');
    expect(landLine(land({ state: 'locking', waiting_for: 'land L-2 of maiTerm prod (pid 9) in web' }))).toBe('waiting for land L-2 of maiTerm prod (pid 9) in web');
    expect(landLine(land({ state: 'locking' }))).toBe('taking the land lock');
    expect(landLine(land({ state: 'queued_batch' }))).toBe('waiting to land in a batch');
    expect(landLine(land({ batch: 'Q-1a2b3c4d' }))).toBe('batch land check running');
    expect(landLine(land({ state: 'someday' }))).toBe('someday');
    expect(landLine(null)).toBeNull();
    expect(cardState(worker({ phase: 'landing' }), null, idle, null).sub).toBeNull();
  });

  it("a land waiting on the human's card is Landing (§4.3), whatever its agent does", () => {
    const land = { id: 'L-1', state: 'awaiting_human', request: 'r_1', repos: [] };
    const s = cardState(worker({ phase: 'landing', land }), null, active, null);
    expect([s.state, s.sub]).toEqual(['landing', 'waiting for your approval']);
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

  it('a Landed card says a push that was held, naming the repo when there were several', () => {
    const land = (repos: { folder: string; status: string; pushed?: string | null }[]) => ({ id: 'L-1', state: 'landed', repos });
    const one = land([{ folder: 'web', status: 'landed', pushed: 'held: the remote has moved on' }]);
    expect(stateLine(cardState(worker({ phase: 'landed', land: one }), null, null, null))).toBe('Landed · push held: the remote has moved on');
    const two = land([{ folder: 'web', status: 'landed', pushed: 'ok' }, { folder: 'api', status: 'landed', pushed: 'held: no auth' }]);
    expect(pushHeld(two)).toBe('push held: api: no auth');
    expect(pushHeld(land([{ folder: 'web', status: 'landed', pushed: 'ok' }]))).toBeNull();
    expect(pushHeld(land([{ folder: 'web', status: 'landed', pushed: 'off' }]))).toBeNull();
    expect(stateLine(cardState(worker({ phase: 'landed', land: one, undone: true }), null, null, null))).toBe('Landed · undone');
    expect(retiredLine({ phase: 'landed', retired: null, land: one })).toBe('Retired · landed · push held: the remote has moved on');
  });

  it('a suspect land says so, below undone and above a held push (§8.6)', () => {
    const one = { id: 'L-1', state: 'landed', repos: [{ folder: 'web', status: 'landed', pushed: 'held: no auth' }] };
    expect(stateLine(cardState(worker({ phase: 'landed', land: one, suspect: true }), null, null, null))).toBe('Landed · suspect');
    expect(stateLine(cardState(worker({ phase: 'landed', suspect: true, undone: true }), null, null, null))).toBe('Landed · undone');
    expect(retiredLine({ phase: 'landed', retired: null, suspect: true })).toBe('Retired · landed · suspect');
  });

  it('a suspect land sorts with Needs you while it stands, an undone or cleared one with Landed (§4.3)', () => {
    const at = (badge: number, over: Partial<HiveWorkerCard>) => ({ state: cardState(worker({ phase: 'landed', ...over }), null, null, null), worker: { badge } });
    const blocked = { state: cardState(worker({ phase: 'running' }), { status: 'blocked', blocker: blocker('decision') }, null, null), worker: { badge: 2 } };
    const out = sortCards([at(1, {}), at(4, { suspect: true, undone: true }), at(3, { suspect: true }), blocked, { state: cardState(worker({ phase: 'running' }), null, active, null), worker: { badge: 5 } }]);
    expect(out.map((c) => c.worker.badge)).toEqual([2, 3, 5, 1, 4]);
    expect(at(3, { suspect: true }).state.state).toBe('landed');
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

describe('reviewLine', () => {
  const review = (over: Partial<NonNullable<HiveWorkerCard['review']>> = {}) => ({ attempts: 0, max_attempts: 3, verdicts: 0, last: null, escalated: false, resolution: null, ...over });

  it('a reviewer names what it reviews', () => {
    expect(reviewLine({ reviews: '#3 login' })).toEqual({ text: 'Reviewing #3 login', urgent: false });
  });

  it('a builder shows how its reviews stand, an open escalation as urgent', () => {
    expect(reviewLine({})).toBeNull();
    expect(reviewLine({ review: review() })).toBeNull();
    expect(reviewLine({ review: review({ verdicts: 1, last: 'pass' }) })?.text).toBe('Review passed');
    expect(reviewLine({ review: review({ verdicts: 2, last: 'fail', attempts: 2 }) })).toEqual({ text: 'Review failed · 2/3', urgent: false });
    expect(reviewLine({ review: review({ verdicts: 3, last: 'fail', attempts: 3, escalated: true }) })).toEqual({ text: 'Escalated · 3 failed reviews', urgent: true });
    expect(reviewLine({ review: review({ verdicts: 3, last: 'fail', attempts: 3, escalated: true, resolution: 'accept' }) })?.text).toBe('Review accepted as is');
    expect(reviewLine({ review: review({ verdicts: 3, last: 'fail', attempts: 3, escalated: true, resolution: 'reassign' }) })?.text).toBe('Review resolved: reassign');
    expect(reviewLine({ review: review({ verdicts: 3, last: 'fail', resolution: 'revise' }) })?.text).toBe('Brief revised after review');
    expect(reviewLine({ review: review({ verdicts: 4, last: 'fail', attempts: 1, resolution: 'revise' }) })?.text).toBe('Review failed · 1/3');
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
