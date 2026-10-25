/** The gallery's pure helpers (docs/hive.md §10). Stored files reach the webview only through
 *  Rust's `gallery://` scheme, by asset id (hive/gallery/serve.rs): never a filesystem path. The
 *  view's layout (a virtualized masonry with group bands), its variant folding, its keys and its
 *  following rule live here, so they are tested without a DOM. */

import { isWindows } from '$lib/utils/platform';
import type { GalleryAssetRow, GalleryChip, GalleryListFilter, GalleryMadeBy, HiveWorkerCard } from '$lib/tauri/types';

/** What `/<hive>/<asset>[/part]` serves: the stored file, its poster, a video's hover strip
 *  (ten frames across), an audio file's waveform (white on transparent, for tinting). */
export type GalleryPart = 'file' | 'thumb' | 'strip' | 'wave';

/** An asset's URL. Windows serves a custom scheme as `http://<scheme>.localhost`. */
export function galleryUrl(hiveId: string, assetId: string, part: GalleryPart = 'file', windows = isWindows()): string {
  const base = windows ? 'http://gallery.localhost' : 'gallery://localhost';
  const tail = part === 'file' ? '' : `/${part}`;
  return `${base}/${encodeURIComponent(hiveId)}/${encodeURIComponent(assetId)}${tail}`;
}

// ── What a tile draws ──

type Pictured = Pick<GalleryAssetRow, 'thumb' | 'extras'>;
const flag = (r: Pictured, k: string) => r.thumb === 'ready' && r.extras?.[k] === true;

/** The picture a tile shows, as Rust's `view::picture`: the poster, else a waveform, else none
 *  (pending, failed, or no tool could make one: the kind's icon). */
export function picture(r: Pictured): 'thumb' | 'wave' | null {
  return flag(r, 'poster') ? 'thumb' : flag(r, 'wave') ? 'wave' : null;
}

/** A video's ten-frame hover strip exists. */
export const hasStrip = (r: Pictured) => flag(r, 'strip');

/** A row as a chip (a card's, a chat's): the same picture rule. */
export const chipOf = (r: GalleryAssetRow): GalleryChip => ({ id: r.id, kind: r.kind, title: r.title, picture: picture(r) });

/** The size a tile is laid out at: an image's or a video's own (until thumbnails read it, a
 *  square or 16:9), a waveform's strip, a document's page. */
export function tileSize(r: Pick<GalleryAssetRow, 'kind' | 'width' | 'height'>): { w: number; h: number } {
  if (r.width && r.height && r.kind !== 'audio') return { w: r.width, h: r.height };
  switch (r.kind) {
    case 'video':
      return { w: 16, h: 9 };
    case 'audio':
      return { w: 2, h: 1 };
    case 'document':
      return { w: 3, h: 4 };
    default:
      return { w: 1, h: 1 };
  }
}

export const KIND_TABS = [
  { key: '', label: 'All' },
  { key: 'image', label: 'Images' },
  { key: 'video', label: 'Video' },
  { key: 'audio', label: 'Audio' },
  { key: 'document', label: 'Docs' },
] as const;

/** The badge a document or 3D tile carries: the file's extension. */
export function typeBadge(file: string): string {
  const ext = file.includes('.') ? file.slice(file.lastIndexOf('.') + 1) : '';
  return ext.toUpperCase().slice(0, 5);
}

export function fmtDuration(ms: number | null | undefined): string {
  if (ms == null || !Number.isFinite(ms) || ms < 0) return '';
  const s = Math.round(ms / 1000);
  const h = Math.floor(s / 3600);
  const m = Math.floor((s % 3600) / 60);
  const ss = String(s % 60).padStart(2, '0');
  return h ? `${h}:${String(m).padStart(2, '0')}:${ss}` : `${m}:${ss}`;
}

export function fmtBytes(n: number): string {
  if (n < 1024) return `${n} B`;
  const units = ['KB', 'MB', 'GB'];
  let v = n / 1024;
  let i = 0;
  while (v >= 1024 && i < units.length - 1) {
    v /= 1024;
    i++;
  }
  return `${v >= 10 ? Math.round(v) : v.toFixed(1)} ${units[i]}`;
}

