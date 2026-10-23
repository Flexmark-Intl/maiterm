/**
 * Pointer drag-and-drop out of the Files tree (docs/file-tree.md §10.4).
 *
 * HTML5 drag-and-drop is dead in this webview (Tauri intercepts it for OS drops, see
 * OverlordBoardView), so a drag is pointer events on the window, the TerminalTabs way: nothing
 * happens until the pointer has moved 5 px, so a click stays a click. Then a ghost chip
 * ("store.ts" / "3 items") follows the pointer and the target under it is hit-tested on every
 * move:
 * - a folder row in the tree (the tree answers, arithmetically; a file row means its folder),
 *   highlighted, and opened after 600 ms of hovering;
 * - the root header (`[data-ft-root-drop]`): the root folder;
 * - an open ComposerDock (`[data-composer-tab]`): attach;
 * - a terminal (`[data-terminal-slot]` of a live terminal tab): insert the paths (§9.5).
 *
 * Alt (macOS) or Ctrl (elsewhere) turns a move into a copy, shown as ＋ on the chip; near the
 * tree's top or bottom edge the tree scrolls; Escape cancels. The click that ends a real drag
 * is swallowed, so dropping onto a row doesn't also select and open it.
 */

export type DropTarget =
  | { kind: 'folder'; rel: string }
  | { kind: 'terminal'; tabId: string }
  | { kind: 'composer'; tabId: string };

export interface DragOptions {
  /** The chip's text. */
  label: string;
  mac: boolean;
  /** The tree's own answer for a point: a target, `null` when the point is over the tree
   *  but nothing there takes a drop, `undefined` when it isn't over the tree at all. */
  hitTree: (x: number, y: number) => DropTarget | null | undefined;
  /** A terminal tab that can take paths (live, not a service). */
  isTerminal: (tabId: string) => boolean;
  /** Whether this drop would do anything (a folder into itself, or where it already is,
   *  is no target). */
  accepts: (t: DropTarget, copy: boolean) => boolean;
  /** The element that scrolls near its edges. */
  scroller: () => HTMLElement | null;
  onHover: (t: DropTarget | null) => void;
  /** A folder hovered for 600 ms: open it. */
  onLinger: (rel: string) => void;
  onDrop: (t: DropTarget, copy: boolean) => void;
  /** Always called last, dropped or not. */
  onEnd: () => void;
}

export const DRAG_THRESHOLD_PX = 5;
export const LINGER_MS = 600;
export const EDGE_PX = 24;
const SCROLL_STEP_PX = 12;
const SCROLL_EVERY_MS = 16;

/** The copy modifier: Alt on macOS (Finder), Ctrl elsewhere (Explorer, Nautilus). */
export function isCopyDrag(e: { altKey: boolean; ctrlKey: boolean }, mac: boolean): boolean {
  return mac ? e.altKey : e.ctrlKey;
}

export function sameTarget(a: DropTarget | null, b: DropTarget | null): boolean {
  if (!a || !b) return a === b;
  if (a.kind !== b.kind) return false;
  return a.kind === 'folder' ? a.rel === (b as { rel: string }).rel : a.tabId === (b as { tabId: string }).tabId;
}

/** Where a page point drops, outside the tree: the root header, a composer, a terminal. */
function pageTarget(x: number, y: number, isTerminal: (id: string) => boolean): DropTarget | null {
  const el = document.elementFromPoint(x, y);
  if (!el) return null;
  if (el.closest('[data-ft-root-drop]')) return { kind: 'folder', rel: '' };
  const composer = el.closest<HTMLElement>('[data-composer-tab]');
  if (composer?.dataset.composerTab) return { kind: 'composer', tabId: composer.dataset.composerTab };
  const slot = el.closest<HTMLElement>('[data-terminal-slot]');
  const id = slot?.dataset.terminalSlot;
  return id && isTerminal(id) ? { kind: 'terminal', tabId: id } : null;
}

function makeGhost(label: string): { el: HTMLDivElement; setCopy: (c: boolean) => void } {
  const el = document.createElement('div');
  // Inline styles: it lives on <body>, outside every scoped stylesheet.
  Object.assign(el.style, {
    position: 'fixed',
    left: '0',
    top: '0',
    zIndex: '10000',
    pointerEvents: 'none',
    padding: '2px 8px',
    borderRadius: '10px',
    background: 'var(--bg-light)',
    color: 'var(--fg)',
    border: '1px solid color-mix(in srgb, var(--accent) 60%, transparent)',
    font: '12px var(--font-ui, system-ui, sans-serif)',
    whiteSpace: 'nowrap',
    maxWidth: '260px',
    overflow: 'hidden',
    textOverflow: 'ellipsis',
    boxShadow: '0 2px 8px rgba(0,0,0,0.3)',
  } satisfies Partial<CSSStyleDeclaration>);
  const plus = document.createElement('span');
  plus.textContent = '＋ ';
  plus.style.color = 'var(--green)';
  plus.hidden = true;
  el.append(plus, document.createTextNode(label));
  document.body.appendChild(el);
  return { el, setCopy: (c) => { plus.hidden = !c; } };
}

