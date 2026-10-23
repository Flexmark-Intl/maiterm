import { describe, expect, it } from 'vitest';
import type { AgentMark } from '$lib/tauri/types';
import { AgentMarks, READ_LIGHT_MS, SWEEP_MS, TurnTracker, agentLensItems, assignColours, changedThisTurn, cwdInRoot, folderBadges, markWords, overlappingWriters, rowView } from './agents';

const T0 = 1_000_000;

function mark(p: Partial<AgentMark> & { rel: string }): AgentMark {
  return { op: 'edit', phase: 'ok', tab_id: 'A', agent: '', turn: 1, evidence: 'tool', at_ms: T0, ...p };
}

/** Marks merged the way the store merges them: the tracker sees every mark. */
function setup(marks: AgentMark[], now = T0) {
  const am = new AgentMarks();
  const turns = new TurnTracker();
  for (const m of marks) turns.noteMark(m);
  am.merge(marks, now);
  return { am, turns };
}

describe('merging marks', () => {
  it('a mark replaces its own slot: pending, then ok', () => {
    const { am, turns } = setup([mark({ rel: 'a.ts', phase: 'pending' })]);
    expect(rowView(am.marksOf('a.ts'), T0, turns).editing).toBe(true);
    am.merge([mark({ rel: 'a.ts', phase: 'ok', at_ms: T0 + 50 })], T0 + 50);
    const v = rowView(am.marksOf('a.ts'), T0 + 60, turns);
    expect(v.editing).toBe(false);
    expect(v.rail).toEqual({ cls: 'write', mode: 'sweep', t0: T0 + 50 });
    expect(v.writers).toEqual(['A']);
  });

  it('a read after an edit lights the row without wiping "written this turn"', () => {
    const { am, turns } = setup([mark({ rel: 'a.ts' })]);
    am.merge([mark({ rel: 'a.ts', op: 'read', at_ms: T0 + 5000 })], T0 + 5000);
    const lit = rowView(am.marksOf('a.ts'), T0 + 5100, turns);
    expect(lit.rail?.mode).toBe('read');
    expect(lit.writers).toEqual(['A']);
    // The read's light goes; the write's dot stays.
    am.pruneReads(T0 + 5000 + READ_LIGHT_MS + 1);
    const after = rowView(am.marksOf('a.ts'), T0 + 5000 + READ_LIGHT_MS + 1, turns);
    expect(after.rail).toBeNull();
    expect(after.writers).toEqual(['A']);
  });

  it('reads leave no lasting mark once their light is over', () => {
    const { am } = setup([mark({ rel: 'r.ts', op: 'read' })]);
    expect(am.size).toBe(1);
    am.merge([], T0 + READ_LIGHT_MS + 1);
    expect(am.size).toBe(0);
  });

  it('an older standing mark arriving late never replaces a newer one', () => {
    const { am } = setup([mark({ rel: 'a.ts', op: 'delete', at_ms: T0 + 10 })]);
    am.merge([mark({ rel: 'a.ts', op: 'edit', at_ms: T0 })], T0 + 20);
    expect(am.marksOf('a.ts').map((m) => m.op)).toEqual(['delete']);
  });

  it('two agents of one tab keep a slot each, and two tabs split the writers', () => {
    const { am, turns } = setup([
      mark({ rel: 'x.ts', agent: '', at_ms: T0 }),
      mark({ rel: 'x.ts', agent: 'sub1', at_ms: T0 + 1 }),
      mark({ rel: 'x.ts', tab_id: 'B', at_ms: T0 + 2 }),
    ]);
    expect(am.marksOf('x.ts')).toHaveLength(3);
    expect(rowView(am.marksOf('x.ts'), T0 + 10_000, turns).writers).toEqual(['B', 'A']);
  });

  it('a reload moves a tab\'s marks to its new id', () => {
    const { am } = setup([mark({ rel: 'a.ts', tab_id: 'old' })]);
    am.remapTab('old', 'new');
    expect(am.marksOf('a.ts').map((m) => m.tab_id)).toEqual(['new']);
    am.merge([mark({ rel: 'a.ts', tab_id: 'new', at_ms: T0 + 1 })], T0 + 1);
    expect(am.marksOf('a.ts')).toHaveLength(1);
  });
});

