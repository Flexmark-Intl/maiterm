import { describe, expect, it } from 'vitest';
import {
  cardTopRow,
  defaultQuery,
  filterToQuery,
  fmtDuration,
  foldVariants,
  followAfterScroll,
  followOnScroll,
  galleryKey,
  galleryUrl,
  groupRows,
  masonry,
  neighbor,
  picture,
  tileSize,
  visible,
} from './gallery';
import type { GalleryAssetRow } from '$lib/tauri/types';

describe('galleryUrl', () => {
  const hive = '6f1c2e7a-3b0d-4c55-9a1e-2f7d8c9b0a11';
  it('uses the custom scheme on macOS and Linux', () => {
    expect(galleryUrl(hive, 'a-0123456789ab', 'file', false)).toBe(`gallery://localhost/${hive}/a-0123456789ab`);
    expect(galleryUrl(hive, 'a-0123456789ab', 'thumb', false)).toBe(`gallery://localhost/${hive}/a-0123456789ab/thumb`);
  });

  it("uses Tauri's http form on Windows", () => {
    expect(galleryUrl(hive, 'a-0123456789ab', 'strip', true)).toBe(`http://gallery.localhost/${hive}/a-0123456789ab/strip`);
    expect(galleryUrl(hive, 'a-0123456789ab', undefined, true)).toBe(`http://gallery.localhost/${hive}/a-0123456789ab`);
  });

  it('escapes, so an id can never add a component', () => {
    expect(galleryUrl('../x', 'a/b', 'wave', false)).toBe('gallery://localhost/..%2Fx/a%2Fb/wave');
  });
});

let seq = 0;
function row(over: Partial<GalleryAssetRow> = {}): GalleryAssetRow {
  seq++;
  return {
    id: `a-${String(seq).padStart(12, '0')}`,
    kind: 'image',
    file: 'x.png',
    mime: 'image/png',
    bytes: 1,
    title: `asset ${seq}`,
    made_by: { kind: 'worker', tab_id: 't', role: 'image', worker_title: 'Hero', badge: 1 },
    task_id: 't-hero',
    cost_micros: 0,
    parents: [],
    children: 0,
    status: '',
    starred: false,
    tags: [],
    collections: [],
    thumb: 'pending',
    extras: {},
    created_at: '2026-10-24T10:00:00Z',
    created_ms: Date.UTC(2026, 9, 24, 10),
    ...over,
  };
}

describe('masonry', () => {
  it('keeps each aspect ratio and packs into the shortest column', () => {
    // Two 100 px columns with a 10 px gap.
    const m = masonry(
      [
        { id: 'square', w: 500, h: 500 },
        { id: 'wide', w: 400, h: 200 },
        { id: 'next', w: 100, h: 300 },
        { id: 'unknown' },
      ],
      210,
      100,
      10,
    );
    expect(m.cols).toBe(2);
    expect(m.pos.get('square')).toEqual({ x: 0, y: 0, w: 100, h: 100 });
    expect(m.pos.get('wide')).toEqual({ x: 110, y: 0, w: 100, h: 50 });
    // The right column is shorter (50 + gap), so the next tile goes there.
    expect(m.pos.get('next')).toEqual({ x: 110, y: 60, w: 100, h: 300 });
    // A missing size reads as a square, in the now shorter left column.
    expect(m.pos.get('unknown')).toEqual({ x: 0, y: 110, w: 100, h: 100 });
    expect(m.height).toBe(360);
  });

  it('stretches columns to the width, clamps slivers, and starts a band below every column', () => {
    const m = masonry([{ id: 'a', w: 10, h: 1000 }, { id: 'b' }, { id: 'band', band: 20 }, { id: 'c' }], 300, 140, 10);
    expect(m.cols).toBe(2);
    expect(m.colWidth).toBe(145);
    expect(m.pos.get('a')!.h).toBe(145 * 4);
    expect(m.pos.get('band')).toEqual({ x: 0, y: 590, w: 300, h: 20 });
    expect(m.pos.get('c')).toEqual({ x: 0, y: 620, w: 145, h: 145 });
    expect(masonry([], 300, 140, 10).height).toBe(0);
    expect(masonry([{ id: 'x' }], 50, 140, 10).cols).toBe(1);
  });

  it('sizes tiles that have no dimensions yet by their kind', () => {
    expect(tileSize({ kind: 'video', width: null, height: null })).toEqual({ w: 16, h: 9 });
    expect(tileSize({ kind: 'audio', width: 10, height: 10 })).toEqual({ w: 2, h: 1 });
    expect(tileSize({ kind: 'image', width: 30, height: 20 })).toEqual({ w: 30, h: 20 });
    expect(tileSize({ kind: 'document' })).toEqual({ w: 3, h: 4 });
  });
});

