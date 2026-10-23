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
 * - Re-root when the tab's ROOT changes (another repository, even one inside a home root);
 *   a plain folder inside the root only moves the tick. `cd src/lib` never reloads anything.
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
import type { AgentMark, BranchInfo, DirReq, FileOp, LedgerEntry, GitLetter, OpItem, OpResult, OpenAck, TreeEnvelope, TreePlace, TreeSearchHit } from '$lib/tauri/types';
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
import { activeFileOf, lookupIn, settleActiveFile, type ActiveFileTarget } from '$lib/filetree/activeFile';
import { ledgerPathIs, onAnotherHost, remoteWithin, sameSshHost } from '$lib/filetree/remote';
import { followStep } from '$lib/filetree/follow';
import { getBridgeInfo } from '$lib/stores/sshMcpBridge.svelte';
import { AgentMarks, TurnTracker, agentLensItems, assignColours, changedThisTurn, cwdInRoot, folderBadges, isLasting, markWords, overlappingWriters, rowView, type RowAgentView } from '$lib/filetree/agents';
import { agentStateStore } from '$lib/stores/agentState.svelte';
import { openBaselineDiff, openReadOnlyDiff } from '$lib/utils/openGitDiff';
import { baselineStatWords, type BaselineScope } from '$lib/filetree/baselines';
import { workspacesStore } from '$lib/stores/workspaces.svelte';
import { terminalsStore } from '$lib/stores/terminals.svelte';
import { activityStore } from '$lib/stores/activity.svelte';
import { preferencesStore } from '$lib/stores/preferences.svelte';
import { toastStore } from '$lib/stores/toasts.svelte';
import { getEditorByTabId, isEditorDirty } from '$lib/stores/editorRegistry.svelte';
import { openInEditor, editorFileInfo } from '$lib/utils/openInEditor';
import { editorTitleAfter } from '$lib/utils/editorTitle';
import { insertPathsIntoTab, type InsertItem } from '$lib/utils/insertPaths';

/** What the column shows when it isn't a tree. */
export type TreeStatus = 'none' | 'resolving' | 'opening' | 'ready' | 'missing' | 'gone' | 'error';

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

/** Whose words an op on this place is told in: a remote root's trash is the host's. */
const platformOf = (p: TreePlace | null): Platform => (p?.kind === 'remote' ? 'remote' : PLATFORM);

interface ViewState {
  model: TreeModel;
  scrollTop: number;
  selection: string[];
  focus: string | null;
  anchor: string | null;
}

/** The Lens bar (§9.4): the real tree, git's changed files, or what the agents changed this
 *  turn or this session. */
export type Lens = 'all' | 'changed' | 'turn' | 'session';

/** What the tree body draws: the real tree, find's results, or a lens's pruned tree. */
export type ViewMode = 'tree' | 'find' | Exclude<Lens, 'all'>;

/** An agent tab working in the root, for the agent strip (§3.2 item 3). */
export interface AgentChip {
  tabId: string;
  name: string;
  /** Index into the six agent colours (`--ft-agent-N`). */
  colour: number;
  /** From agentState: 'active' | 'idle' | 'permission'; null when no session is registered. */
  state: string | null;
  /** Files it changed this turn under this root. */
  changed: number;
}

/** A row's agent decorations, as TreeBody draws them. */
export interface RowAgent extends RowAgentView {
  /** Collapsed-folder badge: files changed under it this turn, in the busiest tab's colour. */
  badge: { count: number; colour: number } | null;
  /** The red ⚠ (§9.3), with the reason in words; `buffer` when an unsaved editor buffer is
   *  involved (the row offers Compare buffer ↔ disk). */
  conflict: { text: string; buffer: boolean } | null;
}

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
/** `revealInTree` waits this long for the folders above its path to list before answering. */
const REVEAL_ANSWER_MS = 2000;

/** How a reveal that waited on listings ended: it got its row; the listings show it isn't on
 *  disk, or that the filters hide it (or a folder above it), or that its folder lists more
 *  entries than the tree shows and it is one left out; or something else took over (another
 *  reveal, the human closing a folder above it, a new root). */
