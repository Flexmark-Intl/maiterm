<!--
  The gallery's grid (docs/hive.md §10.4): a masonry that keeps each asset's aspect ratio, with a
  full-width band opening each group, virtualized: only the tiles near the viewport mount, since
  a media Hive reaches thousands of assets. A ResizeObserver lays it out again when the width
  changes, and the size slider changes the column width.

  Following: newest first, so following keeps the top in view as assets arrive. Scrolling away
  from the top turns it off and back at the top turns it on (`followOnScroll`), as a
  terminal's scroll-lock does.
-->
<script lang="ts">
  import { untrack } from 'svelte';
  import GalleryTile from './GalleryTile.svelte';
  import Tooltip from '$lib/components/Tooltip.svelte';
  import { followOnScroll, masonry, tileSize, visible, type GridEntry, type MasonryItem, type Rect } from '$lib/hive/gallery';

  interface Props {
    hiveId: string;
    entries: GridEntry[];
    /** Column width, px (the size slider). */
    size: number;
    selection: string[];
    following: boolean;
    /** The human's stored choice; null while the default (on while agents work) holds. */
    followChosen: boolean | null;
    isFlashing: (id: string) => boolean;
    /** The coordinator's finals recommendations waiting on the human: asset id → status. */
    recommendations: Record<string, string>;
    onfollowing: (on: boolean) => void;
    onselect: (id: string, e: MouseEvent) => void;
    /** A tile's check circle: add or remove it. */
    ontoggle: (id: string) => void;
    onopen: (id: string) => void;
  }
  let { hiveId, entries, size, selection, following, followChosen, isFlashing, recommendations, onfollowing, onselect, ontoggle, onopen }: Props = $props();

  const GAP = 8;
  const PAD = 12;
  const BAND = 28;

  let scroller = $state<HTMLElement | undefined>();
  let width = $state(0);
  let viewH = $state(0);
  let scrollTop = $state(0);
  $effect(() => {
    const el = scroller;
    if (!el) return;
    const measure = () => {
      width = Math.max(0, el.clientWidth - PAD * 2);
      viewH = el.clientHeight;
    };
    measure();
    const ro = new ResizeObserver(measure);
    ro.observe(el);
    return () => ro.disconnect();
  });

  const items = $derived<MasonryItem[]>(
    entries.map((e) => (e.kind === 'band' ? { id: e.key, band: BAND } : { id: e.row.id, ...tileSize(e.row) })),
  );
  const layout = $derived(masonry(items, width, size, GAP));
  const byId = $derived(new Map(entries.map((e) => [e.kind === 'band' ? e.key : e.row.id, e])));
  const shown = $derived(width > 0 ? visible(layout.pos, Math.max(0, scrollTop - PAD), viewH, viewH) : []);
  const selected = $derived(new Set(selection));
  /** The keyboard's place, marked only among several selected (alone, the selection says it). */
  const cursorId = $derived(selection.length > 1 ? selection.at(-1) : undefined);

  let raf = 0;
  function onScroll() {
    cancelAnimationFrame(raf);
    raf = requestAnimationFrame(() => {
      if (!scroller) return;
      scrollTop = scroller.scrollTop;
      const next = followOnScroll(followChosen, following, scrollTop);
      if (next !== null) onfollowing(next);
    });
  }
  $effect(() => () => cancelAnimationFrame(raf));

  // Following: a new newest asset (the first tile) scrolls the top back into view.
  const firstId = $derived.by(() => {
    for (const e of entries) if (e.kind === 'asset') return e.row.id;
    return null;
  });
  $effect(() => {
    void firstId;
    if (untrack(() => following) && scroller && scroller.scrollTop > 0) scroller.scrollTop = 0;
  });

  /** The keyboard's cursor stays on screen. */
  export function reveal(id: string) {
    const r: Rect | undefined = layout.pos.get(id);
    const el = scroller;
    if (!r || !el) return;
    const top = r.y + PAD;
    if (top < el.scrollTop) el.scrollTop = top - GAP;
    else if (top + r.h > el.scrollTop + el.clientHeight) el.scrollTop = top + r.h - el.clientHeight + GAP;
  }
  /** Back to the newest, the Follow button's. */
  export function toTop() {
    if (scroller) scroller.scrollTop = 0;
  }
  export function positions(): Map<string, Rect> {
    return layout.pos;
  }
  /** Where a tile is on screen (the keyboard's menu opens on it), if it's mounted. */
  export function screenRect(id: string): DOMRect | null {
    const el = scroller?.querySelector<HTMLElement>(`[data-asset-id="${CSS.escape(id)}"]`);
    return el?.getBoundingClientRect() ?? null;
  }
</script>

<div class="scroller" bind:this={scroller} onscroll={onScroll} role="listbox" aria-label="Assets" aria-multiselectable="true">
  <div class="canvas" style="height: {layout.height + PAD * 2}px">
    {#each shown as id (id)}
      {@const r = layout.pos.get(id)}
      {@const e = byId.get(id)}
      {#if r && e}
        {#if e.kind === 'band'}
          <div class="band" style="transform: translate({PAD}px, {r.y + PAD}px); width: {r.w}px; height: {r.h}px">
            <Tooltip text={e.label} block><span class="bl">{e.label}</span></Tooltip><em>{e.count}</em>
          </div>
        {:else}
          <div class="cell" style="transform: translate({r.x + PAD}px, {r.y + PAD}px); width: {r.w}px; height: {r.h}px">
            <GalleryTile
              {hiveId}
              row={e.row}
              selected={selected.has(id)}
              cursor={cursorId === id}
              selecting={selection.length > 0}
              flashing={isFlashing(id)}
              recommended={recommendations[id]}
              onselect={(ev) => onselect(id, ev)}
              ontoggle={() => ontoggle(id)}
              onopen={() => onopen(id)}
            />
          </div>
        {/if}
      {/if}
    {/each}
  </div>
</div>

<style>
  .scroller { flex: 1; min-height: 0; overflow-y: auto; overflow-x: hidden; position: relative; }
  .canvas { position: relative; width: 100%; }
  .cell, .band { position: absolute; left: 0; top: 0; }
  .cell > :global(.tooltip-wrapper) { height: 100%; }
  .band { display: flex; align-items: flex-end; gap: 8px; padding-bottom: 4px; box-sizing: border-box; border-bottom: 1px solid var(--bg-light); font-size: 11px; font-weight: 500; letter-spacing: 0.04em; color: var(--fg-dim); }
  /* One line in a fixed-height band; the whole label is on the tooltip. */
  .band > :global(.tooltip-wrapper) { flex: 0 1 auto; width: auto; }
  .bl { overflow: hidden; text-overflow: ellipsis; white-space: nowrap; min-width: 0; }
  .band em { flex: none; font-style: normal; opacity: 0.7; font-variant-numeric: tabular-nums; }
</style>