/** Start watching a press that may become a drag. Call from the row's pointerdown. */
export function beginPointerDrag(start: PointerEvent, o: DragOptions): void {
  if (start.button !== 0) return;
  const x0 = start.clientX;
  const y0 = start.clientY;
  let dragging = false;
  let ghost: ReturnType<typeof makeGhost> | null = null;
  let target: DropTarget | null = null;
  let copy = isCopyDrag(start, o.mac);
  let last = { x: x0, y: y0 };
  let lingerTimer: ReturnType<typeof setTimeout> | null = null;
  let scrollTimer: ReturnType<typeof setInterval> | null = null;
  let ended = false;

  const setTarget = (t: DropTarget | null) => {
    if (sameTarget(t, target)) return;
    target = t;
    o.onHover(t);
    if (lingerTimer) clearTimeout(lingerTimer);
    lingerTimer = null;
    if (t?.kind === 'folder' && t.rel !== '') {
      const rel = t.rel;
      lingerTimer = setTimeout(() => {
        lingerTimer = null;
        o.onLinger(rel);
        // Opening moved the rows under the pointer: hit-test again.
        retarget();
      }, LINGER_MS);
    }
  };

  const retarget = () => {
    const { x, y } = last;
    const inTree = o.hitTree(x, y);
    let t = inTree === undefined ? pageTarget(x, y, o.isTerminal) : inTree;
    if (t && !o.accepts(t, copy)) t = null;
    ghost?.setCopy(copy && t?.kind === 'folder');
    setTarget(t);
  };

  const autoscroll = () => {
    const sc = o.scroller();
    if (!sc) return stopScroll();
    const r = sc.getBoundingClientRect();
    const inside = last.x >= r.left && last.x <= r.right;
    const dir = !inside ? 0 : last.y < r.top + EDGE_PX && last.y >= r.top - EDGE_PX ? -1 : last.y > r.bottom - EDGE_PX && last.y <= r.bottom + EDGE_PX ? 1 : 0;
    if (dir === 0) return stopScroll();
    if (!scrollTimer) {
      scrollTimer = setInterval(() => {
        const s = o.scroller();
        if (!s) return stopScroll();
        s.scrollTop += dir * SCROLL_STEP_PX;
        retarget();
        autoscroll();
      }, SCROLL_EVERY_MS);
    }
  };

  const stopScroll = () => {
    if (scrollTimer) clearInterval(scrollTimer);
    scrollTimer = null;
  };

  const onMove = (e: PointerEvent) => {
    if (e.pointerId !== start.pointerId) return;
    last = { x: e.clientX, y: e.clientY };
    if (!dragging) {
      if (Math.hypot(e.clientX - x0, e.clientY - y0) < DRAG_THRESHOLD_PX) return;
      dragging = true;
      ghost = makeGhost(o.label);
      document.body.style.cursor = 'grabbing';
    }
    copy = isCopyDrag(e, o.mac);
    ghost!.el.style.transform = `translate(${e.clientX + 12}px, ${e.clientY + 10}px)`;
    retarget();
    stopScroll();
    autoscroll();
  };

  const onKey = (e: KeyboardEvent) => {
    if (!dragging) return;
    if (e.key === 'Escape' && e.type === 'keydown') {
      e.preventDefault();
      e.stopPropagation();
      finish(false);
      return;
    }
    const c = isCopyDrag(e, o.mac);
    if (c !== copy) {
      copy = c;
      retarget();
    }
  };

  const onUp = (e: PointerEvent) => {
    if (e.pointerId !== start.pointerId) return;
    finish(e.type === 'pointerup');
  };

  const swallowClick = () => {
    const eat = (e: MouseEvent) => {
      e.preventDefault();
      e.stopImmediatePropagation();
      off();
    };
    const off = () => window.removeEventListener('click', eat, true);
    window.addEventListener('click', eat, true);
    setTimeout(off, 0);
  };

  function finish(drop: boolean) {
    if (ended) return;
    ended = true;
    window.removeEventListener('pointermove', onMove, true);
    window.removeEventListener('pointerup', onUp, true);
    window.removeEventListener('pointercancel', onUp, true);
    window.removeEventListener('keydown', onKey, true);
    window.removeEventListener('keyup', onKey, true);
    if (lingerTimer) clearTimeout(lingerTimer);
    stopScroll();
    if (dragging) {
      ghost?.el.remove();
      document.body.style.cursor = '';
      swallowClick();
      const t = target;
      o.onHover(null);
      if (drop && t) o.onDrop(t, copy);
    }
    o.onEnd();
  }

  window.addEventListener('pointermove', onMove, true);
  window.addEventListener('pointerup', onUp, true);
  window.addEventListener('pointercancel', onUp, true);
  window.addEventListener('keydown', onKey, true);
  window.addEventListener('keyup', onKey, true);
}
