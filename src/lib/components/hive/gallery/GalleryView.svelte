<!--
  The Hive's gallery (docs/hive.md §10.4): Board · Gallery in the header, in the board's place
  beside the coordinator. Kind tabs, search, Group by and a size slider across the top; the filter
  rail on the left; the grid, or one asset's detail, a comparison or the collections panel in the
  same area (never over the coordinator).

  New assets stream in by `hive-gallery-changed` (hiveGallery), highlighted for a moment, and
  following (on while agents work) keeps the newest in view. Files dropped here are filed as
  the human's (plan D19).

  Curation (step 8e): Cmd-click and Shift-click select several, with bulk star, status, tag,
  collect, compare, send, export and trash; dragging tiles onto a composer attaches their stored
  paths (pointer events, never `draggable`: no HTML5 drop reaches a Tauri webview). Every write
  is the human's own command; the window's event refetches what changed.

  Keys (§4.6): only while focus is inside `[data-gallery]`, never in a field (`galleryKey`), and
  `+layout` passes only app shortcuts on from here, as for `[data-filetree]`. Arrows move, Enter
  opens, Escape goes back, Space plays, F stars, 1/2/3 set Pick/Final/Reject, C compares, and X
  switches audio in a comparison.
-->
<script lang="ts">
  import { untrack } from 'svelte';
  import { getCurrentWebview } from '@tauri-apps/api/webview';
  import { open as dialogOpen } from '@tauri-apps/plugin-dialog';
  import { error as logError } from '@tauri-apps/plugin-log';
  import { dropPointToCss } from '$lib/utils/dropPoint';
  import ContextMenu, { type MenuItem } from '$lib/components/ContextMenu.svelte';
  import Tooltip from '$lib/components/Tooltip.svelte';
  import {
    galleryAddFiles,
    galleryCollect,
    galleryEmptyTrash,
    galleryExport,
    galleryGet,
    galleryPaths,
    galleryRestore,
    galleryTag,
    galleryTrash,
  } from '$lib/tauri/commands';
  import type { GalleryTagChange, HiveWorkerCard } from '$lib/tauri/types';
  import { hiveGallery, SIZE_MAX, SIZE_MIN } from '$lib/stores/hiveGallery.svelte';
  import { toastStore } from '$lib/stores/toasts.svelte';
  import { foldVariants, galleryKey, GROUPS, groupRows, isFiltered, KIND_TABS, neighbor, type GroupBy } from '$lib/hive/gallery';
  import {
    compareSet,
    dispatchGalleryAttach,
    DRAG_THRESHOLD,
    iterateText,
    nextStar,
    nextStatus,
    selectMode,
    selectWith,
    type CurateActions,
  } from '$lib/hive/galleryCurate';
  import { isRetired } from '$lib/hive/cards';
  import GalleryFilters from './GalleryFilters.svelte';
  import GalleryGrid from './GalleryGrid.svelte';
  import GalleryDetail from './GalleryDetail.svelte';
  import GalleryCompare from './GalleryCompare.svelte';
  import CollectionsPanel from './CollectionsPanel.svelte';

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
    /** Iterate: these files and words onto the coordinator's composer. */
    oniterate: (paths: string[], text: string) => void;
    /** Send to worker…: these files onto that worker's composer (its column opens). */
    onsend: (taskId: string, paths: string[]) => void;
  }
  let { workspaceId, hiveId, workers, anyLive, active, onopenworker, onopencoordinator, oniterate, onsend }: Props = $props();

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
  const rowById = $derived(new Map(hiveGallery.rows.map((r) => [r.id, r])));
  /** The selection that is still listed, in the order picked. */
  const selected = $derived(ui.selection.filter((id) => rowById.has(id)));
  const liveWorkers = $derived([...workers].filter((w) => w.tab_id && !isRetired(w)).sort((a, b) => a.badge - b.badge));

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
  let compareEl = $state<GalleryCompare | undefined>();
  let root = $state<HTMLElement | undefined>();

  function select(id: string, e?: MouseEvent) {
    // The click that ends a drag is no selection.
    if (justDragged) return;
    const next = e ? selectWith(order, ui.selection, id, selectMode(e)) : [id];
    hiveGallery.select(workspaceId, next);
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
  function openCompare(ids: string[] | null) {
    hiveGallery.openCompare(workspaceId, ids);
    keepKeys();
  }
  const detailIndex = $derived(ui.detail ? order.indexOf(ui.detail) : -1);
  function step(dir: -1 | 1) {
    const i = detailIndex + dir;
    if (detailIndex >= 0 && i >= 0 && i < order.length) openDetail(order[i]);
  }

  // ── Curation: every write is the human's command; the window's event refetches ──
  const fail = (what: string) => (e: unknown) => {
    toastStore.addToast(what, String(e).replace(/^Error: /, ''), 'info');
    logError(`hive: gallery ${what.toLowerCase()}: ${e}`);
  };

  /** What F, 1/2/3 and the bulk bar act on: the compared pane, the open asset, or the selection. */
  function targets(): string[] {
    if (ui.compare) {
      const f = compareEl?.focusedId();
      return f ? [f] : [];
    }
    if (ui.detail) return [ui.detail];
    return selected;
  }

  function tag(ids: string[], change: GalleryTagChange) {
    if (!ids.length) return;
    galleryTag(workspaceId, ids, change).catch(fail('Not changed'));
  }

  async function pathsOf(ids: string[]): Promise<string[] | null> {
    try {
      return await galleryPaths(workspaceId, ids);
    } catch (e) {
      fail('Not attached')(e);
      return null;
    }
  }

  // Menus: a collection to add to, a worker to send to. One at a time.
  let menu = $state<{ items: MenuItem[]; x: number; y: number; anchor: HTMLElement } | null>(null);
  function openMenu(items: MenuItem[], anchor: HTMLElement) {
    const r = anchor.getBoundingClientRect();
    menu = { items, x: r.left, y: r.bottom + 4, anchor };
  }
  /** A new collection's name, asked for in the bar for these assets. */
  let naming = $state<{ ids: string[]; name: string } | null>(null);
  let nameEl = $state<HTMLInputElement | null>(null);
  function addTo(ids: string[], collection: string, create = false) {
    galleryCollect(workspaceId, { collection, create, add: ids })
      .then((c) => toastStore.addToast(`Added to “${c.name}”`, `${ids.length} asset${ids.length === 1 ? '' : 's'}; ${c.asset_ids.length} in it now.`, 'info'))
      .catch(fail('Not added to the collection'));
  }

  // Export…: the folder from the dialog, then how (new files, or Replace).
  let exporting = $state<{ ids: string[]; collection: string | null; folder: string } | null>(null);
  async function startExport(ids: string[], collection: string | null = null) {
    const folder = await dialogOpen({ directory: true, multiple: false, title: 'Export to a folder' }).catch(() => null);
    if (typeof folder === 'string' && folder) exporting = { ids, collection, folder };
  }
  function runExport(replace: boolean) {
    const x = exporting;
    exporting = null;
    if (!x) return;
    galleryExport(workspaceId, x.ids, x.collection, x.folder, replace)
      .then((r) =>
        toastStore.addToast(
          `Exported ${r.names.length} file${r.names.length === 1 ? '' : 's'}`,
          `${r.folder}${r.skipped ? ` (${r.skipped} in the trash left out)` : ''}`,
          'info',
        ),
      )
      .catch(fail('Not exported'));
  }

  const actions: CurateActions = {
    tag,
    pickCollection(ids, anchor) {
      const items: MenuItem[] = hiveGallery.collections.map((c) => ({ label: `${c.name} (${c.asset_ids.length})`, action: () => addTo(ids, c.id) }));
      if (items.length) items.push({ label: '', action: () => {}, separator: true });
      items.push({
        label: 'New collection…',
        action: () => {
          naming = { ids, name: '' };
          requestAnimationFrame(() => nameEl?.focus());
        },
      });
      openMenu(items, anchor);
    },
    iterate(id) {
      galleryGet(workspaceId, id)
        .then((a) => oniterate([a.path], iterateText(a)))
        .catch(fail('Not attached'));
    },
    pickWorker(ids, anchor) {
      const items: MenuItem[] = liveWorkers.length
        ? liveWorkers.map((w) => ({
            label: `#${w.badge} ${w.title}`,
            action: () => {
              void pathsOf(ids).then((p) => { if (p) onsend(w.task_id, p); });
            },
          }))
        : [{ label: 'No worker is running', action: () => {}, disabled: true }];
      openMenu(items, anchor);
    },
    exportAssets(ids) {
      void startExport(ids);
    },
    trash(ids) {
      galleryTrash(workspaceId, ids)
        .then((n) => {
          toastStore.addToast(`Moved ${n} to the trash`, 'Show the trash in the filters to restore; only you empty it.', 'info');
          hiveGallery.select(workspaceId, ui.selection.filter((s) => !ids.includes(s)));
        })
        .catch(fail('Not moved to the trash'));
    },
    restore(ids) {
      galleryRestore(workspaceId, ids).catch(fail('Not restored'));
    },
  };

  function emptyTrash() {
    galleryEmptyTrash(workspaceId)
      .then((n) => {
        hiveGallery.refreshSide(workspaceId);
        toastStore.addToast(`Emptied the trash`, `${n} file${n === 1 ? '' : 's'} deleted; their records stay.`, 'info');
      })
      .catch(fail('Trash not emptied'));
  }

  // ── The bulk bar's tag field: "tag" adds, "-tag" removes ──
  let tagDraft = $state('');
  function applyTag() {
    const raw = tagDraft.trim();
    if (!raw || !selected.length) return;
    tagDraft = '';
    const remove = raw.startsWith('-');
    const t = raw.replace(/^-/, '').trim();
    if (t) tag(selected, remove ? { untag: [t] } : { tags: [t] });
  }
  const selRows = $derived(selected.map((id) => rowById.get(id)!).filter(Boolean));
  const allTrashed = $derived(selRows.length > 0 && selRows.every((r) => !!r.deleted_at));
  let bulkCollect = $state<HTMLElement | null>(null);
  let bulkSend = $state<HTMLElement | null>(null);

  function onKeydown(e: KeyboardEvent) {
    if (e.defaultPrevented || e.isComposing) return;
    const t = e.target as HTMLElement | null;
    const editable = !!t && (t.isContentEditable || !!t.closest?.('input, textarea, select, [data-composer-drop]'));
    // A focused button, link or player takes its own Enter and Space (a tile is the grid's).
    const onControl = !!t?.closest?.('button, a[href], summary, video, audio, [role=tab], [role=radio], [role=button], [role=link]');
    // X switches audio in a comparison (§10.4).
    if (ui.compare && !editable && !e.metaKey && !e.ctrlKey && !e.altKey && e.key.toLowerCase() === 'x') {
      e.preventDefault();
      compareEl?.switchAudio();
      return;
    }
    const a = galleryKey(e, editable, onControl);
    if (!a) return;
    if (ui.collections) {
      if (a.kind === 'back') {
        e.preventDefault();
        hiveGallery.openCollections(workspaceId, false);
        keepKeys();
      }
      return;
    }
    switch (a.kind) {
      case 'move': {
        e.preventDefault();
        if (ui.compare) {
          compareEl?.moveFocus(a.dir === 'left' || a.dir === 'up' ? -1 : 1);
          return;
        }
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
        if (!ui.detail && !ui.compare && cursor) {
          e.preventDefault();
          openDetail(cursor);
        }
        return;
      case 'back':
        if (ui.compare) {
          e.preventDefault();
          openCompare(null);
        } else if (ui.detail) {
          e.preventDefault();
          openDetail(null);
        } else if (selected.length > 1) {
          e.preventDefault();
          hiveGallery.select(workspaceId, cursor ? [cursor] : []);
        }
        return;
      case 'play':
        if (ui.compare) {
          e.preventDefault();
          compareEl?.togglePlay();
        } else if (ui.detail) {
          e.preventDefault();
          detailEl?.togglePlay();
        } else if (cursor && grid?.togglePlay(cursor)) {
          e.preventDefault();
        }
        return;
      case 'star': {
        const ids = targets();
        if (!ids.length) return;
        e.preventDefault();
        tag(ids, { starred: nextStar(ids.map((id) => rowById.get(id)).filter((r) => !!r)) });
        return;
      }
      case 'status': {
        const ids = targets();
        if (!ids.length) return;
        e.preventDefault();
        tag(ids, { status: nextStatus(ids.map((id) => rowById.get(id)).filter((r) => !!r), a.status) });
        return;
      }
      case 'compare': {
        if (ui.compare || ui.detail) return;
        e.preventDefault();
        const set = compareSet(selected);
        if (set) openCompare(set);
        else toastStore.addToast('Select two to four to compare', 'Cmd-click or Shift-click assets in the grid, then press C.', 'info');
        return;
      }
    }
  }

  // ── Dragging tiles onto a composer: pointer events, hit-tested with elementFromPoint ──
  let press: { id: string; x: number; y: number } | null = null;
  let dragIds = $state<string[] | null>(null);
  let dragAt = $state({ x: 0, y: 0 });
  let dropTarget = $state<{ chat: string; rect: DOMRect } | null>(null);
  let justDragged = false;
  function composerUnder(x: number, y: number): { chat: string; rect: DOMRect } | null {
    const el = document.elementFromPoint(x, y)?.closest<HTMLElement>('[data-composer-drop]');
    const chat = el?.dataset.composerDrop;
    return el && chat ? { chat, rect: el.getBoundingClientRect() } : null;
  }
  function endDrag() {
    window.removeEventListener('pointermove', onDragMove);
    window.removeEventListener('pointerup', onDragUp);
    window.removeEventListener('pointercancel', endDrag);
    window.removeEventListener('keydown', onDragKey, true);
    if (dragIds) {
      justDragged = true;
      requestAnimationFrame(() => { justDragged = false; });
    }
    press = null;
    dragIds = null;
    dropTarget = null;
  }
  function onDragKey(e: KeyboardEvent) {
    if (e.key === 'Escape' && dragIds) {
      e.preventDefault();
      e.stopPropagation();
      endDrag();
    }
  }
  function onMainPointerDown(e: PointerEvent) {
    if (e.button !== 0 || e.metaKey || e.ctrlKey || e.shiftKey) return;
    const t = e.target as HTMLElement;
    if (t.closest('button, input, video, audio')) return;
    const id = t.closest<HTMLElement>('[data-asset-id]')?.dataset.assetId;
    if (!id) return;
    press = { id, x: e.clientX, y: e.clientY };
    window.addEventListener('pointermove', onDragMove);
    window.addEventListener('pointerup', onDragUp);
    window.addEventListener('pointercancel', endDrag);
  }
  function onDragMove(e: PointerEvent) {
    if (!(e.buttons & 1)) {
      endDrag();
      return;
    }
    if (press && !dragIds) {
      if (Math.abs(e.clientX - press.x) < DRAG_THRESHOLD && Math.abs(e.clientY - press.y) < DRAG_THRESHOLD) return;
      // The selection when the pressed tile is in it, else that tile alone.
      dragIds = selected.includes(press.id) ? [...selected] : [press.id];
      window.addEventListener('keydown', onDragKey, true);
    }
    if (!dragIds) return;
    dragAt = { x: e.clientX, y: e.clientY };
    dropTarget = composerUnder(e.clientX, e.clientY);
  }
  function onDragUp(e: PointerEvent) {
    const ids = dragIds;
    const target = ids ? composerUnder(e.clientX, e.clientY) : null;
    endDrag();
    if (!ids || !target) return;
    void pathsOf(ids).then((paths) => {
      if (paths?.length) dispatchGalleryAttach({ chatTabId: target.chat, paths });
    });
  }
  // A drag's listeners are the window's, not the component tree's: an unmount mid-drag ends it.
  $effect(() => () => endDrag());

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
  const inGrid = $derived(!ui.detail && !ui.compare && !ui.collections);
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
    {#if inGrid}
      {#if following}
        <span class="follow on" aria-live="polite">Following the newest</span>
      {:else}
        <button type="button" class="follow" onclick={followNow}>Follow the newest</button>
      {/if}
    {/if}
  </div>
  {#if inGrid && selected.length > 1}
    <div class="bulk" role="toolbar" aria-label="{selected.length} selected">
      <b>{selected.length} selected</b>
      <Tooltip text="Star (F)"><button type="button" onclick={() => tag(selected, { starred: nextStar(selRows) })}>★</button></Tooltip>
      {#each [['picked', 'Pick'], ['final', 'Final'], ['rejected', 'Reject']] as [s, label], k (s)}
        <Tooltip text="{label} ({k + 1})"><button type="button" onclick={() => tag(selected, { status: nextStatus(selRows, s) })}>{label}</button></Tooltip>
      {/each}
      <button type="button" onclick={() => tag(selected, { status: '' })}>Clear status</button>
      <form class="tagf" onsubmit={(e) => { e.preventDefault(); applyTag(); }}>
        <input type="text" placeholder="tag (-tag removes)" bind:value={tagDraft} maxlength="40" aria-label="Tag the selection" />
      </form>
      <button type="button" bind:this={bulkCollect} onclick={() => bulkCollect && actions.pickCollection(selected, bulkCollect)}>Add to collection…</button>
      {#if selected.length <= 4}<Tooltip text="Side by side (C)"><button type="button" onclick={() => openCompare(compareSet(selected))}>Compare</button></Tooltip>{/if}
      <button type="button" bind:this={bulkSend} onclick={() => bulkSend && actions.pickWorker(selected, bulkSend)}>Send to worker…</button>
      <button type="button" onclick={() => actions.exportAssets(selected)}>Export…</button>
      {#if allTrashed}
        <button type="button" onclick={() => actions.restore(selected)}>Restore</button>
      {:else}
        <button type="button" onclick={() => actions.trash(selected)}>Move to trash</button>
      {/if}
      <button type="button" class="clear" onclick={() => hiveGallery.select(workspaceId, cursor ? [cursor] : [])}>Clear selection</button>
    </div>
  {/if}
  {#if naming}
    <form class="bulk" onsubmit={(e) => { e.preventDefault(); const n = naming; naming = null; if (n && n.name.trim()) addTo(n.ids, n.name.trim(), true); }}>
      <span>New collection for {naming.ids.length} asset{naming.ids.length === 1 ? '' : 's'}:</span>
      <input type="text" bind:this={nameEl} bind:value={naming.name} maxlength="100" placeholder="Name" aria-label="New collection name"
        onkeydown={(e) => { if (e.key === 'Escape') { e.stopPropagation(); naming = null; } }} />
      <button type="submit" disabled={!naming.name.trim()}>Add</button>
      <button type="button" onclick={() => (naming = null)}>Cancel</button>
    </form>
  {/if}
  {#if exporting}
    <div class="bulk" role="group" aria-label="Export">
      <span>Export {exporting.collection ? 'the collection' : `${exporting.ids.length} asset${exporting.ids.length === 1 ? '' : 's'}`} to <b>{exporting.folder}</b>:</span>
      <Tooltip text="A name already there gets “ (2)”: nothing is overwritten"><button type="button" onclick={() => runExport(false)}>As new files</button></Tooltip>
      <Tooltip text="Files with the same names are overwritten"><button type="button" class="danger" onclick={() => runExport(true)}>Replace same names</button></Tooltip>
      <button type="button" onclick={() => (exporting = null)}>Cancel</button>
    </div>
  {/if}
  <!-- svelte-ignore a11y_no_static_element_interactions -->
  <div class="main" onpointerdown={onMainPointerDown}>
    <GalleryFilters
      query={ui.query}
      {workers}
      rows={hiveGallery.rows}
      collections={hiveGallery.collections}
      trashed={hiveGallery.trashCount}
      onchange={(patch) => hiveGallery.setQuery(workspaceId, patch)}
      onmanage={() => { hiveGallery.openCollections(workspaceId, true); keepKeys(); }}
      onemptytrash={emptyTrash}
    />
    {#if ui.collections}
      <CollectionsPanel
        {workspaceId}
        {hiveId}
        collections={hiveGallery.collections}
        version={hiveGallery.version}
        onback={() => { hiveGallery.openCollections(workspaceId, false); keepKeys(); }}
        onshow={(id) => { hiveGallery.setQuery(workspaceId, { collection: id }); hiveGallery.openCollections(workspaceId, false); keepKeys(); }}
        onchanged={() => hiveGallery.refreshSide(workspaceId)}
        onexport={(id) => void startExport([], id)}
      />
    {:else if ui.compare}
      <GalleryCompare
        bind:this={compareEl}
        {hiveId}
        ids={ui.compare}
        rows={hiveGallery.rows}
        recommendations={hiveGallery.recommendations}
        onback={() => openCompare(null)}
        ontag={tag}
        onopen={(id) => openDetail(id)}
      />
    {:else if ui.detail}
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
        recommendation={hiveGallery.recommendations[ui.detail]}
        {actions}
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
        recommendations={hiveGallery.recommendations}
        onfollowing={(on) => hiveGallery.setFollowing(workspaceId, on)}
        onselect={(id, e) => select(id, e)}
        onopen={(id) => openDetail(id)}
      />
    {/if}
  </div>
  {#if dragOver}<div class="drop" aria-hidden="true"><span>Drop to add to the gallery</span></div>{/if}
  {#if dragIds}
    <div class="ghost" style="left: {dragAt.x + 12}px; top: {dragAt.y + 12}px" aria-hidden="true">
      {dragIds.length} asset{dragIds.length === 1 ? '' : 's'}{dropTarget ? ': drop to attach' : ': onto a composer'}
    </div>
    {#if dropTarget}
      <div class="target" style="left: {dropTarget.rect.left}px; top: {dropTarget.rect.top}px; width: {dropTarget.rect.width}px; height: {dropTarget.rect.height}px" aria-hidden="true"></div>
    {/if}
  {/if}
  {#if menu}
    <ContextMenu items={menu.items} x={menu.x} y={menu.y} anchor={menu.anchor} onclose={() => (menu = null)} />
  {/if}
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
  .bulk { display: flex; flex-wrap: wrap; align-items: center; gap: 5px 8px; padding: 6px 12px; border-bottom: 1px solid var(--bg-light); background: color-mix(in srgb, var(--accent) 7%, var(--bg-dark)); font-size: 11.5px; }
  .bulk span { overflow-wrap: anywhere; min-width: 0; }
  .bulk button { background: var(--bg-medium); border: 1px solid var(--bg-light); color: var(--fg); border-radius: 5px; padding: 1px 8px; font: inherit; font-size: 11.5px; cursor: pointer; }
  .bulk button:disabled { opacity: 0.45; cursor: default; }
  .bulk button.danger { border-color: var(--red); color: var(--red); }
  .bulk button.clear { background: none; border: none; color: var(--accent); }
  .bulk input { background: var(--bg-medium); border: 1px solid var(--bg-light); color: var(--fg); border-radius: 5px; padding: 1px 6px; font: inherit; font-size: 11.5px; width: 140px; }
  .tagf { display: contents; }
  .main { flex: 1; min-height: 0; display: flex; min-width: 0; }
  .empty { margin: auto; display: flex; flex-direction: column; align-items: center; gap: 4px; padding: 24px; text-align: center; max-width: 340px; color: var(--fg-dim); font-size: 12px; }
  .empty b { font-size: 13px; color: var(--fg); }
  .drop { position: absolute; inset: 0; display: flex; align-items: center; justify-content: center; background: color-mix(in srgb, var(--accent) 12%, transparent); border: 2px dashed var(--accent); pointer-events: none; }
  .drop span { background: var(--bg-dark); padding: 6px 12px; border-radius: 6px; font-size: 12px; }
  .ghost { position: fixed; z-index: 9999; pointer-events: none; background: var(--bg-medium); border: 1px solid var(--accent); color: var(--fg); border-radius: 6px; padding: 3px 8px; font-size: 11.5px; box-shadow: 0 4px 14px rgba(0, 0, 0, 0.4); }
  .target { position: fixed; z-index: 9998; pointer-events: none; border: 2px dashed var(--accent); border-radius: 6px; background: color-mix(in srgb, var(--accent) 10%, transparent); }
  button:focus-visible, input:focus-visible, select:focus-visible { outline: 1px solid var(--accent); outline-offset: 1px; }
</style>
