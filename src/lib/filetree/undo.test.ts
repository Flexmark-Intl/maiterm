import { describe, expect, it } from 'vitest';
import type { FileOp, OpResult } from '$lib/tauri/types';
import { OWN_SETTLE_MS, STALE_CHANGED, UNDO_MAX, UndoStacks, describeResult, undoExpectations, undoLabel, type Lookup } from './undo';

const P = '/repo';

/** A lookup over a set of paths that exist, with listed folders. */
function disk(paths: string[], listed: string[] = ['', 'src', 'lib']): Lookup {
  const set = new Set(paths);
  return (rel) => {
    const i = rel.lastIndexOf('/');
    const dir = i < 0 ? '' : rel.slice(0, i);
    return listed.includes(dir) ? set.has(rel) : undefined;
  };
}

const res = (done: { from: string; to: string | null }[], extra: Partial<OpResult> = {}): OpResult => ({ done, failed: [], skipped: [], undo: null, needs: null, ...extra });

describe('undoExpectations', () => {
  it('a rename back needs the new name there and the old one free', () => {
    expect(undoExpectations({ kind: 'rename', src: ['src/b.ts'], name: 'a.ts' })).toEqual([
      { rel: 'src/b.ts', exists: true },
      { rel: 'src/a.ts', exists: false },
    ]);
  });

  it('a case-only rename back does not require its own other spelling to be free', () => {
    expect(undoExpectations({ kind: 'rename', src: ['Foo.ts'], name: 'foo.ts' })).toEqual([{ rel: 'Foo.ts', exists: true }]);
  });

  it('a move back needs each item where it went and each old place free', () => {
    expect(undoExpectations({ kind: 'move', src: ['lib/a', 'lib/b'], to: ['src/a', 'b'] })).toEqual([
      { rel: 'lib/a', exists: true },
      { rel: 'lib/b', exists: true },
      { rel: 'src/a', exists: false },
      { rel: 'b', exists: false },
    ]);
  });

  it('trash needs the items, restore needs their places free', () => {
    expect(undoExpectations({ kind: 'trash', src: ['x'] })).toEqual([{ rel: 'x', exists: true }]);
    expect(undoExpectations({ kind: 'restore', src: ['x'], trashed_at: 1 })).toEqual([{ rel: 'x', exists: false }]);
  });
});

