/** A Hive's worker columns (docs/hive.md §4.1): which are open, which show, and what gives way on
 *  a narrow window. Pure: `stores/hiveView.svelte.ts` holds the state, HiveView lays it out.
 *
 *  Every column is a transcript (ChatColumn), never a terminal, so nothing here can change a
 *  PTY's width however the columns open, close or squeeze (principle 3). */

export const W = {
  COORD_MIN: 380,
  COORD_MAX: 560,
  COORD_DEFAULT: 440,
  STRIP: 44,
  BOARD_MIN: 260,
  RAIL_MIN: 200,
  RAIL_MAX: 240,
  RAIL_DEFAULT: 220,
  WORKER_MIN: 420,
  RESIZER: 4,
} as const;

/** §4.1: at most two worker columns side by side. */
export const MAX_WORKER_COLUMNS = 2;

export interface OpenColumn {
  taskId: string;
  pinned: boolean;
  /** Epoch ms: the least recently focused unpinned column is the one that gives way. */
  focusedAt: number;
}

/** 'coordinator' | 'board' | a worker's task id. */
export type HiveFocus = string;

export const clampCoord = (n: number) => Math.min(W.COORD_MAX, Math.max(W.COORD_MIN, Math.round(n)));
export const clampRail = (n: number) => Math.min(W.RAIL_MAX, Math.max(W.RAIL_MIN, Math.round(n)));

function leastRecent(cols: readonly OpenColumn[]): OpenColumn | undefined {
  return cols.reduce<OpenColumn | undefined>((a, c) => (!a || c.focusedAt < a.focusedAt ? c : a), undefined);
}

/** Open (or focus) a worker's column. A third replaces the least recently focused unpinned one,
 *  at the right; with both pinned it's refused, never a pin overridden. */
export function openColumn(open: readonly OpenColumn[], taskId: string, now: number): { open: OpenColumn[]; refused?: 'both-pinned' } {
  if (open.some((c) => c.taskId === taskId)) return { open: focusColumn(open, taskId, now) };
  const fresh = { taskId, pinned: false, focusedAt: now };
  if (open.length < MAX_WORKER_COLUMNS) return { open: [...open, fresh] };
  const victim = leastRecent(open.filter((c) => !c.pinned));
  if (!victim) return { open: [...open], refused: 'both-pinned' };
  return { open: [...open.filter((c) => c !== victim), fresh] };
}

export function focusColumn(open: readonly OpenColumn[], taskId: string, now: number): OpenColumn[] {
  return open.map((c) => (c.taskId === taskId ? { ...c, focusedAt: now } : c));
}

export function closeColumn(open: readonly OpenColumn[], taskId: string): OpenColumn[] {
  return open.filter((c) => c.taskId !== taskId);
}

export function togglePin(open: readonly OpenColumn[], taskId: string): OpenColumn[] {
  return open.map((c) => (c.taskId === taskId ? { ...c, pinned: !c.pinned } : c));
}

/** The most recently focused open column: where focus goes when the focused one closes. */
export function mostRecent(open: readonly OpenColumn[]): string | null {
  return open.reduce<OpenColumn | null>((a, c) => (!a || c.focusedAt > a.focusedAt ? c : a), null)?.taskId ?? null;
}

export interface FitInput {
  /** The width of the Hive's columns area (`.cols`): the window less the sidebar and an open
   *  Files column, which count in the budget. */
  main: number;
  coordW: number;
  railW: number;
  /** The human collapsed the coordinator to its strip. */
  coordCollapsed: boolean;
  open: readonly OpenColumn[];
  focused: HiveFocus;
  /** When the coordinator last took focus: while the board has focus, the column focused last
   *  is still the one the human is in. */
  coordFocusedAt: number;
}

export interface Fit {
  coord: 'full' | 'strip';
  board: 'board' | 'rail';
  /** Open columns on screen, left to right. */
  shown: string[];
  /** Open but given way to a narrow window: kept, and back when it widens. */
  suppressed: string[];
}

