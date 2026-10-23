import { describe, expect, it } from 'vitest';
import type { TreeEntry, TreeEvent } from '$lib/tauri/types';
import { TREE_FLAG_HIDDEN, TREE_FLAG_IGNORED, TREE_FLAG_LOOP } from '$lib/tauri/types';
import { TreeModel, ancestorsOf, joinRel, parentRowIndex, stickyAncestors, typeAheadMatch, windowOf, type Row } from './model';

// ── A fake disk that answers like the hub ──────────────────────────────────────────────────
// Folders first, then by name: any fixed order will do, since the model must keep whatever
// order the wire gives and never sort by itself.

type Disk = Map<string, TreeEntry[]>;
const order = (a: TreeEntry, b: TreeEntry) => {
  const fa = a.k === 'd' || a.k === 'L' ? 0 : 1;
  const fb = b.k === 'd' || b.k === 'L' ? 0 : 1;
  return fa - fb || (a.n < b.n ? -1 : a.n > b.n ? 1 : 0);
};

let gen = 1;
const nextGen = () => ++gen;

function dirEvents(disk: Disk, rel: string, g = nextGen(), chunk = 512): TreeEvent[] {
  const entries = [...(disk.get(rel) ?? [])].sort(order);
  const out: TreeEvent[] = [];
  const n = Math.max(1, Math.ceil(entries.length / chunk));
  for (let i = 0; i < n; i++) {
    out.push({ t: 'dir', rel, gen: g, chunk: i, last: i === n - 1, total: entries.length, truncated: false, entries: entries.slice(i * chunk, (i + 1) * chunk) });
  }
  return out;
}

/** The delta the hub sends between two listings of one folder. */
function deltaOf(rel: string, from: number, before: TreeEntry[], after: TreeEntry[]): Extract<TreeEvent, { t: 'delta' }> {
  const sortedAfter = [...after].sort(order);
  const old = new Map(before.map((e) => [e.n, e]));
  const now = new Set(after.map((e) => e.n));
  const add: TreeEntry[] = [];
  const chg: TreeEntry[] = [];
  for (const e of sortedAfter) {
    const o = old.get(e.n);
    if (!o) add.push(e);
    else if (o.k !== e.k || o.f !== e.f) chg.push(e);
  }
  const del = before.filter((e) => !now.has(e.n)).map((e) => e.n);
  const at = [...add, ...chg].map((e) => sortedAfter.findIndex((x) => x.n === e.n));
  return { t: 'delta', rel, from, gen: nextGen(), add, del, chg, at };
}

function apply(m: TreeModel, evs: TreeEvent[]): ('ok' | 'resync')[] {
  return evs.map((ev) => {
    switch (ev.t) {
      case 'dir': return m.applyDir(ev) === 'resync' ? 'resync' : 'ok';
      case 'delta': return m.applyDelta(ev) === 'resync' ? 'resync' : 'ok';
      case 'not_modified': return m.applyNotModified(ev) === 'resync' ? 'resync' : 'ok';
      case 'dir_error': m.applyError(ev); return 'ok';
      default: return 'ok';
    }
  });
}

const f = (n: string, flags = 0): TreeEntry => ({ n, k: 'f', f: flags });
const d = (n: string, flags = 0): TreeEntry => ({ n, k: 'd', f: flags });
const keys = (rows: Row[]) => rows.map((r) => r.key);

function smallDisk(): Disk {
  return new Map<string, TreeEntry[]>([
    ['', [d('src'), d('docs'), f('README.md'), f('.env', TREE_FLAG_HIDDEN), f('dist.js', TREE_FLAG_IGNORED)]],
    ['src', [d('lib'), f('main.ts'), f('app.ts')]],
    ['src/lib', [f('x.ts'), f('y.ts')]],
    ['docs', [f('a.md')]],
  ]);
}

/** Open a model on the root, the way the store does. */
function opened(disk: Disk): TreeModel {
  const m = new TreeModel();
  apply(m, dirEvents(disk, ''));
  return m;
}

function expandAndList(m: TreeModel, disk: Disk, rel: string) {
  const r = m.expand(rel);
  for (const req of r.open) apply(m, dirEvents(disk, req.rel));
  return r;
}