/** Who filed it, as the view says it: `#3 Hero`, `Coordinator`, `You`. */
export function madeByLabel(m: GalleryMadeBy): string {
  if (m.kind === 'worker') return `#${m.badge} ${m.worker_title}`;
  if (m.kind === 'coordinator') return 'Coordinator';
  if (m.kind === 'human') return 'You';
  return m.kind || 'Unknown';
}

/** §4.3: a card with assets shows them across its top in place of the diff stat line. */
export function cardTopRow(card: Pick<HiveWorkerCard, 'assets'>, diffText: string): 'assets' | 'diff' | null {
  if (card.assets?.length) return 'assets';
  return diffText ? 'diff' : null;
}

// ── The query: the filter rail, kind tabs and search, as Rust's `ListFilter` ──

export type GroupBy = 'none' | 'task' | 'worker' | 'day' | 'collection';
export const GROUPS: { key: GroupBy; label: string }[] = [
  { key: 'none', label: 'None' },
  { key: 'task', label: 'Task' },
  { key: 'worker', label: 'Worker' },
  { key: 'day', label: 'Day' },
  { key: 'collection', label: 'Collection' },
];

export interface GalleryQuery {
  kind: string;
  /** '' anyone; 'coordinator' | 'human'; or `task:<id>` for a worker. */
  madeBy: string;
  task: string;
  status: string;
  starred: boolean;
  tag: string;
  collection: string;
  search: string;
  trash: boolean;
}

export const defaultQuery = (): GalleryQuery => ({ kind: '', madeBy: '', task: '', status: '', starred: false, tag: '', collection: '', search: '', trash: false });

/** `status` '' is any; 'none' is assets with no status (Rust's ""). */
export function filterToQuery(q: GalleryQuery): GalleryListFilter {
  const f: GalleryListFilter = {};
  if (q.kind) f.kind = q.kind;
  if (q.madeBy.startsWith('task:')) {
    f.made_by = 'worker';
    f.task_id = q.madeBy.slice(5);
  } else if (q.madeBy) {
    f.made_by = q.madeBy;
  }
  if (q.task) {
    // A worker picked under Made by and another task here can match nothing: say so to Rust.
    f.task_id = f.task_id && f.task_id !== q.task ? '\u0000' : q.task;
  }
  if (q.status) f.status = q.status === 'none' ? '' : q.status;
  if (q.starred) f.starred = true;
  if (q.tag) f.tag = q.tag;
  if (q.collection) f.collection = q.collection;
  const s = q.search.trim();
  if (s) f.query = s.slice(0, 200);
  if (q.trash) f.include_trash = true;
  return f;
}

export const isFiltered = (q: GalleryQuery) => JSON.stringify({ ...q, kind: '' }) !== JSON.stringify({ ...defaultQuery() });

// ── Variants and groups ──

/** §10.4: a child whose parent is in the set folds under it unless "Show variants" is on. The
 *  parent keeps its own `children` count for "⧉ n variants". */
export function foldVariants(rows: GalleryAssetRow[], showVariants: boolean): GalleryAssetRow[] {
  if (showVariants) return rows;
  const here = new Set(rows.map((r) => r.id));
  return rows.filter((r) => !r.parents.some((p) => here.has(p)));
}

export type GridEntry = { kind: 'band'; key: string; label: string; count: number } | { kind: 'asset'; row: GalleryAssetRow };

export interface GroupNames {
  /** A task id's title (its worker's), if the board knows it. */
  task?: (taskId: string) => string | undefined;
  collection?: (id: string) => string | undefined;
}

function dayOf(r: GalleryAssetRow): string {
  const d = new Date(r.created_ms || Date.parse(r.created_at) || 0);
  return `${d.getFullYear()}-${String(d.getMonth() + 1).padStart(2, '0')}-${String(d.getDate()).padStart(2, '0')}`;
}

function groupOf(r: GalleryAssetRow, by: GroupBy, names: GroupNames): { key: string; label: string } {
  switch (by) {
    case 'task':
      return r.task_id ? { key: `t:${r.task_id}`, label: names.task?.(r.task_id) ?? r.task_id } : { key: 't:', label: 'No task' };
    case 'worker':
      return r.made_by.kind === 'worker' ? { key: `w:${r.task_id ?? r.made_by.tab_id}`, label: madeByLabel(r.made_by) } : { key: `w:${r.made_by.kind}`, label: madeByLabel(r.made_by) };
    case 'day': {
      const d = dayOf(r);
      return { key: `d:${d}`, label: d };
    }
    case 'collection': {
      // An asset in several collections shows under its first: a tile is in the grid once.
      const c = r.collections[0];
      return c ? { key: `c:${c}`, label: names.collection?.(c) ?? c } : { key: 'c:', label: 'Not in a collection' };
    }
    default:
      return { key: '', label: '' };
  }
}

