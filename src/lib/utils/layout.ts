/**
 * Run `cb` after the next layout pass without depending on requestAnimationFrame.
 *
 * WKWebView pauses rAF entirely while the window is occluded — display sleep, the
 * lock screen, a window fully covered by another — and throttles DOM timers to
 * roughly one tick per second. Layout itself keeps working; only painting stops.
 * So a bare `await requestAnimationFrame` on a correctness path (spawning a PTY,
 * syncing PTY size) hangs until the screens wake, while the Rust side — and a
 * maiLink phone — carry on as if the tab were live. This races rAF against a
 * timer: the frame wins on a visible page, the timer wins on a hidden one.
 */
export function afterLayout(cb: () => void, fallbackMs = 50): void {
  let raf = 0;
  let timer: ReturnType<typeof setTimeout> | undefined;
  let done = false;
  const run = () => {
    if (done) return;
    done = true;
    cancelAnimationFrame(raf);
    clearTimeout(timer);
    cb();
  };
  raf = requestAnimationFrame(run);
  timer = setTimeout(run, fallbackMs);
}

/** Promise form of {@link afterLayout}. */
export function nextLayout(fallbackMs = 50): Promise<void> {
  return new Promise((resolve) => afterLayout(resolve, fallbackMs));
}