describe('turns', () => {
  it('a prompt makes everything seen so far an earlier turn', () => {
    const { am, turns } = setup([mark({ rel: 'a.ts', turn: 3 })]);
    expect(rowView(am.marksOf('a.ts'), T0 + 10_000, turns).writers).toEqual(['A']);
    turns.turnStarted('A', T0 + 20_000);
    const v = rowView(am.marksOf('a.ts'), T0 + 20_001, turns);
    expect(v.writers).toEqual([]);
    expect(v.marks).toHaveLength(1); // still in the session
    // The ledger's numbering catches up with the next mark.
    const next = mark({ rel: 'b.ts', turn: 4, at_ms: T0 + 21_000 });
    turns.noteMark(next);
    am.merge([next], T0 + 21_000);
    expect(turns.isThisTurn(next)).toBe(true);
  });

  it('a late mark from an earlier turn is not this turn\'s', () => {
    const turns = new TurnTracker();
    turns.noteMark(mark({ rel: 'a', turn: 5 }));
    const late = mark({ rel: 'b', turn: 4, at_ms: T0 + 1 });
    turns.noteMark(late);
    expect(turns.current('A')).toBe(5);
    expect(turns.isThisTurn(late)).toBe(false);
  });

  it('a tracker cleared while the column was closed follows the ledger\'s renumbering', () => {
    const turns = new TurnTracker();
    turns.noteMark(mark({ rel: 'a', turn: 9 }));
    // Closed: a /clear went by unseen and the ledger counts from 1 again. Reopened, the
    // tracker is rebuilt from the primed marks.
    turns.clear();
    const primed = mark({ rel: 'b', turn: 3, at_ms: T0 + 5 });
    turns.noteMark(primed);
    expect(turns.current('A')).toBe(3);
    expect(turns.isThisTurn(primed)).toBe(true);
    turns.turnStarted('A', T0 + 10);
    expect(turns.current('A')).toBe(4);
  });

  it('a running write pulses; a failed one is outlined; a delete is struck', () => {
    const { am, turns } = setup([
      mark({ rel: 'p.ts', phase: 'pending', op: 'write' }),
      mark({ rel: 'f.ts', phase: 'failed' }),
      mark({ rel: 'd.ts', op: 'delete' }),
    ]);
    const later = T0 + SWEEP_MS + 1;
    expect(rowView(am.marksOf('p.ts'), later, turns).rail).toEqual({ cls: 'write', mode: 'pending', t0: T0 });
    expect(rowView(am.marksOf('p.ts'), later, turns).litUntil).toBe(Infinity);
    expect(rowView(am.marksOf('f.ts'), later, turns).rail?.mode).toBe('failed');
    expect(rowView(am.marksOf('f.ts'), later, turns).writers).toEqual([]);
    const d = rowView(am.marksOf('d.ts'), later, turns);
    expect(d.deleted).toBe(true);
    expect(d.rail).toEqual({ cls: 'delete', mode: 'deleted', t0: T0 });
  });
});

describe('folders and chips', () => {
  it('a folder counts the files changed under it this turn, credited to the busiest tab', () => {
    const { am, turns } = setup([
      mark({ rel: 'src/a.ts' }),
      mark({ rel: 'src/lib/b.ts', tab_id: 'B' }),
      mark({ rel: 'src/lib/c.ts', tab_id: 'B' }),
      mark({ rel: 'src/lib/c.ts', tab_id: 'A', at_ms: T0 + 1 }),
      mark({ rel: 'src/lib/d.ts', tab_id: 'B' }),
      mark({ rel: 'src/r.ts', op: 'read' }),
      mark({ rel: 'src/x.ts', phase: 'failed' }),
      mark({ rel: 'top.ts' }),
    ]);
    const b = folderBadges(am, turns);
    // c.ts was written by both tabs: one file, counted once.
    expect(b.get('src')).toEqual({ count: 4, tab: 'B' });
    expect(b.get('src/lib')).toEqual({ count: 3, tab: 'B' });
    expect(b.has('')).toBe(false);
    expect(changedThisTurn(am, turns)).toEqual(new Map([['A', 3], ['B', 3]]));
  });
});

describe('lens filtering', () => {
  const kind = () => undefined;

  it('Turn keeps this turn\'s writes, shell-time changes and commits; Session keeps them all', () => {
    const { am, turns } = setup([
      mark({ rel: 'old.ts', turn: 1 }),
      mark({ rel: 'new.ts', turn: 2, at_ms: T0 + 1 }),
      mark({ rel: 'disk.ts', turn: 2, op: 'changed', evidence: 'fs_during_tool', at_ms: T0 + 2 }),
      mark({ rel: 'c.ts', turn: 2, op: 'commit', evidence: 'commit', at_ms: T0 + 3 }),
      mark({ rel: 'read.ts', turn: 2, op: 'read', at_ms: T0 + 4 }),
      mark({ rel: 'failed.ts', turn: 2, phase: 'failed', at_ms: T0 + 5 }),
      mark({ rel: 'running.ts', turn: 2, phase: 'pending', at_ms: T0 + 6 }),
    ]);
    const rels = (s: 'turn' | 'session', tab: string | null = null) => agentLensItems(am, turns, s, tab, kind).map((i) => i.rel).sort();
    expect(rels('turn')).toEqual(['c.ts', 'disk.ts', 'new.ts', 'running.ts']);
    expect(rels('session')).toEqual(['c.ts', 'disk.ts', 'new.ts', 'old.ts', 'running.ts']);
  });

  it('a chosen chip scopes the lens to its tab; kinds come from the tree when known', () => {
    const { am, turns } = setup([mark({ rel: 'a/x.ts' }), mark({ rel: 'b', tab_id: 'B' })]);
    expect(agentLensItems(am, turns, 'session', 'B', (r) => (r === 'b' ? 'd' : undefined))).toEqual([{ rel: 'b', kind: 'd' }]);
    expect(agentLensItems(am, turns, 'turn', 'A', kind)).toEqual([{ rel: 'a/x.ts', kind: 'f' }]);
    expect(agentLensItems(am, turns, 'turn', 'C', kind)).toEqual([]);
  });
});

