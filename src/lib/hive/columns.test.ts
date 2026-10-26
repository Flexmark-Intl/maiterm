import { describe, expect, it } from 'vitest';
import {
  closeColumn,
  closeUnpinned,
  cycleOrder,
  defaultView,
  fitColumns,
  mostRecent,
  nextFocus,
  openColumn,
  openColumnAs,
  parseView,
  serializeView,
  switchColumn,
  tabsOnScreen,
  togglePin,
  W,
  type OpenColumn,
} from './columns';

const col = (taskId: string, focusedAt: number, pinned = false): OpenColumn => ({ taskId, focusedAt, pinned });
const ids = (open: OpenColumn[]) => open.map((c) => c.taskId);

describe('openColumn', () => {
  it('opens up to two, then replaces the least recently focused unpinned one', () => {
    let open = openColumn([], 'a', 1).open;
    open = openColumn(open, 'b', 2).open;
    expect(ids(open)).toEqual(['a', 'b']);
    // b was focused last, so a gives way; the new one goes on the right.
    expect(ids(openColumn(open, 'c', 3).open)).toEqual(['b', 'c']);
    // Focusing a again makes b the older one.
    open = openColumn(open, 'a', 4).open;
    expect(ids(open)).toEqual(['a', 'b']);
    expect(ids(openColumn(open, 'c', 5).open)).toEqual(['a', 'c']);
  });

  it('never replaces a pinned column, and refuses when both are', () => {
    const open = [col('a', 1, true), col('b', 2)];
    expect(ids(openColumn(open, 'c', 3).open)).toEqual(['a', 'c']);
    const both = [col('a', 1, true), col('b', 2, true)];
    const r = openColumn(both, 'c', 3);
    expect(r.refused).toBe('both-pinned');
    expect(r.open).toEqual(both);
  });

  it('re-opening an open column only focuses it', () => {
    const r = openColumn([col('a', 1), col('b', 2)], 'a', 9);
    expect(r.refused).toBeUndefined();
    expect(r.open).toEqual([col('a', 9), col('b', 2)]);
  });
});

describe('switchColumn (a plain click)', () => {
  it('shows the worker in place of the open one, not beside it', () => {
    expect(switchColumn([], 'a', 1).open).toEqual([col('a', 1)]);
    expect(switchColumn([col('a', 1)], 'b', 2).open).toEqual([col('b', 2)]);
    // Two open (a Shift+click earlier): both unpinned ones give way to the one clicked.
    expect(switchColumn([col('a', 1), col('b', 2)], 'c', 3).open).toEqual([col('c', 3)]);
  });

  it('keeps pinned columns where they are, taking the unpinned one\'s place', () => {
    expect(switchColumn([col('a', 1, true), col('b', 2)], 'c', 3).open).toEqual([col('a', 1, true), col('c', 3)]);
    expect(switchColumn([col('b', 2), col('a', 1, true)], 'c', 3).open).toEqual([col('c', 3), col('a', 1, true)]);
    // One pinned alone: the new one goes beside it.
    expect(switchColumn([col('a', 1, true)], 'c', 3).open).toEqual([col('a', 1, true), col('c', 3)]);
  });

  it('refuses when every slot is pinned', () => {
    const both = [col('a', 1, true), col('b', 2, true)];
    const r = switchColumn(both, 'c', 3);
    expect(r.refused).toBe('both-pinned');
    expect(r.open).toEqual(both);
  });

  it('clicking an open column keeps it (and its pin) and closes the other unpinned one', () => {
    expect(switchColumn([col('a', 1), col('b', 2)], 'a', 9).open).toEqual([col('a', 9)]);
    expect(switchColumn([col('a', 1, true), col('b', 2)], 'a', 9).open).toEqual([col('a', 9, true)]);
    expect(switchColumn([col('a', 1, true), col('b', 2)], 'b', 9).open).toEqual([col('a', 1, true), col('b', 9)]);
  });

  it('openColumnAs: add is a Shift+click, beside the open one', () => {
    expect(ids(openColumnAs([col('a', 1)], 'b', 2, 'add').open)).toEqual(['a', 'b']);
    expect(ids(openColumnAs([col('a', 1)], 'b', 2, 'switch').open)).toEqual(['b']);
  });

  it('closeUnpinned keeps only the pinned columns', () => {
    expect(closeUnpinned([col('a', 1), col('b', 2, true)])).toEqual([col('b', 2, true)]);
    expect(closeUnpinned([col('a', 1)])).toEqual([]);
  });
});

describe('close, pin, most recent', () => {
  it('closing leaves focus to the most recently focused of the rest', () => {
    const open = [col('a', 5), col('b', 2)];
    expect(ids(closeColumn(open, 'a'))).toEqual(['b']);
    expect(mostRecent(open)).toBe('a');
    expect(mostRecent([])).toBeNull();
    expect(togglePin(open, 'b')[1].pinned).toBe(true);
    expect(togglePin(togglePin(open, 'b'), 'b')[1].pinned).toBe(false);
  });
});

