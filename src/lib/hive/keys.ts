/** A Hive's keyboard (docs/hive.md §4.6). Pure: `+layout.svelte` reads the Cmd keys here before
 *  its tab handlers (a Hive has no tab strip, and Cmd+1-9 would otherwise activate a hidden tab)
 *  and sends them to HiveView as `hive-key`; HiveView reads Escape itself (`escapeAction`). */

export type HiveKeyAction =
  | { kind: 'focus-coordinator' }
  | { kind: 'open-badge'; badge: number }
  | { kind: 'cycle'; dir: -1 | 1 }
  | { kind: 'cycle-view' }
  | { kind: 'close-focused' };

export const HIVE_KEY = 'hive-key';

export interface HiveKeyDetail {
  workspaceId: string;
  action: HiveKeyAction;
}

type KeyFields = Pick<KeyboardEvent, 'key' | 'code' | 'metaKey' | 'ctrlKey' | 'shiftKey' | 'altKey'>;

/** Cmd (Ctrl off a Mac) + 1 → the coordinator, 2…9 → badges 1…8, Shift+[ / ] → cycle the focus,
 *  Shift+Y → cycle the view, W → close the focused worker column. Option never: those are other
 *  bindings. Escape is never one: inside a raw terminal it belongs to the agent. */
export function hiveKeyAction(e: KeyFields, isMac: boolean): HiveKeyAction | null {
  if (!(isMac ? e.metaKey : e.ctrlKey) || e.altKey) return null;
  const key = e.key.toLowerCase();
  if (e.shiftKey) {
    if (e.key === '[' || e.key === '{' || e.code === 'BracketLeft') return { kind: 'cycle', dir: -1 };
    if (e.key === ']' || e.key === '}' || e.code === 'BracketRight') return { kind: 'cycle', dir: 1 };
    if (key === 'y') return { kind: 'cycle-view' };
    // Shift+digit is a symbol on most layouts, and never one of these.
    return null;
  }
  if (key === 'w') return { kind: 'close-focused' };
  if (e.key === '1') return { kind: 'focus-coordinator' };
  if (e.key >= '2' && e.key <= '9' && e.key.length === 1) return { kind: 'open-badge', badge: Number(e.key) - 1 };
  return null;
}

/** The Hive's keys reach the app from anywhere (§4.6 Scoping): a composer, the Files tree
 *  (`[data-filetree]`) or a raw terminal. Only `+layout` asks, and only in a Hive. */
export function isHiveAppShortcut(e: KeyFields, isMac: boolean): boolean {
  return hiveKeyAction(e, isMac) !== null;
}

/** Where an Escape was pressed in a Hive view, as HiveView reads it off the DOM. */
export interface EscapeTarget {
  /** Inside a raw terminal (`[data-hive-terminal]`, `.xterm`). */
  terminal: boolean;
  /** An input, a textarea or something contenteditable: a composer. */
  editable: boolean;
  /** The worker column it's in, by task id; null in the coordinator's column or the board. */
  workerColumn: string | null;
}

/** §4.6 Escape: a raw terminal's own (it interrupts the agent), so ignored; in a composer, blur;
 *  in a worker column, close it. The coordinator's column never closes. */
export function escapeAction(t: EscapeTarget): { kind: 'blur' } | { kind: 'close'; taskId: string } | null {
  if (t.terminal) return null;
  if (t.editable) return { kind: 'blur' };
  if (t.workerColumn) return { kind: 'close', taskId: t.workerColumn };
  return null;
}
