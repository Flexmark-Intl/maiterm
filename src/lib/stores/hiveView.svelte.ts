/** A Hive view's state, per workspace (docs/hive.md §4.1): its open worker columns, which column
 *  has focus, the coordinator's strip, and (build step 5d) which raw terminals show. Kept per
 *  viewer in `localStorage['hive.view.<workspace id>']`, keyed by task id, never in the state file.
 *
 *  The focused column's tab is what the Files column follows in a Hive (`focusedTabId`). A worker's
 *  tab is its card's `tab_id`, which HiveView publishes from its board (`setTaskTabs`). */

import { workspacesStore } from './workspaces.svelte';
import {
  closeColumn,
  focusColumn,
  mostRecent,
  openColumn,
  parseView,
  serializeView,
  togglePin,
  validFocus,
  type HiveFocus,
  type HiveViewName,
  type HiveViewState,
} from '$lib/hive/columns';
import { nextView } from '$lib/hive/memory';

const key = (wsId: string) => `hive.view.${wsId}`;

function createHiveViewStore() {
  let views = $state<Map<string, HiveViewState>>(new Map());
  /** What storage held, read once per workspace. Not reactive: filled from inside derivations,
   *  which may not write state; the first change moves a workspace into `views`. */
  const stored = new Map<string, HiveViewState>();
  /** Per workspace, task id → its worker's tab. */
  let taskTabs = $state<Map<string, Map<string, string>>>(new Map());
  /** Per workspace, the tabs whose raw terminal is on screen now: shown, in a column the layout
   *  shows. HiveView publishes it; it is what makes Rust render that terminal (`+page`'s
   *  `visible`). Not persisted. */
  let terminalsOnScreen = $state<Map<string, ReadonlySet<string>>>(new Map());

  function read(wsId: string): HiveViewState {
    const v = views.get(wsId);
    if (v) return v;
    let s = stored.get(wsId);
    if (!s) {
      let raw: string | null = null;
      try {
        raw = localStorage.getItem(key(wsId));
      } catch {
        /* per-viewer nicety only */
      }
      s = parseView(raw);
      stored.set(wsId, s);
    }
    return s;
  }

  function write(wsId: string, next: HiveViewState) {
    const v = { ...next, focused: validFocus(next) };
    views = new Map(views).set(wsId, v);
    try {
      localStorage.setItem(key(wsId), serializeView(v));
    } catch {
      /* per-viewer nicety only */
    }
  }

  function coordinatorTabId(wsId: string): string | null {
    const ws = workspacesStore.workspaces.find((w) => w.id === wsId);
    return ws?.panes.flatMap((p) => p.tabs).find((t) => t.hive_role === 'coordinator')?.id ?? null;
  }

  return {
    view(wsId: string): HiveViewState {
      return read(wsId);
    },

    /** Open or focus a worker's column, and focus it. Refused when both columns are pinned. */
    open(wsId: string, taskId: string, now = Date.now()): { refused?: 'both-pinned' } {
      const v = read(wsId);
      const r = openColumn(v.open, taskId, now);
      if (r.refused) return { refused: r.refused };
      const open = new Set(r.open.map((c) => c.taskId));
      write(wsId, { ...v, open: r.open, focused: taskId, terminalShown: v.terminalShown.filter((k) => k === 'coordinator' || open.has(k)) });
      return {};
    },

    /** A column took the keyboard or a click. Writes only when something moves: this runs on every
     *  pointerdown in a column. */
    focus(wsId: string, to: HiveFocus, now = Date.now()) {
      const v = read(wsId);
      const col = v.open.find((c) => c.taskId === to);
      if (to !== 'coordinator' && to !== 'board' && !col) return;
      // A column's stamp decides what gives way on a narrow window (`fitColumns`' keeper).
      const at = to === 'coordinator' ? v.coordFocusedAt : col?.focusedAt;
      const newest = at === undefined || (v.coordFocusedAt <= at && v.open.every((c) => c.focusedAt <= at));
      if (v.focused === to && newest) return;
      write(wsId, {
        ...v,
        focused: to,
        open: col ? focusColumn(v.open, to, now) : v.open,
        coordFocusedAt: to === 'coordinator' ? now : v.coordFocusedAt,
      });
    },

    /** Close a worker's column (never its agent). Focus goes to the other column, else the board. */
    close(wsId: string, taskId: string) {
      const v = read(wsId);
      if (!v.open.some((c) => c.taskId === taskId)) return;
      const open = closeColumn(v.open, taskId);
      const focused = v.focused === taskId ? (mostRecent(open) ?? 'board') : v.focused;
      write(wsId, { ...v, open, focused, terminalShown: v.terminalShown.filter((k) => k !== taskId) });
    },

    togglePin(wsId: string, taskId: string) {
      const v = read(wsId);
      if (v.open.some((c) => c.taskId === taskId)) write(wsId, { ...v, open: togglePin(v.open, taskId) });
    },

    /** Collapsing the focused coordinator hands focus to the worker focused last, else the board:
     *  Files and the keyboard never stay on a strip. */
    setCoordCollapsed(wsId: string, collapsed: boolean) {
      const v = read(wsId);
      if (v.coordCollapsed === collapsed) return;
      const focused = collapsed && v.focused === 'coordinator' ? (mostRecent(v.open) ?? 'board') : v.focused;
      write(wsId, { ...v, coordCollapsed: collapsed, focused });
    },

    /** Show terminal (§4.5), for the coordinator's column or a worker's (by task id). */
    setTerminalShown(wsId: string, which: 'coordinator' | string, shown: boolean) {
      const v = read(wsId);
      if (v.terminalShown.includes(which) === shown) return;
      write(wsId, { ...v, terminalShown: shown ? [...v.terminalShown, which] : v.terminalShown.filter((k) => k !== which) });
    },

    /** Board or Memory beside the coordinator (§4.1). */
    setView(wsId: string, next: HiveViewName) {
      const v = read(wsId);
      if (v.view !== next) write(wsId, { ...v, view: next });
    },

    /** Cmd+Shift+Y (§4.6). */
    cycleView(wsId: string) {
      const v = read(wsId);
      write(wsId, { ...v, view: nextView(v.view) });
    },

    /** Columns whose worker the board no longer has (a rolled-back spawn) close. */
    prune(wsId: string, taskIds: readonly string[]) {
      const v = read(wsId);
      const known = new Set(taskIds);
      if (v.open.every((c) => known.has(c.taskId))) return;
      const open = v.open.filter((c) => known.has(c.taskId));
      write(wsId, { ...v, open, terminalShown: v.terminalShown.filter((k) => k === 'coordinator' || known.has(k)) });
    },

    /** HiveView's board, as task id → tab: what a worker column's focus resolves to. */
    setTaskTabs(wsId: string, entries: readonly (readonly [string, string])[]) {
      const had = taskTabs.get(wsId);
      if (had && had.size === entries.length && entries.every(([t, tab]) => had.get(t) === tab)) return;
      taskTabs = new Map(taskTabs).set(wsId, new Map(entries));
    },

    /** The tab of the focused column: the coordinator's, a worker's, or null for the board (Files
     *  keeps its last root there) and for a waiting worker, which has no tab yet. */
    focusedTabId(wsId: string): string | null {
      const f = read(wsId).focused;
      if (f === 'board') return null;
      if (f === 'coordinator') return coordinatorTabId(wsId);
      return taskTabs.get(wsId)?.get(f) ?? null;
    },

    /** HiveView's on-screen raw terminals; an empty list when it unmounts. */
    setTerminalsOnScreen(wsId: string, tabIds: readonly string[]) {
      const had = terminalsOnScreen.get(wsId);
      if ((had?.size ?? 0) === tabIds.length && tabIds.every((t) => had?.has(t))) return;
      terminalsOnScreen = new Map(terminalsOnScreen).set(wsId, new Set(tabIds));
    },

    /** Whether a Hive tab's TerminalPane is on screen, so Rust should render it. */
    terminalVisible(wsId: string, tabId: string): boolean {
      return terminalsOnScreen.get(wsId)?.has(tabId) ?? false;
    },

    coordinatorTabId,
  };
}

export const hiveView = createHiveViewStore();
