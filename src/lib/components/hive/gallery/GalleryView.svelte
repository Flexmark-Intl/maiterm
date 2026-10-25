<!--
  The Hive's gallery (docs/hive.md §10.4): Board · Gallery in the header, in the board's place
  beside the coordinator. Kind tabs, search, Group by and a size slider across the top; the filter
  rail on the left; the grid, or one asset's detail in the same area (never over the coordinator).

  New assets stream in by `hive-gallery-changed` (hiveGallery), highlighted for a moment, and
  following (on while agents work) keeps the newest in view. Files dropped here are filed as
  the human's (plan D19).

  Keys (§4.6): only while focus is inside `[data-gallery]`, never in a field (`galleryKey`), and
  `+layout` passes only app shortcuts on from here, as for `[data-filetree]`. Arrows move, Enter
  opens, Escape goes back, Space plays; F, 1/2/3 and C wait for curation (step 8e).
-->
<script lang="ts">
  import { untrack } from 'svelte';
  import { getCurrentWebview } from '@tauri-apps/api/webview';
  import { error as logError } from '@tauri-apps/plugin-log';
  import { dropPointToCss } from '$lib/utils/dropPoint';
  import { galleryAddFiles } from '$lib/tauri/commands';
  import type { HiveWorkerCard } from '$lib/tauri/types';
  import { hiveGallery, SIZE_MAX, SIZE_MIN } from '$lib/stores/hiveGallery.svelte';
  import { toastStore } from '$lib/stores/toasts.svelte';
  import { foldVariants, galleryKey, GROUPS, groupRows, isFiltered, KIND_TABS, neighbor, type GroupBy } from '$lib/hive/gallery';
  import GalleryFilters from './GalleryFilters.svelte';
  import GalleryGrid from './GalleryGrid.svelte';
  import GalleryDetail from './GalleryDetail.svelte';

  interface Props {
    workspaceId: string;
    hiveId: string;
    workers: HiveWorkerCard[];
    /** Some worker is live: following is on by default while one is (§10.4). */
    anyLive: boolean;
    /** The gallery is the view on screen. */
    active: boolean;
    onopenworker: (taskId: string) => void;
    onopencoordinator: () => void;
  }
  let { workspaceId, hiveId, workers, anyLive, active, onopenworker, onopencoordinator }: Props = $props();

  $effect(() => {
    const ws = workspaceId;
    hiveGallery.setActive(ws, active);
    return () => hiveGallery.setActive(ws, false);
  });

  const ui = $derived(hiveGallery.ui(workspaceId));
  const following = $derived(ui.following ?? anyLive);
  const titles = $derived(new Map(workers.map((w) => [w.task_id, w.title])));
  const collNames = $derived(new Map(hiveGallery.collections.map((c) => [c.id, c.name])));
  const rows = $derived(foldVariants(hiveGallery.rows, ui.showVariants));
  const entries = $derived(groupRows(rows, ui.group, { task: (t) => titles.get(t), collection: (c) => collNames.get(c) }));
  const order = $derived(rows.map((r) => r.id));
  const cursor = $derived(ui.selection.at(-1) ?? null);

  // ── Search, debounced into the query Rust runs ──
  let search = $state(untrack(() => hiveGallery.ui(workspaceId).query.search));
  let searchTimer: ReturnType<typeof setTimeout> | null = null;
  function onSearch(v: string) {
    search = v;
    if (searchTimer) clearTimeout(searchTimer);
    searchTimer = setTimeout(() => hiveGallery.setQuery(workspaceId, { search: v }), 250);
  }
  $effect(() => () => { if (searchTimer) clearTimeout(searchTimer); });

  let grid = $state<GalleryGrid | undefined>();
  let detailEl = $state<GalleryDetail | undefined>();
  let root = $state<HTMLElement | undefined>();

  function select(id: string) {
    hiveGallery.select(workspaceId, [id]);
    // The keyboard on the gallery itself, never a tile the virtualized grid may unmount.
    root?.focus({ preventScroll: true });
  }
  /** The tile or grid that had the keyboard unmounts on the switch: it stays in the gallery. */
  function keepKeys() {
    requestAnimationFrame(() => {
      if (!root?.contains(document.activeElement)) root?.focus({ preventScroll: true });
    });
  }
  function openDetail(id: string | null) {
    hiveGallery.openDetail(workspaceId, id);
    if (!id) requestAnimationFrame(() => { if (cursor) grid?.reveal(cursor); });
    keepKeys();
  }
  const detailIndex = $derived(ui.detail ? order.indexOf(ui.detail) : -1);
  function step(dir: -1 | 1) {
    const i = detailIndex + dir;
    if (detailIndex >= 0 && i >= 0 && i < order.length) openDetail(order[i]);
  }

  function onKeydown(e: KeyboardEvent) {
    if (e.defaultPrevented || e.isComposing) return;
    const t = e.target as HTMLElement | null;
    const editable = !!t && (t.isContentEditable || !!t.closest?.('input, textarea, select, [data-composer-drop]'));
    // A focused button, link or player takes its own Enter and Space (a tile is the grid's).
    const onControl = !!t?.closest?.('button, a[href], summary, video, audio, [role=tab], [role=radio], [role=button], [role=link]');
    const a = galleryKey(e, editable, onControl);
    if (!a) return;
    switch (a.kind) {
      case 'move': {
        e.preventDefault();
        if (ui.detail) {
          step(a.dir === 'left' || a.dir === 'up' ? -1 : 1);
          return;
        }
        const next = neighbor(grid?.positions() ?? new Map(), order, cursor, a.dir);
        if (next) {
          select(next);
          grid?.reveal(next);
        }
        return;
      }
      case 'open':
        if (!ui.detail && cursor) {
          e.preventDefault();
          openDetail(cursor);
        }
        return;
      case 'back':
        if (ui.detail) {
          e.preventDefault();
          openDetail(null);
        }
        return;
      case 'play':
        if (ui.detail) {
          e.preventDefault();
          detailEl?.togglePlay();
        } else if (cursor && grid?.togglePlay(cursor)) {
          e.preventDefault();
        }
        return;
      // F, 1/2/3 and C act with curation (step 8e).
      default:
        return;
    }
  }

  // ── A native file drop onto the gallery (bounds-checked: the webview's drop is window-wide). ──
  let dragOver = $state(false);
  $effect(() => {
    if (!active) return;
    let unlisten: (() => void) | undefined;
    let gone = false;
    const ws = workspaceId;
    void getCurrentWebview().onDragDropEvent((ev) => {
      const p = ev.payload;
      const inside = (raw: { x: number; y: number }) => {
        const pos = dropPointToCss(raw);
        const r = root?.getBoundingClientRect();
        return !!r && pos.x >= r.left && pos.x <= r.right && pos.y >= r.top && pos.y <= r.bottom;
      };
      if (p.type === 'over') dragOver = inside(p.position);
      else if (p.type === 'drop') {
        const was = dragOver;
        dragOver = false;
        if (was && p.paths.length) void addFiles(ws, p.paths);
      } else dragOver = false;
    }).then((u) => { if (gone) u(); else unlisten = u; });
    return () => { gone = true; unlisten?.(); dragOver = false; };
  });
  async function addFiles(ws: string, paths: string[]) {
    try {
      const r = await galleryAddFiles(ws, paths);
      if (r.refused.length) {
        const first = r.refused[0];
        toastStore.addToast(
          r.assets.length ? `Added ${r.assets.length}, ${r.refused.length} not added` : 'Nothing added to the gallery',
          `${first.error}${r.refused.length > 1 ? ` (and ${r.refused.length - 1} more)` : ''}`,
          'info',
        );
      }
    } catch (e) {
      toastStore.addToast('Nothing added to the gallery', String(e).replace(/^Error: /, ''), 'info');
      logError(`hive: gallery drop refused: ${e}`);
    }
  }

  function followNow() {
    hiveGallery.setFollowing(workspaceId, true);
    grid?.toTop();
  }
