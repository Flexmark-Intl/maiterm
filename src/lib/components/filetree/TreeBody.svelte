<script lang="ts">
  /**
   * The virtual ARIA tree (docs/file-tree.md §12.3).
   *
   * - Fixed row height; rows sit in a spacer of `rows.length × h`, placed with translateY.
   *   Only the visible window (± 10 rows) is in the DOM, about 60 nodes for any tree size,
   *   plus the focused row, which is always rendered so `aria-activedescendant` points at
   *   something real.
   * - The container holds focus (`tabindex=0`, `aria-activedescendant`), never a row: a
   *   focused row can be recycled out of the DOM by scrolling.
   * - Each row is a snippet, not a component.
   * - Keys come from `filetree/keymap.ts`. A key the tree handles is stopped here; the app's
   *   capture handler has already passed only its own shortcuts through (+layout).
   */
  import { tick, untrack } from 'svelte';
  import { fileTreeStore, type RowAgent } from '$lib/stores/fileTree.svelte';
  import { preferencesStore } from '$lib/stores/preferences.svelte';
  import { workspacesStore } from '$lib/stores/workspaces.svelte';
  import { terminalsStore } from '$lib/stores/terminals.svelte';
  import { getEditorByTabId, isEditorDirty } from '$lib/stores/editorRegistry.svelte';
  import * as commands from '$lib/tauri/commands';
  import { TREE_FLAG_IGNORED, TREE_FLAG_HIDDEN, TREE_FLAG_LOOP, TREE_FLAG_UNREADABLE } from '$lib/tauri/types';
  import { NEW_ROW_KEY, dropFolderAt, highlightRuns, isExpandable, isFolderKind, parentRowIndex, rowAtPoint, stickyAncestors, typeAheadMatch, windowOf, withNewRow, type Row } from '$lib/filetree/model';
  import type { GitLetter } from '$lib/tauri/types';
  import { iconFor } from '$lib/filetree/icons';
  import { treeAction, type TreeAction } from '$lib/filetree/keymap';
  import { renameSelection } from '$lib/filetree/names';
  import { trashLabel } from '$lib/filetree/undo';
  import { beginPointerDrag, type DropTarget } from '$lib/filetree/dnd';
  import InlineName from './InlineName.svelte';
  import Icon from '$lib/components/Icon.svelte';
  import TooltipBubble from '$lib/components/TooltipBubble.svelte';
  import ContextMenu, { type MenuItem } from '$lib/components/ContextMenu.svelte';
  import StickyAncestors from './StickyAncestors.svelte';
  import EdgeChips from './EdgeChips.svelte';
  import { isMac, isWindows, modSymbol } from '$lib/utils/platform';
  import { isServiceTab } from '$lib/workspace/hiddenTabs';

  interface Props {
    rowH: number;
  }

  let { rowH }: Props = $props();

  const INDENT = 12;
  const PAD = 8;
  const TYPEAHEAD_RESET_MS = 1000;
  const TOOLTIP_DELAY_MS = 400;
  const mac = isMac();

  let el = $state<HTMLDivElement | null>(null);
  let scrollTop = $state(0);
  let viewportH = $state(400);
  /** The list itself holds the keyboard (draws the focus ring); the store's `focused` is the
   *  whole column, which is what Cmd+Shift+B asks about. */
  let hasFocus = $state(false);

  const edit = $derived(fileTreeStore.edit);
  /** The store's rows, plus the inline New row at the top of its folder while one is open. */
  const rows = $derived(withNewRow(fileTreeStore.rows, edit?.mode === 'create' ? edit.parent : null));

  // ── Focus index, cached: a focus move must not scan 100k rows every time ───────────────
  let idxHint = 0;
  function idxOf(key: string | null): number {
    if (!key) return -1;
    const rs = rows;
    if (rs[idxHint]?.key === key) return idxHint;
    for (let d = 1; d < 4; d++) {
      if (rs[idxHint + d]?.key === key) return (idxHint += d);
      if (rs[idxHint - d]?.key === key) return (idxHint -= d);
    }
    for (let i = 0; i < rs.length; i++) if (rs[i].key === key) return (idxHint = i);
    return -1;
  }
  const focusIdx = $derived(idxOf(fileTreeStore.focusKey));

  // ── Window and sticky header ──────────────────────────────────────────────────────────
  const stickyMax = $derived(preferencesStore.fileTreeStickyMax);
  const sticky = $derived.by(() => {
    if (stickyMax <= 0 || rows.length === 0) return [] as number[];
    const top = Math.floor(scrollTop / rowH);
    const first = stickyAncestors(rows, top, stickyMax);
    // The header covers rows itself: what matters is the row just under it.
    return stickyAncestors(rows, top + first.length, stickyMax);
  });

  const visible = $derived.by(() => {
    const w = windowOf(scrollTop, viewportH, rowH, rows.length);
    const out: { r: Row; i: number }[] = [];
    for (let i = w.first; i < w.last; i++) out.push({ r: rows[i], i });
    const f = focusIdx;
    if (f >= 0 && (f < w.first || f >= w.last)) out.push({ r: rows[f], i: f });
    // The inline New input stays mounted when scrolled away: unmounting it would drop the
    // name being typed (and a rename's row is the focused one, kept above).
    if (edit?.mode === 'create') {
      const n = idxOf(NEW_ROW_KEY);
      if (n >= 0 && (n < w.first || n >= w.last) && n !== f) out.push({ r: rows[n], i: n });
    }
    return out;
  });

  /** The focused row's parent guide, highlighted (§3.2). */
  const guide = $derived.by(() => {
    const p = parentRowIndex(rows, focusIdx);
    if (p < 0) return null;
    const d = rows[p].depth;
    let end = p + 1;
    while (end < rows.length && rows[end].depth > d) end++;
    return { depth: d, start: p + 1, end };
  });

  // ── Effects: viewport size, scroll and focus requests ─────────────────────────────────
  $effect(() => {
    if (!el) return;
    const node = el;
    const ro = new ResizeObserver(() => { viewportH = node.clientHeight; });
    ro.observe(node);
    viewportH = node.clientHeight;
    return () => ro.disconnect();
  });

  $effect(() => {
    const req = fileTreeStore.scrollRequest;
    if (!el || req.seq === 0) return;
    untrack(() => {
      if (req.top !== undefined && req.key === null) {
        el!.scrollTop = req.top;
        scrollTop = el!.scrollTop;
        return;
      }
      if (req.key) scrollIntoView(idxOf(req.key));
    });
  });

  $effect(() => {
    if (!fileTreeStore.focusPending || !el) return;
    untrack(() => fileTreeStore.takeFocus());
    // tick, not rAF (stalls in an occluded webview) and never `autofocus` (CLAUDE.md).
    void tick().then(() => el?.focus({ preventScroll: true }));
  });

  function scrollIntoView(i: number) {
    if (!el || i < 0) return;
    const top = i * rowH;
    const header = sticky.length * rowH;
    if (top < el.scrollTop + header) el.scrollTop = Math.max(0, top - header);
    else if (top + rowH > el.scrollTop + el.clientHeight) el.scrollTop = top + rowH - el.clientHeight;
    scrollTop = el.scrollTop;
  }

  function onScroll() {
    if (!el) return;
    scrollTop = el.scrollTop;
    fileTreeStore.noteScroll(scrollTop);
    hideTip();
  }

  // ── Movement ──────────────────────────────────────────────────────────────────────────
  function focusable(i: number, dir: 1 | -1): number {
    for (let j = i; j >= 0 && j < rows.length; j += dir) if (rows[j].kind !== 'm') return j;
    return -1;
  }

  function moveTo(i: number, extend = false) {
    if (i < 0 || i >= rows.length) return;
    const key = rows[i].key;
    if (extend) fileTreeStore.extendTo(key);
    else fileTreeStore.selectOnly(key);
    fileTreeStore.setFocus(key, false);
    scrollIntoView(i);
  }

  function step(to: 'up' | 'down' | 'pageUp' | 'pageDown' | 'home' | 'end', extend = false) {
    const cur = focusIdx;
    const page = Math.max(1, Math.floor(viewportH / rowH) - 1);
    let target: number;
    switch (to) {
      case 'up': target = cur < 0 ? focusable(0, 1) : focusable(cur - 1, -1); break;
      case 'down': target = cur < 0 ? focusable(0, 1) : focusable(cur + 1, 1); break;
      case 'pageUp': target = focusable(Math.max(0, cur - page), 1); break;
      case 'pageDown': target = focusable(Math.min(rows.length - 1, Math.max(0, cur) + page), -1); break;
      case 'home': target = focusable(0, 1); break;
      case 'end': target = focusable(rows.length - 1, -1); break;
    }
    if (target >= 0) moveTo(target, extend);
  }

  // ── Type-ahead (§8) ───────────────────────────────────────────────────────────────────
  let prefix = $state('');
  let prefixTimer: ReturnType<typeof setTimeout> | null = null;
  let armed = false; // vim: a leading ' arms type-ahead
  let pendingGAt = 0;

  function typeahead(char: string) {
    if (char === '') {
      armed = true;
      return;
    }
    prefix += char;
    if (prefixTimer) clearTimeout(prefixTimer);
    prefixTimer = setTimeout(() => {
      prefix = '';
      armed = false;
      prefixTimer = null;
    }, TYPEAHEAD_RESET_MS);
    const i = typeAheadMatch(rows, focusIdx, prefix);
    if (i >= 0) moveTo(i);
  }

  // ── Actions ───────────────────────────────────────────────────────────────────────────
  function focusedRow(): Row | null {
    const i = focusIdx;
    return i >= 0 ? rows[i] : null;
  }

  function activate(r: Row, mode: 'preview' | 'pin' | 'side') {
    if (r.kind === 'm') return;
    if (isFolderKind(r.kind)) {
      if (mode !== 'side' && isExpandable(r)) fileTreeStore.toggle(r.key);
      return;
    }
    void fileTreeStore.openFile(r.key, mode, mode === 'preview');
  }

  function escape() {
    // Staged (§11.2): the find first, then a multi-selection, then the keyboard goes back.
    if (fileTreeStore.find) {
      fileTreeStore.closeFind();
      return;
    }
    if (fileTreeStore.selection.size > 1) {
      fileTreeStore.clearSelectionToFocus();
      return;
    }
    // Hand the keyboard back to what's in front.
    const t = workspacesStore.activeTab;
    if (t?.tab_type === 'terminal') terminalsStore.focusTerminal(t.id);
    else if (t?.tab_type === 'editor') getEditorByTabId(t.id)?.view.focus();
    else el?.blur();
  }

  function run(a: TreeAction, e: KeyboardEvent) {
    const r = focusedRow();
    switch (a.type) {
      case 'move': step(a.to); break;
      case 'extend': step(a.to, true); break;
      case 'right':
        if (r && isExpandable(r)) {
          if (!fileTreeStore.isExpanded(r.key)) fileTreeStore.expand(r.key);
          else if (rows[focusIdx + 1]?.depth > r.depth) step('down');
        }
        break;
      case 'left':
        if (r && isExpandable(r) && fileTreeStore.isExpanded(r.key)) fileTreeStore.collapse(r.key);
        else {
          const p = parentRowIndex(rows, focusIdx);
          if (p >= 0) moveTo(p);
        }
        break;
      case 'expandRecursive': if (r && isExpandable(r)) fileTreeStore.expandRecursive(r.key); break;
      case 'collapseRecursive': if (r && isExpandable(r)) fileTreeStore.collapseRecursive(r.key); break;
      case 'selectAll': fileTreeStore.selectAll(); break;
      case 'preview': if (r) activate(r, 'preview'); break;
      case 'open': if (r) activate(r, 'pin'); break;
      case 'openSide': if (r) activate(r, 'side'); break;
      case 'addToTab': void fileTreeStore.addToTab(fileTreeStore.selectedOrFocused()); break;
      case 'copyPath': void fileTreeStore.copyPaths(fileTreeStore.selectedOrFocused(), a.relative); break;
      case 'toggleHidden': void preferencesStore.setFileTreeShowHidden(!preferencesStore.fileTreeShowHidden); break;
      case 'escape': escape(); break;
      case 'contextMenu': if (r) openMenuAt(r, focusIdx); break;
      case 'typeahead': typeahead(a.char); break;
      case 'find': fileTreeStore.openFind(); break;
      case 'pendingG': pendingGAt = e.timeStamp; break;
      case 'rename': if (r && r.kind !== 'm' && fileTreeStore.selectedOrFocused().length <= 1) fileTreeStore.startRename(r.key); break;
      case 'trash': void fileTreeStore.trash(fileTreeStore.selectedOrFocused()); break;
      case 'deletePermanent': void fileTreeStore.deletePermanently(fileTreeStore.selectedOrFocused()); break;
      case 'clipboard':
        if (a.op === 'copy') fileTreeStore.copyFiles(fileTreeStore.selectedOrFocused());
        else if (a.op === 'cut') fileTreeStore.cutFiles(fileTreeStore.selectedOrFocused());
        else void fileTreeStore.paste();
        break;
      case 'undo': void fileTreeStore.undoLast(); break;
    }
  }

  function onKeydown(e: KeyboardEvent) {
    if (e.isComposing) return;
    fileTreeStore.humanTouched();
    const a = treeAction(e, {
      mac,
      vim: preferencesStore.fileTreeVimKeys,
      pendingG: e.timeStamp - pendingGAt < TYPEAHEAD_RESET_MS,
      typeaheadArmed: armed || prefix.length > 0,
    });
    if (!a) return;
    e.preventDefault();
    e.stopPropagation();
    if (a.type !== 'pendingG') pendingGAt = 0;
    run(a, e);
  }

  // ── Mouse ─────────────────────────────────────────────────────────────────────────────
  function onRowClick(e: MouseEvent, r: Row) {
    if (r.kind === 'm') return;
    fileTreeStore.humanTouched();
    const mod = mac ? e.metaKey : e.ctrlKey;
    if (mod) {
      fileTreeStore.toggleSelect(r.key);
      return;
    }
    if (e.shiftKey) {
      fileTreeStore.extendTo(r.key);
      return;
    }
    fileTreeStore.selectOnly(r.key);
    // A click selects a folder (the chevron or Enter opens it); a file opens as a preview
    // when previews are on, keeping the keyboard here (§10.5).
    if (!isFolderKind(r.kind)) {
      const preview = preferencesStore.fileTreePreviewTabs;
      void fileTreeStore.openFile(r.key, preview ? 'preview' : 'pin', preview);
    }
  }

  function onRowDblClick(r: Row) {
    if (r.kind === 'm') return;
    if (isFolderKind(r.kind)) {
      if (isExpandable(r)) fileTreeStore.toggle(r.key);
    } else {
      void fileTreeStore.openFile(r.key, 'pin');
    }
  }

  function onChevron(e: MouseEvent, r: Row) {
    e.stopPropagation();
    fileTreeStore.humanTouched();
    if (e.altKey) {
      if (fileTreeStore.isExpanded(r.key)) fileTreeStore.collapseRecursive(r.key);
      else fileTreeStore.expandRecursive(r.key);
    } else {
      fileTreeStore.toggle(r.key);
    }
    fileTreeStore.setFocus(r.key, false);
  }

  // ── Context menu (§10.4: platform-correct labels; multi-select hides single-item entries)
  let menu = $state<{ x: number; y: number; items: MenuItem[] } | null>(null);

  const revealLabel = mac ? 'Reveal in Finder' : isWindows() ? 'Show in File Explorer' : 'Open Containing Folder';
  const copyPathKey = mac ? `⌥${modSymbol}C` : 'Shift+Alt+C';
  const copyRelKey = mac ? `⌥⇧${modSymbol}C` : 'Ctrl+Shift+Alt+C';
  const trashKey = mac ? `${modSymbol}⌫` : 'Del';
  const deleteKey = mac ? `⌥${modSymbol}⌫` : 'Shift+Del';
  const SEP: MenuItem = { label: '', separator: true, action: () => {} };

  /** New File… / New Folder… / Paste, into `folder`. */
  function createItems(folder: string): MenuItem[] {
    const off = !fileTreeStore.writable;
    return [
      { label: 'New File…', disabled: off, action: () => fileTreeStore.startCreate('file', folder) },
      { label: 'New Folder…', disabled: off, action: () => fileTreeStore.startCreate('dir', folder) },
    ];
  }

  function writeItems(r: Row, rels: string[], single: boolean, folder: boolean): MenuItem[] {
    const off = !fileTreeStore.writable;
    const items: MenuItem[] = [];
    if (single) items.push(...createItems(folder ? r.key : fileTreeStore.folderOf(r.key)));
    items.push(
      { label: 'Cut', shortcut: `${modSymbol}X`, disabled: off, action: () => fileTreeStore.cutFiles(rels) },
      { label: 'Copy', shortcut: `${modSymbol}C`, action: () => fileTreeStore.copyFiles(rels) },
    );
    if (single) items.push({ label: 'Paste', shortcut: `${modSymbol}V`, disabled: off || !fileTreeStore.hasClip, action: () => void fileTreeStore.paste(fileTreeStore.folderOf(r.key)) });
    items.push({ label: 'Duplicate', disabled: off, action: () => fileTreeStore.duplicate(rels) });
    if (single) items.push({ label: 'Rename…', shortcut: 'F2', disabled: off, action: () => fileTreeStore.startRename(r.key) });
    items.push(SEP);
    if (fileTreeStore.canTrash) items.push({ label: trashLabel(fileTreeStore.platform), shortcut: trashKey, disabled: off, action: () => void fileTreeStore.trash(rels) });
    items.push({ label: 'Delete Permanently…', shortcut: deleteKey, disabled: off, action: () => void fileTreeStore.deletePermanently(rels) });
    return items;
  }

  /** Right-click below the rows: the root folder's own actions. */
  function rootMenuItems(): MenuItem[] {
    return [
      ...createItems(''),
      { label: 'Paste', shortcut: `${modSymbol}V`, disabled: !fileTreeStore.writable || !fileTreeStore.hasClip, action: () => void fileTreeStore.paste('') },
      SEP,
      // The file manager is this computer's: a folder on an ssh host has nothing to reveal here.
      ...(fileTreeStore.remote ? [] : [{ label: revealLabel, action: () => void fileTreeStore.revealInOs('') }]),
      { label: 'Open Terminal Here', action: () => void fileTreeStore.openTerminalHere('') },
    ];
  }

  function menuItems(r: Row): MenuItem[] {
    const rels = fileTreeStore.selectedOrFocused(r.key);
    const single = rels.length === 1;
    const folder = isFolderKind(r.kind);
    const target = fileTreeStore.addTarget;
    // A root on an ssh host: this computer's file manager and apps can't reach its files, and
    // the composer attaches files from here; the rest works there as here.
    const remote = fileTreeStore.remote;
    const items: MenuItem[] = [];
    if (single && !folder) {
      items.push(
        { label: 'Open', shortcut: '↩', action: () => void fileTreeStore.openFile(r.key, 'pin') },
        { label: 'Open to the Side', shortcut: `${modSymbol}↩`, action: () => void fileTreeStore.openFile(r.key, 'side') },
        { label: 'Open Preview', shortcut: 'Space', action: () => void fileTreeStore.openFile(r.key, 'preview', true) },
      );
      if (!remote) items.push({ label: 'Open With Default App', action: () => void fileTreeStore.openWithDefaultApp(r.key) });
      items.push(SEP);
    }
    items.push({
      label: target ? `Add to ‹${target.name}›` : 'Add to tab (no live tab followed)',
      shortcut: mac ? '⌥↩' : 'Alt+Enter',
      disabled: !target,
      action: () => void fileTreeStore.addToTab(rels),
    });
    if (fileTreeStore.canAskAbout) {
      items.push({
        label: 'Ask about this…',
        disabled: !target,
        action: () => fileTreeStore.askAbout(rels),
      });
    }
    if (single) items.push({ label: 'Open Terminal Here', action: () => void fileTreeStore.openTerminalHere(r.key) });
    items.push(SEP);
    if (single && !remote) items.push({ label: revealLabel, action: () => void fileTreeStore.revealInOs(r.key) });
    items.push(
      { label: rels.length > 1 ? 'Copy Paths' : 'Copy Path', shortcut: copyPathKey, action: () => void fileTreeStore.copyPaths(rels, false) },
      { label: rels.length > 1 ? 'Copy Relative Paths' : 'Copy Relative Path', shortcut: copyRelKey, action: () => void fileTreeStore.copyPaths(rels, true) },
    );
    items.push(SEP, ...writeItems(r, rels, single, folder));
    const compare: MenuItem[] = [];
    if (single && !folder && fileTreeStore.gitOn) compare.push({ label: 'Compare with HEAD', action: () => void fileTreeStore.compareWithHead(r.key) });
    // Against what the file held before an agent's first edit (§14). With two agents, one
    // pair per agent, named.
    if (single && !folder) {
      const writers = fileTreeStore.baselineWriters(r.key);
      for (const w of writers) {
        const of = writers.length > 1 ? ` (${w.name})` : '';
        if (w.thisTurn) compare.push({ label: `Compare with Turn Start${of}`, action: () => void fileTreeStore.compareWithBaseline(r.key, w.tabId, 'turn') });
        compare.push({ label: `Compare with Session Start${of}`, action: () => void fileTreeStore.compareWithBaseline(r.key, w.tabId, 'session') });
      }
    }
    // An agent wrote a file that has unsaved edits here (§9.3's conflict badge).
    if (single && !folder && fileTreeStore.rowAgent(r.key, false, false)?.conflict?.buffer) {
      compare.push({ label: 'Compare Buffer ↔ Disk', action: () => void fileTreeStore.compareBufferWithDisk(r.key) });
    }
    if (compare.length) items.push(SEP, ...compare);
    if (single && folder && !(r.flags & TREE_FLAG_LOOP)) {
      items.push(SEP, { label: 'Set as Root (pins)', action: () => void fileTreeStore.pinRel(r.key) });
    }
    return items;
  }

  function openMenu(e: MouseEvent, r: Row) {
    e.preventDefault();
    if (r.kind === 'm') return;
    fileTreeStore.humanTouched();
    if (!fileTreeStore.selection.has(r.key)) fileTreeStore.selectOnly(r.key);
    fileTreeStore.setFocus(r.key, false);
    menu = { x: e.clientX, y: e.clientY, items: menuItems(r) };
  }

  function onTreeContext(e: MouseEvent) {
    if (e.defaultPrevented) return; // a row's own menu
    e.preventDefault();
    fileTreeStore.humanTouched();
    menu = { x: e.clientX, y: e.clientY, items: rootMenuItems() };
  }

  // ── Drag and drop (§10.4, filetree/dnd.ts) ────────────────────────────────────────────

  /** The drop target at a point over the tree, arithmetically (rows are a fixed height):
   *  a folder row is itself, a file row its folder, a sticky ancestor that folder, below the
   *  rows the root. Undefined when
   *  the point isn't over the tree. Also used for drops from the OS (FilesColumn). */
  export function hitTree(x: number, y: number): DropTarget | undefined {
    if (!el) return undefined;
    const box = el.getBoundingClientRect();
    if (x < box.left || x > box.right || y < box.top || y > box.bottom) return undefined;
    return { kind: 'folder', rel: dropFolderAt(rows, rowAtPoint(y - box.top, el.scrollTop, rowH, sticky)) };
  }

  function onRowPointerDown(e: PointerEvent, r: Row) {
    if (r.kind === 'm' || e.button !== 0 || edit) return;
    // A press that becomes a drag carries the selection when it starts on a selected row,
    // else just this row (the click that would have selected it never comes).
    const srcs = fileTreeStore.selection.has(r.key) ? fileTreeStore.selectedOrFocused(r.key) : [r.key];
    beginPointerDrag(e, {
      label: srcs.length === 1 ? r.name : `${srcs.length} items`,
      mac,
      hitTree,
      isTerminal: (id) => {
        const loc = workspacesStore._locateTab(id);
        return !!terminalsStore.get(id) && !(loc && isServiceTab(loc.tab));
      },
      accepts: (t, copy) => fileTreeStore.dropAccepts(srcs, t, copy),
      scroller: () => el,
      onHover: (t) => fileTreeStore.setDropTarget(t?.kind === 'folder' ? t.rel : null),
      onLinger: (rel) => { if (!fileTreeStore.isExpanded(rel)) fileTreeStore.expand(rel); },
      onDrop: (t, copy) => { fileTreeStore.humanTouched(); void fileTreeStore.dropItems(srcs, t, copy); },
      onEnd: () => fileTreeStore.setDropTarget(null),
    });
  }

  function openMenuAt(r: Row, i: number) {
    const box = el?.getBoundingClientRect();
    if (!box) return;
    menu = { x: box.left + PAD + (r.depth + 2) * INDENT, y: box.top + i * rowH - scrollTop + rowH, items: menuItems(r) };
  }

  // ── Tooltip: one delegated bubble, only when it has something to add (§3.2) ───────────
  let tip = $state<{ el: Element; text: string } | null>(null);
  let tipTimer: ReturnType<typeof setTimeout> | null = null;
  let tipSeq = 0;

  function hideTip() {
    if (tipTimer) clearTimeout(tipTimer);
    tipTimer = null;
    tipSeq++;
    tip = null;
  }

  function fmtSize(n: number): string {
    if (n < 1024) return `${n} B`;
    if (n < 1024 * 1024) return `${(n / 1024).toFixed(1)} KB`;
    if (n < 1024 ** 3) return `${(n / 1024 / 1024).toFixed(1)} MB`;
    return `${(n / 1024 ** 3).toFixed(1)} GB`;
  }

  function onRowEnter(e: MouseEvent, r: Row) {
    hideTip();
    if (r.kind === 'm') return;
    const rowEl = e.currentTarget as HTMLElement;
    const alt = e.altKey;
    const seq = tipSeq;
    tipTimer = setTimeout(async () => {
      const nameEl = rowEl.querySelector('.name');
      if (!nameEl || !rowEl.isConnected) return;
      const clipped = nameEl.scrollWidth > nameEl.clientWidth;
      const notes: string[] = [];
      const g = fileTreeStore.gitOf(r.key, isFolderKind(r.kind));
      if (g) notes.push(g.rollup ? `changes inside: ${gitWord(g.letter)}` : gitWord(g.letter));
      if (r.flags & TREE_FLAG_IGNORED) notes.push('ignored by git');
      if (r.flags & TREE_FLAG_LOOP) notes.push('link back to a folder above it: not opened');
      if (r.flags & TREE_FLAG_UNREADABLE) notes.push('can’t be read');
      const tabs = fileTreeStore.editorRels.get(r.key);
      if (tabs?.some((t) => isEditorDirty(t))) notes.push('unsaved changes in an editor tab');
      const ag = fileTreeStore.rowAgent(r.key, isFolderKind(r.kind), fileTreeStore.isExpanded(r.key));
      if (ag?.badge) notes.push(`${ag.badge.count} file${ag.badge.count === 1 ? '' : 's'} inside changed by agents this turn`);
      if (!clipped && !alt && notes.length === 0 && !ag?.marks.length && !ag?.conflict) return;
      let text = r.key;
      if (!isFolderKind(r.kind) && fileTreeStore.ack) {
        const st = await commands.ftStat(fileTreeStore.ack.sub_id, r.key).catch(() => null);
        if (seq !== tipSeq) return;
        if (st) {
          const when = st.mtime_ms ? new Date(st.mtime_ms).toLocaleString() : '';
          text += `\n${fmtSize(st.size)}${when ? ` · ${when}` : ''}${st.exec ? ' · executable' : ''}`;
          if (st.link_target) text += `\n→ ${st.link_target}`;
          // The line counts belong with the git word they qualify: "modified · +12 −3".
          if (st.numstat && g && !g.rollup) notes[0] = `${notes[0]} · +${st.numstat[0]} −${st.numstat[1]}`;
        }
      }
      if (notes.length) text += `\n${notes.join(' · ')}`;
      // The agents' marks in words, each with its evidence ("changed on disk while …’s Bash
      // ran" is never "edited by").
      if (ag?.marks.length) {
        const words = await fileTreeStore.agentWords(r.key);
        if (seq !== tipSeq) return;
        if (words.length) text += `\n${words.join('\n')}`;
        // In the Turn lens, how much this turn changed the file: against its turn baseline.
        if (!isFolderKind(r.kind)) {
          const delta = await fileTreeStore.turnDeltaWords(r.key);
          if (seq !== tipSeq) return;
          if (delta.length) text += `\n${delta.join('\n')}`;
        }
      }
      if (ag?.conflict) text += `\n⚠ ${ag.conflict.text}${ag.conflict.buffer ? ' · click ⚠ to compare buffer ↔ disk' : ''}`;
      if (seq === tipSeq) tip = { el: nameEl, text };
    }, TOOLTIP_DELAY_MS);
  }

  // ── Rendering helpers ─────────────────────────────────────────────────────────────────
  function gitWord(letter: GitLetter): string {
    switch (letter) {
      case 'M': return 'modified';
      case 'A': return 'added';
      case 'D': return 'deleted';
      case 'R': return 'renamed';
      case 'C': return 'conflict';
      case '?': return 'untracked';
      case 'T': return 'type changed';
    }
  }

  /** The colour class (§7.3): M/T yellow, A/? green, D red struck through, R cyan, conflict
   *  magenta. */
  function gitClass(letter: GitLetter): string {
    switch (letter) {
      case 'M': case 'T': return 'g-mod';
      case 'A': case '?': return 'g-add';
      case 'D': return 'g-del';
      case 'R': return 'g-ren';
      case 'C': return 'g-conf';
    }
  }

  function ariaLabel(r: Row, expanded: boolean, dirty: boolean, git: { letter: GitLetter; rollup: boolean } | null, ag: RowAgent | null): string {
    const parts = [r.name];
    if (isExpandable(r)) parts.push(expanded ? 'expanded' : 'collapsed');
    if (git) parts.push(git.rollup ? `contains ${gitWord(git.letter)} files` : gitWord(git.letter));
    if (r.flags & TREE_FLAG_IGNORED) parts.push('ignored');
    if (dirty) parts.push('unsaved changes');
    if (ag?.editing) parts.push('an agent is changing it now');
    else if (ag?.deleted) parts.push(`deleted by ${fileTreeStore.tabName(ag.writers[0] ?? '')} this turn`);
    else if (ag?.writers.length) parts.push(`changed by ${ag.writers.map((t) => fileTreeStore.tabName(t)).join(' and ')} this turn`);
    if (ag?.badge) parts.push(`${ag.badge.count} changed inside this turn`);
    if (ag?.conflict) parts.push(`conflict: ${ag.conflict.text}`);
    if (r.key === fileTreeStore.youAreHere) parts.push('this tab’s folder');
    return parts.join(', ');
  }
