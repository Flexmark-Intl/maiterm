/**
 * The Files column's store, one per window (docs/file-tree.md §4, §10.5, §12.4).
 *
 * Rust owns the truth (listings, generations, watching); this store owns the VIEW: which
 * root, what is open, the selection, focus and scroll, and the flattened rows. It talks to
 * Rust over one Channel per subscription (`ftOpen`), applies what arrives to a `TreeModel`,
 * and copies the model's rows into a `$state.raw` so the virtual list re-renders by identity.
 *
 * Rules carried from the spec:
 * - The human owns the view (§1). Nothing here opens or closes the column, and following a
 *   tab never selects anything: the you-are-here folder is revealed, not selected.
 * - Idle is free: the root is re-resolved only on edges (tab activation, a prompt, a cwd or
 *   title change, the tab's agent starting), debounced 150 ms, never on a timer.
 * - Inside the root, reveal; outside, re-root. `cd src/lib` never reloads anything.
 * - Editor and diff tabs never re-root; their file is revealed if it is under the root.
 * - Switching roots is instant: the last 8 roots keep their model (with its generations),
 *   so coming back costs one `ftOpen` whose folders answer `not_modified`.
 * - Preview tabs live here in memory only, never on a Tab (§10.5).
 * - Git decorations live on the model and never touch `rows` (§12.2): only the mounted rows
 *   read them, through `gitVersion`.
 * - Find and the Changed lens draw a PRUNED tree (`prunedRows`) in place of the model's rows,
 *   with an open/closed state of their own: the real tree's expansion is never touched, so
 *   leaving restores it, its scroll and its selection exactly (§8).
 */
import { Channel } from '@tauri-apps/api/core';
import { listen, type UnlistenFn } from '@tauri-apps/api/event';
import { homeDir } from '@tauri-apps/api/path';
import { untrack } from 'svelte';
import { SvelteMap, SvelteSet } from 'svelte/reactivity';
import { writeText as clipboardWriteText } from '@tauri-apps/plugin-clipboard-manager';
import { error as logError, info as logInfo } from '@tauri-apps/plugin-log';
import * as commands from '$lib/tauri/commands';
import type { BranchInfo, DirReq, FileOp, GitLetter, OpItem, OpResult, OpenAck, TreeEnvelope, TreePlace, TreeSearchHit } from '$lib/tauri/types';
import { TREE_FLAG_IGNORED } from '$lib/tauri/types';
import { TreeModel, ancestorsOf, baseName, dropAccepted, isExpandable, isFolderKind, isWithin, joinRel, matchHighlights, parentRel, prunedRows, revealPlan, type RevealKind, type Row } from '$lib/filetree/model';
import { nameProblem, siblingLookup, type NameRules } from '$lib/filetree/names';
import { UndoStacks, describeResult, trashLabel, trashPlace, undoLabel, type Lookup, type Platform, type UndoEntry } from '$lib/filetree/undo';
import type { DropTarget } from '$lib/filetree/dnd';
import { relocateStore } from '$lib/stores/relocate.svelte';
import { attachToComposer } from '$lib/components/terminal/ComposerDock.svelte';
import { isMac, isWindows } from '$lib/utils/platform';
import { openGitDiff } from '$lib/utils/openGitDiff';
import { relToAbs, lastSegment, tildePath } from '$lib/filetree/paths';
import { settleActiveFile, type ActiveFileTarget } from '$lib/filetree/activeFile';
import { workspacesStore } from '$lib/stores/workspaces.svelte';
import { terminalsStore } from '$lib/stores/terminals.svelte';
import { activityStore } from '$lib/stores/activity.svelte';
import { preferencesStore } from '$lib/stores/preferences.svelte';
import { toastStore } from '$lib/stores/toasts.svelte';
import { getEditorByTabId, isEditorDirty } from '$lib/stores/editorRegistry.svelte';
import { openInEditor, editorFileInfo } from '$lib/utils/openInEditor';
import { insertPathsIntoTab } from '$lib/utils/insertPaths';

/** What the column shows when it isn't a tree. */
export type TreeStatus = 'none' | 'resolving' | 'opening' | 'ready' | 'missing' | 'remote' | 'gone' | 'error';

export type OpenMode = 'preview' | 'pin' | 'side';

/** The inline New or Rename input (§10.4). */
export type EditState =
  | { mode: 'create'; kind: 'file' | 'dir'; parent: string; error: string | null; pending: boolean; value: string }
  | { mode: 'rename'; rel: string; error: string | null; pending: boolean; value: string };

/** One button on the ConfirmStrip. */
export interface ConfirmAction {
  label: string;
  tone?: 'danger' | 'primary';
  run: () => void | Promise<unknown>;
}

/** The footer's question (§10.4): `confirm()` is dead in the webview. */
export interface ConfirmState {
  text: string;
  actions: ConfirmAction[];
}

/** The footer's last op result, with Undo (or "Show in Trash" where there is no undo). */
export interface OpNotice {
  text: string;
  action: { label: string; run: () => void } | null;
}

/** How long an op's result stays in the footer. */
const NOTICE_MS = 10_000;

const PLATFORM: Platform = isMac() ? 'mac' : isWindows() ? 'windows' : 'linux';

interface ViewState {
  model: TreeModel;
  scrollTop: number;
  selection: string[];
  focus: string | null;
  anchor: string | null;
}

/** What the tree body draws: the real tree, find's results, or the changed files. */
export type ViewMode = 'tree' | 'find' | 'changed';

/** Where the view was before a lens or find replaced it: put back exactly on leaving. */
interface SavedView {
  scrollTop: number;
  selection: string[];
  focus: string | null;
  anchor: string | null;
}

interface FindState {
  query: string;
  hits: TreeSearchHit[];
  /** rel → matched code-point positions in that row's name. */
  highlights: Map<string, number[]>;
  indexing: boolean;
  unavailable: string | null;
  /** A query is out and nothing has come back for it yet. */
  pending: boolean;
}

/** Results asked of Rust per query (§8). */
const FIND_LIMIT = 500;

/** How long a re-resolve waits for the edges to settle (§4.2). */
const RESOLVE_DEBOUNCE_MS = 150;
/** Roots whose view (model, scroll, selection) is kept for an instant switch back. */
const ROOT_LRU = 8;
/** Ack backpressure (§5.4): every 32nd message, or when 100 ms passed since the last ack. */
const ACK_EVERY = 32;
const ACK_AFTER_MS = 100;
/** A recursive expand stops here and says so (§11.2). */
const RECURSIVE_ROW_BUDGET = 2000;
/** Revalidate open folders when the window comes back after this long (§6.6). */
const REFOCUS_REVALIDATE_MS = 60_000;
/** The human touched the tree this recently: agents' reveals wait (§9.3). */
export const HUMAN_QUIET_MS = 3000;
/** Expanded sets kept in localStorage: places × paths (§12.4). */
const EXP_PLACES = 64;
const EXP_PATHS = 512;
const RECENT_MAX = 5;

// ── localStorage: a per-viewer convenience, every access guarded ───────────────────────────

function lsGet<T>(key: string, fallback: T): T {
  try {
    const raw = localStorage.getItem(key);
    return raw ? (JSON.parse(raw) as T) : fallback;
  } catch {
    return fallback;
  }
}

function lsSet(key: string, value: unknown): void {
  try {
    localStorage.setItem(key, JSON.stringify(value));
  } catch {
    /* full or blocked: losing it costs one re-expand */
  }
}

function lsRemove(key: string): void {
  try {
    localStorage.removeItem(key);
  } catch {
    /* ignore */
  }
}

function saveExpanded(place: string, expanded: Set<string>): void {
  const paths = [...expanded].filter((p) => p !== '').slice(0, EXP_PATHS);
  const index = lsGet<string[]>('ft:exp:index', []).filter((p) => p !== place);
  index.unshift(place);
  for (const old of index.splice(EXP_PLACES)) lsRemove(`ft:exp:${old}`);
  lsSet('ft:exp:index', index);
  lsSet(`ft:exp:${place}`, paths);
}

function loadExpanded(place: string): string[] {
  const v = lsGet<unknown>(`ft:exp:${place}`, []);
  return Array.isArray(v) ? v.filter((x): x is string => typeof x === 'string') : [];
}

