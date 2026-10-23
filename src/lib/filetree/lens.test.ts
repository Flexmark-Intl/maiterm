import { describe, expect, it } from 'vitest';
import type { GitUpdate, TreeEntry } from '$lib/tauri/types';
import { TREE_FLAG_IGNORED } from '$lib/tauri/types';
import { TreeModel, highlightRuns, matchHighlights, prunedRows } from './model';

const update = (u: Partial<GitUpdate>): GitUpdate => ({ gen: 1, full: false, files: [], dirs: [], del: [], branch: null, truncated: false, count: 0, ...u });

describe('git decorations on the model', () => {
  it('a full update replaces, a partial one drops del from both maps before setting', () => {
    const m = new TreeModel();
    m.applyGit(update({ full: true, files: [['src/a.ts', 'M'], ['build/', '?']], dirs: [['src', 'M']] }));
    expect(m.gitOf('src/a.ts', false)).toEqual({ letter: 'M', rollup: false });
    expect(m.gitOf('src', true)).toEqual({ letter: 'M', rollup: true });
    // An untracked folder marks itself and everything in it.
    expect(m.gitOf('build', true)).toEqual({ letter: '?', rollup: false });
    expect(m.gitOf('build/x/y.js', false)).toEqual({ letter: '?', rollup: false });
    expect(m.gitOf('README.md', false)).toBeNull();

    // build/ gains a staged file: its key moves from files to dirs.
    m.applyGit(update({ del: ['build/', 'src/a.ts', 'src'], files: [['build/new.js', 'A']], dirs: [['build', 'A']] }));
    expect(m.gitOf('build', true)).toEqual({ letter: 'A', rollup: true });
    expect(m.gitOf('build/x/y.js', false)).toBeNull();
    expect(m.gitOf('src/a.ts', false)).toBeNull();
    expect(m.gitOf('src', true)).toBeNull();

    m.applyGit(update({ full: true }));
    expect(m.gitFiles.size + m.gitDirs.size).toBe(0);
  });

  it('only an untracked folder (a key ending in /) marks what is inside it', () => {
    const m = new TreeModel();
    m.applyGit(update({ full: true, files: [['lib', '?']] }));
    expect(m.gitOf('lib', false)).toEqual({ letter: '?', rollup: false });
    expect(m.gitOf('lib/inner.ts', false)).toBeNull();
  });

  it('the Changed lens lists files and untracked folders', () => {
    const m = new TreeModel();
    m.applyGit(update({ full: true, files: [['src/a.ts', 'M'], ['out/', '?']] }));
    expect(m.changedItems().sort((a, b) => a.rel.localeCompare(b.rel))).toEqual([
      { rel: 'out', kind: 'd' },
      { rel: 'src/a.ts', kind: 'f' },
    ]);
  });
});

describe('prunedRows', () => {
  const none = () => undefined;

  it('draws each path with its folders, folders first, all open', () => {
    const rows = prunedRows(
      [
        { rel: 'src/lib/z.ts', kind: 'f' },
        { rel: 'README.md', kind: 'f' },
        { rel: 'src/a.ts', kind: 'f' },
        { rel: 'docs/x.md', kind: 'f' },
      ],
      new Set(),
      none,
      { showHidden: true, emptyText: 'No matches' },
    );
    expect(rows.map((r) => [r.key, r.depth, r.kind])).toEqual([
      ['docs', 0, 'd'],
      ['docs/x.md', 1, 'f'],
      ['src', 0, 'd'],
      ['src/lib', 1, 'd'],
      ['src/lib/z.ts', 2, 'f'],
      ['src/a.ts', 1, 'f'],
      ['README.md', 0, 'f'],
    ]);
  });

  it('a closed folder hides its subtree; known entries lend their kind and flags', () => {
    const entries = new Map<string, TreeEntry>([['src', { n: 'src', k: 'L', f: 0 }], ['src/gen.ts', { n: 'gen.ts', k: 'f', f: TREE_FLAG_IGNORED }]]);
    const rows = prunedRows([{ rel: 'src/gen.ts', kind: 'f' }, { rel: 'a.ts', kind: 'f' }], new Set(['src']), (r) => entries.get(r), { showHidden: true, emptyText: '' });
    expect(rows.map((r) => r.key)).toEqual(['src', 'a.ts']);
    expect(rows[0].kind).toBe('L');
    const open = prunedRows([{ rel: 'src/gen.ts', kind: 'f' }], new Set(), (r) => entries.get(r), { showHidden: true, emptyText: '' });
    expect(open[1].flags).toBe(TREE_FLAG_IGNORED);
  });

  it('hidden paths drop out with hidden files off, and nothing left says so', () => {
    const rows = prunedRows([{ rel: '.env', kind: 'f' }, { rel: '.github/ci.yml', kind: 'f' }], new Set(), none, { showHidden: false, emptyText: 'No matches' });
    expect(rows).toEqual([{ key: '\u0000empty', depth: 0, name: 'No matches', kind: 'm', flags: 0, msg: 'empty' }]);
  });

  it('an item that is also a folder above another item stays a folder', () => {
    const rows = prunedRows([{ rel: 'a', kind: 'f' }, { rel: 'a/b.ts', kind: 'f' }], new Set(), none, { showHidden: true, emptyText: '' });
    expect(rows.map((r) => [r.key, r.kind])).toEqual([['a', 'd'], ['a/b.ts', 'f']]);
  });
});

describe('find highlights', () => {
  it('maps matched characters onto each row of the path', () => {
    // "src/lib/model.ts" matching "slm": s(0) in src, l(4) in lib, m(8) in model.ts.
    const h = matchHighlights([{ rel: 'src/lib/model.ts', idx: [0, 4, 8] }]);
    expect(h.get('src')).toEqual([0]);
    expect(h.get('src/lib')).toEqual([0]);
    expect(h.get('src/lib/model.ts')).toEqual([0]);
  });

  it('counts code points, so an emoji or accent before the match does not shift it', () => {
    const rel = 'café/\u{1F600}x.ts';
    // Code points: c a f é / 😀 x . t s → x is 6.
    const h = matchHighlights([{ rel, idx: [6] }]);
    expect(h.get(rel)).toEqual([1]);
    expect(highlightRuns('\u{1F600}x.ts', h.get(rel))).toEqual([
      { text: '\u{1F600}', hit: false },
      { text: 'x', hit: true },
      { text: '.ts', hit: false },
    ]);
  });

  it('a folder shared by two hits lights the letters of both', () => {
    const h = matchHighlights([{ rel: 'abc/x', idx: [0] }, { rel: 'abc/y', idx: [2] }]);
    expect(h.get('abc')).toEqual([0, 2]);
  });

  it('no marks is one plain run', () => {
    expect(highlightRuns('name', undefined)).toEqual([{ text: 'name', hit: false }]);
  });
});