describe('conflicts', () => {
  it('two tabs writing one file in overlapping turns conflict; one after the other does not', () => {
    const marks = [mark({ rel: 'a', tab_id: 'A', at_ms: T0 }), mark({ rel: 'a', tab_id: 'B', at_ms: T0 + 10 })];
    const { turns } = setup(marks);
    const spans: Record<string, { start: number; end: number }> = { A: { start: T0, end: T0 + 100 }, B: { start: T0 + 5, end: Infinity } };
    expect(overlappingWriters(marks, turns, (t) => spans[t] ?? null)).toEqual(['B', 'A']);
    spans.B = { start: T0 + 200, end: Infinity };
    expect(overlappingWriters(marks, turns, (t) => spans[t] ?? null)).toBeNull();
  });

  it('a shell-time change is evidence about the file, not a second writer', () => {
    const marks = [mark({ rel: 'a', tab_id: 'A' }), mark({ rel: 'a', tab_id: 'B', op: 'changed', evidence: 'fs_during_tool' })];
    const { turns } = setup(marks);
    expect(overlappingWriters(marks, turns, () => ({ start: T0, end: Infinity }))).toBeNull();
  });

  it('the tracker\'s span: open while working, else the last mark ends a turn never seen to stop', () => {
    const turns = new TurnTracker();
    turns.noteMark(mark({ rel: 'a', at_ms: T0 }));
    expect(turns.span('A', T0 + 50, true)).toEqual({ start: T0, end: Infinity });
    expect(turns.span('A', T0 + 50, false)).toEqual({ start: T0, end: T0 + 50 });
    turns.turnEnded('A', T0 + 70);
    expect(turns.span('A', T0 + 50, true)).toEqual({ start: T0, end: T0 + 70 });
  });
});

describe('agent colours', () => {
  it('new tabs take the lowest free colour in window order', () => {
    expect(assignColours(new Map(), ['a', 'b', 'c'])).toEqual(new Map([['a', 0], ['b', 1], ['c', 2]]));
  });

  it('a tab keeps its colour when another goes, and the freed one is reused', () => {
    const first = assignColours(new Map(), ['a', 'b', 'c']);
    const second = assignColours(first, ['a', 'c']);
    expect(second).toEqual(new Map([['a', 0], ['c', 2]]));
    // A new tab placed first in the window still doesn't move anyone.
    expect(assignColours(second, ['d', 'a', 'c'])).toEqual(new Map([['a', 0], ['c', 2], ['d', 1]]));
  });

  it('past six the least used colour is shared', () => {
    const tabs = ['a', 'b', 'c', 'd', 'e', 'f', 'g', 'h'];
    const out = assignColours(new Map(), tabs);
    expect([...out.values()]).toEqual([0, 1, 2, 3, 4, 5, 0, 1]);
  });
});

describe('words and folders', () => {
  it('names the evidence, never "edited" for a change seen on disk', () => {
    expect(markWords(mark({ rel: 'a', op: 'changed', evidence: 'fs_during_tool' }), 'claude', true, 'Bash')).toBe('changed on disk while claude’s Bash ran (this turn)');
    expect(markWords(mark({ rel: 'a' }), 'claude', true, 'Edit')).toBe('edited by claude (Edit tool) this turn');
    expect(markWords(mark({ rel: 'a', phase: 'pending', op: 'write' }), 'claude', true)).toBe('being written by claude now');
    expect(markWords(mark({ rel: 'a', op: 'commit', evidence: 'commit' }), 'claude', false)).toBe('committed during claude’s turn (earlier this session)');
  });

  it('cwdInRoot compares whole components, folding case only where asked', () => {
    expect(cwdInRoot('/p/app/src', ['/p/app'], '/', false)).toBe(true);
    expect(cwdInRoot('/p/app', ['/p/app/'], '/', false)).toBe(true);
    expect(cwdInRoot('/p/apples', ['/p/app'], '/', false)).toBe(false);
    expect(cwdInRoot('/P/App/x', ['/p/app'], '/', false)).toBe(false);
    expect(cwdInRoot('/P/App/x', ['/p/app'], '/', true)).toBe(true);
    expect(cwdInRoot('C:\\p\\app\\x', ['C:\\p\\app'], '\\', true)).toBe(true);
  });
});