/** The grid's entries: rows in order, with a full-width band opening each group. Groups keep the
 *  order their newest asset gives them (rows come newest first). */
export function groupRows(rows: GalleryAssetRow[], by: GroupBy, names: GroupNames = {}): GridEntry[] {
  if (by === 'none') return rows.map((row) => ({ kind: 'asset', row }));
  const groups = new Map<string, { label: string; rows: GalleryAssetRow[] }>();
  for (const r of rows) {
    const g = groupOf(r, by, names);
    const had = groups.get(g.key);
    if (had) had.rows.push(r);
    else groups.set(g.key, { label: g.label, rows: [r] });
  }
  const out: GridEntry[] = [];
  for (const [key, g] of groups) {
    out.push({ kind: 'band', key: `band:${key}`, label: g.label, count: g.rows.length });
    for (const row of g.rows) out.push({ kind: 'asset', row });
  }
  return out;
}

// ── Layout: a masonry that keeps aspect ratios, virtualized ──

export interface MasonryItem {
  id: string;
  /** Pixel size; a missing one reads as a square. */
  w?: number | null;
  h?: number | null;
  /** A full-width band of this height (a group header) instead of a tile. */
  band?: number;
}

export interface Rect {
  x: number;
  y: number;
  w: number;
  h: number;
}

export interface Masonry {
  pos: Map<string, Rect>;
  height: number;
  cols: number;
  colWidth: number;
}

/** A tall sliver or a panorama stays a usable tile. */
const ASPECT_MIN = 0.25;
const ASPECT_MAX = 4;

/** As many columns of about `colW` as fit `width`, stretched to fill it. Each tile goes into the
 *  shortest column (the leftmost of equals) at its own aspect ratio; a band starts below every
 *  column and spans them all. */
export function masonry(items: MasonryItem[], width: number, colW: number, gap: number): Masonry {
  const cols = Math.max(1, Math.floor((Math.max(width, 0) + gap) / (Math.max(colW, 1) + gap)));
  const colWidth = Math.max(1, (Math.max(width, 0) - gap * (cols - 1)) / cols);
  const tops = new Array<number>(cols).fill(0);
  const pos = new Map<string, Rect>();
  for (const it of items) {
    if (it.band !== undefined) {
      const y = Math.max(...tops);
      pos.set(it.id, { x: 0, y, w: width, h: it.band });
      tops.fill(y + it.band + gap);
      continue;
    }
    const ratio = it.w && it.h && it.w > 0 && it.h > 0 ? Math.min(ASPECT_MAX, Math.max(ASPECT_MIN, it.h / it.w)) : 1;
    let c = 0;
    for (let i = 1; i < cols; i++) if (tops[i] < tops[c]) c = i;
    const h = Math.round(colWidth * ratio);
    pos.set(it.id, { x: c * (colWidth + gap), y: tops[c], w: colWidth, h });
    tops[c] += h + gap;
  }
  const height = Math.max(0, Math.max(...tops) - (items.length ? gap : 0));
  return { pos, height, cols, colWidth };
}

/** The ids whose box meets the viewport, widened by `overscan` above and below. */
export function visible(pos: Map<string, Rect>, scrollTop: number, viewH: number, overscan: number): string[] {
  const top = scrollTop - overscan;
  const bottom = scrollTop + viewH + overscan;
  const out: string[] = [];
  for (const [id, r] of pos) if (r.y + r.h >= top && r.y <= bottom) out.push(id);
  return out;
}

export type Dir = 'left' | 'right' | 'up' | 'down';

/** Where an arrow goes from `id`: left and right through the order, up and down to the nearest
 *  tile above or below in its column. Stays put at an edge. */