</script>

<TooltipBubble text={tip?.text ?? ''} anchor={tip?.el ?? null} />

{#snippet rowView(r: Row, i: number)}
  {#if r.msg === 'new' && edit?.mode === 'create'}
    {@const ic = iconFor('', edit.kind === 'dir' ? 'd' : 'f', false)}
    <div class="row editing" role="presentation" style="transform: translateY({i * rowH}px); padding-left: {PAD + r.depth * INDENT + 16}px">
      <svg class="ic" style="color: var(--ft-ic-{ic.color})" width="16" height="16" aria-hidden="true"><use href="#{ic.id}" /></svg>
      <InlineName
        initial={edit.value}
        placeholder={edit.kind === 'dir' ? 'Folder name' : 'File name (a/b/c.ts makes folders)'}
        rules={fileTreeStore.editRules()}
        error={edit.error}
        pending={edit.pending}
        oncommit={(v) => void fileTreeStore.commitEdit(v)}
        oncancel={() => fileTreeStore.cancelEdit()}
      />
    </div>
  {:else if r.kind === 'm'}
    <div class="row msg msg-{r.msg}" role="presentation" style="transform: translateY({i * rowH}px); padding-left: {PAD + r.depth * INDENT + 16}px">
      <span class="name">{r.name}</span>
    </div>
  {:else}
    {@const expanded = fileTreeStore.isExpanded(r.key)}
    {@const expandable = isExpandable(r)}
    {@const ic = iconFor(r.name, r.kind, expanded)}
    {@const tabs = fileTreeStore.editorRels.get(r.key)}
    {@const dirty = !!tabs?.some((t) => isEditorDirty(t))}
    {@const focused = i === focusIdx}
    {@const typed = focused && prefix && r.name.toLowerCase().startsWith(prefix.toLowerCase()) ? prefix.length : 0}
    {@const folder = isFolderKind(r.kind)}
    {@const git = fileTreeStore.gitOf(r.key, folder)}
    {@const gcls = git ? gitClass(git.letter) : ''}
    {@const hl = typed ? undefined : fileTreeStore.highlightsFor(r.key)}
    {@const renaming = edit?.mode === 'rename' && edit.rel === r.key ? edit : null}
    {@const ag = fileTreeStore.rowAgent(r.key, folder, expanded)}
    {@const railLeft = Math.max(1, PAD + r.depth * INDENT - 4)}
    <!-- The container holds focus (aria-activedescendant); a row never does, so it has no
         tabindex: clicking it focuses the tree. -->
    <!-- svelte-ignore a11y_interactive_supports_focus -->
    <!-- svelte-ignore a11y_click_events_have_key_events -->
    <div
      id="ftr-{i}"
      class="row"
      class:focused
      class:tree-focused={focused && (hasFocus || !!fileTreeStore.find)}
      class:selected={fileTreeStore.selection.has(r.key)}
      class:ignored={r.flags & TREE_FLAG_IGNORED}
      class:hidden-file={r.flags & TREE_FLAG_HIDDEN}
      class:active-file={r.key === fileTreeStore.activeFile}
      class:drop-target={folder && r.key === fileTreeStore.dropTarget}
      class:cut={fileTreeStore.isCut(r.key)}
      class:editing={!!renaming}
      role="treeitem"
      aria-level={r.depth + 1}
      aria-expanded={expandable ? expanded : undefined}
      aria-selected={fileTreeStore.selection.has(r.key)}
      aria-label={ariaLabel(r, expanded, dirty, git, ag)}
      style="transform: translateY({i * rowH}px); padding-left: {PAD}px"
      onmousedown={(e) => { if (e.detail > 1) e.preventDefault(); }}
      onpointerdown={(e) => onRowPointerDown(e, r)}
      onclick={(e) => onRowClick(e, r)}
      ondblclick={() => onRowDblClick(r)}
      oncontextmenu={(e) => openMenu(e, r)}
      onmouseenter={(e) => onRowEnter(e, r)}
      onmouseleave={hideTip}
    >
      {#if ag?.rail}
        <!-- The rail (§9.3): the op's colour; it pulses while the call runs, fades after a
             completed write's sweep or a read's light. `--t0` is minus the mark's age, so a
             row scrolled back into view carries on rather than starting over. -->
        {@const t0 = `-${Math.max(0, Date.now() - ag.rail.t0)}ms`}
        <span class="ft-rail op-{ag.rail.cls} m-{ag.rail.mode}" style="left: {railLeft}px; --t0: {t0}" aria-hidden="true"></span>
        {#if ag.rail.mode === 'sweep' || ag.rail.mode === 'read'}
          <span class="ft-glow op-{ag.rail.cls} g-{ag.rail.mode}" style="--t0: {t0}" aria-hidden="true"></span>
        {/if}
      {:else if ag && ag.writers.length > 1}
        <!-- Two agents wrote it this turn: the rail splits into their colours. -->
        <span class="ft-rail split" style="left: {railLeft}px; --c1: var(--ft-agent-{fileTreeStore.colourOf(ag.writers[0])}); --c2: var(--ft-agent-{fileTreeStore.colourOf(ag.writers[1])})" aria-hidden="true"></span>
      {/if}
      {#if r.depth > 0}<span class="indent" style="width: {r.depth * INDENT}px"></span>{/if}
      {#if guide && i >= guide.start && i < guide.end}
        <span class="guide-hl" style="left: {PAD + guide.depth * INDENT + 6}px"></span>
      {/if}
      <!-- svelte-ignore a11y_click_events_have_key_events -->
      <!-- svelte-ignore a11y_no_static_element_interactions -->
      <span class="chev" class:open={expanded} onclick={(e) => { if (expandable) onChevron(e, r); }}>
        {#if expandable}<Icon name="chevron" size={11} />{/if}
      </span>
      <svg class="ic" style="color: var(--ft-ic-{ic.color})" width="16" height="16" aria-hidden="true"><use href="#{ic.id}" /></svg>
      <!-- A folder's roll-up is a dot, not a colour: the name stays readable over a big tree. -->
      {#if renaming}
        <InlineName
          initial={renaming.value}
          select={renameSelection(renaming.value, folder)}
          rules={fileTreeStore.editRules()}
          error={renaming.error}
          pending={renaming.pending}
          oncommit={(v) => void fileTreeStore.commitEdit(v)}
          oncancel={() => fileTreeStore.cancelEdit()}
        />
      {:else}
      <span class="name {git && !git.rollup ? gcls : ''}" class:ag-deleted={ag?.deleted}>{#if typed}<mark>{r.name.slice(0, typed)}</mark>{r.name.slice(typed)}{:else if hl}{#each highlightRuns(r.name, hl) as run, j (j)}{#if run.hit}<mark class="hit">{run.text}</mark>{:else}{run.text}{/if}{/each}{:else}{r.name}{/if}</span>
      {/if}
      <span class="gutter">
        {#if ag?.conflict}
          <!-- svelte-ignore a11y_click_events_have_key_events -->
          <!-- svelte-ignore a11y_no_static_element_interactions -->
          <span
            class="ft-conflict"
            class:actionable={ag.conflict.buffer}
            aria-hidden="true"
            onclick={(e) => { if (ag.conflict?.buffer) { e.stopPropagation(); fileTreeStore.humanTouched(); void fileTreeStore.compareBufferWithDisk(r.key); } }}
          >⚠</span>
        {/if}
        {#if ag?.badge}<span class="ft-badge" style="color: var(--ft-agent-{ag.badge.colour})" aria-hidden="true">{ag.badge.count}✎</span>{/if}
        {#if ag && ag.writers.length && !folder}
          <!-- Written this turn: a dot in each writer's colour (two at most). -->
          {#each ag.writers.slice(0, 2) as w (w)}<span class="ft-dot" style="background: var(--ft-agent-{fileTreeStore.colourOf(w)})" aria-hidden="true"></span>{/each}
        {/if}
        {#if r.key === fileTreeStore.youAreHere}<span class="here" aria-hidden="true"></span>{/if}
        {#if tabs}<span class="ed-dot" class:dirty aria-hidden="true"></span>{/if}
        {#if git && folder}<span class="git-dot {gcls}" aria-hidden="true"></span>
        {:else if git}<span class="git-letter {gcls}" aria-hidden="true">{git.letter === 'C' ? '!' : git.letter}</span>{/if}
      </span>
    </div>
  {/if}
{/snippet}

<div class="tree-wrap">
<div
  class="tree"
  bind:this={el}
  onwheel={() => fileTreeStore.humanTouched()}
  role="tree"
  tabindex="0"
  aria-label="Files"
  aria-multiselectable="true"
  aria-activedescendant={focusIdx >= 0 ? `ftr-${focusIdx}` : undefined}
  class:drop-root={fileTreeStore.dropTarget === ''}
  onscroll={onScroll}
  onkeydown={onKeydown}
  oncontextmenu={onTreeContext}
  onfocus={() => { hasFocus = true; if (focusIdx < 0 && rows.length) { const i = focusable(Math.floor(scrollTop / rowH), 1); if (i >= 0) fileTreeStore.setFocus(rows[i].key, false); } }}
  onblur={() => { hasFocus = false; }}
  onmouseleave={hideTip}
>
  <div class="sticky-wrap">
    <StickyAncestors {rows} indices={sticky} {rowH} dropTarget={fileTreeStore.dropTarget} onpick={(key) => { fileTreeStore.selectOnly(key); scrollIntoView(idxOf(key)); }} />
  </div>
  <div class="spacer" style="height: {rows.length * rowH}px">
    {#each visible as v (v.r.key)}
      {@render rowView(v.r, v.i)}
    {/each}
  </div>
</div>
<EdgeChips {rows} {scrollTop} {viewportH} {rowH} headerRows={sticky.length} onjump={(i) => scrollIntoView(i)} />
</div>

{#if menu}
  <ContextMenu items={menu.items} x={menu.x} y={menu.y} onclose={() => { menu = null; void tick().then(() => el?.focus({ preventScroll: true })); }} />
{/if}

<style>
  /* Holds the scrolling list and the edge chips over it (which must not scroll with it). */
  .tree-wrap {
    flex: 1;
    min-height: 0;
    display: flex;
    flex-direction: column;
    position: relative;
  }

  .tree {
    --ft-guide: color-mix(in srgb, var(--fg-dim) 35%, transparent);
    flex: 1;
    min-height: 0;
    overflow-y: auto;
    overflow-x: hidden;
    position: relative;
    outline: none;
  }

  /* Sticks to the top without taking room in the flow. */
  .sticky-wrap {
    position: sticky;
    top: 0;
    height: 0;
    z-index: 2;
  }

  .spacer {
    position: relative;
    width: 100%;
  }

  .row {
    position: absolute;
    top: 0;
    left: 0;
    right: 0;
    height: var(--ft-row-h, 22px);
    display: flex;
    align-items: center;
    gap: 4px;
    padding-right: 6px;
    box-sizing: border-box;
    cursor: default;
    user-select: none;
    color: var(--fg);
    outline: none;
  }

  .row:hover {
    background: color-mix(in srgb, var(--bg-light) 45%, transparent);
  }

  .row.selected {
    background: color-mix(in srgb, var(--accent) 18%, transparent);
  }

  .row.focused.tree-focused {
    box-shadow: inset 0 0 0 1px color-mix(in srgb, var(--accent) 70%, transparent);
  }

  .row.active-file:not(.selected) {
    background: color-mix(in srgb, var(--bg-light) 30%, transparent);
  }

  .row.ignored {
    opacity: 0.55;
  }

  /* Cut to the in-app clipboard: dimmed until pasted. */
  .row.cut {
    opacity: 0.45;
  }

  /* Where a drag would land: the folder row, or (below the rows, or on the header) the root. */
  .row.drop-target {
    background: color-mix(in srgb, var(--accent) 28%, transparent);
    box-shadow: inset 0 0 0 1px var(--accent);
  }

  .tree.drop-root {
    box-shadow: inset 0 0 0 2px color-mix(in srgb, var(--accent) 70%, transparent);
  }

  /* The input's error bubble hangs below the row: above its neighbours. */
  .row.editing {
    z-index: 3;
  }

  .row.msg {
    color: var(--fg-dim);
    font-style: italic;
  }

  .row.msg-error {
    color: var(--red);
  }

  .indent {
    flex-shrink: 0;
    align-self: stretch;
    margin-right: -4px;
    background-image: repeating-linear-gradient(to right, transparent 0 6px, var(--ft-guide) 6px 7px, transparent 7px 12px);
  }

  .guide-hl {
    position: absolute;
    top: 0;
    bottom: 0;
    width: 1px;
    background: color-mix(in srgb, var(--accent) 50%, transparent);
  }

  .chev {
    width: 12px;
    height: 100%;
    flex-shrink: 0;
    display: inline-flex;
    align-items: center;
    justify-content: center;
    color: var(--fg-dim);
  }

  .chev.open :global(svg) {
    transform: rotate(90deg);
  }

  .ic {
    flex-shrink: 0;
  }

  /* One line, ellipsized: a clipped name gets the hover bubble with the full path. */
  .name {
    flex: 1;
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  .name mark {
    background: color-mix(in srgb, var(--yellow) 35%, transparent);
    color: inherit;
    border-radius: 2px;
  }

  .gutter {
    flex-shrink: 0;
    display: inline-flex;
    align-items: center;
    gap: 5px;
  }

  /* You-are-here: the followed tab's folder (§3.2). Revealed, never selected. */
  .here {
    width: 3px;
    height: 12px;
    border-radius: 2px;
    background: var(--accent);
  }

  /* Open in an editor tab: hollow; unsaved: filled. */
  .ed-dot {
    width: 7px;
    height: 7px;
    border-radius: 50%;
    border: 1.5px solid var(--fg-dim);
    box-sizing: border-box;
  }

  .ed-dot.dirty {
    background: var(--fg);
    border-color: var(--fg);
  }

  /* Find's matched letters: the same mark as type-ahead, a little stronger. */
  .name mark.hit {
    background: color-mix(in srgb, var(--accent) 30%, transparent);
    font-weight: 600;
  }

  /* Git (§7.3): the name's colour and a trailing letter; a folder's roll-up is a dot. */
  .g-mod { --git-c: var(--yellow); }
  .g-add { --git-c: var(--green); }
  .g-del { --git-c: var(--red); }
  .g-ren { --git-c: var(--cyan); }
  .g-conf { --git-c: var(--magenta); }

  .name.g-mod,
  .name.g-add,
  .name.g-del,
  .name.g-ren,
  .name.g-conf {
    color: color-mix(in srgb, var(--git-c) 85%, var(--fg));
  }

  .name.g-del {
    text-decoration: line-through;
  }

  .name.g-conf {
    font-weight: 600;
  }

  .git-letter {
    min-width: 10px;
    text-align: center;
    font-size: 0.85em;
    font-weight: 600;
    font-family: var(--font-mono, ui-monospace, monospace);
    color: var(--git-c);
  }

  .git-dot {
    width: 6px;
    height: 6px;
    border-radius: 50%;
    background: var(--git-c);
    margin: 0 2px;
  }

  .row.msg-empty {
    font-style: normal;
  }

  /* Agents (§9.3; tokens and keyframes in app.css). The rail is the op, the dot the agent. */
  .ft-rail {
    position: absolute;
    top: 2px;
    bottom: 2px;
    width: 2px;
    border-radius: 1px;
    background: var(--rail-c);
    pointer-events: none;
  }

  .op-read { --rail-c: var(--ft-read); }
  .op-write { --rail-c: var(--ft-write); }
  .op-create { --rail-c: var(--ft-create); }
  .op-delete { --rail-c: var(--ft-delete); }

  /* Editing now: pulses between Pre and Post. */
  .ft-rail.m-pending {
    animation: ft-pulse var(--ft-pulse-ms) ease-in-out infinite;
    animation-delay: var(--t0, 0ms);
  }

  .ft-rail.m-sweep {
    animation: ft-fade var(--ft-sweep-ms) ease-out forwards;
    animation-delay: var(--t0, 0ms);
  }

  /* Reads light faintly and leave nothing behind. */
  .ft-rail.m-read {
    opacity: 0.7;
    animation: ft-fade var(--ft-read-ms) linear forwards;
    animation-delay: var(--t0, 0ms);
  }

  .ft-rail.m-failed {
    background: transparent;
    box-shadow: 0 0 0 1px var(--ft-fail);
  }

  .ft-rail.split {
    background: linear-gradient(to bottom, var(--c1) 50%, var(--c2) 50%);
  }

  /* A completed write's sweep, and a read's faint light, across the row. */
  .ft-glow {
    position: absolute;
    inset: 0;
    pointer-events: none;
    will-change: transform, opacity;
  }

  .ft-glow.g-sweep {
    background: linear-gradient(90deg, transparent, color-mix(in srgb, var(--rail-c) 30%, transparent), transparent);
    animation: ft-sweep var(--ft-sweep-ms) ease-out forwards;
    animation-delay: var(--t0, 0ms);
  }

  .ft-glow.g-read {
    background: color-mix(in srgb, var(--ft-read) 9%, transparent);
    animation: ft-fade var(--ft-read-ms) linear forwards;
    animation-delay: var(--t0, 0ms);
  }

  /* Reduced motion: nothing moves or pulses; a static tint for as long as it would have run. */
  @media (prefers-reduced-motion: reduce) {
    .ft-rail.m-pending {
      animation: none;
    }

    .ft-rail.m-sweep,
    .ft-rail.m-read,
    .ft-glow.g-sweep,
    .ft-glow.g-read {
      animation-name: ft-hold;
      animation-timing-function: steps(1, end);
    }
  }

  .name.ag-deleted {
    text-decoration: line-through;
    color: color-mix(in srgb, var(--ft-delete) 80%, var(--fg));
  }

  /* Written this turn: a small persistent dot in the agent's colour. */
  .ft-dot {
    width: 6px;
    height: 6px;
    border-radius: 50%;
    flex-shrink: 0;
  }

  /* A collapsed folder over activity: never opened for it. */
  .ft-badge {
    font-size: 0.8em;
    font-weight: 600;
    font-variant-numeric: tabular-nums;
  }

  .ft-conflict {
    color: var(--red);
    font-weight: 700;
    font-size: 0.9em;
  }

  .ft-conflict.actionable {
    cursor: pointer;
  }
</style>