describe('fitColumns', () => {
  const SIDEBAR = 215;
  const FILES = 280;
  const two = [col('a', 1), col('b', 2)];
  // The board has focus and a worker was focused last, unless a test says otherwise.
  const fit = (windowW: number, files: boolean, open = two, coordCollapsed = false, focused = 'board', coordFocusedAt = 0) =>
    fitColumns({ main: windowW - SIDEBAR - (files ? FILES : 0), coordW: W.COORD_DEFAULT, railW: W.RAIL_DEFAULT, coordCollapsed, open, focused, coordFocusedAt });

  it('no worker open is coordinator and board', () => {
    expect(fit(1100, true, [])).toEqual({ coord: 'full', board: 'board', shown: [], suppressed: [] });
    expect(fit(1800, false, [], true).coord).toBe('strip');
  });

  it('gives way in order: one worker, then the coordinator strip, then nothing', () => {
    // 1800 wide: everything fits.
    expect(fit(1800, false)).toEqual({ coord: 'full', board: 'rail', shown: ['a', 'b'], suppressed: [] });
    // 1400: one worker goes (a, focused longer ago); the coordinator stays.
    expect(fit(1400, false)).toEqual({ coord: 'full', board: 'rail', shown: ['b'], suppressed: ['a'] });
    // 1100: one worker and the coordinator's strip.
    expect(fit(1100, false)).toEqual({ coord: 'strip', board: 'rail', shown: ['b'], suppressed: ['a'] });
  });

  it('counts only the columns it shows as on screen: not one given way, nor a strip', () => {
    const tabOf = (t: string) => (t === 'w' ? null : `tab-${t}`);
    expect(tabsOnScreen(fit(1800, false), 'coord', tabOf)).toEqual(['coord', 'tab-a', 'tab-b']);
    expect(tabsOnScreen(fit(1400, false), 'coord', tabOf)).toEqual(['coord', 'tab-b']);
    expect(tabsOnScreen(fit(1100, false), 'coord', tabOf)).toEqual(['tab-b']);
    expect(tabsOnScreen(fit(1800, false, [], true), 'coord', tabOf)).toEqual([]);
    // A waiting worker has no tab yet.
    expect(tabsOnScreen(fit(1800, false, [col('w', 1)]), 'coord', tabOf)).toEqual(['coord']);
  });

  it('counts an open Files column in the budget', () => {
    expect(fit(1800, true)).toMatchObject({ coord: 'full', shown: ['b'], suppressed: ['a'] });
    expect(fit(1400, true)).toMatchObject({ coord: 'strip', shown: ['b'] });
    // Narrower still: the one worker shrinks below its minimum; nothing more gives way.
    expect(fit(1100, true)).toMatchObject({ coord: 'strip', shown: ['b'], suppressed: ['a'] });
  });

  it('of the workers the human is not in, suppresses the unpinned one first, and of two pinned the least recently focused', () => {
    // The coordinator has focus, so neither worker is kept.
    expect(fit(1400, false, [col('a', 1, true), col('b', 2)], false, 'coordinator').shown).toEqual(['a']);
    expect(fit(1400, false, [col('a', 1, true), col('b', 2, true)], false, 'coordinator').shown).toEqual(['b']);
    // So does the board, when the coordinator was focused after the workers.
    expect(fit(1400, false, [col('a', 1, true), col('b', 2)], false, 'board', 3).shown).toEqual(['a']);
  });

  it('never hides the column the human is in, pinned neighbour or not', () => {
    // Room for one worker (1100 ≥ 440 + 220 + 420 + 8). A is pinned; the human opens C, which is
    // focused and newest: C shows, and A waits suppressed (still open) rather than C opening hidden.
    const main = 1100 + SIDEBAR;
    expect(fit(main, false, [col('A', 1, true), col('C', 2)], false, 'C')).toEqual({ coord: 'full', board: 'rail', shown: ['C'], suppressed: ['A'] });
    // The window narrows on a focused unpinned column beside an older pinned one: it stays.
    expect(fit(1400, false, [col('a', 5), col('b', 1, true)], false, 'a').shown).toEqual(['a']);
    // A focus that ties on stamps (a hand-edited view) is still the one kept.
    expect(fit(1400, false, [col('a', 0), col('b', 0)], false, 'b').shown).toEqual(['b']);
    // The board in front keeps the worker focused last, so a click on it moves nothing.
    expect(fit(main, false, [col('A', 1, true), col('C', 2)], false, 'board').shown).toEqual(['C']);
  });

  it('a focused coordinator never goes to its strip: the last worker steps aside', () => {
    // 1000 < 440 + 220 + 420 + 8: the coordinator and a worker don't fit side by side.
    const main = 1000 + SIDEBAR;
    const one = [col('a', 1)];
    expect(fit(main, false, one, false, 'a')).toEqual({ coord: 'strip', board: 'rail', shown: ['a'], suppressed: [] });
    expect(fit(main, false, one, false, 'coordinator')).toEqual({ coord: 'full', board: 'board', shown: [], suppressed: ['a'] });
    expect(fit(main, false, two, false, 'coordinator')).toEqual({ coord: 'full', board: 'board', shown: [], suppressed: ['a', 'b'] });
    // The board in front after the coordinator: still the coordinator.
    expect(fit(main, false, one, false, 'board', 2).shown).toEqual([]);
    // A strip the human chose stays a strip.
    expect(fit(main, false, one, true, 'coordinator')).toEqual({ coord: 'strip', board: 'rail', shown: ['a'], suppressed: [] });
  });

  it('a coordinator the human collapsed leaves room for both workers sooner', () => {
    // 44 + 220 + 840 + 12 = 1116 ≤ 1400 - 215.
    expect(fit(1400, false, two, true)).toEqual({ coord: 'strip', board: 'rail', shown: ['a', 'b'], suppressed: [] });
  });

  it('clamps the widths it is given', () => {
    const base = { coordCollapsed: false, open: two, focused: 'b', coordFocusedAt: 0 };
    const wide = fitColumns({ ...base, main: 1512, coordW: 9000, railW: 9000 });
    // 560 + 240 + 840 + 12 = 1652 > 1512: the clamp, not 9000, decides.
    expect(wide.shown).toEqual(['b']);
    const exact = fitColumns({ ...base, main: 1512, coordW: 440, railW: 220 });
    expect(exact.shown).toEqual(['a', 'b']);
  });
});

