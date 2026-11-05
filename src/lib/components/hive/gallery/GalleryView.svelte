<!--
  The Hive's gallery (docs/hive.md §10.4): Board · Gallery in the header, in the board's place
  beside the coordinator. Across the top: kind tabs, search, Group, Variants and Size; then the
  filters as one wrapping row with the ones in force as removable chips; then the selection
  strip. The grid has the whole width under them, and the viewer, a comparison or the
  collections panel open in the gallery's own area (never over the coordinator).

  The selection strip is always there at one height, so selecting never moves the grid: with
  nothing selected it counts the assets and says how to act; with a selection it says how many,
  offers Star, Pick, Add to collection and Compare, More… (the selection's menu) and Clear.
  Right-click on a tile, the context-menu key or Shift+F10 opens the same menu: right-clicking
  outside the selection selects that one first, as Finder does.

  New assets stream in by `hive-gallery-changed` (hiveGallery), highlighted for a moment, and
  following (on while agents work) keeps the newest in view. Files dropped here are filed as
  the human's (plan D19). Dragging tiles onto a composer attaches their stored paths (pointer
  events, never `draggable`: no HTML5 drop reaches a Tauri webview). Every write is the human's
  own command; the window's event refetches what changed.

  Keys (§4.6): only while focus is inside `[data-gallery]`, never in a field (`galleryKey`), and
  `+layout` passes only app shortcuts on from here, as for `[data-filetree]`. Arrows move (Shift
  extends), Cmd/Ctrl+A selects all, Space or Enter opens the viewer and Space or Escape closes
  it, Escape clears the selection, F stars, 1/2/3 set Pick/Final/Reject, C compares, I toggles
  the viewer's info, and X switches audio in a comparison.
-->
<script lang="ts" module>
  /** The viewer's info drawer, remembered for the session (every gallery, every Hive). */
  let infoForSession = false;
</script>