/** The column the human is in: the focused one, or while the board has focus the one focused
 *  last. It never gives way, so focus, the keyboard and the Files column never sit on something
 *  hidden, and a column just opened is the one that shows. */
function keeper(p: Pick<FitInput, 'open' | 'focused' | 'coordFocusedAt'>): HiveFocus {
  if (p.focused === 'coordinator' || p.open.some((c) => c.taskId === p.focused)) return p.focused;
  const last = p.open.reduce<OpenColumn | null>((a, c) => (!a || c.focusedAt > a.focusedAt ? c : a), null);
  return last && last.focusedAt >= p.coordFocusedAt ? last.taskId : 'coordinator';
}

/** What fits (§4.1). No worker open: coordinator | board. Any open: coordinator | rail | up to
 *  two workers. When that's wider than `main`: first one worker column gives way (of those the
 *  human isn't in, the least recently focused unpinned one, else the least recently focused),
 *  then the coordinator goes to its strip, then the worker shrinks below its minimum. Never a
 *  scroll sideways. The column the human is in never gives way (`keeper`): with the coordinator
 *  focused, the last worker steps aside instead of the coordinator going to its strip. */
export function fitColumns(p: FitInput): Fit {
  const collapsed = p.coordCollapsed ? 'strip' : 'full';
  if (!p.open.length) return { coord: collapsed, board: 'board', shown: [], suppressed: [] };
  const coordW = clampCoord(p.coordW);
  const railW = clampRail(p.railW);
  const need = (n: number, coord: 'full' | 'strip') => (coord === 'full' ? coordW : W.STRIP) + railW + n * W.WORKER_MIN + (n + 1) * W.RESIZER;
  const keep = keeper(p);
  let shown = [...p.open];
  const suppressed: string[] = [];
  while (shown.length > 1 && need(shown.length, collapsed) > p.main) {
    const others = shown.filter((c) => c.taskId !== keep);
    const victim = leastRecent(others.filter((c) => !c.pinned)) ?? leastRecent(others)!;
    shown = shown.filter((c) => c !== victim);
    suppressed.push(victim.taskId);
  }
  if (need(shown.length, collapsed) > p.main && collapsed === 'full' && keep === 'coordinator') {
    return { coord: 'full', board: 'board', shown: [], suppressed: [...suppressed, ...shown.map((c) => c.taskId)] };
  }
  const coord = collapsed === 'full' && need(shown.length, 'full') > p.main ? 'strip' : collapsed;
  return { coord, board: 'rail', shown: shown.map((c) => c.taskId), suppressed };
}

/** The tabs whose column `fit` puts on screen: the coordinator unless it is a strip, and the
 *  shown workers' (one without a tab yet has nothing to count). What a Hive tab's notices count as
 *  visible (§4.7, plan D27); a column given way is not open to the human. */
export function tabsOnScreen(fit: Pick<Fit, 'coord' | 'shown'>, coordTab: string | null | undefined, tabOf: (taskId: string) => string | null | undefined): string[] {
  const ids: string[] = [];
  if (coordTab && fit.coord === 'full') ids.push(coordTab);
  for (const taskId of fit.shown) {
    const tab = tabOf(taskId);
    if (tab) ids.push(tab);
  }
  return ids;
}

/** The focus stops of Cmd+Shift+[ / ], left to right. The board (or rail) is one: Files keeps its
 *  last root while it holds focus. Every open column is a stop, given way or not: focused, one
 *  becomes `fitColumns`' keeper and shows again, where stops taken from `Fit.shown` would lose
 *  the workers a focused coordinator pushed out. A coordinator the human collapsed is skipped:
 *  the keyboard never sits on a strip. */
