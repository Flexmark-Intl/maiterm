/** The gallery's rows for one Hive (docs/hive.md §10.4): fetched when the view shows, refetched
 *  (debounced) when its window hears `hive-gallery-changed`, with the assets that arrived flashed
 *  for a moment. Pure of Svelte and Tauri, so it is tested as it runs; `stores/hiveGallery`
 *  wraps it. Never polls: a change no event reports isn't one the view needs. */

import type { GalleryAssetRow, GalleryChangedEvent, GalleryListFilter } from '$lib/tauri/types';

/** Agents file in bursts (a call's 20 items, a thumbnailer finishing each): one fetch per burst. */
export const REFETCH_MS = 150;
/** How long a new tile is highlighted. */
export const FLASH_MS = 1200;

export interface FeedDeps {
  list(workspaceId: string, filter: GalleryListFilter): Promise<GalleryAssetRow[]>;
  now(): number;
  setTimer(fn: () => void, ms: number): unknown;
  clearTimer(t: unknown): void;
  /** A fetch that failed. */
  onError?(e: unknown): void;
}

export const realTimers = (): Pick<FeedDeps, 'now' | 'setTimer' | 'clearTimer'> => ({
  now: () => Date.now(),
  setTimer: (fn, ms) => setTimeout(fn, ms),
  clearTimer: (t) => clearTimeout(t as ReturnType<typeof setTimeout>),
});

export class GalleryFeed {
  rows: GalleryAssetRow[] = [];
  loaded = false;
  /** Bumped by every event for this Hive, shown or not: the board's cards refetch on it. */
  version = 0;
  private flash = new Map<string, number>();
  private filter: GalleryListFilter = {};
  private active = false;
  private stale = true;
  private seq = 0;
  private debounce: unknown = null;
  private unflash: unknown = null;
  private gone = false;

  constructor(
    readonly workspaceId: string,
    private deps: FeedDeps,
    private onChange: () => void,
  ) {}

  /** The view is on screen: fetch now if anything changed while it wasn't. */
  setActive(on: boolean) {
    this.active = on;
    if (on && this.stale) void this.refetch();
  }

  /** A new query: fetched at once (when shown), never flashing what it brings in. */
  setFilter(f: GalleryListFilter) {
    this.filter = f;
    this.stale = true;
    this.loaded = false;
    if (this.active) void this.refetch(false);
  }

  /** `hive-gallery-changed`. False (and nothing done) when it was another workspace's. */
  onEvent(p: Pick<GalleryChangedEvent, 'workspace_id'>): boolean {
    if (this.gone || p.workspace_id !== this.workspaceId) return false;
    this.version++;
    this.stale = true;
    if (this.active) {
      if (this.debounce !== null) this.deps.clearTimer(this.debounce);
      this.debounce = this.deps.setTimer(() => {
        this.debounce = null;
        void this.refetch();
      }, REFETCH_MS);
    }
    this.onChange();
    return true;
  }

  isFlashing(id: string): boolean {
    const until = this.flash.get(id);
    return until !== undefined && until > this.deps.now();
  }

  async refetch(flashNew = true): Promise<void> {
    const mine = ++this.seq;
    this.stale = false;
    let rows: GalleryAssetRow[];
    try {
      rows = await this.deps.list(this.workspaceId, this.filter);
    } catch (e) {
      if (mine === this.seq && !this.gone) {
        this.stale = true;
        this.deps.onError?.(e);
      }
      return;
    }
    // An older fetch answering late never replaces a newer one's rows.
    if (mine !== this.seq || this.gone) return;
    const known = new Set(this.rows.map((r) => r.id));
    const fresh = flashNew && this.loaded ? rows.filter((r) => !known.has(r.id)).map((r) => r.id) : [];
    this.rows = rows;
    this.loaded = true;
    if (fresh.length) {
      const until = this.deps.now() + FLASH_MS;
      for (const id of fresh) this.flash.set(id, until);
      if (this.unflash !== null) this.deps.clearTimer(this.unflash);
      this.unflash = this.deps.setTimer(() => {
        this.unflash = null;
        const now = this.deps.now();
        for (const [id, t] of this.flash) if (t <= now) this.flash.delete(id);
        this.onChange();
      }, FLASH_MS);
    }
    this.onChange();
  }

  dispose() {
    this.gone = true;
    if (this.debounce !== null) this.deps.clearTimer(this.debounce);
    if (this.unflash !== null) this.deps.clearTimer(this.unflash);
  }
}