describe('focus order', () => {
  it('skips a coordinator the human collapsed', () => {
    expect(cycleOrder([col('a', 1)], true)).toEqual(['board', 'a']);
  });

  it('reaches a worker a focused coordinator pushed out of a narrow window', () => {
    // 1000 < 440 + 220 + 420 + 8: with the coordinator focused, a gives way.
    const open = [col('a', 1)];
    const at = (focused: string, coordFocusedAt: number) =>
      fitColumns({ main: 1000, coordW: W.COORD_DEFAULT, railW: W.RAIL_DEFAULT, coordCollapsed: false, open, focused, coordFocusedAt });
    expect(at('coordinator', 2).shown).toEqual([]);
    const order = cycleOrder(open, false);
    // The board keeps the coordinator as the keeper, so a is still out; one more step reaches it.
    expect(nextFocus(order, 'coordinator', 1)).toBe('board');
    expect(at('board', 2).shown).toEqual([]);
    expect(nextFocus(order, 'board', 1)).toBe('a');
    expect(nextFocus(order, 'coordinator', -1)).toBe('a');
    // Focused, it is the keeper and shows again.
    expect(at('a', 2).shown).toEqual(['a']);
  });

  it('cycles coordinator, board, then the open columns, wrapping both ways', () => {
    const order = cycleOrder([col('a', 1), col('b', 2)], false);
    expect(order).toEqual(['coordinator', 'board', 'a', 'b']);
    expect(nextFocus(order, 'b', 1)).toBe('coordinator');
    expect(nextFocus(order, 'coordinator', -1)).toBe('b');
    expect(nextFocus(order, 'board', 1)).toBe('a');
    // A column that just closed starts from an end.
    expect(nextFocus(order, 'gone', 1)).toBe('coordinator');
    expect(nextFocus(order, 'gone', -1)).toBe('b');
  });
});

describe('the stored view', () => {
  it('round-trips, keyed by task id', () => {
    const v = { ...defaultView(), open: [col('t1', 10, true), col('t2', 20)], focused: 't2', coordFocusedAt: 15, coordCollapsed: true, terminalShown: ['t1'] };
    expect(parseView(serializeView(v))).toEqual(v);
  });

  it('reads anything else as the default, never breaking the view', () => {
    expect(parseView(null)).toEqual(defaultView());
    expect(parseView('{not json')).toEqual(defaultView());
    expect(parseView('"a string"')).toEqual(defaultView());
    const odd = parseView(JSON.stringify({
      open: [{ task_id: 'a' }, { task_id: 'a' }, { nope: 1 }, { task_id: 'b', pinned: 'yes', focused_at: 'x' }, { task_id: 'c' }],
      focused: 'zzz',
      coord_collapsed: 1,
      terminal_shown: ['a', 'a', 3],
      view: 'timeline',
    }));
    // Two columns at most, deduplicated; a focus naming nothing open is the coordinator.
    expect(odd.open).toEqual([col('a', 0), col('b', 0)]);
    expect(odd.focused).toBe('coordinator');
    expect(odd.coordCollapsed).toBe(false);
    expect(odd.terminalShown).toEqual(['a']);
    expect(odd.view).toBe('board');
  });
});
