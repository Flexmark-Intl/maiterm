import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { FLASH_MS, GalleryFeed, REFETCH_MS, realTimers, type FeedDeps } from './galleryFeed';
import type { GalleryAssetRow, GalleryListFilter } from '$lib/tauri/types';

const r = (id: string) => ({ id }) as GalleryAssetRow;

function rig(pages: GalleryAssetRow[][]) {
  const calls: GalleryListFilter[] = [];
  let changes = 0;
  const deps: FeedDeps = {
    list: vi.fn(async (_ws: string, f: GalleryListFilter) => {
      calls.push(f);
      return pages[Math.min(calls.length - 1, pages.length - 1)];
    }),
    ...realTimers(),
  };
  const feed = new GalleryFeed('ws-1', deps, () => changes++);
  return { feed, calls, changes: () => changes, deps };
}

describe('GalleryFeed', () => {
  beforeEach(() => vi.useFakeTimers());
  afterEach(() => vi.useRealTimers());

  it('fetches when shown, refetches once per burst of events, and flashes what arrived', async () => {
    const { feed, calls } = rig([[r('a')], [r('b'), r('a')]]);
    feed.setFilter({ kind: 'image' });
    expect(calls).toHaveLength(0);
    feed.setActive(true);
    await vi.runAllTicks();
    await Promise.resolve();
    expect(calls).toEqual([{ kind: 'image' }]);
    expect(feed.rows.map((x) => x.id)).toEqual(['a']);
    expect(feed.isFlashing('a')).toBe(false);

    for (let i = 0; i < 5; i++) expect(feed.onEvent({ workspace_id: 'ws-1' })).toBe(true);
    expect(feed.version).toBe(5);
    await vi.advanceTimersByTimeAsync(REFETCH_MS - 1);
    expect(calls).toHaveLength(1);
    await vi.advanceTimersByTimeAsync(1);
    expect(calls).toHaveLength(2);
    expect(feed.rows.map((x) => x.id)).toEqual(['b', 'a']);
    expect(feed.isFlashing('b')).toBe(true);
    expect(feed.isFlashing('a')).toBe(false);
    await vi.advanceTimersByTimeAsync(FLASH_MS);
    expect(feed.isFlashing('b')).toBe(false);
    feed.dispose();
  });

  it("ignores another workspace's event", async () => {
    const { feed, calls } = rig([[r('a')]]);
    feed.setActive(true);
    await vi.advanceTimersByTimeAsync(0);
    expect(feed.onEvent({ workspace_id: 'ws-2' })).toBe(false);
    await vi.advanceTimersByTimeAsync(REFETCH_MS * 2);
    expect(calls).toHaveLength(1);
    expect(feed.version).toBe(0);
    feed.dispose();
  });

  it('counts events while hidden and fetches once on showing, without flashing a new query', async () => {
    const { feed, calls } = rig([[r('a')], [r('b'), r('a')], [r('c')]]);
    feed.setActive(true);
    await vi.advanceTimersByTimeAsync(0);
    feed.setActive(false);
    feed.onEvent({ workspace_id: 'ws-1' });
    feed.onEvent({ workspace_id: 'ws-1' });
    await vi.advanceTimersByTimeAsync(REFETCH_MS * 2);
    expect(calls).toHaveLength(1);
    expect(feed.version).toBe(2);
    feed.setActive(true);
    await vi.advanceTimersByTimeAsync(0);
    expect(calls).toHaveLength(2);
    expect(feed.isFlashing('b')).toBe(true);
    feed.setFilter({ starred: true });
    await vi.advanceTimersByTimeAsync(0);
    expect(calls).toHaveLength(3);
    expect(feed.rows.map((x) => x.id)).toEqual(['c']);
    expect(feed.isFlashing('c')).toBe(false);
    feed.dispose();
  });

  it('keeps the newer answer when an older fetch lands late', async () => {
    let release: (rows: GalleryAssetRow[]) => void = () => {};
    const deps: FeedDeps = {
      list: vi.fn()
        .mockImplementationOnce(() => new Promise<GalleryAssetRow[]>((res) => (release = res)))
        .mockImplementationOnce(async () => [r('new')]),
      ...realTimers(),
    };
    const feed = new GalleryFeed('ws-1', deps, () => {});
    feed.setActive(true);
    feed.setFilter({ tag: 'hero' });
    await vi.advanceTimersByTimeAsync(0);
    release([r('old')]);
    await vi.advanceTimersByTimeAsync(0);
    expect(feed.rows.map((x) => x.id)).toEqual(['new']);
    feed.dispose();
  });
});