<script lang="ts">
  import { untrack } from 'svelte';
  import { getCurrentWebview } from '@tauri-apps/api/webview';
  import { writeText as clipboardWriteText } from '@tauri-apps/plugin-clipboard-manager';
  import { open as dialogOpen } from '@tauri-apps/plugin-dialog';
  import { error as logError } from '@tauri-apps/plugin-log';
  import { dropPointToCss } from '$lib/utils/dropPoint';
  import { isMac, isWindows } from '$lib/utils/platform';
  import ContextMenu, { type MenuItem } from '$lib/components/ContextMenu.svelte';
  import Icon from '$lib/components/Icon.svelte';
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
    revealInFileManager,
  } from '$lib/tauri/commands';
  import type { GalleryTagChange, HiveWorkerCard } from '$lib/tauri/types';
  import { hiveGallery, SIZE_MAX, SIZE_MIN } from '$lib/stores/hiveGallery.svelte';
  import { toastStore } from '$lib/stores/toasts.svelte';
  import { foldVariants, galleryKey, GROUPS, groupRows, isFiltered, KIND_TABS, neighbor, PURPOSE_TABS, type GalleryQuery, type GroupBy } from '$lib/hive/gallery';
  import {
    compareSet,
    COMPARE_MAX,
    COMPARE_MIN,
    dispatchGalleryAttach,
    DRAG_THRESHOLD,
    afterRemoval,
    extendTo,
    iterateText,
    listedSelection,
    nextPurpose,
    nextStar,
    nextStatus,
    selectionMenu,
    selectionTags,
    selectMode,
    selectWith,
    type MenuRow,
    type SelectionEntry,
  } from '$lib/hive/galleryCurate';
  import { isRetired } from '$lib/hive/cards';
  import GalleryFilters from './GalleryFilters.svelte';
  import GalleryFilterMenu from './GalleryFilterMenu.svelte';
  import GalleryGrid from './GalleryGrid.svelte';
  import GalleryDetail from './GalleryDetail.svelte';
  import GalleryCompare from './GalleryCompare.svelte';
  import CollectionsPanel from './CollectionsPanel.svelte';

  const revealLabel = isMac() ? 'Reveal in Finder' : isWindows() ? 'Show in File Explorer' : 'Open Containing Folder';

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
  const rowById = $derived(new Map(hiveGallery.rows.map((r) => [r.id, r])));
  /** The selection the grid shows, in the order picked: never a folded variant or a filtered-out
   *  asset (the viewer's own asset is acted on by id, not through this). */
  const selected = $derived(listedSelection(ui.selection, order));
  const cursor = $derived(selected.at(-1) ?? null);
  // What the filters or the variants fold hide leaves the selection (not while the viewer shows a
  // lineage step the grid doesn't list, nor before the rows are read).
  $effect(() => {
    if (!hiveGallery.loaded || ui.detail) return;
    const kept = selected;
    if (kept.length !== untrack(() => ui.selection.length)) hiveGallery.select(workspaceId, kept);
  });
  const selRows = $derived(selected.map((id) => rowById.get(id)!).filter(Boolean));
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
  /** A filter change from the bar; a chip that clears the search clears the field too. */
  function setQuery(patch: Partial<GalleryQuery>) {
    if (patch.search !== undefined) {
      if (searchTimer) clearTimeout(searchTimer);
      search = patch.search;
    }
    hiveGallery.setQuery(workspaceId, patch);
  }

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
  function toggle(id: string) {
    if (justDragged) return;
    hiveGallery.select(workspaceId, selectWith(order, ui.selection, id, 'toggle'));
    root?.focus({ preventScroll: true });
  }
  function selectAll() {
    // The cursor stays last, so the keyboard keeps its place.
    hiveGallery.select(workspaceId, cursor && order.includes(cursor) ? extendTo(order, cursor) : [...order]);
  }
  function clearSelection() {
    hiveGallery.select(workspaceId, []);
  }
  /** The tile or grid that had the keyboard unmounts on the switch: it stays in the gallery. */
  function keepKeys() {
    requestAnimationFrame(() => {
      if (!root?.contains(document.activeElement)) root?.focus({ preventScroll: true });
    });
  }

  // ── The viewer: the detail view and the Space preview are one ──
  let info = $state(infoForSession);
  function toggleInfo() {
    info = !info;
    infoForSession = info;
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
  /** Where the viewer's asset was listed, so it can move on when an action takes it out of the
   *  list (trashed, rejected under a status filter): ←/→ keep working. */
  let lastPlace: { id: string; index: number } | null = null;
  $effect(() => {
    const id = ui.detail;
    const i = detailIndex;
    if (!id) {
      lastPlace = null;
      return;
    }
    if (i >= 0) {
      lastPlace = { id, index: i };
      return;
    }
    // Only an asset that WAS listed moves on; a lineage step the grid never listed stays.
    if (!hiveGallery.loaded || lastPlace?.id !== id) return;
    const next = afterRemoval(lastPlace.index, order.length);
    lastPlace = null;
    untrack(() => openDetail(next === null ? null : order[next]));
  });

  // ── Curation: every write is the human's command; the window's event refetches ──
  const fail = (what: string) => (e: unknown) => {
    toastStore.addToast(what, String(e).replace(/^Error: /, ''), 'info');
    logError(`hive: gallery ${what.toLowerCase()}: ${e}`);
  };

  /** What F and 1/2/3 act on: the compared pane, the open asset, or the selection. */
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

  async function pathsOf(ids: string[], what = 'Not attached'): Promise<string[] | null> {
    try {
      return await galleryPaths(workspaceId, ids);
    } catch (e) {
      fail(what)(e);
      return null;
    }
  }

  function addTo(ids: string[], collection: string, create = false) {
    galleryCollect(workspaceId, { collection, create, add: ids })
      .then((c) => toastStore.addToast(`Added to “${c.name}”`, `${ids.length} asset${ids.length === 1 ? '' : 's'}; ${c.asset_ids.length} in it now.`, 'info'))
      .catch(fail('Not added to the collection'));
  }

  // Export…: the folder from the dialog, then how (new files, or Replace).
  async function startExport(ids: string[], collection: string | null = null) {
    const folder = await dialogOpen({ directory: true, multiple: false, title: 'Export to a folder' }).catch(() => null);
    if (typeof folder === 'string' && folder) prompt = { kind: 'export', ids, collection, folder };
  }
  function runExport(replace: boolean) {
    const x = prompt?.kind === 'export' ? prompt : null;
    prompt = null;
    keepKeys();
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

  function iterate(id: string) {
    galleryGet(workspaceId, id)
      .then((a) => oniterate([a.path], iterateText(a)))
      .catch(fail('Not attached'));
  }
  function trash(ids: string[]) {
    galleryTrash(workspaceId, ids)
      .then((n) => {
        toastStore.addToast(`Moved ${n} to the trash`, 'Turn on Trash in the filters to restore; only you empty it.', 'info');
        hiveGallery.select(workspaceId, ui.selection.filter((s) => !ids.includes(s)));
      })
      .catch(fail('Not moved to the trash'));
  }
  function restore(ids: string[]) {
    galleryRestore(workspaceId, ids).catch(fail('Not restored'));
  }
  async function reveal(id: string) {
    const p = await pathsOf([id], 'Not revealed');
    if (p?.[0]) revealInFileManager(p[0]).catch(fail('Not revealed'));
  }
  async function copyPaths(ids: string[]) {
    const p = await pathsOf(ids, 'Not copied');
    if (!p?.length) return;
    try {
      await clipboardWriteText(p.join('\n'));
      toastStore.addToast(p.length === 1 ? 'Copied the path' : `Copied ${p.length} paths`, p.length === 1 ? p[0] : 'One per line.', 'info');
    } catch (e) {
      fail('Not copied')(e);
    }
  }

  function emptyTrash() {
    galleryEmptyTrash(workspaceId)
      .then((n) => {
        hiveGallery.refreshSide(workspaceId);
        toastStore.addToast(`Emptied the trash`, `${n} file${n === 1 ? '' : 's'} deleted; their records stay.`, 'info');
      })
      .catch(fail('Trash not emptied'));
  }

  // ── A question the human answers at the bottom of the gallery, over it (never pushing the grid):
  //    tags to add, a new collection's name, how an export lands ──
  type Prompt =
    | { kind: 'tag'; ids: string[]; text: string }
    | { kind: 'name'; ids: string[]; text: string }
    | { kind: 'export'; ids: string[]; collection: string | null; folder: string };
  let prompt = $state<Prompt | null>(null);
  let promptInput = $state<HTMLInputElement | null>(null);
  function ask(p: Prompt) {
    prompt = p;
    requestAnimationFrame(() => promptInput?.focus());
  }
  function cancelPrompt() {
    prompt = null;
    keepKeys();
  }
  function submitPrompt() {
    const p = prompt;
    if (!p || p.kind === 'export') return;
    const text = p.text.trim();
    if (!text) return;
    prompt = null;
    keepKeys();
    if (p.kind === 'name') addTo(p.ids, text, true);
    else {
      const tags = [...new Set(text.split(',').map((t) => t.trim()).filter(Boolean))];
      if (tags.length) tag(p.ids, { tags });
    }
  }

  // ── The selection's menu: right-click, the context-menu key, Shift+F10, More… ──
  let menu = $state<{ items: MenuItem[]; x: number; y: number; anchor: HTMLElement | null } | null>(null);

  function collectionItems(ids: string[]): MenuItem[] {
    const items: MenuItem[] = hiveGallery.collections.map((c) => ({ label: `${c.name} (${c.asset_ids.length})`, action: () => addTo(ids, c.id) }));
    if (items.length) items.push({ label: '', action: () => {}, separator: true });
    items.push({ label: 'New collection…', action: () => ask({ kind: 'name', ids, text: '' }) });
    return items;
  }
  function workerItems(ids: string[]): MenuItem[] {
    if (!liveWorkers.length) return [{ label: 'No worker is running', action: () => {}, disabled: true }];
    return liveWorkers.map((w) => ({
      label: `#${w.badge} ${w.title}`,
      action: () => {
        void pathsOf(ids).then((p) => { if (p) onsend(w.task_id, p); });
      },
    }));
  }

  function menuItems(ids: string[], mrows: MenuRow[]): MenuItem[] {
    // The viewer's asset may be one the grid doesn't list: its own row then stands for it.
    const inTrash = (id: string) => !!(rowById.get(id) ?? (ids.length === 1 ? mrows[0] : null))?.deleted_at;
    const run = (e: SelectionEntry): (() => void) => {
      switch (e.action) {
        case 'open':
          return () => openDetail(ids[0]);
        case 'compare':
          return () => { const set = compareSet(ids); if (set) openCompare(set); };
        case 'star':
          return () => tag(ids, { starred: nextStar(mrows) });
        case 'picked':
        case 'final':
        case 'rejected': {
          const s = e.action;
          return () => tag(ids, { status: nextStatus(mrows, s) });
        }
        case 'clearStatus':
          return () => tag(ids, { status: '' });
        case 'purpose':
          return () => tag(ids, { purpose: nextPurpose(mrows) });
        case 'iterate':
          return () => iterate(ids[0]);
        case 'tag':
          return () => ask({ kind: 'tag', ids, text: '' });
        case 'export':
          return () => void startExport(ids);
        case 'reveal':
          return () => void reveal(ids[0]);
        case 'copyPath':
          return () => void copyPaths(ids);
        case 'trash':
          return () => trash(ids.filter((id) => !inTrash(id)));
        case 'restore':
          return () => restore(ids.filter(inTrash));
        default:
          return () => {};
      }
    };
    return selectionMenu(mrows, revealLabel).map((e): MenuItem => {
      if (e.separator) return { label: '', action: () => {}, separator: true };
      const submenu =
        e.submenu === 'workers'
          ? workerItems(ids)
          : e.submenu === 'collections'
            ? collectionItems(ids)
            : e.submenu === 'untag'
              ? selectionTags(mrows).map((t) => ({ label: t, action: () => tag(ids, { untag: [t] }) }))
              : undefined;
      return { label: e.label, shortcut: e.shortcut, disabled: e.disabled, submenu, action: run(e) };
    });
  }

  /** The menu for `ids` at a point; `byKey` puts the keyboard on its first item. */
  function openMenu(ids: string[], x: number, y: number, anchor: HTMLElement | null = null, byKey = false) {
    let mrows: MenuRow[] = ids.map((id) => rowById.get(id)).filter((r) => !!r);
    // The viewer's asset may be one the filters don't list (a lineage step, a chat's link).
    if (!mrows.length && ui.detail && ids.length === 1 && ids[0] === ui.detail) {
      const a = detailEl?.current();
      if (a) mrows = [a];
    }
    if (!mrows.length) return;
    menu = { items: menuItems(ids, mrows), x, y, anchor };
    if (byKey) requestAnimationFrame(() => document.querySelector<HTMLElement>('.context-menu .menu-item:not(:disabled)')?.focus());
  }
  function closeMenu() {
    menu = null;
    keepKeys();
  }
  /** Right-click on a tile: outside the selection it selects that one first, as Finder does. */
  function onMainContextMenu(e: MouseEvent) {
    const id = (e.target as HTMLElement).closest<HTMLElement>('[data-asset-id]')?.dataset.assetId;
    if (!id || !inGrid) return;
    e.preventDefault();
    const ids = selected.includes(id) ? selected : [id];
    if (!selected.includes(id)) hiveGallery.select(workspaceId, [id]);
    root?.focus({ preventScroll: true });
    // The keyboard goes into the menu, as from the menu key: arrows, Enter and Space are its,
    // never the grid's under it.
    openMenu(ids, e.clientX, e.clientY, null, true);
  }
  /** The keyboard's menu: on the cursor's tile, or under the viewer's More…. */
  function openMenuByKey() {
    if (ui.detail) {
      const a = detailEl?.menuAnchor();
      const r = a?.getBoundingClientRect();
      if (r) openMenu([ui.detail], r.left, r.bottom + 4, a ?? null, true);
      return;
    }
    if (!inGrid || !cursor || !selected.length) return;
    grid?.reveal(cursor);
    requestAnimationFrame(() => {
      const r = grid?.screenRect(cursor);
      if (r) openMenu(selected, r.left + Math.min(24, r.width / 2), r.top + Math.min(24, r.height / 2), null, true);
    });
  }
  /** Arrows inside the menu: ContextMenu itself is pointer-first; this keeps it keyboard-usable. */
  function onMenuKey(e: KeyboardEvent, inMenu: HTMLElement) {
    const items = [...inMenu.querySelectorAll<HTMLElement>(':scope > .menu-item:not(:disabled)')];
    const i = items.indexOf(document.activeElement as HTMLElement);
    if (e.key === 'ArrowDown' || e.key === 'ArrowUp') {
      e.preventDefault();
      const n = items.length;
      if (n) items[(i + (e.key === 'ArrowDown' ? 1 : n - 1) + n) % n]?.focus();
    } else if (e.key === 'ArrowRight' && i >= 0 && items[i].getAttribute('aria-haspopup')) {
      e.preventDefault();
      items[i].click();
      requestAnimationFrame(() => inMenu.querySelector<HTMLElement>(':scope > .context-menu .menu-item:not(:disabled)')?.focus());
    } else if (e.key === 'ArrowLeft') {
      const parent = inMenu.parentElement?.closest<HTMLElement>('.context-menu');
      if (parent) {
        e.preventDefault();
        parent.querySelector<HTMLElement>(':scope > .menu-item.open')?.focus();
      }
    }
    e.stopPropagation();
  }

  // The strip's own menus: Add to collection, and More….
  let stripCollect = $state<HTMLButtonElement | null>(null);
  let stripMore = $state<HTMLButtonElement | null>(null);
  function openCollectMenu() {
    if (!stripCollect || !selected.length) return;
    if (menu?.anchor === stripCollect) return closeMenu();
    const r = stripCollect.getBoundingClientRect();
    menu = { items: collectionItems([...selected]), x: r.left, y: r.bottom + 4, anchor: stripCollect };
  }
  function openMoreMenu() {
    if (!stripMore) return;
    if (menu?.anchor === stripMore) return closeMenu();
    const r = stripMore.getBoundingClientRect();
    openMenu([...selected], r.left, r.bottom + 4, stripMore);
  }
  const noFocus = (e: MouseEvent) => e.preventDefault();

  function onKeydown(e: KeyboardEvent) {
    if (e.defaultPrevented || e.isComposing) return;
    const t = e.target as HTMLElement | null;
    const inMenu = t?.closest?.<HTMLElement>('.context-menu');
    if (inMenu) return onMenuKey(e, inMenu);
    // A menu open with the keyboard elsewhere: a gallery key goes into it (the arrows) or closes
    // it first, never acting on the grid under it, whose selection the menu's actions hold.
    const editable = !!t && (t.isContentEditable || !!t.closest?.('input, textarea, select, [data-composer-drop]'));
    if (menu && !editable) {
      const first = document.querySelector<HTMLElement>('.context-menu .menu-item:not(:disabled)');
      if ((e.key === 'ArrowDown' || e.key === 'ArrowUp') && first) {
        e.preventDefault();
        first.focus();
        return;
      }
      if (galleryKey(e, false)) {
        e.preventDefault();
        closeMenu();
        return;
      }
    }
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
    if (a.kind === 'back' && prompt) {
      e.preventDefault();
      cancelPrompt();
      return;
    }
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
          hiveGallery.select(workspaceId, a.extend ? extendTo(ui.selection, next) : [next]);
          grid?.reveal(next);
        }
        return;
      }
      case 'open':
        if (!ui.detail && !ui.compare && cursor && rowById.has(cursor)) {
          e.preventDefault();
          openDetail(cursor);
        }
        return;
      case 'space':
        e.preventDefault();
        if (ui.compare) compareEl?.togglePlay();
        else if (ui.detail) openDetail(null);
        else if (cursor && rowById.has(cursor)) openDetail(cursor);
        return;
      case 'back':
        if (ui.compare) {
          e.preventDefault();
          openCompare(null);
        } else if (ui.detail) {
          e.preventDefault();
          openDetail(null);
        } else if (ui.selection.length) {
          e.preventDefault();
          clearSelection();
        }
        return;
      case 'selectAll':
        // Elsewhere (the viewer's info drawer) Cmd+A is the text's.
        if (!inGrid) return;
        e.preventDefault();
        selectAll();
        return;
      case 'menu':
        e.preventDefault();
        openMenuByKey();
        return;
      case 'info':
        if (ui.detail) {
          e.preventDefault();
          toggleInfo();
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
    if (t.closest('button, input, video, audio, .check')) return;
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
  const canCompare = $derived(selected.length >= COMPARE_MIN && selected.length <= COMPARE_MAX);
  const allStarred = $derived(!nextStar(selRows));
  const allPicked = $derived(!nextStatus(selRows, 'picked'));
  const groupOptions = GROUPS.map((g) => ({ key: g.key, label: g.label }));
</script>

<!-- svelte-ignore a11y_no_noninteractive_tabindex, a11y_no_noninteractive_element_interactions -->
<section class="gallery" class:drag-over={dragOver} data-gallery bind:this={root} tabindex="-1" aria-label="Gallery" onkeydown={onKeydown}>
  <div class="top">
    <div class="kinds purposes" role="tablist" aria-label="Show">
      {#each PURPOSE_TABS as p (p.key)}
        <Tooltip text={p.tip}>
          <button type="button" role="tab" aria-selected={ui.query.purpose === p.key} class:on={ui.query.purpose === p.key} onmousedown={noFocus} onclick={() => hiveGallery.setQuery(workspaceId, { purpose: p.key })}>{p.label}</button>
        </Tooltip>
      {/each}
    </div>
    <div class="kinds" role="tablist" aria-label="Kind">
      {#each KIND_TABS as k (k.key)}
        <button type="button" role="tab" aria-selected={ui.query.kind === k.key} class:on={ui.query.kind === k.key} onmousedown={noFocus} onclick={() => hiveGallery.setQuery(workspaceId, { kind: k.key })}>{k.label}</button>
      {/each}
    </div>
    <input class="search" type="search" placeholder="Search prompts, titles, tags" value={search} oninput={(e) => onSearch((e.currentTarget as HTMLInputElement).value)} aria-label="Search the gallery" />
    <span class="view">
      <GalleryFilterMenu label="Group" showValue options={groupOptions} value={ui.group} active={ui.group !== 'none'} onpick={(k) => hiveGallery.setGroup(workspaceId, k as GroupBy)} />
      <Tooltip text="Show each variant as its own tile instead of under its original">
        <button type="button" class="tog" class:on={ui.showVariants} aria-pressed={ui.showVariants} onmousedown={noFocus} onclick={() => hiveGallery.setShowVariants(workspaceId, !ui.showVariants)}>Variants</button>
      </Tooltip>
      <label class="size">
        <span class="sr">Tile size</span>
        <Icon name="image" size={9} />
        <input type="range" min={SIZE_MIN} max={SIZE_MAX} step="10" value={hiveGallery.size} oninput={(e) => hiveGallery.setSize(Number((e.currentTarget as HTMLInputElement).value))} aria-label="Tile size" />
        <Icon name="image" size={13} />
      </label>
      {#if following}
        <span class="follow on" aria-live="polite">Following the newest</span>
      {:else}
        <button type="button" class="follow" onmousedown={noFocus} onclick={followNow}>Follow the newest</button>
      {/if}
    </span>
  </div>
  <GalleryFilters
    query={ui.query}
    {workers}
    rows={hiveGallery.rows}
    collections={hiveGallery.collections}
    trashed={hiveGallery.trashCount}
    onchange={setQuery}
    onmanage={() => { hiveGallery.openCollections(workspaceId, true); keepKeys(); }}
    onemptytrash={emptyTrash}
  />

  <!-- One height whatever it holds: selecting never moves the grid. -->
  <div class="strip" class:has={selected.length > 0} role="toolbar" aria-label={selected.length ? `${selected.length} selected` : 'Selection'}>
    {#if selected.length}
      <span class="count"><span class="tick" aria-hidden="true"><Icon name="check" size={10} /></span>{selected.length} selected</span>
      <Tooltip text={allStarred ? 'Unstar (F)' : 'Star (F)'}>
        <button type="button" class="sb" class:on={allStarred} aria-pressed={allStarred} onmousedown={noFocus} onclick={() => tag(selected, { starred: nextStar(selRows) })}><span class="star" aria-hidden="true">★</span> Star</button>
      </Tooltip>
      <Tooltip text={allPicked ? 'Unpick (1)' : 'Pick (1)'}>
        <button type="button" class="sb" class:on={allPicked} aria-pressed={allPicked} onmousedown={noFocus} onclick={() => tag(selected, { status: nextStatus(selRows, 'picked') })}>Pick</button>
      </Tooltip>
      <button type="button" class="sb" bind:this={stripCollect} onmousedown={noFocus} onclick={openCollectMenu}><span class="long">Add to collection…</span><span class="short">Collect…</span></button>
      {#if canCompare}
        <Tooltip text="Side by side (C)">
          <button type="button" class="sb" onmousedown={noFocus} onclick={() => openCompare(compareSet(selected))}>Compare</button>
        </Tooltip>
      {/if}
      <Tooltip text="Every action on the selection (or right-click a tile)">
        <button type="button" class="sb" bind:this={stripMore} onmousedown={noFocus} onclick={openMoreMenu}>More…</button>
      </Tooltip>
      <span class="spacer"></span>
      {#if selected.length < order.length}
        <Tooltip text="Select every asset listed (Cmd+A)">
          <button type="button" class="link wide" onmousedown={noFocus} onclick={selectAll}>Select all {order.length}</button>
        </Tooltip>
      {/if}
      <Tooltip text="Clear the selection (Esc)">
        <button type="button" class="link" onmousedown={noFocus} onclick={clearSelection}>Clear</button>
      </Tooltip>
    {:else}
      <span class="total">{rows.length} asset{rows.length === 1 ? '' : 's'}</span>
      {#if rows.length}
        <span class="hint">Click to select, Shift or Cmd-click for more<span class="wide">. Space previews, right-click for actions.</span></span>
      {/if}
    {/if}
  </div>

  <!-- svelte-ignore a11y_no_static_element_interactions -->
  <div class="main" onpointerdown={onMainPointerDown} oncontextmenu={onMainContextMenu}>
    {#if ui.collections}
      <CollectionsPanel
        {workspaceId}
        {hiveId}
        collections={hiveGallery.collections}
        version={hiveGallery.version}
        onback={() => { hiveGallery.openCollections(workspaceId, false); keepKeys(); }}
        onshow={(id) => { setQuery({ collection: id }); hiveGallery.openCollections(workspaceId, false); keepKeys(); }}
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
      <!-- The grid stays mounted under the viewer, so closing it returns to the same place. -->
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
        ontoggle={toggle}
        onopen={(id) => openDetail(id)}
      />
    {/if}
  </div>

  {#if ui.detail}
    <GalleryDetail
      bind:this={detailEl}
      {workspaceId}
      {hiveId}
      id={ui.detail}
      version={hiveGallery.version}
      {workers}
      position={detailIndex + 1}
      total={order.length}
      hasPrev={detailIndex > 0}
      hasNext={detailIndex >= 0 && detailIndex < order.length - 1}
      {info}
      oninfo={toggleInfo}
      onclose={() => openDetail(null)}
      onnavigate={(id) => openDetail(id)}
      onstep={step}
      {onopenworker}
      {onopencoordinator}
      onmenu={(x, y, anchor) => { if (ui.detail) openMenu([ui.detail], x, y, anchor ?? null); }}
      ontag={tag}
      recommendation={hiveGallery.recommendations[ui.detail]}
    />
  {/if}

  {#if prompt}
    <div class="prompt" role="group" aria-label={prompt.kind === 'export' ? 'Export' : prompt.kind === 'name' ? 'New collection' : 'Add tags'}>
      {#if prompt.kind === 'export'}
        <span class="what">Export {prompt.collection ? 'the collection' : `${prompt.ids.length} asset${prompt.ids.length === 1 ? '' : 's'}`} to</span>
        <Tooltip text={prompt.folder}><b class="folder">{prompt.folder}</b></Tooltip>
        <Tooltip text="A name already there gets “ (2)”: nothing is overwritten"><button type="button" class="pb primary" onclick={() => runExport(false)}>As new files</button></Tooltip>
        <Tooltip text="Files with the same names are overwritten"><button type="button" class="pb danger" onclick={() => runExport(true)}>Replace same names</button></Tooltip>
        <button type="button" class="pb" onclick={cancelPrompt}>Cancel</button>
      {:else}
        <form onsubmit={(e) => { e.preventDefault(); submitPrompt(); }}>
          <span class="what">
            {prompt.kind === 'name' ? 'New collection for' : 'Tag'} {prompt.ids.length} asset{prompt.ids.length === 1 ? '' : 's'}:
          </span>
          <input
            type="text"
            bind:this={promptInput}
            bind:value={prompt.text}
            maxlength={prompt.kind === 'name' ? 100 : 120}
            placeholder={prompt.kind === 'name' ? 'Collection name' : 'Tags, separated by commas'}
            aria-label={prompt.kind === 'name' ? 'New collection name' : 'Tags to add'}
            onkeydown={(e) => { if (e.key === 'Escape') { e.stopPropagation(); cancelPrompt(); } }}
          />
          <button type="submit" class="pb primary" disabled={!prompt.text.trim()}>{prompt.kind === 'name' ? 'Add to new collection' : 'Add tags'}</button>
          <button type="button" class="pb" onclick={cancelPrompt}>Cancel</button>
        </form>
      {/if}
    </div>
  {/if}

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
    <ContextMenu items={menu.items} x={menu.x} y={menu.y} anchor={menu.anchor} onclose={closeMenu} />
  {/if}
</section>

<style>
  .gallery { position: relative; flex: 1 1 260px; min-width: 260px; min-height: 0; display: flex; flex-direction: column; outline: none; container-type: inline-size; }
  .top { display: flex; flex-wrap: wrap; align-items: center; gap: 6px 12px; padding: 8px 12px 6px; font-size: 11.5px; }
  .kinds { display: flex; gap: 2px; }
  .kinds button { background: none; border: none; border-bottom: 2px solid transparent; padding: 2px 6px; color: var(--fg-dim); font: inherit; font-size: 12px; cursor: pointer; }
  .kinds button.on { color: var(--fg); border-bottom-color: var(--accent); }
  .kinds button:hover { color: var(--fg); }
  /* For you / Proof / All: which assets, before which kind; ruled off from the kind tabs. */
  .purposes { padding-right: 8px; border-right: 1px solid var(--bg-light); }
  .purposes button.on { font-weight: 600; }
  .search { flex: 1 1 180px; min-width: 120px; height: 24px; box-sizing: border-box; background: var(--bg-medium); border: 1px solid var(--bg-light); border-radius: 6px; color: var(--fg); font: inherit; font-size: 12px; padding: 0 8px; }
  .search:focus { outline: none; border-color: var(--accent); }
  .view { display: inline-flex; flex-wrap: wrap; align-items: center; gap: 6px 10px; }
  .tog { height: 24px; box-sizing: border-box; background: var(--bg-medium); border: 1px solid var(--bg-light); color: var(--fg-dim); border-radius: 5px; padding: 0 8px; font: inherit; font-size: 11.5px; cursor: pointer; }
  .tog:hover { color: var(--fg); }
  .tog.on { color: var(--fg); border-color: color-mix(in srgb, var(--accent) 70%, var(--bg-light)); background: color-mix(in srgb, var(--accent) 12%, var(--bg-medium)); }
  .size { display: inline-flex; align-items: center; gap: 5px; color: var(--fg-dim); }
  .size input { width: 84px; }
  .sr { position: absolute; width: 1px; height: 1px; overflow: hidden; clip: rect(0 0 0 0); }
  .follow { background: none; border: none; padding: 0; color: var(--accent); font: inherit; font-size: 11.5px; cursor: pointer; white-space: nowrap; }
  .follow.on { color: var(--fg-dim); cursor: default; }

  .strip { flex: none; height: 34px; box-sizing: border-box; display: flex; align-items: center; gap: 6px; padding: 0 12px; border-top: 1px solid var(--bg-light); border-bottom: 1px solid var(--bg-light); font-size: 11.5px; white-space: nowrap; overflow: hidden; color: var(--fg-dim); }
  .strip.has { background: color-mix(in srgb, var(--accent) 9%, var(--bg-dark)); color: var(--fg); }
  .count { display: inline-flex; align-items: center; gap: 6px; font-weight: 600; margin-right: 6px; font-variant-numeric: tabular-nums; }
  .tick { display: inline-flex; align-items: center; justify-content: center; width: 16px; height: 16px; border-radius: 50%; background: var(--accent); color: var(--bg-dark); }
  .sb { height: 24px; box-sizing: border-box; display: inline-flex; align-items: center; gap: 4px; background: var(--bg-medium); border: 1px solid var(--bg-light); color: var(--fg); border-radius: 5px; padding: 0 9px; font: inherit; font-size: 11.5px; cursor: pointer; }
  .sb:hover { border-color: color-mix(in srgb, var(--accent) 60%, var(--bg-light)); }
  .sb.on { border-color: var(--accent); background: color-mix(in srgb, var(--accent) 16%, var(--bg-medium)); }
  .sb.on .star { color: var(--yellow); }
  .short { display: none; }
  .spacer { flex: 1; }
  .link { background: none; border: none; padding: 0 4px; color: var(--accent); font: inherit; font-size: 11.5px; cursor: pointer; }
  .total { color: var(--fg); font-variant-numeric: tabular-nums; }
  .hint { overflow: hidden; text-overflow: ellipsis; min-width: 0; }

  .main { position: relative; flex: 1; min-height: 0; display: flex; min-width: 0; }
  .empty { margin: auto; display: flex; flex-direction: column; align-items: center; gap: 4px; padding: 24px; text-align: center; max-width: 340px; color: var(--fg-dim); font-size: 12px; }
  .empty b { font-size: 13px; color: var(--fg); }

  .prompt { position: absolute; left: 50%; bottom: 14px; transform: translateX(-50%); z-index: 30; width: max-content; max-width: calc(100% - 24px); box-sizing: border-box; display: flex; flex-wrap: wrap; align-items: center; gap: 6px 8px; padding: 8px 10px; background: var(--bg-medium); border: 1px solid var(--accent); border-radius: 8px; box-shadow: 0 8px 28px rgba(0, 0, 0, 0.45); font-size: 12px; }
  .prompt form { display: contents; }
  .prompt input { flex: 1 1 160px; min-width: 120px; background: var(--bg-dark); border: 1px solid var(--bg-light); color: var(--fg); border-radius: 5px; padding: 3px 8px; font: inherit; }
  .prompt input:focus { outline: none; border-color: var(--accent); }
  .prompt .what { color: var(--fg-dim); }
  .prompt :global(.tooltip-wrapper) { min-width: 0; }
  .folder { display: block; max-width: 280px; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
  .pb { height: 26px; box-sizing: border-box; background: var(--bg-dark); border: 1px solid var(--bg-light); color: var(--fg); border-radius: 5px; padding: 0 10px; font: inherit; cursor: pointer; }
  .pb:disabled { opacity: 0.45; cursor: default; }
  .pb.primary { background: var(--accent); border-color: var(--accent); color: var(--bg-dark); font-weight: 600; }
  .pb.danger { border-color: var(--red); color: var(--red); }

  .drop { position: absolute; inset: 0; z-index: 40; display: flex; align-items: center; justify-content: center; background: color-mix(in srgb, var(--accent) 12%, transparent); border: 2px dashed var(--accent); pointer-events: none; }
  .drop span { background: var(--bg-dark); padding: 6px 12px; border-radius: 6px; font-size: 12px; }
  .ghost { position: fixed; z-index: 9999; pointer-events: none; background: var(--bg-medium); border: 1px solid var(--accent); color: var(--fg); border-radius: 6px; padding: 3px 8px; font-size: 11.5px; box-shadow: 0 4px 14px rgba(0, 0, 0, 0.4); }
  .target { position: fixed; z-index: 9998; pointer-events: none; border: 2px dashed var(--accent); border-radius: 6px; background: color-mix(in srgb, var(--accent) 10%, transparent); }
  button:focus-visible, input:focus-visible { outline: 1px solid var(--accent); outline-offset: 1px; }

  /* A laptop with the queen's chat open: the strip drops its longer words, never wraps. */
  @container (max-width: 760px) {
    .wide { display: none; }
  }
  @container (max-width: 560px) {
    .long { display: none; }
    .short { display: inline; }
    .follow { display: none; }
  }
</style>
