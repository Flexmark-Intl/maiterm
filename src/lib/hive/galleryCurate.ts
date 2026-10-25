/** The gallery's curation, pure (docs/hive.md §10.4, step 8e): selection by click, Cmd-click and
 *  Shift-click; what the bulk star and status keys do; Iterate's words; the compare view's sync
 *  of its players and its audio A/B; and how an asset dragged onto a composer finds its chat. */

import type { GalleryRecipe, GalleryTagChange } from '$lib/tauri/types';

/** What the gallery's surfaces (grid, detail, compare) ask its view to do with assets. The menus
 *  and panels (a collection to add to, a worker to send to, Export…'s folder) open from `anchor`. */
export interface CurateActions {
  tag(ids: string[], change: GalleryTagChange): void;
  pickCollection(ids: string[], anchor: HTMLElement): void;
  iterate(id: string): void;
  pickWorker(ids: string[], anchor: HTMLElement): void;
  exportAssets(ids: string[]): void;
  trash(ids: string[]): void;
  restore(ids: string[]): void;
}

// ── Selection ──

export type SelectMode = 'only' | 'toggle' | 'range';

/** A click's mode: Cmd/Ctrl toggles one, Shift extends from the cursor, a plain click selects
 *  one. */
export function selectMode(e: Pick<MouseEvent, 'metaKey' | 'ctrlKey' | 'shiftKey'>): SelectMode {
  if (e.shiftKey) return 'range';
  if (e.metaKey || e.ctrlKey) return 'toggle';
  return 'only';
}

/** The selection after clicking `id`, ordered, the last being the keyboard's cursor. A range
 *  runs through `order` (the grid's order) from the cursor to `id` and adds to what was there;
 *  a cursor not in the order starts the range at `id`. */
export function selectWith(order: readonly string[], selection: readonly string[], id: string, mode: SelectMode): string[] {
  if (mode === 'only') return [id];
  if (mode === 'toggle') return selection.includes(id) ? selection.filter((s) => s !== id) : [...selection, id];
  const cursor = selection.at(-1);
  const from = cursor ? order.indexOf(cursor) : -1;
  const to = order.indexOf(id);
  if (from < 0 || to < 0) return selection.includes(id) ? [...selection.filter((s) => s !== id), id] : [...selection, id];
  const step = from <= to ? 1 : -1;
  const span: string[] = [];
  for (let i = from; i !== to + step; i += step) span.push(order[i]);
  // The range ends on `id`, the new cursor.
  const kept = selection.filter((s) => !span.includes(s));
  return [...kept, ...span];
}

/** F on the selection: all starred unstars them, otherwise stars them all. */
export function nextStar(rows: readonly { starred: boolean }[]): boolean {
  return !(rows.length > 0 && rows.every((r) => r.starred));
}

/** 1/2/3 on the selection: every one already that status clears it, otherwise sets it. */
export function nextStatus(rows: readonly { status: string }[], status: string): string {
  return rows.length > 0 && rows.every((r) => r.status === status) ? '' : status;
}

// ── Iterate (§10.4): the asset on the coordinator's composer with its recipe quoted ──

export interface IterateSource {
  title: string;
  kind: string;
  recipe?: Pick<GalleryRecipe, 'tool' | 'model' | 'seed' | 'prompt'> | null;
}

/** `Iterate on "<title>" (<kind>, <model> via <tool>, seed <n>):` then the prompt as `> ` lines,
 *  ready for "three more like this, warmer light". */
export function iterateText(a: IterateSource): string {
  const r = a.recipe;
  const made = r ? (r.model ? `${r.model} via ${r.tool}` : r.tool) : '';
  const facts = [a.kind, made, r?.seed != null ? `seed ${r.seed}` : ''].filter(Boolean).join(', ');
  const title = a.title.replace(/\s+/g, ' ').trim();
  let out = `Iterate on "${title}"${facts ? ` (${facts})` : ''}:`;
  const prompt = r?.prompt?.trim();
  if (prompt) out += '\n' + prompt.split(/\r?\n/).map((l) => `> ${l}`).join('\n');
  return out;
}

