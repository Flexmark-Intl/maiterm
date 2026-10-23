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
 */
import { Channel } from '@tauri-apps/api/core';
import { listen, type UnlistenFn } from '@tauri-apps/api/event';
import { homeDir } from '@tauri-apps/api/path';
import { untrack } from 'svelte';
import { SvelteMap, SvelteSet } from 'svelte/reactivity';
import { writeText as clipboardWriteText } from '@tauri-apps/plugin-clipboard-manager';
import { error as logError, info as logInfo } from '@tauri-apps/plugin-log';
import * as commands from '$lib/tauri/commands';
import type { DirReq, OpenAck, TreeEnvelope, TreePlace } from '$lib/tauri/types';
import { TREE_FLAG_IGNORED } from '$lib/tauri/types';
import { TreeModel, ancestorsOf, isExpandable, isWithin, joinRel, parentRel, type Row } from '$lib/filetree/model';
import { relToAbs, lastSegment, tildePath } from '$lib/filetree/paths';
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

interface ViewState {
  model: TreeModel;
  scrollTop: number;
  selection: string[];
  focus: string | null;
  anchor: string | null;
}

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

  homeDir().then((h) => { home = h; }).catch(() => {});

  // ── Small helpers ──────────────────────────────────────────────────────────────────────

  /** Called after every change to the model (expand, collapse, reveal, a message). Bumping
   *  `expandedVersion` each time re-reads only the visible rows' open state, which is cheap,
   *  and no path that opens or closes a folder can forget to. */
  function syncRows() {
    if (model && rows !== model.rows) rows = model.rows;
    // Untracked: `++` reads the counter, and a caller inside an effect must not subscribe.
    untrack(() => { expandedVersion++; });
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
    const token = ++resolveSeq;
    const ws = workspacesStore.activeWorkspace;
    if (!ws) {
      status = 'none';
      statusText = null;
      return;
    }
    const sameWorkspace = placeWorkspaceId === ws.id;
    let tabId = followableTab();
    if (!tabId) {
      // An editor, diff or board tab: reveal its file, never re-root (§4.2).
      if (place && sameWorkspace && !force) {
        void revealActiveEditor();
        return;
      }
      tabId = fallbackTab();
    }
    if (!tabId) {
      if (!place || !sameWorkspace) {
        status = 'none';
        statusText = 'Open a terminal tab, or pick a folder.';
      }
      return;
    }
    // Follow off: the root stays where it is until the human moves it, unless there is none
    // yet or the workspace changed (a pin is per workspace).
    if (!force && !preferencesStore.fileTreeFollow && place && sameWorkspace) return;

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
      if (token !== resolveSeq) return;
      if (!place || !sameWorkspace) {
        closeSub();
        place = null;
        status = 'error';
        statusText = String(e);
      }
      return;
    }
    if (token !== resolveSeq) return;
    sourceTabId = tabId;
    placeWorkspaceId = ws.id;
    await adoptPlace(p);
    if (!followableTab()) void revealActiveEditor();
  }

  async function adoptPlace(p: TreePlace) {
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
      const located = await commands.ftLocate(subId, [cwdAbs]).catch(() => [null]);
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
    if (rel) revealRel(rel, false);
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
    let opened: OpenAck;
    try {
      opened = await commands.ftOpen(p.place, model.restoreReqs(), channel);
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
    acked = { count: 0, at: performance.now() };
    status = 'ready';
    ready = true;
    for (const env of early) handle(env);
    if (typeof document !== 'undefined' && document.hidden) commands.ftPause(opened.sub_id, true).catch(() => {});
    if (youAreHere && !kept) revealRel(youAreHere, false);
    void revealActiveEditor();
  }

  function handle(env: TreeEnvelope) {
    const m = model;
    const sub = subId;
    if (!m || sub === null) return;
    switch (env.t) {
      case 'dir': {
        const r = m.applyDir(env);
        if (r === 'resync') commands.ftRefresh(sub, env.rel).catch(() => {});
        else if (env.last) onListed(env.rel);
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
        else if (selection.size || focusKey) pruneSelection();
        break;
      }
      case 'dir_error': {
        const r = m.applyError(env);
        sendClose(r.close);
        break;
      }
      case 'health':
        health = env.note ? { watch: env.watch, git: env.git, note: env.note } : null;
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
    const r = model.expand(rel);
    sendOpen(r.open);
    syncRows();
    scheduleSaveExpanded();
  }

  function collapse(rel: string) {
    if (!model) return;
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
    collapse(rel);
    for (const r of [...model.expanded]) if (r !== '' && r !== rel && isWithin(r, rel)) model.expanded.delete(r);
    scheduleSaveExpanded();
  }

  // ── Revealing ─────────────────────────────────────────────────────────────────────────

  /** Open the folders above `rel` and scroll it into view when it has a row; select it only
   *  when the human asked (`select`). */
  function revealRel(rel: string, select: boolean) {
    if (!model) return;
    const r = model.reveal(rel);
    sendOpen(r.open);
    syncRows();
    if (r.open.length) scheduleSaveExpanded();
    pendingReveal = { rel, select };
    checkPendingReveal();
  }

  /** The active editor's file: highlighted and revealed when it is under the root (with
   *  auto-reveal on), named in the header when it isn't. Never focused, never selected. */
  async function revealActiveEditor(force = false) {
    const t = workspacesStore.activeTab;
    const f = t?.tab_type === 'editor' ? t.editor_file : null;
    if (!f || f.is_remote || subId === null) {
      activeFile = null;
      outsideFile = null;
      return;
    }
    const sub = subId;
    const [rel] = await commands.ftLocate(sub, [f.file_path]).catch(() => [null]);
    if (sub !== subId) return;
    if (rel == null) {
      activeFile = null;
      outsideFile = f.file_path;
      return;
    }
    outsideFile = null;
    activeFile = rel;
    if ((force || preferencesStore.fileTreeAutoReveal) && rel) revealRel(rel, false);
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
    if (rel) revealRel(rel, true);
    focusPending = true;
  }

  /** "Reveal in Files" on a terminal tab: that tab comes to the front (so the tree follows
   *  it), the column opens because the human asked, and its folder is selected. */
  async function revealTabFolder(workspaceId: string, paneId: string, tabId: string) {
    await workspacesStore.setActiveTab(workspaceId, paneId, tabId);
    await workspacesStore.setFileTreeOpen(true);
    for (let i = 0; i < 40 && !attached; i++) await new Promise((r) => setTimeout(r, 50));
    await resolveNow(false);
    if (youAreHere) revealRel(youAreHere, true);
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
  function extendTo(key: string) {
    if (!model) return;
    const from = anchorKey ? model.indexOf(anchorKey) : -1;
    const to = model.indexOf(key);
    if (to < 0) return;
    const [a, b] = from < 0 ? [to, to] : from < to ? [from, to] : [to, from];
    selection.clear();
    for (let i = a; i <= b; i++) {
      const r = model.rows[i];
      if (r.kind !== 'm') selection.add(r.key);
    }
    focusKey = key;
  }

  function selectAll() {
    if (!model) return;
    selection.clear();
    for (const r of model.rows) if (r.kind !== 'm') selection.add(r.key);
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
    /** A folder is open, read reactively (the model's own set is not). */
    isExpanded(key: string): boolean {
      void expandedVersion;
      return !!model?.expanded.has(key);
    },
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
        const t = workspacesStore.activeTab;
        if (t?.tab_type === 'terminal') terminalsStore.focusTerminal(t.id);
        return;
      }
      const t = workspacesStore.activeTab;
      if (t?.tab_type === 'editor' && t.editor_file && !t.editor_file.is_remote) {
        for (let i = 0; i < 40 && subId === null; i++) await new Promise((r) => setTimeout(r, 50));
        const [rel] = subId !== null ? await commands.ftLocate(subId, [t.editor_file.file_path]).catch(() => [null]) : [null];
        if (rel) {
          revealRel(rel, true);
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
    toggle(rel: string) {
      if (!model) return;
      if (model.expanded.has(rel)) collapse(rel);
      else expand(rel);
    },
    expandRecursive,
    collapseRecursive,
    collapseAll() {
      if (!model) return;
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
        syncRows();
        pruneSelection();
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
    revealActiveEditor,
    revealYouAreHere() { if (youAreHere) revealRel(youAreHere, false); },
    absOf,
    copyPaths,
    revealInOs,
    openWithDefaultApp,
    openTerminalHere,
    addToTab,

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