export function neighbor(pos: Map<string, Rect>, order: readonly string[], id: string | null, dir: Dir): string | null {
  if (!order.length) return null;
  const i = id ? order.indexOf(id) : -1;
  if (i < 0) return order[0];
  if (dir === 'left') return order[Math.max(0, i - 1)];
  if (dir === 'right') return order[Math.min(order.length - 1, i + 1)];
  const me = pos.get(id!);
  if (!me) return id;
  const cx = me.x + me.w / 2;
  let best: string | null = null;
  let bestD = Infinity;
  for (const other of order) {
    const r = pos.get(other);
    if (!r || other === id || cx < r.x || cx > r.x + r.w) continue;
    const d = dir === 'down' ? r.y - me.y : me.y - r.y;
    if (d > 0 && d < bestD) {
      best = other;
      bestD = d;
    }
  }
  return best ?? id;
}

// ── Following (§10.4): on while agents work; scrolling away from the newest turns it off ──

/** Within this of the top counts as at the top. */
export const FOLLOW_SLACK = 24;

/** Like a terminal's scroll-lock: scrolled away from the newest (the top) stops following,
 *  back at the top follows again. In between, it stays as it was. */
export function followAfterScroll(following: boolean, scrollTop: number): boolean {
  if (scrollTop <= 2) return true;
  if (scrollTop > FOLLOW_SLACK) return false;
  return following;
}

/** What a scroll stores as the human's choice, or null for nothing. `chosen` is the stored one
 *  (null: the default, on while a worker is live), `following` the effective one. A scroll away
 *  is stored even when it matches the default, or a worker starting later would turn following
 *  back on and pull the grid to the top. */
export function followOnScroll(chosen: boolean | null, following: boolean, scrollTop: number): boolean | null {
  const next = followAfterScroll(following, scrollTop);
  if (next !== following) return next;
  if (chosen === null && scrollTop > FOLLOW_SLACK) return false;
  return null;
}

// ── Keys (§4.6, §10.4): bare keys, only inside `[data-gallery]` and never in a field ──

export type GalleryKeyAction =
  | { kind: 'move'; dir: Dir }
  | { kind: 'open' }
  | { kind: 'back' }
  | { kind: 'play' }
  | { kind: 'star' }
  | { kind: 'status'; status: 'picked' | 'final' | 'rejected' }
  | { kind: 'compare' };

type KeyFields = Pick<KeyboardEvent, 'key' | 'metaKey' | 'ctrlKey' | 'altKey'>;

/** A key's gallery action. `editable`: focus is in an input, a textarea or a composer, where no
 *  bare key is the gallery's. `onControl`: focus is on a button, link or player inside the
 *  gallery, whose own Enter and Space activate it. A modifier is never one: those are the app's. */
export function galleryKey(e: KeyFields, editable: boolean, onControl = false): GalleryKeyAction | null {
  if (editable || e.metaKey || e.ctrlKey || e.altKey) return null;
  if (onControl && (e.key === 'Enter' || e.key === ' ')) return null;
  switch (e.key) {
    case 'ArrowLeft':
      return { kind: 'move', dir: 'left' };
    case 'ArrowRight':
      return { kind: 'move', dir: 'right' };
    case 'ArrowUp':
      return { kind: 'move', dir: 'up' };
    case 'ArrowDown':
      return { kind: 'move', dir: 'down' };
    case 'Enter':
      return { kind: 'open' };
    case 'Escape':
      return { kind: 'back' };
    case ' ':
      return { kind: 'play' };
  }
  switch (e.key.toLowerCase()) {
    case 'f':
      return { kind: 'star' };
    case '1':
      return { kind: 'status', status: 'picked' };
    case '2':
      return { kind: 'status', status: 'final' };
    case '3':
      return { kind: 'status', status: 'rejected' };
    case 'c':
      return { kind: 'compare' };
  }
  return null;
}

// ── Opening an asset from elsewhere (a chat's inline strip, a card) ──

export const HIVE_OPEN_ASSET = 'hive-open-asset';

export interface HiveOpenAssetDetail {
  workspaceId: string;
  assetId: string;
}

/** HiveView switches to the gallery and opens the asset's detail. */
export function dispatchHiveOpenAsset(detail: HiveOpenAssetDetail): void {
  if (typeof window === 'undefined') return;
  window.dispatchEvent(new CustomEvent<HiveOpenAssetDetail>(HIVE_OPEN_ASSET, { detail }));
}
