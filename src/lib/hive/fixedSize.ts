/** A Hive agent's raw terminal (docs/hive.md §4.5, spike 3). Its PTY spawns at one size and keeps
 *  it for life: every column renders the transcript, so no layout needs another size, and a width
 *  change at a streaming Claude duplicates its scrollback. Rust enforces the same size
 *  (`pty::manager::HIVE_PTY_SIZE`) and refuses any resize of a Hive PTY, so these must match it. */
export const HIVE_PTY_COLS = 120;
export const HIVE_PTY_ROWS = 40;

/** Whether a TerminalPane may fit its xterm to its container and resize the PTY to match. A
 *  `fixedSize` pane never does: its xterm keeps the PTY's size and its slot scrolls instead. */
export function fitAndSyncAllowed(fixedSize: boolean, visible: boolean, connected: boolean): boolean {
  return !fixedSize && visible && connected;
}

/** Whether a wheel gesture over a `fixedSize` pane belongs to its slot (native sideways scroll)
 *  rather than to the scrollback: one that is mostly horizontal, or Shift+wheel, which the browser
 *  turns sideways. An ordinary pane always fits its container, so it never gives one up. */
export function wheelScrollsSlot(fixedSize: boolean, deltaX: number, deltaY: number, shiftKey: boolean): boolean {
  return fixedSize && (shiftKey || Math.abs(deltaX) > Math.abs(deltaY));
}

/** The raw terminal's share of its column, as a percentage, kept per viewer. */
export const TERMINAL_HEIGHT_KEY = 'hive.terminal.height';
export const TERMINAL_HEIGHT_DEFAULT = 45;
export function clampTerminalHeight(pct: number): number {
  if (!Number.isFinite(pct)) return TERMINAL_HEIGHT_DEFAULT;
  return Math.min(85, Math.max(15, Math.round(pct)));
}
