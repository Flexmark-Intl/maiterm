/**
 * The tree's keys, as a pure function from a key event to an action (docs/file-tree.md §11.2).
 *
 * Kept out of the component so the map reads as one table and the platform split (macOS
 * Cmd vs Ctrl elsewhere, Alt-chords) is decided in one place.
 *
 * Two rules from the spec this enforces:
 * - Bare letters are never commands outside vim mode: an accidental `a` while typing a name
 *   must not do anything but type-ahead.
 * - Cmd/Ctrl+N stays the app's (a new window). The app's capture handler sees app shortcuts
 *   before the tree, so nothing here may claim one.
 */

export type TreeAction =
  | { type: 'move'; to: 'up' | 'down' | 'pageUp' | 'pageDown' | 'home' | 'end' }
  | { type: 'extend'; to: 'up' | 'down' }
  | { type: 'right' }
  | { type: 'left' }
  | { type: 'expandRecursive' }
  | { type: 'collapseRecursive' }
  | { type: 'selectAll' }
  | { type: 'preview' }
  | { type: 'open' }
  | { type: 'openSide' }
  | { type: 'addToTab' }
  | { type: 'copyPath'; relative: boolean }
  | { type: 'toggleHidden' }
  | { type: 'escape' }
  | { type: 'contextMenu' }
  | { type: 'typeahead'; char: string }
  /** `/` or Cmd/Ctrl+F: the find bar (§8). */
  | { type: 'find' }
  /** Vim `g`, waiting for its second `g`. */
  | { type: 'pendingG' }
  /** F2 on every platform (Enter opens, also on macOS). */
  | { type: 'rename' }
  /** Delete; macOS Cmd+Backspace. */
  | { type: 'trash' }
  /** Shift+Delete; macOS Cmd+Alt+Backspace. Always through the ConfirmStrip. */
  | { type: 'deletePermanent' }
  /** Cmd/Ctrl+C / X / V: the tree's own clipboard of files. */
  | { type: 'clipboard'; op: 'copy' | 'cut' | 'paste' }
  | { type: 'undo' };

export interface KeyLike {
  key: string;
  code?: string;
  metaKey: boolean;
  ctrlKey: boolean;
  altKey: boolean;
  shiftKey: boolean;
  /** A KeyboardEvent's, for AltGr. */
  getModifierState?(key: string): boolean;
}

export interface KeymapOptions {
  mac: boolean;
  vim: boolean;
  /** Vim: a `g` was pressed just before. */
  pendingG?: boolean;
  /** Vim: type-ahead is armed by a leading `'`. */
  typeaheadArmed?: boolean;
}

