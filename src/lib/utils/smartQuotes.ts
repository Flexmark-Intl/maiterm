import { EditorState, Transaction, type ChangeSpec, type Extension } from '@codemirror/state';

// macOS WebKit substitutes straight quotes typed into editable fields with
// curly typographic quotes when "Smart quotes" is enabled system-wide (System
// Settings > Keyboard > Text Input > Edit > Smart quotes). In a developer tool
// this is silently destructive: a search like 'identity' becomes ‘identity’ and
// never matches the straight quotes in source code; the same corruption hits
// file names, snippets, and notes. Strip them back to straight ASCII quotes
// everywhere the user types. Replacement is 1:1 (length preserved), so caret
// offsets stay valid.

const CURLY_SINGLE = /[‘’‚‛]/g; // ‘ ’ ‚ ‛
const CURLY_DOUBLE = /[“”„‟]/g; // “ ” „ ‟

/** Replace curly/typographic quotes with straight ASCII quotes. */
export function normalizeSmartQuotes(value: string): string {
  return value.replace(CURLY_SINGLE, "'").replace(CURLY_DOUBLE, '"');
}

/**
 * Install an app-wide fix for plain `<input>`/`<textarea>` fields (search bars,
 * preferences, rename fields, notes, etc.). A single capture-phase `input`
 * listener normalizes the value before bubble-phase framework handlers
 * (Svelte `bind:value`, CodeMirror's panel `onkeyup`) read it — so no
 * re-dispatch is needed. The terminal's xterm helper textarea is left alone:
 * xterm manages its own input path.
 *
 * Returns a cleanup function.
 */
export function installGlobalSmartQuoteFix(): () => void {
  const onInput = (e: Event) => {
    const target = e.target;
    if (!(target instanceof HTMLInputElement) && !(target instanceof HTMLTextAreaElement)) return;
    if (target.closest('.xterm')) return; // terminal input belongs to xterm
    const fixed = normalizeSmartQuotes(target.value);
    if (fixed === target.value) return;
    const start = target.selectionStart;
    const end = target.selectionEnd;
    target.value = fixed;
    try {
      if (start !== null && end !== null) target.setSelectionRange(start, end);
    } catch {
      // Some input types (number, email, …) don't support selection — ignore.
    }
  };
  document.addEventListener('input', onInput, true);
  return () => document.removeEventListener('input', onInput, true);
}

/**
 * CodeMirror extension that normalizes smart quotes inside editor/diff *content*
 * (a contenteditable surface, not an `<input>`, so the global listener above
 * doesn't see it). Rewrites inserted curly quotes to straight in any
 * doc-changing transaction — covering both direct typing and macOS deferred
 * `insertReplacementText` substitution, since CodeMirror syncs either through a
 * transaction. Idempotent: the rewritten transaction has no curly quotes left,
 * so it passes back through unchanged (no loop).
 */
export const contentSmartQuoteFix: Extension = EditorState.transactionFilter.of((tr) => {
  if (!tr.docChanged) return tr;
  let changed = false;
  const specs: ChangeSpec[] = [];
  tr.changes.iterChanges((fromA, toA, _fromB, _toB, inserted) => {
    const text = inserted.toString();
    const fixed = normalizeSmartQuotes(text);
    if (fixed !== text) changed = true;
    specs.push({ from: fromA, to: toA, insert: fixed });
  });
  if (!changed) return tr;
  return tr.startState.update({
    changes: specs,
    selection: tr.selection,
    scrollIntoView: tr.scrollIntoView,
    userEvent: tr.annotation(Transaction.userEvent),
  });
});
