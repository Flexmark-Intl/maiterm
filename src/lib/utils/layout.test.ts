import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { afterLayout, nextLayout } from './layout';

// The incident: a TerminalPane mount awaited a bare requestAnimationFrame before fitting
// and spawning its PTY. WKWebView never fires rAF while the window is occluded (display
// sleep, lock screen), so a tab created from maiLink — or every tab of a session restore
// after a deploy — sat PTY-less until the screens woke. These tests model that: a rAF
// that never comes, and one that does.

type RafCb = (t: number) => void;
let rafQueue: RafCb[] = [];
let cancelled: number[] = [];

beforeEach(() => {
  vi.useFakeTimers();
  rafQueue = [];
  cancelled = [];
  vi.stubGlobal('requestAnimationFrame', (cb: RafCb) => rafQueue.push(cb));
  vi.stubGlobal('cancelAnimationFrame', (id: number) => { cancelled.push(id); });
});

afterEach(() => {
  vi.unstubAllGlobals();
  vi.useRealTimers();
});

describe('afterLayout', () => {
  it('falls back to the timer when the frame never arrives (occluded window)', () => {
    const cb = vi.fn();
    afterLayout(cb, 50);
    expect(cb).not.toHaveBeenCalled();
    vi.advanceTimersByTime(49);
    expect(cb).not.toHaveBeenCalled();
    vi.advanceTimersByTime(1);
    expect(cb).toHaveBeenCalledTimes(1);
    // The pending frame is released so it can't double-fire later.
    expect(cancelled).toEqual([1]);
  });

  it('runs on the frame when the page is visible, and only once', () => {
    const cb = vi.fn();
    afterLayout(cb, 50);
    rafQueue.shift()?.(16);
    expect(cb).toHaveBeenCalledTimes(1);
    vi.advanceTimersByTime(1000);
    expect(cb).toHaveBeenCalledTimes(1);
  });

  it('runs once even if the frame lands after the fallback fired', () => {
    const cb = vi.fn();
    afterLayout(cb, 50);
    vi.advanceTimersByTime(50);
    rafQueue.shift()?.(16);
    expect(cb).toHaveBeenCalledTimes(1);
  });
});

describe('nextLayout', () => {
  it('resolves without a frame', async () => {
    const p = nextLayout(50);
    vi.advanceTimersByTime(50);
    await expect(p).resolves.toBeUndefined();
  });
});