describe('visible', () => {
  const pos = new Map([
    ['top', { x: 0, y: 0, w: 10, h: 100 }],
    ['mid', { x: 0, y: 500, w: 10, h: 100 }],
    ['far', { x: 0, y: 2000, w: 10, h: 100 }],
  ]);
  it('takes what meets the viewport, widened by the overscan', () => {
    expect(visible(pos, 0, 300, 0)).toEqual(['top']);
    expect(visible(pos, 0, 300, 250)).toEqual(['top', 'mid']);
    expect(visible(pos, 1700, 300, 0)).toEqual(['far']);
    expect(visible(pos, 1000, 300, 0)).toEqual([]);
  });
});

describe('neighbor', () => {
  const m = masonry([{ id: 'a' }, { id: 'b' }, { id: 'c' }, { id: 'd' }], 210, 100, 10);
  const order = ['a', 'b', 'c', 'd'];
  it('goes through the order sideways and stays in the column up and down', () => {
    expect(neighbor(m.pos, order, 'b', 'right')).toBe('c');
    expect(neighbor(m.pos, order, 'a', 'left')).toBe('a');
    expect(neighbor(m.pos, order, 'a', 'down')).toBe('c');
    expect(neighbor(m.pos, order, 'd', 'up')).toBe('b');
    expect(neighbor(m.pos, order, 'b', 'up')).toBe('b');
    expect(neighbor(m.pos, order, null, 'down')).toBe('a');
    expect(neighbor(m.pos, [], null, 'down')).toBeNull();
  });
});

describe('groupRows', () => {
  const a = row({ task_id: 't-hero', collections: ['c-1', 'c-2'], created_ms: Date.UTC(2026, 9, 24, 12) });
  const b = row({ task_id: 't-logo', made_by: { kind: 'worker', tab_id: 'x', role: 'image', worker_title: 'Logo', badge: 2 }, created_ms: Date.UTC(2026, 9, 23, 12) });
  const c = row({ task_id: null, made_by: { kind: 'human', tab_id: '', role: '', worker_title: '', badge: 0 }, created_ms: Date.UTC(2026, 9, 24, 9) });
  const d = row({ task_id: 't-hero', created_ms: Date.UTC(2026, 9, 22, 12) });
  const rows = [a, b, c, d];
  const bands = (by: Parameters<typeof groupRows>[1]) =>
    groupRows(rows, by, { task: (t) => ({ 't-hero': 'Hero shots' })[t], collection: (id) => ({ 'c-1': 'Shortlist' })[id] })
      .filter((e) => e.kind === 'band')
      .map((e) => (e.kind === 'band' ? `${e.label}:${e.count}` : ''));

  it('is the rows as they are without grouping', () => {
    expect(groupRows(rows, 'none').map((e) => (e.kind === 'asset' ? e.row.id : 'band'))).toEqual(rows.map((r) => r.id));
  });

  it('bands each group in the order its newest asset gives it', () => {
    expect(bands('task')).toEqual(['Hero shots:2', 't-logo:1', 'No task:1']);
    expect(bands('worker')).toEqual(['#1 Hero:2', '#2 Logo:1', 'You:1']);
    expect(bands('collection')).toEqual(['Shortlist:1', 'Not in a collection:3']);
    expect(bands('day')).toHaveLength(3);
    // Each asset appears once, under its group's band.
    const ids = groupRows(rows, 'task').flatMap((e) => (e.kind === 'asset' ? [e.row.id] : []));
    expect(ids).toEqual([a.id, d.id, b.id, c.id]);
  });
});

describe('foldVariants', () => {
  const parent = row({ children: 2 });
  const v1 = row({ parents: [parent.id] });
  const v2 = row({ parents: [parent.id] });
  const grand = row({ parents: [v1.id] });
  const orphan = row({ parents: ['a-ffffffffffff'] });
  it('hides children whose parent is listed and keeps the parent with its count', () => {
    const folded = foldVariants([grand, v2, v1, parent, orphan], false);
    expect(folded.map((r) => r.id)).toEqual([parent.id, orphan.id]);
    expect(folded[0].children).toBe(2);
    expect(foldVariants([grand, v2, v1, parent], true)).toHaveLength(4);
  });
});

describe('the query', () => {
  it('maps the rail to what Rust filters by', () => {
    expect(filterToQuery(defaultQuery())).toEqual({});
    expect(filterToQuery({ ...defaultQuery(), kind: 'video', madeBy: 'task:t-1', status: 'none', starred: true, search: '  warm  ', trash: true })).toEqual({
      kind: 'video',
      made_by: 'worker',
      task_id: 't-1',
      status: '',
      starred: true,
      query: 'warm',
      include_trash: true,
    });
    expect(filterToQuery({ ...defaultQuery(), madeBy: 'human' }).made_by).toBe('human');
    // A worker under Made by and another task can match nothing.
    expect(filterToQuery({ ...defaultQuery(), madeBy: 'task:t-1', task: 't-2' }).task_id).not.toMatch(/^t-/);
    expect(filterToQuery({ ...defaultQuery(), madeBy: 'task:t-1', task: 't-1' }).task_id).toBe('t-1');
  });
});