describe('TreeModel listing and expansion', () => {
  it('flattens the root in wire order, folders first, with nothing sorted locally', () => {
    const m = opened(smallDisk());
    expect(keys(m.rows)).toEqual(['docs', 'src', '.env', 'README.md', 'dist.js']);
    expect(m.rows.every((r) => r.depth === 0)).toBe(true);
  });

  it('shows Loading… until the root lists', () => {
    const m = new TreeModel();
    expect(m.rows).toHaveLength(1);
    expect(m.rows[0].kind).toBe('m');
  });

  it('expand splices the subtree in after the folder; collapse takes exactly that range out', () => {
    const disk = smallDisk();
    const m = opened(disk);
    const r = expandAndList(m, disk, 'src');
    expect(r.open).toEqual([{ rel: 'src', known_gen: null }]);
    expect(keys(m.rows)).toEqual(['docs', 'src', 'src/lib', 'src/app.ts', 'src/main.ts', '.env', 'README.md', 'dist.js']);
    expandAndList(m, disk, 'src/lib');
    expect(keys(m.rows).slice(2, 5)).toEqual(['src/lib', 'src/lib/x.ts', 'src/lib/y.ts']);
    expect(m.rows[3].depth).toBe(2);

    const c = m.collapse('src');
    expect(c.close.sort()).toEqual(['src', 'src/lib']);
    expect(keys(m.rows)).toEqual(['docs', 'src', '.env', 'README.md', 'dist.js']);
    // The open child is remembered and comes back with its parent, asked with its generation.
    const again = m.expand('src');
    expect(again.open.map((o) => o.rel)).toEqual(['src', 'src/lib']);
    expect(again.open.every((o) => typeof o.known_gen === 'number')).toBe(true);
    expect(keys(m.rows)).toContain('src/lib/x.ts');
  });

  it('collapse-all leaves only the root rows and closes everything else', () => {
    const disk = smallDisk();
    const m = opened(disk);
    expandAndList(m, disk, 'src');
    expandAndList(m, disk, 'docs');
    const { close } = m.collapseAll();
    expect(close.sort()).toEqual(['docs', 'src']);
    expect(keys(m.rows)).toEqual(['docs', 'src', '.env', 'README.md', 'dist.js']);
  });

  it('filters hidden and ignored entries on the client only', () => {
    const m = opened(smallDisk());
    expect(m.setFilters(false, true)).toBe(true);
    expect(keys(m.rows)).not.toContain('.env');
    m.setFilters(false, false);
    expect(keys(m.rows)).toEqual(['docs', 'src', 'README.md']);
    m.setFilters(true, true);
    expect(keys(m.rows)).toContain('dist.js');
  });

  it('never opens a link loop', () => {
    const disk: Disk = new Map([['', [{ n: 'self', k: 'L', f: TREE_FLAG_LOOP }]]]);
    const m = opened(disk);
    expect(m.expand('self').open).toEqual([]);
    expect(m.rows).toHaveLength(1);
  });

  it('reveal opens every ancestor and follows the ones whose rows are still to come', () => {
    const disk = smallDisk();
    const m = opened(disk);
    const r = m.reveal('src/lib/x.ts');
    expect(r.open.map((o) => o.rel)).toEqual(['src', 'src/lib']);
    apply(m, dirEvents(disk, 'src'));
    apply(m, dirEvents(disk, 'src/lib'));
    expect(keys(m.rows)).toContain('src/lib/x.ts');
    expect(m.rows).toEqual(m.childRows('', 0));
  });

  it('a deeper chunked listing paints as chunks land and ends equal to a full render', () => {
    const many = Array.from({ length: 1300 }, (_, i) => f(`file${String(i).padStart(4, '0')}`));
    const disk: Disk = new Map([['', many]]);
    const m = new TreeModel();
    const evs = dirEvents(disk, '');
    expect(evs).toHaveLength(3);
    apply(m, [evs[0]]);
    expect(m.rows).toHaveLength(512);
    apply(m, [evs[1]]);
    expect(m.rows).toHaveLength(1024);
    apply(m, [evs[2]]);
    expect(m.rows).toHaveLength(1300);
    expect(m.rows).toEqual(m.childRows('', 0));
  });
});