/** Text into a composer's draft: an empty draft takes it, one with text gets it on a new line. */
export function appendToDraft(draft: string, text: string): string {
  if (!text) return draft;
  if (!draft.trim()) return text;
  return `${draft.replace(/\s+$/, '')}\n${text}`;
}

// ── Compare (§10.4): videos play in sync, audio is A/B switched ──

/** Past this a follower is sought back to the leader. */
export const DRIFT_S = 0.1;

export interface PlayerState {
  paused: boolean;
  currentTime: number;
}

export type SyncEvent = { kind: 'play' | 'pause' | 'seek' | 'tick'; time: number };

export interface SyncAction {
  index: number;
  play?: boolean;
  seek?: number;
}

/** What the other players do when the leader (the pane the human last touched) plays, pauses,
 *  seeks or ticks. Only the leader's events count: the followers' own echo of being driven is
 *  ignored, or each correction would bounce between the players. */
export function syncActions(leader: number, from: number, ev: SyncEvent, players: readonly PlayerState[]): SyncAction[] {
  if (from !== leader) return [];
  const out: SyncAction[] = [];
  players.forEach((p, i) => {
    if (i === leader) return;
    const a: SyncAction = { index: i };
    const drift = Math.abs(p.currentTime - ev.time) > DRIFT_S;
    switch (ev.kind) {
      case 'play':
        if (p.paused) a.play = true;
        if (drift) a.seek = ev.time;
        break;
      case 'pause':
        if (!p.paused) a.play = false;
        if (drift) a.seek = ev.time;
        break;
      case 'seek':
        if (drift) a.seek = ev.time;
        break;
      case 'tick':
        if (drift) a.seek = ev.time;
        break;
    }
    if (a.play !== undefined || a.seek !== undefined) out.push(a);
  });
  return out;
}

/** X in an audio comparison: the next pane plays from where the playing one is, which stops. */
export function abSwitch(active: number, count: number, time: number): { stop: number; play: number; at: number } | null {
  if (count < 2) return null;
  const from = active >= 0 && active < count ? active : 0;
  return { stop: from, play: (from + 1) % count, at: time };
}

export const COMPARE_MIN = 2;
export const COMPARE_MAX = 4;

/** Compare takes two to four assets: the selection's last four, in the order picked. */
export function compareSet(selection: readonly string[]): string[] | null {
  if (selection.length < COMPARE_MIN) return null;
  return selection.slice(-COMPARE_MAX);
}

// ── A drag onto a composer (§10.4): pointer events, never `draggable` ──

export const GALLERY_ATTACH = 'hive-gallery-attach';

export interface GalleryAttachDetail {
  /** The chat the drop landed on: its composer's `data-composer-drop`. */
  chatTabId: string;
  paths: string[];
}

export function dispatchGalleryAttach(detail: GalleryAttachDetail): void {
  if (typeof window === 'undefined') return;
  window.dispatchEvent(new CustomEvent<GalleryAttachDetail>(GALLERY_ATTACH, { detail }));
}

/** The paths a chat takes from a `GALLERY_ATTACH`: only one dropped on its own composer, and
 *  only the absolute paths in it. */
export function attachFor(detail: GalleryAttachDetail | null | undefined, openId: string | null): string[] | null {
  if (!detail || !openId || detail.chatTabId !== openId || !Array.isArray(detail.paths)) return null;
  const paths = detail.paths.filter((p) => typeof p === 'string' && (p.startsWith('/') || /^[A-Za-z]:[\\/]/.test(p)));
  return paths.length ? paths : null;
}

/** Past this many pixels a press on a tile is a drag, not a click. */
export const DRAG_THRESHOLD = 5;