describe('galleryKey', () => {
  const k = (key: string, mods: Partial<{ metaKey: boolean; ctrlKey: boolean; altKey: boolean }> = {}) => ({ key, metaKey: false, ctrlKey: false, altKey: false, ...mods });
  it('maps the bare keys', () => {
    expect(galleryKey(k('ArrowLeft'), false)).toEqual({ kind: 'move', dir: 'left' });
    expect(galleryKey(k('ArrowDown'), false)).toEqual({ kind: 'move', dir: 'down' });
    expect(galleryKey(k('Enter'), false)).toEqual({ kind: 'open' });
    expect(galleryKey(k('Escape'), false)).toEqual({ kind: 'back' });
    expect(galleryKey(k(' '), false)).toEqual({ kind: 'play' });
    expect(galleryKey(k('F'), false)).toEqual({ kind: 'star' });
    expect(galleryKey(k('2'), false)).toEqual({ kind: 'status', status: 'final' });
    expect(galleryKey(k('c'), false)).toEqual({ kind: 'compare' });
    expect(galleryKey(k('x'), false)).toBeNull();
  });

  it('never acts in a field or with a modifier', () => {
    expect(galleryKey(k('f'), true)).toBeNull();
    expect(galleryKey(k('ArrowLeft'), true)).toBeNull();
    expect(galleryKey(k('1', { metaKey: true }), false)).toBeNull();
    expect(galleryKey(k('c', { ctrlKey: true }), false)).toBeNull();
    expect(galleryKey(k('f', { altKey: true }), false)).toBeNull();
  });

  it("leaves Enter and Space to a focused button, link or player, but still moves and goes back", () => {
    expect(galleryKey(k('Enter'), false, true)).toBeNull();
    expect(galleryKey(k(' '), false, true)).toBeNull();
    expect(galleryKey(k('ArrowRight'), false, true)).toEqual({ kind: 'move', dir: 'right' });
    expect(galleryKey(k('Escape'), false, true)).toEqual({ kind: 'back' });
  });
});

describe('following', () => {
  it('turns off when scrolled away from the newest and back on at the top', () => {
    expect(followAfterScroll(true, 400)).toBe(false);
    expect(followAfterScroll(false, 0)).toBe(true);
    // A nudge inside the slack changes nothing either way.
    expect(followAfterScroll(true, 10)).toBe(true);
    expect(followAfterScroll(false, 10)).toBe(false);
  });

  it("stores a scroll away even when it matches the default, so a worker starting doesn't pull the grid back", () => {
    // No worker live: the default is off, the human scrolls down: their choice is stored.
    expect(followOnScroll(null, false, 800)).toBe(false);
    // Once stored, the same scroll stores nothing more.
    expect(followOnScroll(false, false, 900)).toBeNull();
    // Following by default and scrolled away: off.
    expect(followOnScroll(null, true, 800)).toBe(false);
    // Back at the top: on.
    expect(followOnScroll(false, false, 0)).toBe(true);
    // A nudge inside the slack stores nothing.
    expect(followOnScroll(null, true, 10)).toBeNull();
    expect(followOnScroll(null, false, 10)).toBeNull();
  });
});

describe('pictures and cards', () => {
  it('shows the poster, else the waveform, only once thumbnails are ready', () => {
    expect(picture({ thumb: 'ready', extras: { poster: true, wave: true } })).toBe('thumb');
    expect(picture({ thumb: 'ready', extras: { poster: false, wave: true } })).toBe('wave');
    expect(picture({ thumb: 'pending', extras: { poster: true } })).toBeNull();
    expect(picture({ thumb: 'ready', extras: {} })).toBeNull();
  });

  it('puts a card’s assets in place of its diff line', () => {
    expect(cardTopRow({ assets: [{ id: 'a-1', kind: 'image', title: 'x', picture: 'thumb' }] }, '+3 −1')).toBe('assets');
    expect(cardTopRow({ assets: [] }, '+3 −1')).toBe('diff');
    expect(cardTopRow({}, '')).toBeNull();
  });

  it('formats durations', () => {
    expect(fmtDuration(12_000)).toBe('0:12');
    expect(fmtDuration(124_400)).toBe('2:04');
    expect(fmtDuration(3_723_000)).toBe('1:02:03');
    expect(fmtDuration(null)).toBe('');
  });
});