</script>

<!-- svelte-ignore a11y_no_noninteractive_tabindex, a11y_no_noninteractive_element_interactions -->
<section class="gallery" class:drag-over={dragOver} data-gallery bind:this={root} tabindex="-1" aria-label="Gallery" onkeydown={onKeydown}>
  <div class="top">
    <div class="kinds" role="tablist" aria-label="Kind">
      {#each KIND_TABS as k (k.key)}
        <button type="button" role="tab" aria-selected={ui.query.kind === k.key} class:on={ui.query.kind === k.key} onclick={() => hiveGallery.setQuery(workspaceId, { kind: k.key })}>{k.label}</button>
      {/each}
    </div>
    <input class="search" type="search" placeholder="Search prompts, titles, tags" value={search} oninput={(e) => onSearch((e.currentTarget as HTMLInputElement).value)} aria-label="Search the gallery" />
    <label class="ctl">Group
      <select value={ui.group} onchange={(e) => hiveGallery.setGroup(workspaceId, (e.currentTarget as HTMLSelectElement).value as GroupBy)}>
        {#each GROUPS as g (g.key)}<option value={g.key}>{g.label}</option>{/each}
      </select>
    </label>
    <label class="ctl"><input type="checkbox" checked={ui.showVariants} onchange={(e) => hiveGallery.setShowVariants(workspaceId, (e.currentTarget as HTMLInputElement).checked)} /> Show variants</label>
    <label class="ctl size">Size
      <input type="range" min={SIZE_MIN} max={SIZE_MAX} step="10" value={hiveGallery.size} oninput={(e) => hiveGallery.setSize(Number((e.currentTarget as HTMLInputElement).value))} aria-label="Tile size" />
    </label>
    {#if !ui.detail}
      {#if following}
        <span class="follow on" aria-live="polite">Following the newest</span>
      {:else}
        <button type="button" class="follow" onclick={followNow}>Follow the newest</button>
      {/if}
    {/if}
  </div>
  <div class="main">
    <GalleryFilters query={ui.query} {workers} rows={hiveGallery.rows} collections={hiveGallery.collections} onchange={(patch) => hiveGallery.setQuery(workspaceId, patch)} />
    {#if ui.detail}
      <GalleryDetail
        bind:this={detailEl}
        {workspaceId}
        {hiveId}
        id={ui.detail}
        version={hiveGallery.version}
        {workers}
        hasPrev={detailIndex > 0}
        hasNext={detailIndex >= 0 && detailIndex < order.length - 1}
        onback={() => openDetail(null)}
        onnavigate={(id) => openDetail(id)}
        onstep={step}
        {onopenworker}
        {onopencoordinator}
      />
    {:else if !hiveGallery.loaded}
      <p class="empty">Reading the gallery…</p>
    {:else if !rows.length}
      <div class="empty">
        {#if isFiltered(ui.query) || ui.query.kind}
          <b>Nothing matches</b><span>Clear a filter or the search to see more.</span>
        {:else}
          <b>Nothing filed yet</b>
          <span>What the agents make lands here as they file it, newest first. Drop files here to add your own references.</span>
        {/if}
      </div>
    {:else}
      <GalleryGrid
        bind:this={grid}
        {hiveId}
        {entries}
        size={hiveGallery.size}
        selection={ui.selection}
        {following}
        followChosen={ui.following}
        isFlashing={(id) => hiveGallery.isFlashing(id)}
        onfollowing={(on) => hiveGallery.setFollowing(workspaceId, on)}
        onselect={(id) => select(id)}
        onopen={(id) => openDetail(id)}
      />
    {/if}
  </div>
  {#if dragOver}<div class="drop" aria-hidden="true"><span>Drop to add to the gallery</span></div>{/if}
</section>

<style>
  .gallery { position: relative; flex: 1 1 260px; min-width: 260px; min-height: 0; display: flex; flex-direction: column; outline: none; }
  .top { display: flex; flex-wrap: wrap; align-items: center; gap: 6px 14px; padding: 8px 12px; border-bottom: 1px solid var(--bg-light); font-size: 11.5px; }
  .kinds { display: flex; gap: 2px; }
  .kinds button { background: none; border: none; border-bottom: 1px solid transparent; padding: 1px 6px; color: var(--fg-dim); font: inherit; font-size: 12px; cursor: pointer; }
  .kinds button.on { color: var(--fg); border-bottom-color: var(--accent); }
  .kinds button:hover { color: var(--fg); }
  .search { flex: 1 1 180px; min-width: 120px; background: var(--bg-medium); border: 1px solid var(--bg-light); border-radius: 6px; color: var(--fg); font: inherit; font-size: 12px; padding: 3px 8px; }
  .search:focus { outline: none; border-color: var(--accent); }
  .ctl { display: inline-flex; align-items: center; gap: 5px; color: var(--fg-dim); cursor: pointer; }
  .ctl select { background: var(--bg-medium); color: var(--fg); border: 1px solid var(--bg-light); border-radius: 5px; font: inherit; font-size: 11.5px; padding: 1px 3px; }
  .size input { width: 90px; }
  .follow { background: none; border: none; padding: 0; color: var(--accent); font: inherit; font-size: 11.5px; cursor: pointer; }
  .follow.on { color: var(--fg-dim); cursor: default; }
  .main { flex: 1; min-height: 0; display: flex; min-width: 0; }
  .empty { margin: auto; display: flex; flex-direction: column; align-items: center; gap: 4px; padding: 24px; text-align: center; max-width: 340px; color: var(--fg-dim); font-size: 12px; }
  .empty b { font-size: 13px; color: var(--fg); }
  .drop { position: absolute; inset: 0; display: flex; align-items: center; justify-content: center; background: color-mix(in srgb, var(--accent) 12%, transparent); border: 2px dashed var(--accent); pointer-events: none; }
  .drop span { background: var(--bg-dark); padding: 6px 12px; border-radius: 6px; font-size: 12px; }
  button:focus-visible, input:focus-visible, select:focus-visible { outline: 1px solid var(--accent); outline-offset: 1px; }
</style>