function createFileTreeStore() {
  // ── State ──────────────────────────────────────────────────────────────────────────────
  let status = $state<TreeStatus>('none');
  let statusText = $state<string | null>(null);
  let place = $state<TreePlace | null>(null);
  let placeWorkspaceId: string | null = null;
  let sourceTabId = $state<string | null>(null);
  let ack = $state<OpenAck | null>(null);
  let subId: number | null = null;
  let rows = $state.raw<Row[]>([]);
  let health = $state<{ watch: string; git: string; note: string | null } | null>(null);
  let focused = $state(false);
  let focusKey = $state<string | null>(null);
  let anchorKey = $state<string | null>(null);
  const selection = new SvelteSet<string>();
  /** paneId → the preview editor tab in it. In memory only (§10.5). */
  const previewTabs = new SvelteMap<string, string>();
  let youAreHere = $state<string | null>(null);
  /** The active editor's file, when it is under the root (a highlight, never a selection). */
  let activeFile = $state<string | null>(null);
  /** The active editor's file is outside the root: the header offers "Show". */
  let outsideFile = $state<string | null>(null);
  let announcement = $state('');
  let home: string | null = null;
  /** Bumped to ask TreeBody to scroll a row into view / restore a scroll offset / focus. */
  let scrollRequest = $state<{ key: string | null; top?: number; seq: number }>({ key: null, seq: 0 });
  /** The tree should take the keyboard as soon as it can. A flag the tree consumes, not a
   *  counter: a counter left over from an earlier request would pull focus into the tree
   *  every time it remounts (the column reopened from the titlebar, say). */
  let focusPending = $state(false);
  let lastHumanInputMs = 0;
  /** Bumped per active-file lookup: only the newest may set the highlight. */
  let activeSeq = 0;

  /** Raw state: the tree reads it, and a reroot swaps it for another root's model while the
   *  tree stays mounted, so a plain `let` would leave the tree drawing the old root. */
  let model = $state.raw<TreeModel | null>(null);
  /** Bumped whenever `model.expanded` changes. The set is plain (the model is pure), and an
   *  empty folder opens or closes without changing `rows`, so the chevron needs this. */
  let expandedVersion = $state(0);
  const roots = new Map<string, ViewState>();
  let scrollTop = 0;
  let openSeq = 0;
  let resolveSeq = 0;
  let resolveTimer: ReturnType<typeof setTimeout> | null = null;
  let resolveFresh = false;
  let attached = false;
  let unlisteners: (() => void)[] = [];
  let lastOscKey = new Map<string, string>();
  let blurredAt: number | null = null;
  let acked = { count: 0, at: 0 };
  let saveExpTimer: ReturnType<typeof setTimeout> | null = null;
  /** A row to scroll to (and optionally select) once its listing arrives. */
  let pendingReveal: { rel: string; select: boolean } | null = null;
  /** An Alt+→ in progress: the folder it started from and the row count it stops at. */
  let recursive: { root: string; budget: number } | null = null;

  // Git (§7): the decorations live on the model; these are what the branch row reads.
  let gitVersion = $state(0);
  let gitState = $state('off');
  let branch = $state<BranchInfo | null>(null);
  let gitCount = $state(0);

  // Find and the Changed lens (§8, §9.4).
  let lens = $state<'all' | 'changed'>('all');
  let find = $state.raw<FindState | null>(null);
  /** The find bar's input should take the keyboard (`/`, Cmd+F with the bar already open). */
  let findFocusSeq = $state(0);
  let searchSeq = 0;
  /** The pruned tree's rows, and the folders closed in it (open by default). */
  let lensRows: Row[] = [];
  const lensCollapsed = new Set<string>();
  /** The view to put back when find (or the Changed lens) gives way. */
  let savedForFind: SavedView | null = null;
  let savedForChanged: SavedView | null = null;

  // File operations (§10).
  let edit = $state<EditState | null>(null);
  let confirm = $state<ConfirmState | null>(null);
  let notice = $state<OpNotice | null>(null);
  let noticeTimer: ReturnType<typeof setTimeout> | null = null;
  /** The tree's own clipboard of files (Cmd/Ctrl+C / X); never the OS clipboard. */
  let clip = $state<{ place: string; rels: string[]; abs: string[]; cut: boolean } | null>(null);
  /** The folder a drag (or an OS drop) would land in now: '' the root, null none. */
  let dropTarget = $state<string | null>(null);
  const undo = new UndoStacks();

  homeDir().then((h) => { home = h; }).catch(() => {});

  // ── Small helpers ──────────────────────────────────────────────────────────────────────

  /** Called after every change to the model (expand, collapse, reveal, a message). Bumping
   *  `expandedVersion` each time re-reads only the visible rows' open state, which is cheap,
   *  and no path that opens or closes a folder can forget to. */
  function syncRows() {
    if (viewMode() === 'tree') {
      if (model && rows !== model.rows) rows = model.rows;
    } else if (rows !== lensRows) {
      rows = lensRows;
    }
    // Untracked: `++` reads the counter, and a caller inside an effect must not subscribe.
    untrack(() => { expandedVersion++; });
  }

  // ── Find and the Changed lens ─────────────────────────────────────────────────────────

  /** Find with a query wins; then the Changed lens; else the real tree. Read untracked by
   *  callers inside effects. */
  function viewMode(): ViewMode {
    if (find && find.query.trim()) return 'find';
    if (lens === 'changed') return 'changed';
    return 'tree';
  }

  function snapshotView(): SavedView {
    return { scrollTop, selection: [...selection], focus: focusKey, anchor: anchorKey };
  }

  function restoreView(s: SavedView | null) {
    if (!s) return;
    selection.clear();
    for (const k of s.selection) selection.add(k);
    focusKey = s.focus;
    anchorKey = s.anchor;
    scrollTop = s.scrollTop;
    scrollRequest = { key: null, top: s.scrollTop, seq: scrollRequest.seq + 1 };
  }

  function scrollToTop() {
    scrollTop = 0;
    scrollRequest = { key: null, top: 0, seq: scrollRequest.seq + 1 };
  }

  /** Re-derive the pruned tree for the current mode and show it. */
  function rebuildLens() {
    const m = model;
    const mode = viewMode();
    if (!m || mode === 'tree') {
      lensRows = [];
      syncRows();
      return;
    }
    const entryOf = (rel: string) => m.entry(rel);
    const showHidden = preferencesStore.fileTreeShowHidden;
    if (mode === 'find' && find) {
      const items = find.hits.map((h) => ({ rel: h.rel, kind: m.entry(h.rel)?.k ?? ('f' as const) }));
      const emptyText = find.unavailable ?? (find.indexing || find.pending ? 'Indexing…' : 'No matches');
      lensRows = prunedRows(items, lensCollapsed, entryOf, { showHidden, emptyText });
    } else {
      const emptyText = m.gitTruncated ? 'Too many changes to list here: changed folders are marked in the tree' : 'No changes';
      lensRows = prunedRows(m.changedItems(), lensCollapsed, entryOf, { showHidden, emptyText });
    }
    syncRows();
  }

  /** Moving between modes: save the view being left, put back the one returned to. */
  function changeMode(from: ViewMode, to: ViewMode) {
    if (from === to) return;
    if (to === 'find') savedForFind = snapshotView();
    else if (from === 'find') {
      restoreView(savedForFind);
      savedForFind = null;
    }
    if (to === 'changed' && from === 'tree') savedForChanged = snapshotView();
    else if (to === 'tree' && from === 'changed') {
      restoreView(savedForChanged);
      savedForChanged = null;
    }
    if (to !== 'tree') {
      lensCollapsed.clear();
      if (from === 'tree' || to === 'find') scrollToTop();
    }
  }

  function openFind() {
    if (!model) return;
    if (!find) find = { query: '', hits: [], highlights: new Map(), indexing: false, unavailable: null, pending: false };
    findFocusSeq++;
  }

  /** Escape (or the bar's close button): the view goes back exactly as it was. */
  function closeFind() {
    if (!find) return;
    const from = viewMode();
    find = null;
    searchSeq++;
    changeMode(from, viewMode());
    rebuildLens();
    focusPending = true;
  }

  function setFindQuery(q: string) {
    if (!find) return;
    const from = viewMode();
    find = { ...find, query: q, pending: !!q.trim(), ...(q.trim() ? {} : { hits: [], highlights: new Map(), unavailable: null }) };
    changeMode(from, viewMode());
    rebuildLens();
    if (q.trim()) void runSearch();
    else searchSeq++;
  }

  async function runSearch(indexShallow = false) {
    const sub = subId;
    const f = find;
    if (!f || sub === null || !f.query.trim()) return;
    const q = f.query;
    const seq = ++searchSeq;
    let r;
    try {
      r = await commands.ftSearch(sub, q, FIND_LIMIT, seq, indexShallow);
    } catch (e) {
      logError(`[files] search: ${e}`);
      return;
    }
    if (seq !== searchSeq || sub !== subId || !find || find.query !== q || r.seq !== seq) return;
    find = { ...find, hits: r.items, highlights: matchHighlights(r.items), indexing: r.indexing, unavailable: r.unavailable, pending: false };
    lensCollapsed.clear();
    rebuildLens();
    // The best hit takes focus (not selection), so Enter opens it and the arrows start there.
    const best = r.items[0]?.rel ?? null;
    if (best && !rows.some((x) => x.key === focusKey && x.kind !== 'm')) {
      focusKey = best;
      requestScroll(best);
    }
  }

  /** Find's ↑/↓ from the input: the focus moves over the results, the input keeps the keys. */
  function findMove(delta: 1 | -1) {
    const rs = rows;
    let i = focusKey ? rs.findIndex((r) => r.key === focusKey) : -1;
    for (let n = 0; n < rs.length; n++) {
      i = i < 0 ? (delta > 0 ? 0 : rs.length - 1) : i + delta;
      if (i < 0 || i >= rs.length) return;
      if (rs[i].kind !== 'm') break;
    }
    const r = rs[i];
    if (!r || r.kind === 'm') return;
    selectOnly(r.key);
    requestScroll(r.key);
  }

  /** Find's Enter: a file opens (and keeps its tab), a folder opens or closes in the results. */
  function findActivate() {
    const r = rows.find((x) => x.key === focusKey);
    if (!r || r.kind === 'm') return;
    if (isFolderKind(r.kind)) {
      toggle(r.key);
      return;
    }
    void openFile(r.key, 'pin');
  }

  /** The branch row's "N changed": the Changed lens on or off. */
  function toggleChanged() {
    const from = viewMode();
    const wasOn = lens === 'changed';
    lens = wasOn ? 'all' : 'changed';
    const to = viewMode();
    if (from === 'find' && to === 'find') {
      // The branch row stays clickable under find with a query, which stays in front: the
      // lens changed the view BENEATH it, so what Escape puts back moves with it. Off: the
      // tree's own view (held for the lens) is now the one under find. On: the tree's view
      // is held for the lens, and find will close onto the lens, from its top.
      if (wasOn) {
        savedForFind = savedForChanged;
        savedForChanged = null;
      } else {
        savedForChanged = savedForFind;
        savedForFind = savedForFind ? { ...savedForFind, scrollTop: 0 } : null;
      }
    } else {
      changeMode(from, to);
    }
    rebuildLens();
  }

  function toggle(rel: string) {
    if (!model) return;
    if (viewMode() !== 'tree') {
      if (lensCollapsed.has(rel)) lensCollapsed.delete(rel);
      else lensCollapsed.add(rel);
      rebuildLens();
      return;
    }
    if (model.expanded.has(rel)) collapse(rel);
    else expand(rel);
  }

  async function compareWithHead(rel: string) {
    const abs = absOf(rel);
    const sub = subId;
    if (!abs || sub === null) return;
    try {
      await openGitDiff({ subId: sub, abs }, rel, 'HEAD');
    } catch (e) {
      toastStore.addToast('Couldn’t compare with HEAD', e instanceof Error ? e.message : String(e), 'error');
    }
  }

  function announce(text: string) {
    // Re-set even when unchanged so a screen reader repeats it.
    announcement = '';
    queueMicrotask(() => { announcement = text; });
  }

  function requestScroll(key: string) {
    scrollRequest = { key, seq: scrollRequest.seq + 1 };
  }

  function scheduleSaveExpanded() {
    if (saveExpTimer) clearTimeout(saveExpTimer);
    saveExpTimer = setTimeout(() => {
      saveExpTimer = null;
      if (model && place) saveExpanded(place.place, model.expanded);
    }, 500);
  }

  function sendOpen(open: DirReq[]) {
    if (subId !== null && open.length) commands.ftExpand(subId, open).catch((e) => logError(`[files] expand: ${e}`));
  }

  function sendClose(close: string[]) {
    if (subId !== null && close.length) commands.ftCollapse(subId, close).catch((e) => logError(`[files] collapse: ${e}`));
  }

  function rememberRecent(p: string) {
    const list = lsGet<string[]>('ft:recent', []).filter((x) => x !== p);
    list.unshift(p);
    lsSet('ft:recent', list.slice(0, 12));
  }

  // ── Resolving the root (§4) ───────────────────────────────────────────────────────────

  /** The tab the tree follows: the active pane's active terminal tab, never a service. */
  function followableTab(): string | null {
    const t = workspacesStore.activeTab;
    if (t && (t.tab_type ?? 'terminal') === 'terminal' && !t.service_id) return t.id;
    return null;
  }

  /** With an editor or board in front and no root yet, any terminal tab in the workspace
   *  (active pane first) gives the tree somewhere to start. */
  function fallbackTab(): string | null {
    const ws = workspacesStore.activeWorkspace;
    if (!ws) return null;
    const panes = [...ws.panes].sort((a, b) => (a.id === ws.active_pane_id ? -1 : b.id === ws.active_pane_id ? 1 : 0));
    for (const p of panes) {
      const t = p.tabs.find((x) => (x.tab_type ?? 'terminal') === 'terminal' && !x.service_id);
      if (t) return t.id;
    }
    return null;
  }

  function scheduleResolve(fresh = false) {
    if (!attached) return;
    resolveFresh ||= fresh;
    if (resolveTimer) clearTimeout(resolveTimer);
    resolveTimer = setTimeout(() => {
      resolveTimer = null;
      const f = resolveFresh;
      resolveFresh = false;
      void resolveNow(f);
    }, RESOLVE_DEBOUNCE_MS);
  }

  async function resolveNow(fresh: boolean, force = false) {
    if (!attached || !preferencesStore.fileTreeEnabled) return;
    // Every outcome that acts takes a token, so an older resolve still in flight yields to
    // it. The follow-off no-op does not: it would supersede a forced resolve (Reveal in
    // Files) and then do nothing, leaving the tree on the previous tab's folder. Because
    // that no-op cancels nothing, a resolve also checks after its await that its workspace
    // is still the one in front: leave a workspace and come back while its resolve is in
    // flight, and the no-op on return would otherwise let it re-root the tree onto the
    // workspace just left.
    const ws = workspacesStore.activeWorkspace;
    if (!ws) {
      ++resolveSeq;
      status = 'none';
      statusText = null;
      return;
    }
    const sameWorkspace = placeWorkspaceId === ws.id;
    let tabId = followableTab();
    if (!tabId) {
      // An editor, diff or board tab never re-roots (§4.2); its file is highlighted by
      // `syncActiveFile`, which follows the tab rather than these resolves.
      if (place && sameWorkspace && !force) {
        ++resolveSeq;
        return;
      }
      tabId = fallbackTab();
    }
    if (!tabId) {
      ++resolveSeq;
      if (!place || !sameWorkspace) {
        status = 'none';
        statusText = 'Open a terminal tab, or pick a folder.';
      }
      return;
    }
    // Follow off: the root stays where it is until the human moves it, unless there is none
    // yet or the workspace changed (a pin is per workspace).
    if (!force && !preferencesStore.fileTreeFollow && place && sameWorkspace) return;
    const token = ++resolveSeq;
    const superseded = () =>
      token !== resolveSeq || workspacesStore.activeWorkspace?.id !== ws.id;

    const osc = terminalsStore.getOsc(tabId);
    if (!place) status = 'resolving';
    let p: TreePlace;
    try {
      p = await commands.ftResolvePlace(tabId, {
        osc_cwd: osc?.cwd ?? null,
        osc_host: osc?.cwdHost ?? null,
        prompt_cwd: osc?.promptCwd ?? null,
        fresh,
      });
    } catch (e) {
      if (superseded()) return;
      if (!place || !sameWorkspace) {
        closeSub();
        place = null;
        status = 'error';
        statusText = String(e);
      }
      return;
    }
    if (superseded()) return;
    sourceTabId = tabId;
    placeWorkspaceId = ws.id;
    await adoptPlace(p, token);
  }

  /** `token` is the resolve that found `p`: a newer resolve supersedes it across the await. */
  async function adoptPlace(p: TreePlace, token: number) {
    if (p.missing) {
      saveView();
      closeSub();
      place = p;
      status = 'missing';
      statusText = p.missing;
      return;
    }
    if (p.kind !== 'local') {
      saveView();
      closeSub();
      place = p;
      status = 'remote';
      statusText = null;
      return;
    }
    if (place && place.place === p.place && subId !== null) {
      place = p;
      moveYouAreHere(p.cwd_rel);
      return;
    }
    // Inside the current root: only the tick moves (`cd src/lib` reloads nothing). Pins
    // always come back as their own place, so this is the follow case only.
    if (place && subId !== null && ack && p.source !== 'pin' && place.source !== 'pin') {
      const cwdAbs = relToAbs({ root_native: p.root, sep: ack.sep }, p.cwd_rel ?? '');
      const sub = subId;
      const located = await commands.ftLocate(sub, [cwdAbs]).catch(() => [null]);
      // A newer resolve (or a reroot under it) took over while this was asking: its rel was
      // computed under the old root, and its failure may only mean the sub was closed, so
      // neither may move the tick or re-root over the newer place.
      if (token !== resolveSeq || sub !== subId) return;
      if (located[0] != null) {
        moveYouAreHere(located[0] || null);
        return;
      }
    }
    await reroot(p);
  }

  function moveYouAreHere(rel: string | null) {
    if (youAreHere === rel) return;
    youAreHere = rel;
    if (rel) revealRel(rel, 'auto');
  }

  // ── Subscriptions ──────────────────────────────────────────────────────────────────────

  function saveView() {
    if (!model || !place || place.kind !== 'local' || place.missing) return;
    roots.delete(place.place);
    roots.set(place.place, {
      model,
      scrollTop,
      selection: [...selection],
      focus: focusKey,
      anchor: anchorKey,
    });
    while (roots.size > ROOT_LRU) roots.delete(roots.keys().next().value!);
    saveExpanded(place.place, model.expanded);
  }

  function closeSub() {
    openSeq++;
    if (subId !== null) {
      const id = subId;
      commands.ftClose(id).catch(() => {});
    }
    subId = null;
    ack = null;
    health = null;
    pendingReveal = null;
    recursive = null;
    gitState = 'off';
    branch = null;
    gitCount = 0;
    gitVersion++;
    // A query and a lens belong to the root they were made in.
    searchSeq++;
    find = null;
    lens = 'all';
    savedForFind = null;
    savedForChanged = null;
    lensRows = [];
    // An edit, a question and a result all name rels of the root being left. An unfinished
    // name is dropped, never committed (§10.4).
    edit = null;
    confirm = null;
    dropTarget = null;
    setNotice(null);
  }

  function newModel(p: TreePlace): TreeModel {
    const m = new TreeModel({
      showHidden: preferencesStore.fileTreeShowHidden,
      showIgnored: preferencesStore.fileTreeShowIgnored,
    });
    // The folders this viewer had open last time, followed again where every parent is.
    for (const rel of loadExpanded(p.place)) m.expanded.add(rel);
    for (const rel of m.expanded) {
      if (rel !== '' && ancestorsOf(rel).every((a) => m.expanded.has(a))) m.interest.add(rel);
    }
    return m;
  }

  async function reroot(p: TreePlace) {
    saveView();
    closeSub();
    const token = openSeq;
    const kept = roots.get(p.place);
    roots.delete(p.place);
    model = kept?.model ?? newModel(p);
    model.setFilters(preferencesStore.fileTreeShowHidden, preferencesStore.fileTreeShowIgnored);
    // A kept model's decorations came from the old subscription; the new one starts with a
    // full update of its own (or none, with decorations off).
    model.clearGit();
    selection.clear();
    for (const k of kept?.selection ?? []) selection.add(k);
    focusKey = kept?.focus ?? null;
    anchorKey = kept?.anchor ?? null;
    scrollTop = kept?.scrollTop ?? 0;
    scrollRequest = { key: null, top: scrollTop, seq: scrollRequest.seq + 1 };
    place = p;
    youAreHere = p.cwd_rel;
    activeFile = null;
    outsideFile = null;
    status = 'opening';
    statusText = null;
    syncRows();
    rememberRecent(p.place);

    // Events can arrive before `ftOpen` resolves: they are held until the subscription id
    // is known, since resyncs and acks need it.
    const early: TreeEnvelope[] = [];
    let ready = false;
    const channel = new Channel<TreeEnvelope>();
    channel.onmessage = (env) => {
      if (token !== openSeq) return;
      if (!ready) early.push(env);
      else handle(env);
    };
    // The tree is on screen while ftOpen runs, and an expand or reveal in that window has
    // no subscription to send to (`sendOpen` drops it). What was asked here is remembered,
    // so whatever the model started following meanwhile is sent once the sub exists.
    const m = model;
    const restore = m.restoreReqs();
    const asked = new Set(restore.map((r) => r.rel));
    let opened: OpenAck;
    try {
      opened = await commands.ftOpen(p.place, restore, channel);
    } catch (e) {
      if (token !== openSeq) return;
      status = 'error';
      statusText = String(e);
      return;
    }
    if (token !== openSeq) {
      commands.ftClose(opened.sub_id).catch(() => {});
      return;
    }
    subId = opened.sub_id;
    ack = opened;
    m.nfcNames = opened.nfc_names;
    acked = { count: 0, at: performance.now() };
    status = 'ready';
    ready = true;
    sendOpen(m.restoreReqs().filter((r) => !asked.has(r.rel)));
    sendClose([...asked].filter((r) => r !== '' && !m.interest.has(r)));
    for (const env of early) handle(env);
    if (typeof document !== 'undefined' && document.hidden) commands.ftPause(opened.sub_id, true).catch(() => {});
    if (youAreHere && !kept) revealRel(youAreHere, 'auto');
    // The active file is placed by FilesColumn's effect, which re-runs on the new `ack`.
  }

  function handle(env: TreeEnvelope) {
    const m = model;
    const sub = subId;
    if (!m || sub === null) return;
    switch (env.t) {
      case 'dir': {
        const r = m.applyDir(env);
        if (r === 'resync') commands.ftRefresh(sub, env.rel).catch(() => {});
        else if (env.last) {
          onListed(env.rel);
          noteDiskChange(env.rel, null);
          pruneEdit();
        }
        break;
      }
      case 'not_modified': {
        const r = m.applyNotModified(env);
        if (r === 'resync') commands.ftRefresh(sub, env.rel).catch(() => {});
        else onListed(env.rel);
        break;
      }
      case 'delta': {
        if (m.applyDelta(env) === 'resync') commands.ftRefresh(sub, env.rel).catch(() => {});
        else {
          if ((selection.size || focusKey) && viewMode() === 'tree') pruneSelection();
          pruneEdit();
          noteDiskChange(env.rel, [...env.add.map((e) => e.n), ...env.del, ...env.chg.map((e) => e.n)]);
        }
        break;
      }
      case 'dir_error': {
        const r = m.applyError(env);
        sendClose(r.close);
        break;
      }
      case 'health':
        health = env.note ? { watch: env.watch, git: env.git, note: env.note } : null;
        gitState = env.git;
        if (env.git === 'off' && lens === 'changed') toggleChanged();
        break;
      case 'git':
        m.applyGit(env);
        branch = env.branch;
        gitCount = env.count;
        gitVersion++;
        if (viewMode() === 'changed') rebuildLens();
        break;
      case 'search_ready':
        if (find?.query.trim()) void runSearch();
        break;
      case 'root_gone':
        // The hub is dead for good (Rust never revives a gone root), so drop the
        // subscription and the model with it. The next resolve of this same place then
        // finds no live subscription and re-roots onto a fresh hub, so a folder deleted and
        // re-created (a rename away and back, `rm -rf && git clone`) comes back. The old
        // model's generations belong to the dead hub and must not be offered to a new one;
        // the open folders survive through localStorage.
        status = 'gone';
        statusText = place ? tildePath(place.root, home) : null;
        if (place) roots.delete(place.place);
        closeSub();
        model = null;
        rows = [];
        return;
    }
    if (m.pendingClose.length) sendClose(m.pendingClose.splice(0));
    syncRows();
    // Ack from inside onmessage (§5.4): a stalled webview stops acking by itself.
    acked.count++;
    const now = performance.now();
    if (acked.count >= ACK_EVERY || now - acked.at >= ACK_AFTER_MS) {
      commands.ftAck(sub, env.seq).catch(() => {});
      acked = { count: 0, at: now };
    }
    checkPendingReveal();
  }

  /** A folder's listing is complete: continue an Alt+→, land a pending reveal. */
  function onListed(rel: string) {
    if (recursive && model && isWithin(rel, recursive.root) && model.expanded.has(rel)) expandChildren(rel);
  }

  function checkPendingReveal() {
    if (!pendingReveal || !model) return;
    const { rel, select } = pendingReveal;
    if (model.indexOf(rel) < 0) {
      // Its folder has listed and it isn't there (deleted, or filtered out): stop waiting,
      // so later messages don't each scan the rows for it.
      const parent = model.dirs.get(parentRel(rel));
      const e = model.entry(rel);
      const m = model;
      if ((parent?.state === 'ok' && (!e || !m.visible(e))) || !ancestorsOf(rel).every((a) => m.expanded.has(a))) pendingReveal = null;
      return;
    }
    pendingReveal = null;
    if (select) {
      selection.clear();
      selection.add(rel);
      focusKey = rel;
      anchorKey = rel;
    }
    requestScroll(rel);
  }

  /** Drop selected/focused keys whose entries are gone or filtered out. Checked per key
   *  against the folder data, never by scanning the rows (a delta must stay cheap in a
   *  100k-row tree). */
  function pruneSelection() {
    const m = model;
    if (!m) return;
    const alive = (k: string) => {
      const e = m.entry(k);
      return !!e && m.visible(e);
    };
    for (const k of [...selection]) if (!alive(k)) selection.delete(k);
    if (focusKey && !alive(focusKey)) focusKey = null;
  }

  // ── Expand / collapse ─────────────────────────────────────────────────────────────────

  function expand(rel: string) {
    if (!model) return;
    if (viewMode() !== 'tree') {
      if (lensCollapsed.delete(rel)) rebuildLens();
      return;
    }
    const r = model.expand(rel);
    sendOpen(r.open);
    syncRows();
    scheduleSaveExpanded();
  }

  function collapse(rel: string) {
    if (!model) return;
    if (viewMode() !== 'tree') {
      if (!lensCollapsed.has(rel)) {
        lensCollapsed.add(rel);
        rebuildLens();
      }
      return;
    }
    const r = model.collapse(rel);
    sendClose(r.close);
    syncRows();
    scheduleSaveExpanded();
  }

  /** Alt+→: open every folder below, listing as they come, until 2,000 rows (§11.2).
   *  Ignored folders (node_modules, target) are left closed: opening them is never what a
   *  recursive expand is for, and it would spend the budget on them. */
  function expandRecursive(rel: string) {
    if (!model) return;
    if (viewMode() !== 'tree') {
      // A pruned tree is small: everything under the folder opens.
      for (const k of [...lensCollapsed]) if (isWithin(k, rel)) lensCollapsed.delete(k);
      rebuildLens();
      return;
    }
    recursive = { root: rel, budget: model.rows.length + RECURSIVE_ROW_BUDGET };
    if (rel !== '' && !model.expanded.has(rel)) expand(rel);
    if (model.dirs.get(rel)?.state === 'ok') expandChildren(rel);
  }

  function expandChildren(rel: string) {
    const m = model;
    if (!m || !recursive) return;
    const d = m.dirs.get(rel);
    if (!d) return;
    for (const name of d.names) {
      const e = d.entries.get(name);
      if (!e || !isExpandable(e) || e.f & TREE_FLAG_IGNORED || !m.visible(e)) continue;
      if (m.rows.length >= recursive.budget) {
        announce(`Stopped opening folders at ${RECURSIVE_ROW_BUDGET.toLocaleString()} rows`);
        toastStore.addToast('Files', `Stopped opening folders at ${RECURSIVE_ROW_BUDGET.toLocaleString()} rows.`, 'info');
        recursive = null;
        return;
      }
      const key = joinRel(rel, name);
      if (!m.expanded.has(key)) expand(key);
      else if (m.dirs.get(key)?.state === 'ok') expandChildren(key);
    }
  }

  /** Alt+←: close the folder and forget every open folder below it. */
  function collapseRecursive(rel: string) {
    if (!model) return;
    if (viewMode() !== 'tree') {
      collapse(rel);
      return;
    }
    collapse(rel);
    for (const r of [...model.expanded]) if (r !== '' && r !== rel && isWithin(r, rel)) model.expanded.delete(r);
    scheduleSaveExpanded();
  }

  // ── Revealing ─────────────────────────────────────────────────────────────────────────

  /** Open the folders above `rel` and scroll it into view when it has a row; select it
   *  unless the reveal is automatic. Under find or the Changed lens, see `revealPlan`. */
  function revealRel(rel: string, kind: RevealKind) {
    if (!model) return;
    const select = kind !== 'auto';
    const plan = revealPlan(kind, viewMode() !== 'tree', rows.some((x) => x.key === rel));
    if (plan === 'none') return;
    if (plan === 'scroll' || plan === 'select') {
      if (plan === 'select') {
        selection.clear();
        selection.add(rel);
        focusKey = rel;
        anchorKey = rel;
      }
      requestScroll(rel);
      return;
    }
    if (plan === 'put-away') {
      // The real tree comes back to show it, with the lens and find put away first so the
      // restored view can't replace the selection.
      if (find) closeFind();
      if (lens === 'changed') toggleChanged();
    }
    const r = model.reveal(rel);
    sendOpen(r.open);
    syncRows();
    if (r.open.length) scheduleSaveExpanded();
    pendingReveal = { rel, select };
    checkPendingReveal();
  }

  /** The active tab's file: highlighted and revealed when it is under the root (with
   *  auto-reveal on), named in the header when it isn't. Never focused, never selected.
   *
   *  Driven by FilesColumn's effect on the active tab's FILE (its type and path) and the
   *  subscription, not by root resolves: a preview tab swapping its file keeps its tab id,
   *  and a terminal tab coming to the front re-resolves nothing when its folder is under the
   *  root, so a highlight kept up by resolves went stale in both cases. No target clears at
   *  once; a lookup still in flight for an earlier target is dropped by `activeSeq`. */
  async function syncActiveFile(target: ActiveFileTarget | null) {
    const my = ++activeSeq;
    const sub = subId;
    if (!target || sub === null) {
      activeFile = null;
      outsideFile = null;
      return;
    }
    const s = await settleActiveFile(target, async (abs) => (await commands.ftLocate(sub, [abs]))[0] ?? null);
    if (my !== activeSeq || sub !== subId) return;
    activeFile = s.active;
    outsideFile = s.outside;
    if (preferencesStore.fileTreeAutoReveal && s.active) revealRel(s.active, 'auto');
  }

  /** "Reveal in Files" from a tab menu: the human asked, so the column opens (if it was
   *  closed), the row is selected and the tree takes focus. */
  async function revealPath(abs: string) {
    await workspacesStore.setFileTreeOpen(true);
    // The column mounts and resolves; wait for a subscription before locating.
    for (let i = 0; i < 40 && subId === null; i++) await new Promise((r) => setTimeout(r, 50));
    if (subId === null) return;
    const [rel] = await commands.ftLocate(subId, [abs]).catch(() => [null]);
    if (rel == null) {
      toastStore.addToast('Files', 'That file is outside the folder Files is showing.', 'info');
      return;
    }
    if (rel) revealRel(rel, 'asked');
    focusPending = true;
  }

  /** "Reveal in Files" on a terminal tab: that tab comes to the front (so the tree follows
   *  it), the column opens because the human asked, and its folder is selected. */
  async function revealTabFolder(workspaceId: string, paneId: string, tabId: string) {
    await workspacesStore.setActiveTab(workspaceId, paneId, tabId);
    await workspacesStore.setFileTreeOpen(true);
    for (let i = 0; i < 40 && !attached; i++) await new Promise((r) => setTimeout(r, 50));
    // Forced: the human asked for THIS tab's folder, so it moves the tree even with
    // following turned off (otherwise the previous tab's folder would be selected).
    await resolveNow(false, true);
    if (youAreHere) revealRel(youAreHere, 'asked');
    focusPending = true;
  }

  // ── Opening ───────────────────────────────────────────────────────────────────────────

  function absOf(rel: string): string | null {
    return ack ? relToAbs(ack, rel) : null;
  }

  async function openFile(rel: string, mode: OpenMode, keepFocus = false) {
    const abs = absOf(rel);
    const ws = workspacesStore.activeWorkspace;
    const pane = workspacesStore.activePane;
    if (!abs || !ws || !pane) return;
    // A real open hands the keyboard to the editor; EditorPane won't take it from the tree
    // by itself (a preview keeps it here).
    const handOver = (tabId: string | null) => {
      if (keepFocus || !tabId) return;
      if (document.activeElement?.closest('[data-filetree]')) (document.activeElement as HTMLElement).blur();
      setTimeout(() => getEditorByTabId(tabId)?.view.focus(), 0);
    };
    try {
      if (mode === 'side') {
        const other = ws.panes.find((p) => p.id !== pane.id);
        const tabId = other
          ? (await openInEditor({ abs, workspaceId: ws.id, paneId: other.id })).tabId
          : await workspacesStore.openEditorInNewSplit(ws.id, pane.id, editorFileInfo(abs));
        handOver(tabId);
        return;
      }
      const usePreview = mode === 'preview' && preferencesStore.fileTreePreviewTabs;
      let replaceTabId: string | null = null;
      if (usePreview) {
        const prev = previewTabs.get(pane.id);
        const loc = prev ? workspacesStore._locateTab(prev) : null;
        if (prev && loc && loc.paneId === pane.id && loc.tab.tab_type === 'editor' && !isEditorDirty(prev)) replaceTabId = prev;
        else previewTabs.delete(pane.id);
      }
      const res = await openInEditor({ abs, workspaceId: ws.id, paneId: pane.id, replaceTabId });
      if (usePreview) {
        if (!res.existing) previewTabs.set(pane.id, res.tabId);
      } else {
        pinPreview(res.tabId);
      }
      handOver(res.tabId);
    } catch (e) {
      logError(`[files] open ${rel}: ${e}`);
      toastStore.addToast('Couldn’t open file', String(e), 'error');
    }
    if (keepFocus) focusPending = true;
  }

  /** A preview becomes a normal tab (Enter, a double-click, its first edit). */
  function pinPreview(tabId: string) {
    for (const [p, t] of previewTabs) if (t === tabId) previewTabs.delete(p);
  }

  /** Previews that were edited, or whose tab is gone, stop being previews. Read inside an
   *  effect: `isEditorDirty` is reactive. */
  function settlePreviews() {
    for (const [p, t] of [...previewTabs]) {
      if (isEditorDirty(t) || !workspacesStore._locateTab(t)) previewTabs.delete(p);
    }
  }

  // ── Editor dots (§3.2: open, filled when dirty) ───────────────────────────────────────

  /** rel → the editor tabs showing it, for the row's editor dot. */
  let editorRels = $state.raw<Map<string, string[]>>(new Map());
  let editorKey = '';

  /** Map this window's local editor tabs onto rows. Rust spells them (`ftLocate`); called
   *  from an effect when the editor set or the root changes, so never per render. */
  async function syncEditorMarks(tabs: { id: string; path: string }[]) {
    const sub = subId;
    const key = `${sub}\u0000${tabs.map((t) => `${t.id}=${t.path}`).join('\u0001')}`;
    if (key === editorKey) return;
    editorKey = key;
    if (sub === null || tabs.length === 0) {
      editorRels = new Map();
      return;
    }
    const rels = await commands.ftLocate(sub, tabs.map((t) => t.path)).catch(() => null);
    if (sub !== subId || key !== editorKey || !rels) return;
    const out = new Map<string, string[]>();
    rels.forEach((rel, i) => {
      if (rel == null || rel === '') return;
      const list = out.get(rel) ?? [];
      list.push(tabs[i].id);
      out.set(rel, list);
    });
    editorRels = out;
  }

  // ── Context-menu actions ──────────────────────────────────────────────────────────────

  async function copy(text: string, what: string) {
    try {
      await clipboardWriteText(text);
      announce(`${what} copied`);
      toastStore.addToast(`${what} copied`, text, 'success');
    } catch (e) {
      toastStore.addToast('Copy failed', String(e), 'error');
    }
  }

  function selectedOrFocused(rel?: string): string[] {
    if (rel !== undefined && !selection.has(rel)) return [rel];
    if (selection.size) return [...selection];
    return focusKey ? [focusKey] : [];
  }

  async function copyPaths(rels: string[], relative: boolean) {
    if (!rels.length) return;
    const text = rels.map((r) => (relative ? r || '.' : absOf(r) ?? r)).join('\n');
    await copy(text, relative ? (rels.length > 1 ? 'Relative paths' : 'Relative path') : rels.length > 1 ? 'Paths' : 'Path');
  }

  async function revealInOs(rel: string) {
    const abs = absOf(rel);
    if (abs) await commands.revealInFileManager(abs).catch((e) => toastStore.addToast('Couldn’t reveal', String(e), 'error'));
  }

  async function openWithDefaultApp(rel: string) {
    const abs = absOf(rel);
    if (abs) await commands.openWithDefaultApp(abs).catch((e) => toastStore.addToast('Couldn’t open', String(e), 'error'));
  }

  /** A new terminal tab whose shell starts in that folder (or the file's folder). */
  async function openTerminalHere(rel: string) {
    const m = model;
    const ws = workspacesStore.activeWorkspace;
    const pane = workspacesStore.activePane;
    if (!m || !ws || !pane) return;
    const e = m.entry(rel);
    const dirRel = rel === '' || (e && isExpandable(e)) ? rel : parentRel(rel);
    const cwd = absOf(dirRel);
    if (!cwd) return;
    await workspacesStore.createTab(ws.id, pane.id, lastSegment(cwd), {
      context: { cwd, sshCommand: null, remoteCwd: null },
    });
  }

  /** The tab "Add to ‹tab›" types into: the tab the tree follows, when it's live. */
  const addTarget = $derived.by(() => {
    void terminalsStore.instanceVersion;
    const id = sourceTabId;
    if (!id || !terminalsStore.get(id)) return null;
    const loc = workspacesStore._locateTab(id);
    if (!loc || loc.tab.service_id) return null;
    return { tabId: id, name: loc.tab.name };
  });

  /** Insert the paths into the followed tab's command line, never submitting (§9.5). */
  async function addToTab(rels: string[]) {
    const target = addTarget;
    if (!target || !rels.length) return;
    const items = rels.map((r) => absOf(r)).filter((a): a is string => !!a).map((abs) => ({ abs }));
    const r = await insertPathsIntoTab(target.tabId, items);
    if (r.ok) announce(`Added ${items.length} path${items.length > 1 ? 's' : ''} to ${target.name}`);
  }

  // ── File operations (§10) ─────────────────────────────────────────────────────────────
  //
  // Every write is one `ftOp`, checked in Rust (containment, never clobber, the guards). What
  // lives here is the human's side: the inline name, the footer's questions, the undo stack,
  // editor tabs following their file, and the in-app clipboard.

  const writable = () => !!ack?.caps.writable && subId !== null;

  /** Exists in the model, when the folder holding it is listed; undefined when nobody knows. */
  const lookup: Lookup = (rel) => {
    const d = model?.dirs.get(parentRel(rel));
    if (!d || d.state !== 'ok') return undefined;
    return d.entries.has(baseName(rel));
  };

  /** A folder changed on disk: undo entries it contradicts go stale (unless it was us). */
  function noteDiskChange(dir: string, names: string[] | null) {
    if (place && undo.size(place.place)) undo.noteChange(place.place, dir, names, lookup, Date.now());
  }

  function setNotice(n: OpNotice | null) {
    if (noticeTimer) clearTimeout(noticeTimer);
    noticeTimer = null;
    notice = n;
    if (n) noticeTimer = setTimeout(() => { notice = null; noticeTimer = null; }, NOTICE_MS);
  }

  function ask(text: string, actions: ConfirmAction[]) {
    confirm = { text, actions };
  }

  /** The folder an action without a folder of its own lands in: the focused folder, the
   *  focused file's folder, else the root. */
  function folderOf(rel: string | null | undefined): string {
    if (rel == null || rel === '') return '';
    const e = model?.entry(rel);
    return e && isExpandable(e) ? rel : parentRel(rel);
  }

  function quotedNames(rels: string[]): string {
    return rels.length === 1 ? `“${baseName(rels[0])}”` : `${rels.length} items`;
  }

  /** The editor tabs showing a file at or under any of `rels`: [rel, tabId]. */
  function editorsWithin(rels: string[]): [string, string][] {
    const out: [string, string][] = [];
    for (const [rel, tabs] of editorRels) {
      if (rels.some((r) => isWithin(rel, r))) for (const t of tabs) out.push([rel, t]);
    }
    return out;
  }

  /** Renamed or moved files keep their editor tabs: each tab is pointed at the new path and
   *  its watcher re-armed there (§10.3). Unsaved edits stay in the buffer. `root` and `eds`
   *  are the root the op ran in and its editor marks. */
  async function retargetEditors(done: OpItem[], root: OpenAck | null, eds: Map<string, string[]>) {
    const moves = done.filter((d): d is OpItem & { to: string } => !!d.to && d.from !== d.to);
    if (!moves.length || !root) return;
    for (const [rel, tabs] of eds) {
      const m = moves.find((d) => isWithin(rel, d.from));
      if (!m) continue;
      const abs = relToAbs(root, m.to + rel.slice(m.from.length));
      for (const tabId of tabs) {
        const loc = workspacesStore._locateTab(tabId);
        const f = loc?.tab.editor_file;
        if (!loc || loc.tab.tab_type !== 'editor' || !f || f.is_remote) continue;
        const info = { ...f, file_path: abs, language: editorFileInfo(abs).language ?? f.language };
        const name = loc.tab.custom_name ? loc.tab.name : lastSegment(abs);
        try {
          await workspacesStore.updateEditorTabFile(tabId, name, info);
          window.dispatchEvent(new CustomEvent('editor-retarget-file', { detail: { tabId } }));
        } catch (e) {
          logError(`[files] retarget editor ${tabId}: ${e}`);
        }
      }
    }
  }

  /** Select what an op made or moved, revealing the first. */
  function selectResult(rels: string[]) {
    if (!rels.length) return;
    revealRel(rels[0], 'asked');
    if (rels.length > 1) {
      for (const r of rels) selection.add(r);
    }
  }

  interface RunOpts {
    /** Undoing: the result is not pushed (there is no redo), and the footer says "Undid". */
    undoing?: UndoEntry;
    /** Select what the op made or moved (create, rename, move, copy, duplicate, import). */
    select?: boolean;
    /** Called with the result once handled. */
    after?: (r: OpResult) => void;
  }

  /**
   * Run one op and settle everything around it: the guards the human answers (on the
   * ConfirmStrip), editor tabs following renamed files, the undo stack, the footer and the
   * announcement. Resolves with the result, or null when it was refused or asks first.
   */
  async function runOp(op: FileOp, o: RunOpts = {}): Promise<OpResult | null> {
    const sub = subId;
    const p = place;
    // The root the op runs in, and its editor tabs: still the ones to retarget if the tree
    // re-roots while a slow op (a cross-volume folder move) runs.
    const a = ack;
    const eds = editorRels;
    if (sub === null || !p) return null;
    confirm = null;
    let r: OpResult;
    undo.beginOp();
    try {
      r = await commands.ftOp(sub, op);
    } catch (e) {
      // A refusal of the whole op: nothing changed.
      const msg = e instanceof Error ? e.message : String(e);
      setNotice({ text: msg, action: null });
      announce(msg);
      return null;
    } finally {
      undo.endOp(Date.now());
    }
    // Before the re-root check: a tab left on the old path would save the file back there.
    if (op.kind === 'rename' || op.kind === 'move') void retargetEditors(r.done, a, eds);
    if (sub !== subId) return r;

    if (r.needs) {
      const n = r.needs;
      if (n.code === 'agent_active') {
        ask(`${n.detail} Do it anyway?`, [{ label: 'Do It Anyway', tone: 'danger', run: () => runOp({ ...op, ack_agents: true }, o) }]);
      } else if (n.code === 'live_cwd') {
        const folder = op.src?.length === 1 ? absOf(op.src[0]) : null;
        const suggested = op.kind === 'rename' && op.src?.length === 1 && op.name && ack
          ? relToAbs(ack, joinRel(parentRel(op.src[0]), op.name))
          : op.kind === 'move' && op.src?.length === 1 && ack ? relToAbs(ack, joinRel(op.dest ?? '', baseName(op.src[0]))) : null;
        ask(n.detail, folder
          ? [{ label: 'Move Project…', tone: 'primary', run: () => relocateStore.open({ mode: 'move', old: folder, suggested }) }]
          : []);
      } else if (n.code === 'no_trash') {
        const left = r.failed.filter((f) => f.code === 'no_trash').map((f) => f.rel);
        if (left.length) void deletePermanently(left, n.detail);
      }
    }

    const text = describeResult(op, r, PLATFORM);
    let action: OpNotice['action'] = null;
    if (o.undoing) {
      const words = `Undid the ${o.undoing.label}${op.kind === 'trash' && r.done.length ? ` (moved to ${trashPlace(PLATFORM)})` : ''}`;
      const extra = r.failed.length ? ` · ${r.failed[0].msg}` : '';
      if (r.done.length || r.failed.length) setNotice({ text: words + extra, action: null });
      announce(words + extra);
    } else {
      if (r.undo && r.done.length) {
        const entry = undo.push(p.place, r.undo, undoLabel(op, r));
        action = { label: 'Undo', run: () => void undoLast(entry) };
      } else if (op.kind === 'trash' && r.done.length && PLATFORM === 'mac') {
        // NSFileManager can't put things back from here: say where they went instead.
        action = { label: 'Show in Trash', run: () => void showTrash() };
      }
      if (text) {
        setNotice({ text, action });
        announce(text);
      }
    }

    // Same-named items: offer to keep both rather than fail the drop quietly.
    const clashes = r.failed.filter((f) => f.code === 'exists');
    if (!r.needs && clashes.length && (op.kind === 'move' || op.kind === 'copy' || op.kind === 'import') && op.conflict !== 'keep_both' && !op.to?.length) {
      const where = op.dest ? `“${baseName(op.dest)}”` : 'the root folder';
      ask(`${quotedNames(clashes.map((c) => c.rel))} ${clashes.length === 1 ? 'is' : 'are'} already in ${where}.`, [
        { label: 'Keep Both', tone: 'primary', run: () => runOp({ ...op, src: clashes.map((c) => c.rel), conflict: 'keep_both' }, o) },
      ]);
    }

    // `to` is the new rel (an import's `from` is the native source); a create's is `from`.
    if (o.select && sub === subId) selectResult(r.done.map((d) => d.to ?? d.from));
    o.after?.(r);
    return r;
  }

  async function showTrash() {
    const h = home ?? (await homeDir().catch(() => null));
    if (h) await commands.openWithDefaultApp(`${h.replace(/\/$/, '')}/.Trash`).catch((e) => toastStore.addToast('Couldn’t open the Trash', String(e), 'error'));
  }

  // The inline name (§10.4).

  /** New File / New Folder: the folder opens and the input sits at the top of its children.
   *  Find and the Changed lens are put away first: the new item belongs in the real tree. */
  function startCreate(kind: 'file' | 'dir', parent?: string) {
    if (!writable() || !model) return;
    const dir = parent ?? folderOf(focusKey);
    if (find) closeFind();
    if (lens === 'changed') toggleChanged();
    if (dir !== '') {
      revealRel(dir, 'auto');
      expand(dir);
    }
    confirm = null;
    edit = { mode: 'create', kind, parent: dir, error: null, pending: false, value: '' };
    if (dir) requestScroll(dir);
    else scrollToTop();
  }

  function startRename(rel: string) {
    if (!writable() || !rel) return;
    confirm = null;
    edit = { mode: 'rename', rel, error: null, pending: false, value: baseName(rel) };
    // Focused, so the row stays rendered while scrolled away (TreeBody keeps the focused row).
    focusKey = rel;
    requestScroll(rel);
  }

  function cancelEdit() {
    if (!edit) return;
    edit = null;
    refocusAfterEdit();
  }

  /** An inline edit ended: the keyboard comes back to the tree unless it has gone somewhere
   *  else. A blur onto a terminal or an editor is the human moving on, and pulling focus back
   *  would send their next keys (Enter, Delete) to the tree. */
  function refocusAfterEdit() {
    const a = typeof document === 'undefined' ? null : document.activeElement;
    if (!a || a === document.body || a.closest('[data-filetree]')) focusPending = true;
  }

  /** The row being renamed, or the folder a New input sits in, went away on disk (an agent
   *  renamed or deleted it). The input unmounted with its row without a word, so the edit is
   *  dropped with it: left set, it would block every drag in the tree. A commit already sent
   *  settles itself. */
  function pruneEdit() {
    const e = edit;
    const m = model;
    if (!e || e.pending || !m) return;
    // Known gone: its folder is listed without it, or that folder is itself gone.
    const gone = (rel: string): boolean => {
      if (rel === '') return false;
      const x = m.entry(rel);
      if (x) return !m.visible(x);
      const d = m.dirs.get(parentRel(rel));
      return d ? d.state === 'ok' : gone(parentRel(rel));
    };
    if (gone(e.mode === 'rename' ? e.rel : e.parent)) edit = null;
  }

  /** The rules the input checks as the human types. */
  function editRules(): NameRules | null {
    const e = edit;
    const a = ack;
    if (!e || !a || !model) return null;
    const dir = e.mode === 'create' ? e.parent : parentRel(e.rel);
    const d = model.dirs.get(dir);
    const own = e.mode === 'rename' ? baseName(e.rel) : undefined;
    const names: [string, 'dir' | 'file'][] = d ? [...d.entries.values()].map((x) => [x.n, isFolderKind(x.k) ? 'dir' : 'file']) : [];
    return {
      mode: e.mode,
      strictWindows: PLATFORM === 'windows' || a.case_insensitive,
      caseInsensitive: a.case_insensitive,
      nfc: a.nfc_names,
      sibling: siblingLookup(names, a.case_insensitive, a.nfc_names, own),
      current: own,
    };
  }

  /** Enter (or a real blur) on the input. Nothing is sent for an invalid or unchanged name;
   *  a failure keeps the input up with the reason under it. */
  async function commitEdit(value: string) {
    const e = edit;
    const rules = editRules();
    if (!e || e.pending || !rules) return;
    const problem = nameProblem(value, rules);
    if (problem) {
      edit = { ...e, error: problem, value };
      return;
    }
    if (e.mode === 'rename' && value === baseName(e.rel)) {
      cancelEdit();
      return;
    }
    edit = { ...e, pending: true, error: null, value };
    const op: FileOp = e.mode === 'create'
      ? { kind: e.kind === 'file' ? 'create_file' : 'create_dir', dest: e.parent, name: value }
      : { kind: 'rename', src: [e.rel], name: value };
    const r = await runOp(op, { select: true });
    if (edit?.pending !== true) return; // cancelled or re-rooted meanwhile
    if (r?.needs) {
      // The footer asks now (agents at work, a tab's folder); its answer reruns the op.
      edit = null;
      return;
    }
    if (!r || !r.done.length) {
      const why = r?.failed[0]?.msg ?? (r?.skipped.length ? 'Nothing to change' : notice?.text ?? 'That didn’t work');
      edit = { ...e, pending: false, value, error: why };
      return;
    }
    edit = null;
    refocusAfterEdit();
    // A new file opens as a pinned editor tab (§10.4).
    const made = r.done[r.done.length - 1]?.from;
    if (e.mode === 'create' && e.kind === 'file' && made) void openFile(made, 'pin');
  }

  // Trash, delete, duplicate.

  function dirtyNames(rels: string[]): string[] {
    return [...new Set(editorsWithin(rels).filter(([, t]) => isEditorDirty(t)).map(([rel]) => baseName(rel)))];
  }

  function unsavedNote(names: string[]): string {
    if (!names.length) return '';
    return names.length === 1 ? `“${names[0]}” has unsaved changes. ` : `${names.length} open files have unsaved changes. `;
  }

  /** Delete / Cmd+Backspace: to the trash, asking first only over unsaved edits (§10.3). */
  async function trash(rels: string[]) {
    rels = rels.filter((r) => r !== '');
    if (!rels.length || !writable()) return;
    if (!ack?.caps.trash) return deletePermanently(rels);
    const dirty = dirtyNames(rels);
    const go = () => runOp({ kind: 'trash', src: rels });
    if (dirty.length) ask(`${unsavedNote(dirty)}${trashLabel(PLATFORM)} anyway?`, [{ label: trashLabel(PLATFORM), tone: 'danger', run: go }]);
    else await go();
  }

  /** Shift+Delete, or a trash that can't: always asked, with what it will remove, and run
   *  with a single-use ticket bound to exactly these rels (§10.1). */
  async function deletePermanently(rels: string[], why?: string) {
    rels = rels.filter((r) => r !== '');
    const sub = subId;
    if (!rels.length || sub === null || !writable()) return;
    let t;
    try {
      t = await commands.ftPrepareDelete(sub, rels);
    } catch (e) {
      toastStore.addToast('Couldn’t delete', String(e), 'error');
      return;
    }
    if (sub !== subId) return;
    const counts = [t.files && `${t.files} file${t.files === 1 ? '' : 's'}`, t.dirs && `${t.dirs} folder${t.dirs === 1 ? '' : 's'}`].filter(Boolean).join(' and ');
    const lead = why ? `${why} ` : '';
    ask(`${lead}${unsavedNote(dirtyNames(rels))}Delete ${quotedNames(rels)} permanently${counts ? ` (${counts})` : ''}? This can’t be undone.`, [
      { label: 'Delete Permanently', tone: 'danger', run: () => runOp({ kind: 'delete', src: rels, token: t.token }) },
    ]);
  }

  function duplicate(rels: string[]) {
    rels = rels.filter((r) => r !== '');
    if (rels.length && writable()) void runOp({ kind: 'duplicate', src: rels }, { select: true });
  }

  // The in-app clipboard.

  function clipTo(rels: string[], cut: boolean) {
    rels = rels.filter((r) => r !== '');
    if (!rels.length || !place || !ack) return;
    clip = { place: place.place, rels, abs: rels.map((r) => relToAbs(ack!, r)), cut };
    announce(`${quotedNames(rels)} ${cut ? 'cut' : 'copied'}: paste into a folder`);
  }

  /** Paste into `dest` (or the focused folder). A cut moves within this root; anywhere else
   *  (another root) it can only copy, and says so. A copy into the folder it came from gets
   *  a fresh name, like Finder and Explorer. */
  async function paste(dest?: string) {
    const c = clip;
    if (!c || !place || !writable()) return;
    const to = dest ?? folderOf(focusKey);
    if (c.place === place.place) {
      if (!dropAccepted(c.rels, to, !c.cut)) {
        announce(c.cut ? 'Already in that folder' : 'A folder can’t be copied into itself');
        return;
      }
      if (c.cut) {
        const r = await runOp({ kind: 'move', src: c.rels, dest: to }, { select: true });
        if (r && !r.failed.length && !r.needs) clip = null;
      } else {
        const same = c.rels.every((s) => parentRel(s) === to);
        await runOp({ kind: 'copy', src: c.rels, dest: to, conflict: same ? 'keep_both' : 'fail' }, { select: true });
      }
      return;
    }
    const r = await runOp({ kind: 'import', src: c.abs, dest: to }, { select: true });
    if (r && c.cut) announce('Files from another root are copied, not moved');
  }

  // Undo (§10.4).

  /** Cmd/Ctrl+Z: the newest op on this root, unless it changed on disk since. `expected`
   *  (the footer's Undo) only acts while that entry is still the newest. */
  async function undoLast(expected?: UndoEntry) {
    if (!place || !writable()) return;
    const top = undo.peek(place.place);
    if (!top || (expected && top !== expected)) {
      announce('Nothing to undo');
      if (!top) setNotice({ text: 'Nothing to undo in this folder', action: null });
      return;
    }
    undo.pop(place.place);
    const stale = undo.check(top, lookup);
    if (stale) {
      const text = `Can’t undo the ${top.label}: ${stale}`;
      setNotice({ text, action: null });
      announce(text);
      return;
    }
    const sel = top.op.kind === 'rename' || top.op.kind === 'move' || top.op.kind === 'restore';
    await runOp(top.op, { undoing: top, select: sel });
  }

  // Drag and drop (§10.4).

  /** Whether a drag of `srcs` would do anything at `t`. */
  function dropAccepts(srcs: string[], t: DropTarget, copy: boolean): boolean {
    if (t.kind !== 'folder') return true;
    return writable() && dropAccepted(srcs, t.rel, copy);
  }

  async function dropItems(srcs: string[], t: DropTarget, copy: boolean) {
    const abs = srcs.map((r) => absOf(r)).filter((a): a is string => !!a);
    if (t.kind === 'terminal') {
      const r = await insertPathsIntoTab(t.tabId, abs.map((a) => ({ abs: a })));
      if (r.ok) announce(`Added ${quotedNames(srcs)} to the tab`);
      return;
    }
    if (t.kind === 'composer') {
      attachToComposer(t.tabId, abs);
      announce(`Attached ${quotedNames(srcs)}`);
      return;
    }
    if (!dropAccepts(srcs, t, copy)) return;
    // Undoable beats confirmable: a move never asks (§10.4).
    if (copy) {
      const same = srcs.every((s) => parentRel(s) === t.rel);
      await runOp({ kind: 'copy', src: srcs, dest: t.rel, conflict: same ? 'keep_both' : 'fail' }, { select: true });
    } else {
      await runOp({ kind: 'move', src: srcs, dest: t.rel }, { select: true });
    }
  }

  /** Files dropped from the OS onto the tree: copied in (§10.4). */
  async function importPaths(paths: string[], dest: string) {
    if (!paths.length || !writable()) return;
    await runOp({ kind: 'import', src: paths, dest }, { select: true });
  }

  /** "Ask about this…": the paths go into the followed tab's composer as attachments; the
   *  human writes the prompt and sends it (§9.5). The tree never submits. */
  function askAbout(rels: string[]) {
    const target = addTarget;
    if (!target || !rels.length) return;
    const abs = rels.map((r) => absOf(r)).filter((a): a is string => !!a);
    if (!workspacesStore.isComposerOpen(target.tabId)) workspacesStore.toggleComposer(target.tabId);
    attachToComposer(target.tabId, abs);
    announce(`Attached ${quotedNames(rels)} to ${target.name}’s composer`);
  }

  // ── Selection and focus ───────────────────────────────────────────────────────────────

  function setFocus(key: string | null, scroll = true) {
    focusKey = key;
    if (key && scroll) requestScroll(key);
  }

  function selectOnly(key: string) {
    selection.clear();
    selection.add(key);
    focusKey = key;
    anchorKey = key;
  }

  function toggleSelect(key: string) {
    if (selection.has(key)) selection.delete(key);
    else selection.add(key);
    focusKey = key;
    anchorKey = key;
  }

  /** Shift-extend from the anchor to `key`, over the visible rows. */
  /** Over the rows on screen: the real tree's, or a pruned tree's. */
  function extendTo(key: string) {
    const rs = rows;
    const from = anchorKey ? rs.findIndex((r) => r.key === anchorKey) : -1;
    const to = rs.findIndex((r) => r.key === key);
    if (to < 0) return;
    const [a, b] = from < 0 ? [to, to] : from < to ? [from, to] : [to, from];
    selection.clear();
    for (let i = a; i <= b; i++) {
      const r = rs[i];
      if (r.kind !== 'm') selection.add(r.key);
    }
    focusKey = key;
  }

  function selectAll() {
    selection.clear();
    for (const r of rows) if (r.kind !== 'm') selection.add(r.key);
  }

  // ── Pinning (§4.2) ────────────────────────────────────────────────────────────────────

  /** Pin a root for the active workspace and show it (root switcher, "Set as Root",
   *  "Open Folder…"). `p` is a `watch::place`. */
  async function pin(p: string) {
    const ws = workspacesStore.activeWorkspace;
    if (!ws) return;
    try {
      await workspacesStore.setWorkspaceFileTreePin(ws.id, p);
    } catch (e) {
      toastStore.addToast('Couldn’t set the Files root', String(e), 'error');
      return;
    }
    logInfo(`[files] pinned ${p}`);
    await resolveNow(false, true);
  }

  /** Unpinning resumes following the active tab. */
  async function unpin() {
    const ws = workspacesStore.activeWorkspace;
    if (!ws) return;
    await workspacesStore.setWorkspaceFileTreePin(ws.id, null).catch(() => {});
    await resolveNow(true, true);
  }

  // ── Attach / detach (the column mounting) ─────────────────────────────────────────────

  function onVisibility() {
    if (subId !== null) commands.ftPause(subId, document.hidden).catch(() => {});
  }

  function onWindowBlur() {
    blurredAt = Date.now();
  }

  function onWindowFocus() {
    // Back after a while: anything FSEvents coalesced away while the machine slept is
    // caught by asking every open folder again with its generation (§6.6).
    if (blurredAt !== null && Date.now() - blurredAt > REFOCUS_REVALIDATE_MS && model && subId !== null) {
      sendOpen(model.restoreReqs());
    }
    blurredAt = null;
  }

  async function attach() {
    if (attached) return;
    attached = true;
    const offOsc = terminalsStore.onOscChange((tabId, osc) => {
      if (tabId !== sourceTabId) return;
      // Titles change constantly (an agent's spinner); only a folder signal is an edge.
      const key = `${osc.cwd}\u0000${osc.cwdHost}\u0000${osc.promptCwd}`;
      const prev = lastOscKey.get(tabId);
      if (prev === key) return;
      const hostChanged = prev !== undefined && prev.split('\u0000')[1] !== String(osc.cwdHost);
      lastOscKey.set(tabId, key);
      scheduleResolve(hostChanged);
    });
    const offPrompt = activityStore.onShellPrompt((tabId) => {
      if (tabId === sourceTabId) scheduleResolve(false);
    });
    let offStart: UnlistenFn | null = null;
    listen<{ tab_id?: string }>('agent-hook-session-start', (e) => {
      if (e.payload?.tab_id && e.payload.tab_id === sourceTabId) scheduleResolve(true);
    }).then((u) => {
      if (attached) offStart = u;
      else u();
    }).catch(() => {});
    document.addEventListener('visibilitychange', onVisibility);
    window.addEventListener('blur', onWindowBlur);
    window.addEventListener('focus', onWindowFocus);
    unlisteners = [
      offOsc,
      offPrompt,
      () => offStart?.(),
      () => document.removeEventListener('visibilitychange', onVisibility),
      () => window.removeEventListener('blur', onWindowBlur),
      () => window.removeEventListener('focus', onWindowFocus),
    ];
    if (place && place.kind === 'local' && !place.missing) await reroot(place);
    void resolveNow(false);
  }

  /** The column unmounted: close the subscription (not pause — the hub's 30 s grace and the
   *  held generations make a reopen cheap). The view is kept for the next open. */
  function detach() {
    if (!attached) return;
    attached = false;
    if (resolveTimer) clearTimeout(resolveTimer);
    resolveTimer = null;
    for (const u of unlisteners) u();
    unlisteners = [];
    saveView();
    closeSub();
    focused = false;
  }

  return {
    get status() { return status; },
    get statusText() { return statusText; },
    get place() { return place; },
    get ack() { return ack; },
    get rows() { return rows; },
    get model() { return model; },
    /** A folder is open, read reactively (the model's own set is not). In a pruned tree,
     *  open unless closed there. */
    isExpanded(key: string): boolean {
      void expandedVersion;
      if (viewMode() !== 'tree') return !lensCollapsed.has(key);
      return !!model?.expanded.has(key);
    },
    get viewMode(): ViewMode { return viewMode(); },
    /** A row's git state, read reactively through `gitVersion`. */
    gitOf(key: string, folder: boolean): { letter: GitLetter; rollup: boolean } | null {
      void gitVersion;
      return model?.gitOf(key, folder) ?? null;
    },
    /** Health's word, not `caps.git`: Health follows `file_tree_git` turned on or off while
     *  the tree is open, the caps only say how it was at open. */
    get gitOn() { return gitState !== 'off'; },
    get gitState() { return gitState; },
    get branch() { return branch; },
    get gitCount() { return gitCount; },
    get gitTruncated() { void gitVersion; return !!model?.gitTruncated; },
    get lens() { return lens; },
    toggleChanged,
    compareWithHead,
    // Find
    get find() { return find; },
    get findFocusSeq() { return findFocusSeq; },
    /** Find's matched letters in a row's name, while results are showing. */
    highlightsFor(key: string): number[] | undefined {
      return find && find.query.trim() ? find.highlights.get(key) : undefined;
    },
    openFind,
    closeFind,
    setFindQuery,
    findMove,
    findActivate,
    /** "Index this folder": the human asked to index a home or volume root. */
    indexShallow() { void runSearch(true); },
    get health() { return health; },
    get focused() { return focused; },
    set focused(v: boolean) { focused = v; },
    get focusKey() { return focusKey; },
    get anchorKey() { return anchorKey; },
    get selection() { return selection; },
    get previewTabs() { return previewTabs; },
    get youAreHere() { return youAreHere; },
    get activeFile() { return activeFile; },
    get outsideFile() { return outsideFile; },
    get announcement() { return announcement; },
    get scrollRequest() { return scrollRequest; },
    get focusPending() { return focusPending; },
    /** Taken by whoever focuses (the tree, or the column when there is no tree to focus). */
    takeFocus(): boolean {
      const was = focusPending;
      focusPending = false;
      return was;
    },
    get sourceTabId() { return sourceTabId; },
    get addTarget() { return addTarget; },
    get editorRels() { return editorRels; },
    get subscribed() { return ack !== null; },
    syncEditorMarks,
    get home() { return home; },
    get recentPlaces(): string[] { return lsGet<string[]>('ft:recent', []).slice(0, RECENT_MAX); },
    get lastHumanInputMs() { return lastHumanInputMs; },
    /** The workspace's pin, as the mirror has it. */
    get pinned(): string | null { return workspacesStore.activeWorkspace?.file_tree_pin ?? null; },

    attach,
    detach,
    scheduleResolve,
    /** The human asked the tree to follow the tab now (the gone banner's button): forced,
     *  so it acts even with following turned off. */
    followTabNow() { return resolveNow(true, true); },
    humanTouched() { lastHumanInputMs = Date.now(); },
    noteScroll(top: number) { scrollTop = top; },

    /** Cmd/Ctrl+Shift+B (§11.2): closed → open and focus; open, unfocused → focus;
     *  focused → close. With an editor tab in front, its file is revealed too. */
    async toggleFromShortcut() {
      if (!preferencesStore.fileTreeEnabled) return;
      if (!workspacesStore.fileTreeOpen) {
        await workspacesStore.setFileTreeOpen(true);
        focusPending = true;
      } else if (!focused) {
        focusPending = true;
      } else {
        await workspacesStore.setFileTreeOpen(false);
        // The column unmounts with the keyboard inside it: hand it to what's in front, as
        // the tree's own Escape does, or it falls to <body> and typing goes nowhere.
        const t = workspacesStore.activeTab;
        if (t?.tab_type === 'terminal') terminalsStore.focusTerminal(t.id);
        else if (t?.tab_type === 'editor') getEditorByTabId(t.id)?.view.focus();
        return;
      }
      const t = workspacesStore.activeTab;
      if (t?.tab_type === 'editor' && t.editor_file && !t.editor_file.is_remote) {
        for (let i = 0; i < 40 && subId === null; i++) await new Promise((r) => setTimeout(r, 50));
        const [rel] = subId !== null ? await commands.ftLocate(subId, [t.editor_file.file_path]).catch(() => [null]) : [null];
        if (rel) {
          // Not "Reveal in Files": find's results or the lens stay (see revealPlan).
          revealRel(rel, 'focus');
          focusPending = true;
        }
      }
    },

    /** The titlebar button: a plain toggle. */
    async toggleColumn() {
      await workspacesStore.setFileTreeOpen(!workspacesStore.fileTreeOpen);
    },

    expand,
    collapse,
    toggle,
    expandRecursive,
    collapseRecursive,
    collapseAll() {
      if (!model) return;
      if (viewMode() !== 'tree') {
        for (const r of lensRows) if (isFolderKind(r.kind) && r.depth === 0) lensCollapsed.add(r.key);
        rebuildLens();
        scrollToTop();
        return;
      }
      sendClose(model.collapseAll().close);
      syncRows();
      scheduleSaveExpanded();
      pruneSelection();
      scrollRequest = { key: null, top: 0, seq: scrollRequest.seq + 1 };
    },
    refresh() {
      if (subId !== null) commands.ftRefresh(subId, null).catch((e) => logError(`[files] refresh: ${e}`));
    },
    setFilters(showHidden: boolean, showIgnored: boolean) {
      if (model?.setFilters(showHidden, showIgnored)) {
        if (viewMode() === 'tree') {
          syncRows();
          pruneSelection();
        } else {
          rebuildLens();
        }
      }
    },

    setFocus,
    selectOnly,
    toggleSelect,
    extendTo,
    selectAll,
    clearSelectionToFocus() {
      selection.clear();
      if (focusKey) selection.add(focusKey);
    },
    selectedOrFocused,

    openFile,
    pinPreview,
    settlePreviews,
    isPreviewTab(tabId: string): boolean {
      for (const t of previewTabs.values()) if (t === tabId) return true;
      return false;
    },
    revealPath,
    revealTabFolder,
    syncActiveFile,
    /** "Reveal Active File" (⋯ menu): the human asked, so it reveals with auto-reveal off. */
    revealActiveFile() { if (activeFile) revealRel(activeFile, 'auto'); },
    revealYouAreHere() { if (youAreHere) revealRel(youAreHere, 'auto'); },
    absOf,
    copyPaths,
    revealInOs,
    openWithDefaultApp,
    openTerminalHere,
    addToTab,

    // File operations (§10)
    get writable() { return !!ack?.caps.writable; },
    get canTrash() { return !!ack?.caps.trash; },
    get platform(): Platform { return PLATFORM; },
    get edit() { return edit; },
    get confirm() { return confirm; },
    get notice() { return notice; },
    get dropTarget() { return dropTarget; },
    setDropTarget(rel: string | null) { dropTarget = rel; },
    get hasClip() { return !!clip; },
    /** A row cut to the in-app clipboard, drawn dimmed until pasted. */
    isCut(key: string): boolean {
      return !!clip?.cut && clip.place === place?.place && clip.rels.includes(key);
    },
    folderOf,
    startCreate,
    startRename,
    cancelEdit,
    commitEdit,
    editRules,
    /** The ConfirmStrip: run a button (the strip closes first), or put it away. */
    async confirmRun(a: ConfirmAction) {
      confirm = null;
      focusPending = true;
      await a.run();
    },
    cancelConfirm() {
      confirm = null;
      focusPending = true;
    },
    dismissNotice() { setNotice(null); },
    trash,
    deletePermanently,
    duplicate,
    copyFiles(rels: string[]) { clipTo(rels, false); },
    cutFiles(rels: string[]) { clipTo(rels, true); },
    paste,
    undoLast() { return undoLast(); },
    dropAccepts,
    dropItems,
    importPaths,
    askAbout,

    pin,
    unpin,
    /** "Set as Root" on a folder row: its native path is a local place. */
    async pinRel(rel: string) {
      const abs = absOf(rel);
      if (abs) await pin(abs);
    },

    /** The roots of every terminal tab in the workspace, deduped, for the root switcher. */
    async tabRoots(): Promise<{ place: TreePlace; tabs: { id: string; name: string }[] }[]> {
      const ws = workspacesStore.activeWorkspace;
      if (!ws) return [];
      const out = new Map<string, { place: TreePlace; tabs: { id: string; name: string }[] }>();
      const tabs = ws.panes.flatMap((p) => p.tabs).filter((t) => (t.tab_type ?? 'terminal') === 'terminal' && !t.service_id);
      await Promise.all(tabs.map(async (t) => {
        const osc = terminalsStore.getOsc(t.id);
        try {
          const p = await commands.ftResolvePlace(t.id, { osc_cwd: osc?.cwd ?? null, osc_host: osc?.cwdHost ?? null, prompt_cwd: osc?.promptCwd ?? null, fresh: false });
          // A pinned workspace answers every tab with its pin, which says nothing new.
          if (p.missing || p.source === 'pin') return;
          const hit = out.get(p.place) ?? { place: p, tabs: [] };
          hit.tabs.push({ id: t.id, name: t.name });
          out.set(p.place, hit);
        } catch {
          /* a tab with no known folder isn't a root */
        }
      }));
      return [...out.values()];
    },

    displayRoot(p: TreePlace | null): string {
      return p ? tildePath(p.root, home) : '';
    },
  };
}

export const fileTreeStore = createFileTreeStore();
