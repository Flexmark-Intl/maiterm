/**
 * A ResizeObserver whose callback runs on the next animation frame, once per frame.
 *
 * For a callback that changes layout. A ResizeObserver callback that sets Svelte state is
 * flushed in a microtask, which runs inside the browser's resize-observation loop: when that
 * flush resizes the observed element again (or anything as shallow), the browser gives up on
 * the frame and raises "ResizeObserver loop completed with undelivered notifications" as a
 * window error, which the [WEBVIEW_ERROR] forwarder logs. A frame later the layout write is
 * an ordinary change, observed on the next frame like any other.
 *
 * Returns the disconnect.
 */
export function observeResizeNextFrame(el: Element, fn: () => void): () => void {
  let raf = 0;
  const ro = new ResizeObserver(() => {
    if (raf) return;
    raf = requestAnimationFrame(() => {
      raf = 0;
      fn();
    });
  });
  ro.observe(el);
  return () => {
    ro.disconnect();
    if (raf) cancelAnimationFrame(raf);
    raf = 0;
  };
}