describe('TreeModel convergence protocol', () => {
  it('a chunk from another listing, or out of order, asks for a resync', () => {
    const many = Array.from({ length: 1100 }, (_, i) => f(`n${i}`));
    const disk: Disk = new Map([['', many]]);
    const m = new TreeModel();
    const evs = dirEvents(disk, '');
    apply(m, [evs[0]]);
    expect(apply(m, [evs[2]])).toEqual(['resync']);
    const other = dirEvents(disk, '');
    apply(m, [evs[0]]);
    expect(apply(m, [other[1]])).toEqual(['resync']);
  });

  it('a delta applies only to the generation it was computed from', () => {
    const disk = smallDisk();
    const m = opened(disk);
    const held = m.dirs.get('')!.gen;
    const before = disk.get('')!;
    const after = [...before, f('new.txt')];
    expect(apply(m, [deltaOf('', held + 999, before, after)])).toEqual(['resync']);
    expect(apply(m, [deltaOf('', held, before, after)])).toEqual(['ok']);
    expect(keys(m.rows)).toContain('new.txt');
  });

  it('not_modified is accepted only for the generation held', () => {
    const m = opened(smallDisk());
    const held = m.dirs.get('')!.gen;
    expect(m.applyNotModified({ t: 'not_modified', rel: '', gen: held })).toEqual([]);
    expect(m.applyNotModified({ t: 'not_modified', rel: '', gen: held + 1 })).toBe('resync');
  });

  it('events for a folder nobody follows are ignored', () => {
    const disk = smallDisk();
    const m = opened(disk);
    expect(apply(m, dirEvents(disk, 'src'))).toEqual(['ok']);
    expect(m.dirs.has('src')).toBe(false);
  });

  it('a delta that changes a file into a folder moves it to where Rust says', () => {
    const disk = smallDisk();
    const m = opened(disk);
    const before = disk.get('')!;
    const after = before.map((e) => (e.n === 'README.md' ? d('README.md') : e));
    apply(m, [deltaOf('', m.dirs.get('')!.gen, before, after)]);
    expect(keys(m.rows)).toEqual(['README.md', 'docs', 'src', '.env', 'dist.js']);
  });

  it('deleting an open folder drops its rows and queues it for ftCollapse', () => {
    const disk = smallDisk();
    const m = opened(disk);
    expandAndList(m, disk, 'src');
    expandAndList(m, disk, 'src/lib');
    const before = disk.get('')!;
    apply(m, [deltaOf('', m.dirs.get('')!.gen, before, before.filter((e) => e.n !== 'src'))]);
    expect(keys(m.rows).some((k) => k.startsWith('src'))).toBe(false);
    expect(m.pendingClose.sort()).toEqual(['src', 'src/lib']);
    expect(m.expanded.has('src')).toBe(false);
  });

  it('dir_error gone drops the folder; denied shows a message row', () => {
    const disk = smallDisk();
    const m = opened(disk);
    expandAndList(m, disk, 'src');
    const r = m.applyError({ t: 'dir_error', rel: 'src', code: 'gone', msg: '' });
    expect(r.close).toEqual(['src']);
    expect(keys(m.rows)).not.toContain('src/main.ts');
    expandAndList(m, disk, 'docs');
    m.applyError({ t: 'dir_error', rel: 'docs', code: 'denied', msg: 'no' });
    const i = m.indexOf('docs');
    expect(m.rows[i + 1].kind).toBe('m');
    expect(m.rows[i + 1].name).toBe('Permission denied');
  });

  it('random bursts of deltas, expands, collapses and filter flips always equal a full render', () => {
    // A seeded LCG: the same sequence every run.
    let seed = 12345;
    const rand = (n: number) => {
      seed = (seed * 1103515245 + 12345) & 0x7fffffff;
      return seed % n;
    };
    const disk: Disk = new Map([['', []]]);
    let counter = 0;
    const folders = () => [...disk.keys()];
    for (let i = 0; i < 25; i++) {
      const parent = folders()[rand(disk.size)];
      const name = `d${counter++}`;
      disk.get(parent)!.push(d(name));
      disk.set(joinRel(parent, name), []);
    }
    for (const dir of folders()) for (let j = 0; j < 4; j++) disk.get(dir)!.push(f(`f${counter++}`, rand(5) === 0 ? TREE_FLAG_HIDDEN : 0));

    const m = opened(disk);
    const held = new Map<string, TreeEntry[]>([['', [...disk.get('')!]]]);
    const sync = (evs: TreeEvent[]) => {
      for (const res of apply(m, evs)) {
        if (res === 'resync') throw new Error('unexpected resync');
      }
    };

    for (let step = 0; step < 600; step++) {
      const op = rand(10);
      const all = folders();
      if (op < 3) {
        const rel = all[rand(all.length)];
        const r = m.expand(rel);
        for (const req of r.open) {
          sync(dirEvents(disk, req.rel, nextGen(), 3));
          held.set(req.rel, [...disk.get(req.rel)!]);
        }
      } else if (op < 4) {
        const rel = all[rand(all.length)];
        for (const c of m.collapse(rel).close) held.delete(c);
      } else if (op < 9) {
        // Change a followed folder on "disk" and send its delta.
        const followed = [...m.interest].filter((r) => disk.has(r) && m.dirs.get(r)?.state === 'ok');
        const rel = followed[rand(followed.length)];
        const before = held.get(rel) ?? disk.get(rel)!;
        const now = [...disk.get(rel)!];
        const kind = rand(4);
        if (kind === 0 && now.length > 0) {
          const victim = now[rand(now.length)];
          if (victim.k === 'f') now.splice(now.indexOf(victim), 1);
        } else if (kind === 1) {
          now.push(f(`n${counter++}`, rand(4) === 0 ? TREE_FLAG_IGNORED : 0));
        } else if (kind === 2 && now.length > 0) {
          const i = rand(now.length);
          if (now[i].k === 'f') now[i] = { ...now[i], f: now[i].f ^ TREE_FLAG_HIDDEN };
        } else {
          for (let k = 0; k < 40; k++) now.push(f(`b${counter++}`));
        }
        disk.set(rel, now);
        sync([deltaOf(rel, m.dirs.get(rel)!.gen, before, now)]);
        held.set(rel, [...now]);
      } else {
        m.setFilters(rand(2) === 0, rand(2) === 0);
      }
      expect(m.rows).toEqual(m.childRows('', 0));
      // And the data held is exactly the disk, in the wire's order.
      for (const rel of m.interest) {
        const dd = m.dirs.get(rel);
        if (dd?.state === 'ok') expect(dd.names).toEqual([...disk.get(rel)!].sort(order).map((e) => e.n));
      }
    }
  });
});