type RevealOutcome = 'shown' | 'missing' | 'hidden' | 'unlisted' | 'dropped';
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

  // Remote roots (§15). A root on an ssh host reopened from the LRU paints its last listing at
  // once, dimmed until the host's helper has listed the root again (stale-while-revalidate).
  let staleRoot = $state(false);
  /** The machine the followed tab's shell last reported (OSC 7), for the wrong-host warning. */
  let sourceOscHost = $state<string | null>(null);
  /** This computer's name: an OSC 7 naming it is the local shell's, from before ssh. */
  let localHost = $state<string | null>(null);

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
  /** A row to scroll to (and optionally select) once its listing arrives. `done` hears how it
   *  ended (`revealInTree` answers from it). */
  let pendingReveal: { rel: string; select: boolean; done?: (o: RevealOutcome) => void } | null = null;
  /** An Alt+→ in progress: the folder it started from and the row count it stops at. */
  let recursive: { root: string; budget: number } | null = null;

  // Git (§7): the decorations live on the model; these are what the branch row reads.
  let gitVersion = $state(0);
  let gitState = $state('off');
  let branch = $state<BranchInfo | null>(null);
  let gitCount = $state(0);

  // Find and the lenses (§8, §9.4).
  let lens = $state<Lens>('all');
  let find = $state.raw<FindState | null>(null);
  /** The find bar's input should take the keyboard (`/`, Cmd+F with the bar already open). */
  let findFocusSeq = $state(0);
  let searchSeq = 0;
  /** The pruned tree's rows, and the folders closed in it (open by default). */
  let lensRows: Row[] = [];
  const lensCollapsed = new Set<string>();
  /** The view to put back when find (or a lens) gives way. */
  let savedForFind: SavedView | null = null;
  let savedForLens: SavedView | null = null;

  // Agents (§9.3). Marks are the current root's (cleared on a re-root: the new subscription
  // is sent the marks still standing); turns and colours are the window's, by tab.
  const agentMarks = new AgentMarks();
  const turns = new TurnTracker();
  /** Bumped per applied `agent` event and per turn edge: rows read marks through it. */
  let agentVersion = $state(0);
  /** Bumped when a lit row stops being lit (the edge chips' only clock, and only while
   *  something is lit: idle sets no timer). */
  let litVersion = $state(0);
  let litTimer: ReturnType<typeof setTimeout> | null = null;
  let litAt = Infinity;
  /** tab → colour index, stable while the tab is in the window (`assignColours`). */
  let colours = new Map<string, number>();
  /** The chip the human chose: scopes the Turn and Session lenses (null: every agent). */
  let agentFilter = $state<string | null>(null);
  /** Follow agent (§9.3): off by default, per window, in memory only. */
  let followAgent = $state<string | null>(null);
  /** An agent pointed at a path (`revealInTree`) while the human was using the tree, or with
   *  the column closed: a chip offers it instead of moving their view. */
  let pointer = $state<{ abs: string; tabId: string; name: string } | null>(null);
  let badgeCache: { v: number; map: Map<string, { count: number; tab: string }> } = { v: -1, map: new Map() };

  // File operations (§10).
  let edit = $state<EditState | null>(null);
  let confirm = $state<ConfirmState | null>(null);
  let notice = $state<OpNotice | null>(null);
  let noticeTimer: ReturnType<typeof setTimeout> | null = null;
  /** The tree's own clipboard of files (Cmd/Ctrl+C / X); never the OS clipboard. */
  let clip = $state<{ place: string; rels: string[]; abs: string[]; cut: boolean; remote: boolean } | null>(null);
  /** The folder a drag (or an OS drop) would land in now: '' the root, null none. */
  let dropTarget = $state<string | null>(null);
  const undo = new UndoStacks();

  homeDir().then((h) => { home = h; }).catch(() => {});

  /** The place's host, or null for a root on this computer. */
  function rootHost(p: TreePlace | null = place): string | null {
    return p?.kind === 'remote' ? p.host_key : null;
  }

  /** Where a terminal tab's shell is: null on this computer (or not known to be elsewhere),
   *  else an ssh spelling of its host: the bridge's, or with the bridge off the ssh command
   *  saved with the tab. Read synchronously, for drag targets and the agent strip; typing
   *  into a tab checks its live ssh again (`insertPathsIntoTab`). */
  function tabHost(tabId: string): string | null {
    const b = getBridgeInfo(tabId);
    if (b) return b.hostKey;
    const t = workspacesStore._locateTab(tabId)?.tab;
    return t?.restore_ssh_command ?? null;
  }

  /** `tabHost`, read live where it can be: a tab with a running shell says what holds its
   *  terminal now (its ssh, or nothing), not what was last saved. For answers that refuse
   *  (`revealInTree`), where a stale "on a host" turns a local agent away from a local root. */
  async function liveTabHost(tabId: string): Promise<string | null> {
    const b = getBridgeInfo(tabId);
    if (b) return b.hostKey;
    const inst = terminalsStore.get(tabId);
    if (!inst) return tabHost(tabId);
    return commands.getPtyInfo(inst.ptyId).then((i) => i.foreground_command ?? null).catch(() => tabHost(tabId));
  }

  /** The tab's shell is on the root's machine. */
  function tabOnRootHost(tabId: string): boolean {
    const host = rootHost();
    const th = tabHost(tabId);
    return host === null ? th === null : th !== null && sameSshHost(th, host);
  }

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

  /** Find with a query wins; then a lens; else the real tree. Read untracked by callers
   *  inside effects. */
  function viewMode(): ViewMode {
    if (find && find.query.trim()) return 'find';
    if (lens !== 'all') return lens;
    return 'tree';
  }

  const isLensMode = (m: ViewMode) => m !== 'tree' && m !== 'find';

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
    } else if (mode === 'turn' || mode === 'session') {
      const items = agentLensItems(agentMarks, turns, mode, agentFilter, (rel) => m.entry(rel)?.k);
      const who = agentFilter ? tabName(agentFilter) : 'the agents';
      const emptyText = mode === 'turn' ? `Nothing changed by ${who} this turn` : `Nothing changed by ${who} this session`;
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
    if (isLensMode(to) && from === 'tree') savedForLens = snapshotView();
    else if (to === 'tree' && isLensMode(from)) {
      restoreView(savedForLens);
      savedForLens = null;
    }
    if (to !== 'tree') {
      lensCollapsed.clear();
      // One lens to another keeps the tree's view held and starts the new list at its top.
      if (from === 'tree' || to === 'find' || (isLensMode(from) && isLensMode(to))) scrollToTop();
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

  /** The Lens bar (and the branch row's "N changed"): show a lens, or the whole tree. */
  function setLens(next: Lens) {
    if (next === lens) return;
    const from = viewMode();
    const wasOn = lens !== 'all';
    lens = next;
    const to = viewMode();
    if (from === 'find' && to === 'find') {
      // The lens bar stays clickable under find with a query, which stays in front: the
      // lens changed the view BENEATH it, so what Escape puts back moves with it. Off: the
      // tree's own view (held for the lens) is now the one under find. On: the tree's view
      // is held for the lens, and find will close onto the lens, from its top. One lens to
      // another: find closes onto the new one, from its top.
      if (wasOn && next === 'all') {
        savedForFind = savedForLens;
        savedForLens = null;
      } else if (!wasOn) {
        savedForLens = savedForFind;
        savedForFind = savedForFind ? { ...savedForFind, scrollTop: 0 } : null;
      } else if (savedForFind) {
        savedForFind = { ...savedForFind, scrollTop: 0 };
      }
    } else {
      changeMode(from, to);
    }
    rebuildLens();
  }

  /** The branch row's "N changed": the Changed lens on or off. */
  function toggleChanged() {
    setLens(lens === 'changed' ? 'all' : 'changed');
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

  /** A file's content now, from the disk it is on: a remote root's through the editor's own
   *  remote read (scp over the place's ssh), which refuses what `readFile` refuses. */
  function readNow(abs: string) {
    const remote = remoteOf();
    return remote ? commands.scpReadFile(remote.sshCommand, abs) : commands.readFile(abs);
  }

  async function compareWithHead(rel: string) {
    const abs = absOf(rel);
    const sub = subId;
    if (!abs || sub === null) return;
    try {
      await openGitDiff({ subId: sub, abs, read: readNow }, rel, 'HEAD');
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
      // The failure is this tab's to show. Keeping the root on screen made it read as this
      // tab's folder: another tab's, or this one's from before it ssh'd somewhere Files
      // couldn't place (§4.1). The view is kept, so coming back costs nothing.
      saveView();
      closeSub();
      place = null;
      status = 'error';
      statusText = String(e);
      sourceTabId = tabId;
      sourceOscHost = null;
      placeWorkspaceId = ws.id;
      return;
    }
    if (superseded()) return;
    sourceTabId = tabId;
    sourceOscHost = terminalsStore.getOsc(tabId)?.cwdHost ?? null;
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
    const step = followStep(place, p);
    if (step === 'same' && subId !== null) {
      place = p;
      moveYouAreHere(p.cwd_rel);
      return;
    }
    // A plain folder inside the current root: only the tick moves. Another project's root
    // re-roots even when it lies inside this one (a home root), see `followStep`.
    if (step === 'locate' && place && subId !== null && ack) {
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
    if (!model || !place || place.missing) return;
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
    staleRoot = false;
    settleReveal('dropped');
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
    savedForLens = null;
    lensRows = [];
    // Marks are rels of the root being left; the next subscription is sent its own.
    agentMarks.clear();
    agentVersion++;
    stopLitTimer();
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
    // A host's helper may take a second or more to start (or be down): its last listing is
    // shown meanwhile, dimmed, until the root is listed again (§15.3). A local root answers in
    // milliseconds and needs no such hint.
    staleRoot = p.kind === 'remote' && kept?.model.dirs.get('')?.state === 'ok';
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
          if (env.rel === '') staleRoot = false;
          onListed(env.rel);
          noteDiskChange(env.rel, null);
          pruneEdit();
        }
        break;
      }
      case 'not_modified': {
        const r = m.applyNotModified(env);
        if (r === 'resync') commands.ftRefresh(sub, env.rel).catch(() => {});
        else {
          // The host confirmed what was painted.
          if (env.rel === '') staleRoot = false;
          onListed(env.rel);
        }
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
        if (env.rel === '') staleRoot = false;
        break;
      }
      case 'health':
        health = env.note ? { watch: env.watch, git: env.git, note: env.note } : null;
        gitState = env.git;
        if (env.git === 'off' && lens === 'changed') setLens('all');
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
      case 'agent':
        applyAgentMarks(env.marks);
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

  /** The pending reveal is over, one way or another: tell whoever waits on it. */
  function settleReveal(o: RevealOutcome) {
    const p = pendingReveal;
    pendingReveal = null;
    p?.done?.(o);
  }

  function checkPendingReveal() {
    if (!pendingReveal || !model) return;
    const { rel, select } = pendingReveal;
    const m = model;
    if (m.indexOf(rel) < 0) {
      // The listings show it can never get a row (not on disk, or it or a folder above it is
      // filtered out, or left out of a truncated listing): stop waiting, so later messages
      // don't each scan the rows for it. Nor does a folder the human closed meanwhile.
      if (m.knownGone(rel)) {
        // The reveal opened folders that aren't there: they don't stay open (and saved) for
        // nothing. The shallowest one closes everything it opened below it.
        const gone = ancestorsOf(rel).find((a) => m.expanded.has(a) && m.knownGone(a));
        if (gone !== undefined) {
          sendClose(m.collapse(gone).close);
          syncRows();
          scheduleSaveExpanded();
        }
        settleReveal('missing');
      } else if ([...ancestorsOf(rel), rel].some((a) => { const e = m.entry(a); return !!e && !m.visible(e); })) settleReveal('hidden');
      else if (m.rowGone(rel)) settleReveal('unlisted');
      else if (!ancestorsOf(rel).every((a) => m.expanded.has(a))) settleReveal('dropped');
      return;
    }
    settleReveal('shown');
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
   *  unless the reveal is automatic. Under find or the Changed lens, see `revealPlan`. `done`
   *  hears whether it got a row (not called for 'none'). */
  function revealRel(rel: string, kind: RevealKind, done?: (o: RevealOutcome) => void): ReturnType<typeof revealPlan> {
    if (!model) return 'none';
    const select = kind !== 'auto';
    const plan = revealPlan(kind, viewMode() !== 'tree', rows.some((x) => x.key === rel));
    if (plan === 'none') return plan;
    if (plan === 'scroll' || plan === 'select') {
      if (plan === 'select') {
        selection.clear();
        selection.add(rel);
        focusKey = rel;
        anchorKey = rel;
      }
      requestScroll(rel);
      done?.('shown');
      return plan;
    }
    if (plan === 'put-away') {
      // The real tree comes back to show it, with the lens and find put away first so the
      // restored view can't replace the selection.
      if (find) closeFind();
      if (lens !== 'all') setLens('all');
    }
    const r = model.reveal(rel);
    sendOpen(r.open);
    syncRows();
    if (r.open.length) scheduleSaveExpanded();
    settleReveal('dropped');
    pendingReveal = { rel, select, done };
    checkPendingReveal();
    return plan;
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
    // A file on another machine than the root is never under it, whatever its path spells.
    const here = lookupIn(target, rootHost(), sameSshHost);
    const s = await settleActiveFile(target, async (abs) => (here ? (await commands.ftLocate(sub, [abs]))[0] ?? null : null));
    if (my !== activeSeq || sub !== subId) return;
    activeFile = s.active;
    outsideFile = s.outside;
    if (preferencesStore.fileTreeAutoReveal && s.active) revealRel(s.active, 'auto');
  }

  /** "Reveal in Files" from a tab menu: the human asked, so the column opens (if it was
   *  closed), the row is selected and the tree takes focus. */
  async function revealPath(abs: string, host: string | null = null) {
    await workspacesStore.setFileTreeOpen(true);
    // The column mounts and resolves; wait for a subscription before locating.
    for (let i = 0; i < 40 && subId === null; i++) await new Promise((r) => setTimeout(r, 50));
    if (subId === null) return;
    // `host`: the ssh command a remote editor's file is behind. Only a root on that machine
    // holds the file, whatever its path spells.
    const here = lookupIn({ path: abs, offerShow: false, host }, rootHost(), sameSshHost);
    const [rel] = here ? await commands.ftLocate(subId, [abs]).catch(() => [null]) : [null];
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

  /** A remote root's files open through the editor's remote path (scp over the tab's own ssh
   *  command, as a click on a remote path in a terminal does): the place's connect args. */
  function remoteOf(p: TreePlace | null = place): { sshCommand: string } | null {
    const host = rootHost(p);
    return host ? { sshCommand: host } : null;
  }

  async function openFile(rel: string, mode: OpenMode, keepFocus = false) {
    const abs = absOf(rel);
    const ws = workspacesStore.activeWorkspace;
    const pane = workspacesStore.activePane;
    if (!abs || !ws || !pane) return;
    const remote = remoteOf();
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
          ? (await openInEditor({ abs, remote, workspaceId: ws.id, paneId: other.id })).tabId
          : await workspacesStore.openEditorInNewSplit(ws.id, pane.id, editorFileInfo(abs, remote));
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
      const res = await openInEditor({ abs, remote, workspaceId: ws.id, paneId: pane.id, replaceTabId });
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

  /** Map this window's editor tabs onto rows. Rust spells them (`ftLocate`); called from an
   *  effect when the editor set or the root changes, so never per render. Only the tabs on
   *  the root's machine (`host`: null local, else the remote editor's ssh command). */
  async function syncEditorMarks(all: { id: string; path: string; host: string | null }[]) {
    const sub = subId;
    const host = rootHost();
    const tabs = all.filter((t) => (host === null ? t.host === null : t.host !== null && sameSshHost(t.host, host)));
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
    // The file manager and default apps are this computer's: a host's path means nothing here.
    if (abs && !rootHost()) await commands.revealInFileManager(abs).catch((e) => toastStore.addToast('Couldn’t reveal', String(e), 'error'));
  }

  async function openWithDefaultApp(rel: string) {
    const abs = absOf(rel);
    if (abs && !rootHost()) await commands.openWithDefaultApp(abs).catch((e) => toastStore.addToast('Couldn’t open', String(e), 'error'));
  }

  /** A new terminal tab whose shell starts in that folder (or the file's folder). On a remote
   *  root, a new ssh session to the host that starts there: the tab is started the way a
   *  cloned ssh tab is (`ssh -t <host> '… cd <folder> && exec $SHELL -l'`, buildSshCommand),
   *  so it gets its own tab identity and bridge like any maiTerm-started session. */
  async function openTerminalHere(rel: string) {
    const m = model;
    const ws = workspacesStore.activeWorkspace;
    const pane = workspacesStore.activePane;
    if (!m || !ws || !pane) return;
    const e = m.entry(rel);
    const dirRel = rel === '' || (e && isExpandable(e)) ? rel : parentRel(rel);
    const cwd = absOf(dirRel);
    if (!cwd) return;
    const host = rootHost();
    await workspacesStore.createTab(ws.id, pane.id, lastSegment(cwd) || cwd, {
      context: host ? { cwd: null, sshCommand: host, remoteCwd: cwd } : { cwd, sshCommand: null, remoteCwd: null },
    });
  }

  /** The tab "Add to ‹tab›" (and Alt+Enter) types into: the agent being followed when it is
   *  live, else the tab the tree follows. */
  const addTarget = $derived.by(() => {
    void terminalsStore.instanceVersion;
    for (const id of [followAgent, sourceTabId]) {
      if (!id || !terminalsStore.get(id)) continue;
      const loc = workspacesStore._locateTab(id);
      if (!loc || loc.tab.service_id) continue;
      return { tabId: id, name: loc.tab.name };
    }
    return null;
  });

  /** What `insertPathsIntoTab` is handed for these rows: local files, or paths on the root's
   *  host, which go only to a tab on that same host (typed as they are, never uploaded). */
  function insertItems(rels: string[]): InsertItem[] {
    const host = rootHost();
    return rels.map((r) => absOf(r)).filter((a): a is string => !!a).map((abs) => (host ? { abs, remote: { hostKey: host, path: abs } } : { abs }));
  }

  /** Insert the paths into the followed tab's command line, never submitting (§9.5). */
  async function addToTab(rels: string[]) {
    const target = addTarget;
    if (!target || !rels.length) return;
    const items = insertItems(rels);
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
        if (!loc || loc.tab.tab_type !== 'editor' || !f) continue;
        // A remote editor (marked here only on its own host's root) keeps its ssh command and
        // follows by its path there.
        const info = { ...f, file_path: abs, remote_path: f.is_remote ? abs : f.remote_path, language: editorFileInfo(abs).language ?? f.language };
        // A title that is still the file's name follows it; one the human typed stays.
        const name = editorTitleAfter(loc.tab.name, f, info, isWindows());
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
        // The second confirm (§10.3): name the agents at work, and resend with ack_agents
        // (a delete's ticket is still good: Rust uses it up only after the guards).
        ask(agentActiveQuestion(op, n.tabs, n.detail), [{ label: 'Do It Anyway', tone: 'danger', run: () => runOp({ ...op, ack_agents: true }, o) }]);
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

    const text = describeResult(op, r, platformOf(p));
    let action: OpNotice['action'] = null;
    if (o.undoing) {
      const words = `Undid the ${o.undoing.label}${op.kind === 'trash' && r.done.length ? ` (moved to ${trashPlace(platformOf(p))})` : ''}`;
      const extra = r.failed.length ? ` · ${r.failed[0].msg}` : '';
      if (r.done.length || r.failed.length) setNotice({ text: words + extra, action: null });
      announce(words + extra);
    } else {
      if (r.undo && r.done.length) {
        const entry = undo.push(p.place, r.undo, undoLabel(op, r));
        action = { label: 'Undo', run: () => void undoLast(entry) };
      } else if (op.kind === 'trash' && r.done.length && platformOf(p) === 'mac') {
        // (A remote trash always answers with an undo: its `restore` runs on the host.)
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
    if (lens !== 'all') setLens('all');
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
   *  would send their next keys (Enter, Delete) to the tree.
   *
   *  Body is ambiguous. The input's blur handler decides in a microtask, and on a click
   *  that microtask runs between the blur and the focus of whatever was clicked, so focus
   *  reads as body on its way to a terminal or an editor. Body is judged again once the
   *  click's task is over, where it can only mean focus really went nowhere. */
  function refocusAfterEdit() {
    if (typeof document === 'undefined') return;
    const intoTree = (a: Element | null) => !!a && a !== document.body && !!a.closest('[data-filetree]');
    const a = document.activeElement;
    if (intoTree(a)) {
      focusPending = true;
      return;
    }
    if (a && a !== document.body) return;
    setTimeout(() => {
      // A new inline edit owns the keyboard by now; taking it for the tree would blur it.
      if (edit) return;
      const now = document.activeElement;
      if (!now || now === document.body || intoTree(now)) focusPending = true;
    }, 0);
  }

  /** The row being renamed, or the folder a New input sits in, went away on disk (an agent
   *  renamed or deleted it). The input unmounted with its row without a word, so the edit is
   *  dropped with it: left set, it would block every drag in the tree. A commit already sent
   *  settles itself. */
  function pruneEdit() {
    const e = edit;
    const m = model;
    if (!e || e.pending || !m) return;
    const rel = e.mode === 'rename' ? e.rel : e.parent;
    if (m.knownGone(rel)) {
      edit = null;
      return;
    }
    // In the real tree the input lives on its row, so it goes when the row does: the filters
    // now hide it (it became ignored), or its folder's listing dropped it (a truncated folder
    // re-listed after a delete, which knownGone rightly can't call proof). Find and the
    // Changed lens draw their rows from elsewhere, so there it stays.
    if (viewMode() !== 'tree') return;
    const x = m.entry(rel);
    if (x ? !m.visible(x) : m.rowGone(rel)) edit = null;
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
    // The names the host's disk takes: a root on an ssh host follows its own rules, not this
    // computer's (a `:` is fine on Linux, from a Windows client too).
    const windows = PLATFORM === 'windows' && place?.kind !== 'remote';
    return {
      mode: e.mode,
      strictWindows: windows || a.case_insensitive,
      windows,
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
    const label = trashLabel(platformOf(place));
    if (dirty.length) ask(`${unsavedNote(dirty)}${label} anyway?`, [{ label, tone: 'danger', run: go }]);
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
    clip = { place: place.place, rels, abs: rels.map((r) => relToAbs(ack!, r)), cut, remote: place.kind === 'remote' };
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
    // An import reads its sources on this computer (onto a host too: Rust sends them over),
    // so a cut or copy taken from a root on an ssh host has nothing here to read.
    if (c.remote) {
      const text = 'Files can’t copy from an ssh host into another folder yet: copy within that host’s folder instead';
      setNotice({ text, action: null });
      announce(text);
      return;
    }
    const r = await runOp({ kind: 'import', src: c.abs, dest: to }, { select: true });
    if (r && c.cut) announce('Files from another root are copied, not moved');
  }

  // Undo (§10.4).

  /** Undos run one at a time: a held or double Cmd+Z must not send the second undo before the
   *  first has landed (it would find its file not there yet). */
  let undoQueue: Promise<void> = Promise.resolve();

  /** Cmd/Ctrl+Z: the newest op on this root, unless it changed on disk since. `expected`
   *  (the footer's Undo) only acts while that entry is still the newest. */
  function undoLast(expected?: UndoEntry): Promise<void> {
    const next = undoQueue.then(() => undoOne(expected));
    undoQueue = next.catch((e) => logError(`[files] undo: ${e}`));
    return next;
  }

  async function undoOne(expected?: UndoEntry) {
    if (!place || !writable()) return;
    const where = place.place;
    const top = undo.peek(where);
    if (!top || (expected && top !== expected)) {
      announce('Nothing to undo');
      if (!top) setNotice({ text: 'Nothing to undo in this folder', action: null });
      return;
    }
    const stale = undo.check(top, lookup, Date.now());
    if (stale) {
      undo.pop(where);
      const text = `Can’t undo the ${top.label}: ${stale}`;
      setNotice({ text, action: null });
      announce(text);
      return;
    }
    const go = async () => {
      // Asked first, and the answer came later: only while it is still this root's newest.
      if (place?.place !== where || undo.peek(where) !== top) return;
      undo.pop(where);
      const sel = top.op.kind === 'rename' || top.op.kind === 'move' || top.op.kind === 'restore';
      await runOp(top.op, { undoing: top, select: sel });
    };
    // Undoing a create, copy, duplicate or import trashes what it made, whatever was typed
    // into it since: the same question over unsaved edits as any trash (§10.3). Left on the
    // stack until the answer, so Cancel loses nothing.
    const dirty = top.op.kind === 'trash' ? dirtyNames(top.op.src ?? []) : [];
    if (dirty.length) {
      ask(`${unsavedNote(dirty)}Undoing the ${top.label} moves it to ${trashPlace(platformOf(place))}.`, [
        { label: 'Undo Anyway', tone: 'danger', run: go },
      ]);
      return;
    }
    await go();
  }

  // Drag and drop (§10.4).

  /** Whether a drag of `srcs` would do anything at `t`. */
  function dropAccepts(srcs: string[], t: DropTarget, copy: boolean): boolean {
    // A host's files go only to a terminal on that host, typed as its paths; the composer
    // attaches files from this computer, so it takes none of them.
    if (t.kind === 'terminal') return !rootHost() || tabOnRootHost(t.tabId);
    if (t.kind === 'composer') return !rootHost();
    return writable() && dropAccepted(srcs, t.rel, copy);
  }

  async function dropItems(srcs: string[], t: DropTarget, copy: boolean) {
    const abs = srcs.map((r) => absOf(r)).filter((a): a is string => !!a);
    if (t.kind === 'terminal') {
      const r = await insertPathsIntoTab(t.tabId, insertItems(srcs));
      if (r.ok) announce(`Added ${quotedNames(srcs)} to the tab`);
      return;
    }
    if (t.kind === 'composer') {
      if (rootHost()) return;
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
    // Composer attachments are files on this computer (`canAskAbout`).
    if (!target || !rels.length || rootHost()) return;
    const abs = rels.map((r) => absOf(r)).filter((a): a is string => !!a);
    if (!workspacesStore.isComposerOpen(target.tabId)) workspacesStore.toggleComposer(target.tabId);
    attachToComposer(target.tabId, abs);
    announce(`Attached ${quotedNames(rels)} to ${target.name}’s composer`);
  }

  // ── Agents (§9.3–§9.6) ────────────────────────────────────────────────────────────────
  //
  // Rust sends this root's marks on the subscription (`agent` events, and the marks still
  // standing whenever a subscription starts or resumes). What lives here is the human's side:
  // how rows, folders, the strip and the lenses show them, follow-agent, and the two tools an
  // agent asks this window (`revealInTree`, `getTreeSelection`).

  /** Bump without subscribing: callers can run inside an effect (a re-root does). */
  function bumpAgents() {
    untrack(() => { agentVersion++; });
  }

  function tabName(tabId: string): string {
    return workspacesStore._locateTab(tabId)?.tab.name ?? 'an agent';
  }

  /** tab → its newest mark's time: the end of a turn whose Stop this window never saw. */
  const tabLastAt = new Map<string, number>();

  function applyAgentMarks(marks: AgentMark[]) {
    const now = Date.now();
    for (const m of marks) {
      turns.noteMark(m);
      if ((tabLastAt.get(m.tab_id) ?? 0) < m.at_ms) tabLastAt.set(m.tab_id, m.at_ms);
    }
    agentMarks.merge(marks, now);
    bumpAgents();
    scheduleLit(now);
    const mode = viewMode();
    if (mode === 'turn' || mode === 'session') rebuildLens();
    followAgentTo(marks);
  }

  function stopLitTimer() {
    if (litTimer) clearTimeout(litTimer);
    litTimer = null;
    litAt = Infinity;
  }

  /** One timer, for the next lit row to go dark (a read's light, a write's sweep), and only
   *  while one is lit. A running call needs none: its Post is an event of its own. */
  function scheduleLit(now: number) {
    let next = Infinity;
    for (const slots of agentMarks.byRel.values()) {
      const until = rowView([...slots.values()], now, turns).litUntil;
      if (until !== null && until > now && until < next) next = until;
    }
    if (next === Infinity) {
      stopLitTimer();
      return;
    }
    if (litTimer && litAt <= next) return;
    stopLitTimer();
    litAt = next;
    litTimer = setTimeout(() => {
      litTimer = null;
      litAt = Infinity;
      const t = Date.now();
      agentMarks.pruneReads(t);
      untrack(() => { litVersion++; });
      bumpAgents();
      scheduleLit(t);
    }, Math.max(0, next - now) + 20);
  }

  /** The window's agent tabs in window order: an agent session registered for the tab, or
   *  marks from it in this root. Colours are assigned over this list, so a tab keeps its
   *  colour whichever root is showing. */
  const windowAgents = $derived.by(() => {
    void agentVersion;
    const marked = agentMarks.tabs();
    const out: { id: string; name: string }[] = [];
    for (const ws of workspacesStore.workspaces) {
      for (const p of ws.panes) {
        for (const t of p.tabs) {
          if ((t.tab_type ?? 'terminal') !== 'terminal' || t.service_id) continue;
          if (agentStateStore.getState(t.id) || marked.has(t.id)) out.push({ id: t.id, name: t.name });
        }
      }
    }
    return out;
  });

  const colourMap = $derived.by(() => {
    const next = assignColours(colours, windowAgents.map((t) => t.id));
    colours = next;
    return next;
  });

  function colourOf(tabId: string): number {
    return colourMap.get(tabId) ?? 0;
  }

  /** The agent strip (§3.2 item 3): agent tabs whose folder is in the root, or that left
   *  marks in it. */
  const agentChips = $derived.by((): AgentChip[] => {
    void agentVersion;
    const a = ack;
    const p = place;
    if (!a || !p || !preferencesStore.fileTreeAgentActivity) return [];
    const marked = agentMarks.tabs();
    const counts = changedThisTurn(agentMarks, turns);
    const roots = [a.root_native, p.root];
    const host = rootHost(p);
    return windowAgents
      .filter((t) => {
        if (marked.has(t.id)) return true;
        const cwd = agentStateStore.getState(t.id)?.cwd;
        if (!cwd) return false;
        // A remote agent's folder is a path on its host (§15.3: host and path together), so
        // it is in a remote root only when its tab's ssh reaches the root's host.
        if (host) {
          const th = tabHost(t.id);
          return !!th && sameSshHost(th, host) && roots.some((r) => remoteWithin(cwd, r));
        }
        return cwdInRoot(cwd, roots, a.sep, a.case_insensitive);
      })
      .map((t) => ({
        tabId: t.id,
        name: t.name,
        colour: colourOf(t.id),
        state: agentStateStore.getState(t.id)?.state ?? null,
        changed: counts.get(t.id) ?? 0,
      }));
  });

  /** How many paths the Turn and Session lenses would show now. */
  const lensCounts = $derived.by(() => {
    void agentVersion;
    if (!preferencesStore.fileTreeAgentActivity) return { turn: 0, session: 0 };
    const kind = () => undefined;
    return {
      turn: agentLensItems(agentMarks, turns, 'turn', agentFilter, kind).length,
      session: agentLensItems(agentMarks, turns, 'session', agentFilter, kind).length,
    };
  });

  function badges(): Map<string, { count: number; tab: string }> {
    const v = agentVersion;
    if (badgeCache.v !== v) badgeCache = { v, map: folderBadges(agentMarks, turns) };
    return badgeCache.map;
  }

  /** A tab's newest turn as a time span, for the two-writer conflict. */
  function turnSpan(tabId: string): { start: number; end: number } | null {
    const st = agentStateStore.getState(tabId)?.state;
    return turns.span(tabId, tabLastAt.get(tabId) ?? 0, st === 'active' || st === 'permission');
  }

  /** A row's agent decorations, read reactively through `agentVersion` (and the editor
   *  dots, for the unsaved-buffer conflict). Null when there is nothing to draw. */
  function rowAgent(key: string, folder: boolean, expanded: boolean): RowAgent | null {
    void agentVersion;
    if (!preferencesStore.fileTreeAgentActivity) return null;
    let badge: RowAgent['badge'] = null;
    // Collapsed folders over activity get a count; they never open by themselves (§9.3).
    if (folder && !expanded) {
      const b = badges().get(key);
      if (b) badge = { count: b.count, colour: colourOf(b.tab) };
    }
    const marks = agentMarks.marksOf(key);
    if (!marks.length) {
      return badge ? { rail: null, writers: [], deleted: false, editing: false, litUntil: null, marks: [], badge, conflict: null } : null;
    }
    const v = rowView(marks, Date.now(), turns);
    let conflict: RowAgent['conflict'] = null;
    const dirty = !folder && v.writers.length > 0 && !!editorRels.get(key)?.some((t) => isEditorDirty(t));
    if (dirty) {
      const who = v.writers.map((t) => `“${tabName(t)}”`).join(' and ');
      conflict = { text: `unsaved changes in an editor tab, and ${who} wrote this file this turn`, buffer: true };
    } else if (!folder) {
      const pair = overlappingWriters(marks, turns, turnSpan);
      if (pair) conflict = { text: `“${tabName(pair[0])}” and “${tabName(pair[1])}” both wrote this file in turns that overlapped`, buffer: false };
    }
    return { ...v, badge, conflict };
  }

  /** The tooltip's words for a row's agent marks, newest first: who, what, and on what
   *  evidence. The tool name comes from the tab's ledger (hover only, like `ftStat`). */
  async function agentWords(rel: string): Promise<string[]> {
    if (!preferencesStore.fileTreeAgentActivity) return [];
    const marks = agentMarks.marksOf(rel).filter((m) => isLasting(m.op)).sort((a, b) => b.at_ms - a.at_ms).slice(0, 4);
    if (!marks.length) return [];
    const abs = absOf(rel);
    const host = rootHost();
    const ledgers = new Map<string, Promise<LedgerEntry[]>>();
    const out: string[] = [];
    for (const m of marks) {
      let tool: string | null = null;
      if (abs && m.evidence !== 'commit') {
        let l = ledgers.get(m.tab_id);
        if (!l) ledgers.set(m.tab_id, (l = commands.agentFilesActivity(m.tab_id, 'session', false).catch(() => [])));
        // An ssh tab's ledger keys its paths by host (`host:/abs`).
        const hit = (await l).filter((e) => e.op === m.op && e.evidence === m.evidence && ledgerPathIs(e.path, abs, host)).pop();
        tool = hit?.tool ?? null;
      }
      out.push(markWords(m, `“${tabName(m.tab_id)}”`, turns.isThisTurn(m), tool));
    }
    return out;
  }

  /** The words for the second confirm (§10.3): which agent tabs, doing what. */
  function agentActiveQuestion(op: FileOp, tabs: string[], fallback: string): string {
    const names = [...new Set(tabs.map(tabName))];
    if (!names.length) return fallback;
    const who = names.length === 1 ? `“${names[0]}” is` : names.length === 2 ? `“${names[0]}” and “${names[1]}” are` : `${names.length} agent tabs are`;
    const it = (op.src?.length ?? 0) > 1 ? 'them' : 'it';
    const verb = op.kind === 'rename' ? `Rename ${it}` : op.kind === 'move' ? `Move ${it}` : op.kind === 'trash' ? trashLabel(platformOf(place)) : op.kind === 'delete' ? `Delete ${it}` : 'Go ahead';
    return `${who} working on ${quotedNames(op.src ?? [])} right now, or changed ${it} this turn. ${verb} anyway?`;
  }

  /** Follow agent (§9.3): reveal what the followed tab writes, but only once the human has
   *  left the tree alone for 3 s, and never over an inline edit. */
  function followAgentTo(marks: AgentMark[]) {
    const id = followAgent;
    if (!id || edit || viewMode() === 'find' || Date.now() - lastHumanInputMs < HUMAN_QUIET_MS) return;
    let latest: AgentMark | null = null;
    for (const m of marks) {
      if (m.tab_id !== id || !isLasting(m.op) || m.op === 'delete' || m.phase === 'failed') continue;
      if (!latest || m.at_ms >= latest.at_ms) latest = m;
    }
    if (latest && turns.isThisTurn(latest)) revealRel(latest.rel, 'auto');
  }

  /** The hook edges that move turns: a prompt starts one, Stop ends it, a new session (not a
   *  compaction) starts the ledger's numbering again. Hook events reach every window. */
  function onTurnEdge(kind: 'prompt' | 'stop' | 'session', tabId: string, turn?: number | null) {
    const now = Date.now();
    if (kind === 'prompt') turns.turnStarted(tabId, now, turn);
    else if (kind === 'stop') turns.turnEnded(tabId, now);
    else {
      turns.forget(tabId);
      agentMarks.forgetTab(tabId);
      tabLastAt.delete(tabId);
    }
    bumpAgents();
    const mode = viewMode();
    if (mode === 'turn' || mode === 'session') rebuildLens();
  }

  /** The column opens: start each tab's turn from the ledger's count. The primed marks alone
   *  would leave a turn that has only read (or not yet called a tool) unknown, and the
   *  previous turn's writes would count as this one's. Asked once the hook listeners are up
   *  (an edge before then is in the read; one after it is heard), before the subscription
   *  has primed its marks; `seed` never lowers a turn heard since, and skips a tab with a
   *  prompt or a new session heard after the read was sent, whose event the read can't be
   *  ordered against. */
  async function seedTurns() {
    const readAt = turns.edgeMark();
    const list = await commands.agentFilesTurns().catch(() => []);
    if (!attached || list.length === 0) return;
    for (const t of list) turns.seed(t.tab_id, t.turn, t.start_ms, t.end_ms, readAt);
    bumpAgents();
    const mode = viewMode();
    if (mode === 'turn' || mode === 'session') rebuildLens();
  }

  /** `revealInTree` (§9.6), in the caller's window. It never opens, resizes or focuses the
   *  column, and never moves a view the human is using: those cases leave a chip instead. */
  async function agentReveal(callerTab: string | undefined, path: string, select: boolean): Promise<{ result: string; note?: string }> {
    const name = callerTab ? tabName(callerTab) : 'an agent';
    const point = () => { pointer = { abs: path, tabId: callerTab ?? '', name }; };
    if (!workspacesStore.fileTreeOpen) {
      point();
      return { result: 'tree_closed' };
    }
    for (let i = 0; i < 40 && subId === null && (status === 'resolving' || status === 'opening'); i++) await new Promise((r) => setTimeout(r, 50));
    const sub = subId;
    if (sub === null) {
      return { result: 'outside_root', note: 'Files is not showing a folder right now.' };
    }
    // A path names a file on the caller's own machine: only a root on that machine holds it.
    const host = rootHost();
    const callerHost = callerTab ? await liveTabHost(callerTab) : null;
    if (host ? !callerHost || !sameSshHost(callerHost, host) : callerHost !== null) {
      return { result: 'outside_root', note: host ? `Files is showing a folder on ${place?.host_label ?? host}, another machine than yours.` : 'Files is showing a folder on your human’s computer, not on your host.' };
    }
    const [rel] = await commands.ftLocate(sub, [path]).catch(() => [null]);
    if (sub !== subId) return { result: 'outside_root', note: 'Files moved to another folder meanwhile.' };
    if (rel == null) return { result: 'outside_root' };
    if (edit || Date.now() - lastHumanInputMs < HUMAN_QUIET_MS) {
      point();
      return { result: 'noted' };
    }
    if (rel === '') {
      scrollToTop();
      return { result: 'revealed' };
    }
    // Answered from what the tree did, not from having asked: a path that isn't on disk, or
    // that the filters hide, opens folders but never gets a row, and "revealed" would send the
    // human looking for something the tree isn't showing.
    let settle!: (o: RevealOutcome) => void;
    const outcome = new Promise<RevealOutcome>((r) => { settle = r; });
    const plan = revealRel(rel, select ? 'focus' : 'auto', (o) => settle(o));
    if (plan === 'none') {
      point();
      return { result: 'noted', note: 'Your human has the tree on a filtered view (find or a lens) that leaves this path out: a chip offers it instead.' };
    }
    let timer: ReturnType<typeof setTimeout> | undefined;
    const o = await Promise.race([outcome, new Promise<'slow'>((r) => { timer = setTimeout(() => r('slow'), REVEAL_ANSWER_MS); })]);
    clearTimeout(timer);
    switch (o) {
      case 'shown':
        return { result: 'revealed' };
      case 'slow':
        return { result: 'revealed', note: 'The folders above it are still listing: it is scrolled into view when they arrive.' };
      case 'missing':
        return { result: 'not_found', note: 'Nothing by that name is on disk in the folder Files shows.' };
      case 'hidden':
        return { result: 'hidden', note: 'It is on disk, but the tree hides it, or a folder above it: your human has hidden or ignored files turned off.' };
      case 'unlisted':
        return { result: 'hidden', note: 'Its folder holds more entries than the tree lists, and it is one of those left out.' };
      case 'dropped':
        point();
        return { result: 'noted', note: 'The tree moved on before it was shown (another reveal, your human closed a folder above it, or a new root): a chip offers it instead.' };
    }
  }

  /** `getTreeSelection` (§9.6): the human's selection, as absolute paths, in this window. */
  function selectionForAgent() {
    const open = workspacesStore.fileTreeOpen;
    const p = place;
    const humanIdleMs = lastHumanInputMs ? Date.now() - lastHumanInputMs : null;
    if (!p || p.missing) return { open, host: null, root: null, focused: null, selected: [] as string[], humanIdleMs };
    // A closed column has no subscription and so no root to spell paths with: nothing is
    // selected there that the human can see. On a remote root the paths are the host's,
    // named by `host`.
    const selected = [...selection].map((r) => absOf(r)).filter((x): x is string => !!x);
    const host = p.kind === 'remote' ? p.host_label ?? p.host_key : null;
    return { open, host, root: ack?.root_native ?? p.root, focused: focusKey ? absOf(focusKey) : null, selected, humanIdleMs };
  }

  /** The pointer chip's "Show": the human asked, so the view may change. */
  async function showPointer() {
    const p = pointer;
    const sub = subId;
    pointer = null;
    if (!p || sub === null) return;
    const [rel] = await commands.ftLocate(sub, [p.abs]).catch(() => [null]);
    if (sub !== subId) return;
    if (rel == null) {
      toastStore.addToast('Files', 'That path is outside the folder Files is showing.', 'info');
      return;
    }
    if (rel) revealRel(rel, 'asked');
    else scrollToTop();
  }

  /** "Compare buffer ↔ disk" (§9.3): an agent wrote a file that has unsaved edits in an
   *  editor tab. The disk's version on the left, the unsaved buffer on the right. */
  async function compareBufferWithDisk(rel: string) {
    const abs = absOf(rel);
    const tabId = editorRels.get(rel)?.find((t) => isEditorDirty(t)) ?? editorRels.get(rel)?.[0];
    const ed = tabId ? getEditorByTabId(tabId) : undefined;
    const ws = workspacesStore.activeWorkspace;
    const pane = workspacesStore.activePane;
    if (!abs || !ed || !ws || !pane) return;
    let disk: string;
    try {
      disk = (await readNow(abs)).content;
    } catch (e) {
      toastStore.addToast('Couldn’t read the file on disk', e instanceof Error ? e.message : String(e), 'error');
      return;
    }
    await openReadOnlyDiff({
      filePath: abs,
      oldContent: disk,
      newContent: ed.view.state.doc.toString(),
      tabName: `Diff: ${baseName(rel)} (disk ↔ unsaved)`,
      workspaceId: ws.id,
      paneId: pane.id,
      afterTabId: pane.active_tab_id,
    }).catch((e) => toastStore.addToast('Couldn’t compare', String(e), 'error'));
  }

  // ── Turn baselines (§14) ──────────────────────────────────────────────────────────────

  /** The agent tabs that edited `rel` through a tool (Edit, Write, a patch: the calls that
   *  capture a baseline), newest first, and whether that was this turn. A change seen while a
   *  shell ran, or a commit, has no baseline (Compare with HEAD covers those). */
  function baselineWriters(rel: string): { tabId: string; name: string; thisTurn: boolean }[] {
    // Baselines are read off this computer's disk at the agent's PreToolUse, so a remote
    // agent's files have none: on a host the menu keeps Compare with HEAD only.
    if (!preferencesStore.fileTreeTurnBaselines || !preferencesStore.fileTreeAgentActivity || !absOf(rel) || rootHost()) return [];
    const byTab = new Map<string, { at: number; thisTurn: boolean }>();
    for (const m of agentMarks.marksOf(rel)) {
      if (m.evidence !== 'tool' || !isLasting(m.op)) continue;
      const cur = byTab.get(m.tab_id) ?? { at: 0, thisTurn: false };
      byTab.set(m.tab_id, { at: Math.max(cur.at, m.at_ms), thisTurn: cur.thisTurn || turns.isThisTurn(m) });
    }
    return [...byTab.entries()].sort((a, b) => b[1].at - a[1].at).map(([tabId, v]) => ({ tabId, name: tabName(tabId), thisTurn: v.thisTurn }));
  }

  /** "Compare with Turn Start" / "Session Start": the file before `tabId`'s agent first
   *  edited it, against the disk now. */
  async function compareWithBaseline(rel: string, tabId: string, scope: BaselineScope) {
    const abs = absOf(rel);
    const ws = workspacesStore.activeWorkspace;
    const pane = workspacesStore.activePane;
    if (!abs || !ws || !pane) return;
    try {
      await openBaselineDiff({ tabId, agentTabName: tabName(tabId), abs, scope, workspaceId: ws.id, paneId: pane.id, afterTabId: pane.active_tab_id });
    } catch (e) {
      toastStore.addToast(scope === 'turn' ? 'Couldn’t compare with the turn’s start' : 'Couldn’t compare with the session’s start', e instanceof Error ? e.message : String(e), 'error');
    }
  }

  /** The Turn lens tooltip's line counts (§14): for each tab that edited the file this turn
   *  (the chosen chip's only, when one is chosen), "+N −N since turn 14 began", with the tab
   *  named when there are two. Hover only: each is one read of the file. */
  async function turnDeltaWords(rel: string): Promise<string[]> {
    if (lens !== 'turn') return [];
    const abs = absOf(rel);
    if (!abs) return [];
    const writers = baselineWriters(rel).filter((w) => w.thisTurn && (!agentFilter || w.tabId === agentFilter));
    const out: string[] = [];
    for (const w of writers) {
      const st = await commands.agentFilesBaselineStat(w.tabId, abs, 'turn').catch(() => null);
      const words = st ? baselineStatWords(st) : null;
      if (words) out.push(writers.length > 1 ? `${words} (“${w.name}”)` : words);
    }
    return out;
  }

  /** A reload gave an agent tab a new id: what this window keeps by tab follows it. */
  function remapAgentTab(oldId: string, newId: string) {
    agentMarks.remapTab(oldId, newId);
    turns.remap(oldId, newId);
    const at = tabLastAt.get(oldId);
    if (at !== undefined) {
      tabLastAt.delete(oldId);
      tabLastAt.set(newId, at);
    }
    const c = colours.get(oldId);
    if (c !== undefined) {
      colours = new Map(colours);
      colours.delete(oldId);
      colours.set(newId, c);
    }
    if (agentFilter === oldId) agentFilter = newId;
    if (followAgent === oldId) followAgent = newId;
    if (pointer?.tabId === oldId) pointer = { ...pointer, tabId: newId };
    bumpAgents();
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
      if (sourceOscHost !== (osc.cwdHost ?? null)) sourceOscHost = osc.cwdHost ?? null;
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
    const hookOffs: UnlistenFn[] = [];
    const hooksUp: Promise<void>[] = [];
    const onHook = <T>(event: string, fn: (p: T) => void) => {
      hooksUp.push(listen<T>(event, (e) => { if (e.payload) fn(e.payload); }).then((u) => {
        if (attached) hookOffs.push(u);
        else u();
      }).catch(() => {}));
    };
    onHook<{ tab_id?: string; source?: string }>('agent-hook-session-start', (p) => {
      if (!p.tab_id) return;
      if (p.tab_id === sourceTabId) scheduleResolve(true);
      // A startup, resume or /clear is a new session, numbered afresh. A compaction carries
      // the turn on, and initSession's re-emit (no source) is the same session named late.
      if (p.source && p.source !== 'compact') onTurnEdge('session', p.tab_id);
    });
    // `turn`: the ledger's number for the turn the prompt began (absent when the ledger had no
    // tab for it), so the tracker can't drift from the seed below.
    onHook<{ tab_id?: string | null; turn?: number | null }>('agent-hook-user-prompt', (p) => { if (p.tab_id) onTurnEdge('prompt', p.tab_id, p.turn); });
    onHook<{ tab_id?: string | null }>('agent-hook-stop', (p) => { if (p.tab_id) onTurnEdge('stop', p.tab_id); });
    void Promise.all(hooksUp).then(() => { if (attached) void seedTurns(); });
    document.addEventListener('visibilitychange', onVisibility);
    window.addEventListener('blur', onWindowBlur);
    window.addEventListener('focus', onWindowFocus);
    unlisteners = [
      offOsc,
      offPrompt,
      () => { for (const u of hookOffs.splice(0)) u(); },
      () => document.removeEventListener('visibilitychange', onVisibility),
      () => window.removeEventListener('blur', onWindowBlur),
      () => window.removeEventListener('focus', onWindowFocus),
    ];
    if (localHost === null) commands.ftLocalHostname().then((h) => { localHost = h; }).catch(() => {});
    if (place && !place.missing) await reroot(place);
    void resolveNow(false);
  }

  /** A remote root that couldn't open (needs key-based ssh, no answer, a backoff): the host's
   *  helper tries again now, and the root opens again, its last listing dimmed meanwhile. */
  async function retryRemote() {
    const p = place;
    if (!p || p.kind !== 'remote' || p.missing) return;
    await commands.ftRetryHost(p.place).catch((e) => logError(`[files] retry ${p.place}: ${e}`));
    if (place !== p) return;
    await reroot(p);
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
    // With the hook listeners gone, a prompt or a new session (the ledger renumbers from 1)
    // would pass unseen, and noteMark only ever raises a turn: kept, the tracker could stay
    // ahead of the ledger for good and no mark would count as this turn's. The next attach
    // rebuilds it from the ledger's own count (seedTurns) and the primed marks.
    turns.clear();
    tabLastAt.clear();
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
    setLens,
    toggleChanged,
    // Agents (§9.3–§9.6)
    get agentChips() { return agentChips; },
    get lensCounts() { return lensCounts; },
    get agentFilter() { return agentFilter; },
    get followAgent() { return followAgent; },
    /** Bumped when a lit row goes dark: the edge chips re-read `litRels` on it. */
    get litVersion() { return litVersion; },
    rowAgent,
    agentWords,
    colourOf,
    tabName,
    /** Choose a chip (scoping Turn and Session to it), or clear the choice. */
    chooseAgent(tabId: string | null) {
      agentFilter = agentFilter === tabId ? null : tabId;
      // Follow agent belongs to the chosen chip: choosing another stops following.
      if (followAgent && followAgent !== agentFilter) followAgent = null;
      const mode = viewMode();
      if (mode === 'turn' || mode === 'session') rebuildLens();
    },
    /** The ⋯ menu's eye: follow the chosen chip's writes, or stop. */
    setFollowAgent(on: boolean) {
      followAgent = on ? agentFilter : null;
    },
    /** Rows lit now (running, sweeping, a read's light), for the edge chips. */
    litRels(): { rel: string; editing: boolean }[] {
      void litVersion;
      void agentVersion;
      if (!preferencesStore.fileTreeAgentActivity) return [];
      const now = Date.now();
      const out: { rel: string; editing: boolean }[] = [];
      for (const [rel, slots] of agentMarks.byRel) {
        const v = rowView([...slots.values()], now, turns);
        if (v.litUntil !== null && v.litUntil > now) out.push({ rel, editing: v.editing });
      }
      return out;
    },
    agentReveal,
    selectionForAgent,
    get pointer() { return pointer; },
    /** The pointer chip's path, root-relative when it is under the root. */
    get pointerLabel(): string {
      const p = pointer;
      if (!p) return '';
      const a = ack;
      if (a && (p.abs === a.root_native || p.abs.startsWith(a.root_native.replace(/[\\/]$/, '') + a.sep))) {
        return p.abs.slice(a.root_native.replace(/[\\/]$/, '').length + a.sep.length).split(a.sep).join('/') || lastSegment(p.abs);
      }
      return tildePath(p.abs, home);
    },
    /** An agent pointed at something while the column was closed: the titlebar button's dot. */
    get pointerWaiting() { return !!pointer && !workspacesStore.fileTreeOpen; },
    showPointer,
    dismissPointer() { pointer = null; },
    compareBufferWithDisk,
    remapAgentTab,
    compareWithHead,
    baselineWriters,
    compareWithBaseline,
    turnDeltaWords,
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
      const target = t?.tab_type === 'editor' ? activeFileOf(t) : null;
      if (target) {
        for (let i = 0; i < 40 && subId === null; i++) await new Promise((r) => setTimeout(r, 50));
        // A remote editor's file is found only in a root on its host, and vice versa.
        const here = lookupIn(target, rootHost(), sameSshHost);
        const [rel] = subId !== null && here ? await commands.ftLocate(subId, [target.path]).catch(() => [null]) : [null];
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
    /** Whose words the trash and its results use: the host's on a remote root. */
    get platform(): Platform { return platformOf(place); },
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
    /** "Set as Root" on a folder row: its native path is a local place; on a remote root the
     *  place is `host:/abs`, as Rust spells it (`watch::place`). */
    async pinRel(rel: string) {
      const abs = absOf(rel);
      const host = rootHost();
      if (abs) await pin(host ? `${host}:${abs}` : abs);
    },

    // Remote roots (§15)
    /** The root is on an ssh host. */
    get remote() { return place?.kind === 'remote'; },
    /** The last listing is painted, not yet confirmed by the host (dim it). Also while the
     *  host's helper is down or can't log in: what is shown may be out of date. */
    get stale() {
      return staleRoot || (place?.kind === 'remote' && subId !== null && health?.watch === 'none');
    },
    /** The followed tab's shell reports another machine than the one shown (an onward ssh):
     *  the header warns, naming both. */
    get wrongHost(): { showing: string; shell: string } | null {
      const p = place;
      const a = ack;
      if (!p || p.kind !== 'remote' || !a?.remote_hostname) return null;
      if (!onAnotherHost(sourceOscHost, localHost, a.remote_hostname)) return null;
      return { showing: p.host_label ?? a.remote_hostname, shell: sourceOscHost ?? '' };
    },
    retryRemote,
    /** The followed agent can take these files in its composer (files on this computer). */
    get canAskAbout() { return !rootHost(); },

    /** The roots of every terminal tab in the workspace, deduped, for the root switcher.
     *  `onUpdate` gets the list so far, in tab order, as each tab answers: one slow host must
     *  not hold back every other tab's folder. Resolved `passive`ly, so opening the switcher
     *  never starts a helper on a host or waits on one that is down. */
    async tabRoots(onUpdate: (roots: { place: TreePlace; tabs: { id: string; name: string }[] }[]) => void): Promise<void> {
      const ws = workspacesStore.activeWorkspace;
      if (!ws) return;
      const tabs = ws.panes.flatMap((p) => p.tabs).filter((t) => (t.tab_type ?? 'terminal') === 'terminal' && !t.service_id);
      const answers: (TreePlace | null)[] = tabs.map(() => null);
      const publish = () => {
        const out = new Map<string, { place: TreePlace; tabs: { id: string; name: string }[] }>();
        tabs.forEach((t, i) => {
          const p = answers[i];
          if (!p) return;
          const hit = out.get(p.place) ?? { place: p, tabs: [] };
          hit.tabs.push({ id: t.id, name: t.name });
          out.set(p.place, hit);
        });
        onUpdate([...out.values()]);
      };
      await Promise.all(tabs.map(async (t, i) => {
        const osc = terminalsStore.getOsc(t.id);
        try {
          const p = await commands.ftResolvePlace(t.id, { osc_cwd: osc?.cwd ?? null, osc_host: osc?.cwdHost ?? null, prompt_cwd: osc?.promptCwd ?? null, fresh: false, passive: true });
          // A pinned workspace answers every tab with its pin, which says nothing new.
          if (p.missing || p.source === 'pin') return;
          answers[i] = p;
          publish();
        } catch {
          /* a tab with no known folder isn't a root */
        }
      }));
    },

    displayRoot(p: TreePlace | null): string {
      if (!p) return '';
      // This computer's home means nothing on a host: a remote root is named with its host.
      return p.kind === 'remote' ? `${p.host_label ?? p.host_key}:${p.root}` : tildePath(p.root, home);
    },
  };
}

export const fileTreeStore = createFileTreeStore();
