import { afterEach, describe, expect, it, vi } from 'vitest';
import { observeResizeNextFrame } from './frameResize';

/** A ResizeObserver and requestAnimationFrame we can drive by hand. */
function fakes() {
  let notify: (() => void) | null = null;
  let disconnected = false;
  const frames = new Map<number, () => void>();
  let next = 1;
  vi.stubGlobal('ResizeObserver', class {
    constructor(cb: () => void) { notify = cb; }
    observe() {}
    disconnect() { disconnected = true; }
  });
  vi.stubGlobal('requestAnimationFrame', (cb: () => void) => { frames.set(next, cb); return next++; });
  vi.stubGlobal('cancelAnimationFrame', (id: number) => { frames.delete(id); });
  return {
    resize: () => notify?.(),
    runFrame: () => { const fs = [...frames.values()]; frames.clear(); for (const f of fs) f(); },
    pending: () => frames.size,
    disconnected: () => disconnected,
  };
}

afterEach(() => vi.unstubAllGlobals());

describe('observeResizeNextFrame', () => {
  it('never runs the layout-changing callback inside the observer', () => {
    // The tab bar's hidden-tab count resized the bar it was measured from inside the
    // observation loop: "ResizeObserver loop completed with undelivered notifications" on every
    // Files column open and close.
    const f = fakes();
    const fn = vi.fn();
    observeResizeNextFrame({} as Element, fn);
    f.resize();
    expect(fn).not.toHaveBeenCalled();
    f.runFrame();
    expect(fn).toHaveBeenCalledTimes(1);
  });

  it('runs once per frame however many notifications arrive', () => {
    const f = fakes();
    const fn = vi.fn();
    observeResizeNextFrame({} as Element, fn);
    f.resize();
    f.resize();
    f.resize();
    expect(f.pending()).toBe(1);
    f.runFrame();
    expect(fn).toHaveBeenCalledTimes(1);
    f.resize();
    f.runFrame();
    expect(fn).toHaveBeenCalledTimes(2);
  });

  it('drops a pending frame on disconnect', () => {
    const f = fakes();
    const fn = vi.fn();
    const stop = observeResizeNextFrame({} as Element, fn);
    f.resize();
    stop();
    expect(f.disconnected()).toBe(true);
    f.runFrame();
    expect(fn).not.toHaveBeenCalled();
  });
});