export function cycleOrder(open: readonly Pick<OpenColumn, 'taskId'>[], coordCollapsed: boolean): HiveFocus[] {
  return [...(coordCollapsed ? [] : ['coordinator']), 'board', ...open.map((c) => c.taskId)];
}

/** The next stop, wrapping. A focus not in the order (a column that just closed) starts from an end. */
export function nextFocus(order: readonly HiveFocus[], cur: HiveFocus, dir: -1 | 1): HiveFocus {
  if (!order.length) return cur;
  const i = order.indexOf(cur);
  if (i < 0) return dir === 1 ? order[0] : order[order.length - 1];
  return order[(i + dir + order.length) % order.length];
}

// ── The view's state, per workspace, kept per viewer in localStorage (never the state file) ──

export interface HiveViewState {
  /** Keyed by task id: a restart gives a worker a new tab, never a new task. */
  open: OpenColumn[];
  focused: HiveFocus;
  /** Epoch ms, as `OpenColumn.focusedAt`: `fitColumns`' keeper while the board has focus. */
  coordFocusedAt: number;
  coordCollapsed: boolean;
  /** 'coordinator' or task ids whose raw terminal is shown (§4.5, build step 5d). */
  terminalShown: string[];
  /** What the main area shows beside the coordinator (§4.1); the gallery arrives in step 8. */
  view: HiveViewName;
}

export type HiveViewName = 'board' | 'memory';
export const HIVE_VIEWS: readonly HiveViewName[] = ['board', 'memory'];

export const defaultView = (): HiveViewState => ({ open: [], focused: 'coordinator', coordFocusedAt: 0, coordCollapsed: false, terminalShown: [], view: 'board' });

/** A focus that names nothing on screen falls back to the coordinator. */
export function validFocus(v: Pick<HiveViewState, 'open' | 'focused'>): HiveFocus {
  const f = v.focused;
  return f === 'coordinator' || f === 'board' || v.open.some((c) => c.taskId === f) ? f : 'coordinator';
}

/** What localStorage held, read defensively: an older or hand-edited value never breaks the view. */
export function parseView(raw: string | null): HiveViewState {
  const v = defaultView();
  if (!raw) return v;
  let j: unknown;
  try {
    j = JSON.parse(raw);
  } catch {
    return v;
  }
  if (!j || typeof j !== 'object') return v;
  const o = j as Record<string, unknown>;
  const seen = new Set<string>();
  if (Array.isArray(o.open)) {
    for (const c of o.open) {
      const id = c && typeof c === 'object' ? (c as Record<string, unknown>).task_id : null;
      if (typeof id !== 'string' || !id || seen.has(id) || v.open.length >= MAX_WORKER_COLUMNS) continue;
      seen.add(id);
      const at = Number((c as Record<string, unknown>).focused_at);
      v.open.push({ taskId: id, pinned: (c as Record<string, unknown>).pinned === true, focusedAt: Number.isFinite(at) ? at : 0 });
    }
  }
  if (typeof o.focused === 'string') v.focused = o.focused;
  v.focused = validFocus(v);
  const coordAt = Number(o.coord_focused_at);
  if (Number.isFinite(coordAt)) v.coordFocusedAt = coordAt;
  v.coordCollapsed = o.coord_collapsed === true;
  if (Array.isArray(o.terminal_shown)) v.terminalShown = [...new Set(o.terminal_shown.filter((s): s is string => typeof s === 'string' && !!s))];
  if (HIVE_VIEWS.includes(o.view as HiveViewName)) v.view = o.view as HiveViewName;
  return v;
}

export function serializeView(v: HiveViewState): string {
  return JSON.stringify({
    open: v.open.map((c) => ({ task_id: c.taskId, pinned: c.pinned, focused_at: c.focusedAt })),
    focused: v.focused,
    coord_focused_at: v.coordFocusedAt,
    coord_collapsed: v.coordCollapsed,
    terminal_shown: v.terminalShown,
    view: v.view,
  });
}
