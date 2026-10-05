/**
 * Cross-platform keyboard modifier helpers.
 *
 * On macOS the "action" modifier is Cmd (metaKey).
 * On Linux / Windows it is Ctrl (ctrlKey).
 */

const _isMac =
  typeof navigator !== 'undefined' && /Mac|iPhone|iPad/.test(navigator.platform);

/** True when running on macOS (or iOS, unlikely for Tauri). */
export function isMac(): boolean {
  return _isMac;
}

/** True when running on Windows. */
export function isWindows(): boolean {
  return typeof navigator !== 'undefined' && /Win/.test(navigator.platform);
}

/** Check the platform action-modifier on a keyboard event (Cmd on mac, Ctrl elsewhere). */
export function isModKey(e: KeyboardEvent): boolean {
  return _isMac ? e.metaKey : e.ctrlKey;
}

/** Human-readable label for the action modifier key. */
export const modLabel = _isMac ? 'Cmd' : 'Ctrl';

/** Symbol for the action modifier (⌘ / Ctrl). */
export const modSymbol = _isMac ? '⌘' : 'Ctrl+';

/** Human-readable label for the option/alt key. */
export const altLabel = _isMac ? 'Opt' : 'Alt';