describe('UndoStacks', () => {
  it('keeps one stack per root, capped at 50, newest on top', () => {
    const u = new UndoStacks();
    for (let i = 0; i < UNDO_MAX + 5; i++) u.push(P, { kind: 'trash', src: [`f${i}`] }, `f${i}`);
    u.push('/other', { kind: 'trash', src: ['o'] }, 'o');
    expect(u.size(P)).toBe(UNDO_MAX);
    expect(u.peek(P)?.label).toBe(`f${UNDO_MAX + 4}`);
    expect(u.pop(P)?.label).toBe(`f${UNDO_MAX + 4}`);
    expect(u.size('/other')).toBe(1);
    expect(u.pop('/nowhere')).toBeNull();
  });

  it('a foreign change that breaks an expectation marks the entry stale', () => {
    const u = new UndoStacks();
    const e = u.push(P, { kind: 'rename', src: ['src/b.ts'], name: 'a.ts' }, 'rename');
    // Somebody else made src/a.ts, where the rename would put the file back.
    const marked = u.noteChange(P, 'src', ['a.ts'], disk(['src/b.ts', 'src/a.ts']), 10_000);
    expect(marked).toBe(1);
    expect(e.stale).toBe(STALE_CHANGED);
  });

  it('a foreign change to unrelated names leaves the entry alone', () => {
    const u = new UndoStacks();
    const e = u.push(P, { kind: 'rename', src: ['src/b.ts'], name: 'a.ts' }, 'rename');
    u.noteChange(P, 'src', ['other.ts'], disk(['src/b.ts']), 10_000);
    u.noteChange(P, 'lib', null, disk(['src/b.ts']), 10_000);
    expect(e.stale).toBeNull();
  });

  it('a whole re-listing checks every expectation in that folder', () => {
    const u = new UndoStacks();
    const e = u.push(P, { kind: 'trash', src: ['src/new.ts'] }, 'create');
    u.noteChange(P, 'src', null, disk([]), 10_000);
    expect(e.stale).toBe(STALE_CHANGED);
  });

  it('our own ops, in flight or just settled, never spoil an entry', () => {
    const u = new UndoStacks();
    // Created a.ts (undo: trash it), then renamed it to b.ts ourselves.
    const create = u.push(P, { kind: 'trash', src: ['a.ts'] }, 'create');
    u.beginOp();
    expect(u.noteChange(P, '', ['a.ts', 'b.ts'], disk(['b.ts']), 5_000)).toBe(0);
    u.endOp(5_000);
    expect(u.noteChange(P, '', ['a.ts', 'b.ts'], disk(['b.ts']), 5_000 + OWN_SETTLE_MS - 1)).toBe(0);
    expect(create.stale).toBeNull();
    // After the settle window the same contradiction is somebody else's.
    expect(u.noteChange(P, '', ['a.ts'], disk(['b.ts']), 5_000 + OWN_SETTLE_MS + 1)).toBe(1);
  });

  it('checks against the model right before running, unknown folders never block', () => {
    const u = new UndoStacks();
    const e = u.push(P, { kind: 'move', src: ['deep/x'], to: ['src/x'] }, 'move');
    // `deep` isn't listed: nothing known, so it may run (Rust refuses a clobber anyway).
    expect(u.check(e, disk([]), 0)).toBeNull();
    const f = u.push(P, { kind: 'trash', src: ['src/gone.ts'] }, 'create');
    expect(u.check(f, disk([]), 0)).toBe(STALE_CHANGED);
    // Once stale, stays stale.
    expect(u.check(f, disk(['src/gone.ts']), 0)).toBe(STALE_CHANGED);
  });

  it('the rename then create sequence undoes in order', () => {
    const u = new UndoStacks();
    u.push(P, { kind: 'trash', src: ['a.ts'] }, 'create');
    u.push(P, { kind: 'rename', src: ['b.ts'], name: 'a.ts' }, 'rename');
    const top = u.pop(P)!;
    expect(u.check(top, disk(['b.ts']), 0)).toBeNull();
    // After the rename back, a.ts is there again for the create's undo.
    const next = u.pop(P)!;
    expect(u.check(next, disk(['a.ts']), 0)).toBeNull();
  });

  it('a quick second undo isn’t judged by a model the first hasn’t reached yet', () => {
    const u = new UndoStacks();
    // a.ts → b.ts → c.ts; undoing both renames.
    u.push(P, { kind: 'rename', src: ['b.ts'], name: 'a.ts' }, 'rename of a');
    u.push(P, { kind: 'rename', src: ['c.ts'], name: 'b.ts' }, 'rename of b');
    const first = u.pop(P)!;
    expect(u.check(first, disk(['c.ts']), 10_000)).toBeNull();
    u.beginOp();
    u.endOp(10_050);
    // The first undo answered, but its re-list hasn't arrived: the model still has c.ts.
    const second = u.pop(P)!;
    expect(u.check(second, disk(['c.ts']), 10_060)).toBeNull();
    // Once settled, the model is the word again.
    expect(u.check(second, disk(['c.ts']), 10_050 + OWN_SETTLE_MS + 1)).toBe(STALE_CHANGED);
  });
});

describe('words', () => {
  it('says what happened in the platform’s words', () => {
    const op: FileOp = { kind: 'trash', src: ['src/a.ts'] };
    expect(describeResult(op, res([{ from: 'src/a.ts', to: null }]), 'mac')).toBe('Moved “a.ts” to the Trash');
    expect(describeResult(op, res([{ from: 'src/a.ts', to: null }]), 'windows')).toBe('Moved “a.ts” to the Recycle Bin');
    const mv: FileOp = { kind: 'move', src: ['a', 'b'], dest: 'src' };
    expect(describeResult(mv, res([{ from: 'a', to: 'src/a' }, { from: 'b', to: 'src/b' }]), 'linux')).toBe('Moved 2 items');
    expect(describeResult({ kind: 'rename', src: ['a'], name: 'b' }, res([{ from: 'x/a', to: 'x/b' }]), 'linux')).toBe('Renamed “a” to “b”');
  });

  it('reports failures and skips, and nothing at all as null', () => {
    const op: FileOp = { kind: 'move', src: ['a', 'b'], dest: 'src' };
    const r = res([{ from: 'a', to: 'src/a' }], { failed: [{ rel: 'b', code: 'exists', msg: '“b” is already there' }] });
    expect(describeResult(op, r, 'mac')).toBe('Moved “a” · “b”: “b” is already there');
    expect(describeResult(op, res([]), 'mac')).toBeNull();
    expect(describeResult(op, res([], { skipped: ['a'] }), 'mac')).toBe('1 left as it was');
  });

  it('labels what Cmd+Z reverses', () => {
    expect(undoLabel({ kind: 'create_file', name: 'x.ts' }, res([{ from: 'src/x.ts', to: null }]))).toBe('new file “x.ts”');
    expect(undoLabel({ kind: 'move', src: ['a', 'b'] }, res([{ from: 'a', to: 'z/a' }, { from: 'b', to: 'z/b' }]))).toBe('move of 2 items');
  });
});