describe('virtual list helpers', () => {
  it('a parent is found in the rows the index belongs to, never past them', () => {
    const r = (key: string, depth: number): Row => ({ key, depth, name: key, kind: 'f', flags: 0 });
    // A pruned list (find, the Changed lens) longer than the real tree's rows.
    const rows = [r('a', 0), r('a/b', 1), r('a/b/c.ts', 2), r('a/d.ts', 1), r('e.ts', 0)];
    expect(parentRowIndex(rows, 2)).toBe(1);
    expect(parentRowIndex(rows, 3)).toBe(0);
    expect(parentRowIndex(rows, 4)).toBe(-1);
    expect(parentRowIndex(rows, 12)).toBe(-1);
    expect(parentRowIndex(rows, -1)).toBe(-1);
  });

  it('a 100k-row tree renders a window of ~60 rows and splices a 5k subtree fast', () => {
    const root = Array.from({ length: 100_000 }, (_, i) => (i === 50 ? d('big') : f(`r${String(i).padStart(6, '0')}`)));
    const disk: Disk = new Map([['', root], ['big', Array.from({ length: 5000 }, (_, i) => f(`b${i}`))]]);
    const m = new TreeModel();
    apply(m, dirEvents(disk, ''));
    expect(m.rows).toHaveLength(100_000);

    const w = windowOf(40_000 * 22, 600, 22, m.rows.length);
    expect(w.last - w.first).toBeLessThanOrEqual(60);
    expect(w.first).toBe(40_000 - 10);

    const t0 = performance.now();
    m.expand('big');
    apply(m, dirEvents(disk, 'big'));
    const expandMs = performance.now() - t0;
    expect(m.rows).toHaveLength(105_000);
    const t1 = performance.now();
    m.collapse('big');
    const collapseMs = performance.now() - t1;
    expect(m.rows).toHaveLength(100_000);
    // Generous bounds for CI machines; the budget is 2 ms on a dev box (§17).
    expect(expandMs).toBeLessThan(200);
    expect(collapseMs).toBeLessThan(100);
  });

  it('sticky ancestors are the scrolled-away parent chain, deepest kept', () => {
    const rows: Row[] = [
      { key: 'a', depth: 0, name: 'a', kind: 'd', flags: 0 },
      { key: 'a/b', depth: 1, name: 'b', kind: 'd', flags: 0 },
      { key: 'a/b/c', depth: 2, name: 'c', kind: 'd', flags: 0 },
      { key: 'a/b/c/1', depth: 3, name: '1', kind: 'f', flags: 0 },
      { key: 'a/b/c/2', depth: 3, name: '2', kind: 'f', flags: 0 },
    ];
    expect(stickyAncestors(rows, 4, 5)).toEqual([0, 1, 2]);
    expect(stickyAncestors(rows, 4, 2)).toEqual([1, 2]);
    expect(stickyAncestors(rows, 0, 5)).toEqual([]);
    expect(stickyAncestors(rows, 4, 0)).toEqual([]);
  });

  it('type-ahead finds the next match, skips message rows and wraps', () => {
    const rows: Row[] = ['alpha', 'beta', 'apple', 'Avocado'].map((n) => ({ key: n, depth: 0, name: n, kind: 'f' as const, flags: 0 }));
    rows.splice(1, 0, { key: '\u0000loading', depth: 0, name: 'ahh', kind: 'm', flags: 0, msg: 'loading' });
    expect(typeAheadMatch(rows, 0, 'a')).toBe(3);
    expect(typeAheadMatch(rows, 4, 'a')).toBe(0);
    expect(typeAheadMatch(rows, 3, 'ap')).toBe(3);
    expect(typeAheadMatch(rows, 0, 'zz')).toBe(-1);
  });

  it('ancestorsOf lists the folders above a path', () => {
    expect(ancestorsOf('a/b/c.ts')).toEqual(['a', 'a/b']);
    expect(ancestorsOf('top')).toEqual([]);
  });
});