export function treeAction(e: KeyLike, o: KeymapOptions): TreeAction | null {
  // AltGr (Windows and Linux layouts) types a character: `@` on German, `ł` on Polish.
  // Chromium reports it as Ctrl+Alt, which would read as a chord below, so it is text.
  if (!o.mac && e.key.length === 1 && e.getModifierState?.('AltGraph')) {
    return o.vim && !o.typeaheadArmed ? null : { type: 'typeahead', char: e.key };
  }
  const mod = o.mac ? e.metaKey : e.ctrlKey;
  // The other platform's modifier is never ours (Ctrl+letter on macOS is emacs motion in
  // inputs; Meta elsewhere is the OS's).
  const otherMod = o.mac ? e.ctrlKey : e.metaKey;
  if (otherMod) return null;
  const k = e.key;
  const lower = k.length === 1 ? k.toLowerCase() : k;

  // Copy path: macOS Cmd+Alt+C / Cmd+Alt+Shift+C; elsewhere Shift+Alt+C / Ctrl+Shift+Alt+C.
  // `code`, since Alt changes the produced character on macOS.
  const isC = e.code === 'KeyC' || lower === 'c';
  if (isC && e.altKey) {
    if (o.mac && mod) return { type: 'copyPath', relative: e.shiftKey };
    if (!o.mac && e.shiftKey) return { type: 'copyPath', relative: mod };
  }
  // Toggle hidden files: Cmd/Ctrl+Shift+.
  if (mod && e.shiftKey && (e.code === 'Period' || k === '>' || k === '.')) return { type: 'toggleHidden' };
  if (mod && !e.shiftKey && !e.altKey && lower === 'a') return { type: 'selectAll' };
  if (mod && !e.shiftKey && !e.altKey && (lower === 'f' || e.code === 'KeyF')) return { type: 'find' };
  // `/` opens find, except as a typed character once vim's `'` armed type-ahead.
  if (!mod && !e.altKey && k === '/' && !(o.vim && o.typeaheadArmed)) return { type: 'find' };
  if (mod && !e.altKey && k === 'Enter') return { type: 'openSide' };
  if (mod && !e.altKey && !e.shiftKey) {
    // `code` too: a non-Latin layout types another letter on these keys.
    if (lower === 'c' || e.code === 'KeyC') return { type: 'clipboard', op: 'copy' };
    if (lower === 'x' || e.code === 'KeyX') return { type: 'clipboard', op: 'cut' };
    if (lower === 'v' || e.code === 'KeyV') return { type: 'clipboard', op: 'paste' };
    if (lower === 'z' || e.code === 'KeyZ') return { type: 'undo' };
  }
  if (o.mac && mod && k === 'Backspace') return e.altKey ? { type: 'deletePermanent' } : { type: 'trash' };

  if (mod) return null; // every other chord is the app's or nobody's

  if (k === 'F2' && !e.altKey && !e.shiftKey) return { type: 'rename' };
  if (k === 'Delete' && !e.altKey) return e.shiftKey ? { type: 'deletePermanent' } : { type: 'trash' };

  switch (k) {
    case 'ArrowUp': return e.shiftKey ? { type: 'extend', to: 'up' } : { type: 'move', to: 'up' };
    case 'ArrowDown': return e.shiftKey ? { type: 'extend', to: 'down' } : { type: 'move', to: 'down' };
    case 'ArrowRight': return e.altKey ? { type: 'expandRecursive' } : { type: 'right' };
    case 'ArrowLeft': return e.altKey ? { type: 'collapseRecursive' } : { type: 'left' };
    case 'PageUp': return { type: 'move', to: 'pageUp' };
    case 'PageDown': return { type: 'move', to: 'pageDown' };
    case 'Home': return { type: 'move', to: 'home' };
    case 'End': return { type: 'move', to: 'end' };
    case 'Enter': return e.altKey ? { type: 'addToTab' } : { type: 'open' };
    case 'Escape': return { type: 'escape' };
    case 'ContextMenu': return { type: 'contextMenu' };
    case 'F10': return e.shiftKey ? { type: 'contextMenu' } : null;
  }
  if (k === ' ' && !e.altKey) return o.typeaheadArmed ? { type: 'typeahead', char: ' ' } : { type: 'preview' };

  if (k.length !== 1 || e.altKey) return null;

  if (o.vim && !o.typeaheadArmed) {
    if (o.pendingG && k === 'g') return { type: 'move', to: 'home' };
    switch (k) {
      case 'j': return { type: 'move', to: 'down' };
      case 'k': return { type: 'move', to: 'up' };
      case 'h': return { type: 'left' };
      case 'l': return { type: 'right' };
      case 'G': return { type: 'move', to: 'end' };
      case 'g': return { type: 'pendingG' };
      case 'o': return { type: 'open' };
      case 'y': return { type: 'copyPath', relative: true };
      case 'Y': return { type: 'copyPath', relative: false };
      case 'a': return { type: 'addToTab' };
      case "'": return { type: 'typeahead', char: '' }; // arms type-ahead
    }
    return null;
  }
  return { type: 'typeahead', char: k };
}
