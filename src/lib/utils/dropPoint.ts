/**
 * Where an OS file drop happened, in the CSS pixels `getBoundingClientRect` uses.
 *
 * Tauri's `onDragDropEvent` position is typed as physical, but what it carries depends on the
 * platform: macOS sends points and Linux logical pixels (both already CSS pixels at zoom 1),
 * while Windows (WebView2, a per-monitor DPI aware process) sends real physical pixels with no
 * scaling applied. Compared raw at 150% scaling, a drop is hit-tested 1.5× further from the
 * corner than the pointer: outside a narrow column, or several rows down. `devicePixelRatio`
 * is the scale factor times the webview's zoom, which is exactly the physical-to-CSS ratio.
 */
import { isWindows } from './platform';

export function dropPointToCss(p: { x: number; y: number }, windows = isWindows(), ratio = typeof window === 'undefined' ? 1 : window.devicePixelRatio || 1): { x: number; y: number } {
  if (!windows || ratio === 1) return { x: p.x, y: p.y };
  return { x: p.x / ratio, y: p.y / ratio };
}
