/** Done (docs/hive.md §11.4), pure: the header's Done label and the report's lines. The report is
 *  Rust's (`hive/finish.rs` `FinishRecord`): every receipt line there is maiTerm's description of
 *  what it verified, so the view only splits off the kind. */

import type { HiveFinishLanded } from '$lib/tauri/types';

export type ReceiptKind = 'land' | 'check' | 'commit' | 'file' | 'asset' | 'other';

const KINDS: readonly ReceiptKind[] = ['land', 'check', 'commit', 'file', 'asset'];

/** A receipt line as Rust wrote it ("land L-…: …", "commit web:a1b2c3d on main"), its kind split off. */
export function receiptLine(line: string): { kind: ReceiptKind; text: string } {
  const space = line.indexOf(' ');
  const head = space > 0 ? line.slice(0, space) : '';
  const kind = KINDS.find((k) => k === head);
  return kind ? { kind, text: line.slice(space + 1) } : { kind: 'other', text: line };
}

/** `“Login API” (api) into main in web, at a1b2c3d`, and what became of it since. */
export function landedLine(l: HiveFinishLanded): string {
  const who = l.worker ? ` (${l.worker})` : '';
  const at = l.base_after ? `, at ${l.base_after.slice(0, 7)}` : '';
  let s = `“${l.title}”${who} into ${l.target} in ${l.folder}${at}`;
  if (l.undone) s += ' · undone';
  if (l.suspect) s += ' · suspect';
  return s;
}

const MONTHS = ['Jan', 'Feb', 'Mar', 'Apr', 'May', 'Jun', 'Jul', 'Aug', 'Sep', 'Oct', 'Nov', 'Dec'];

/** "Done · 14:20" today, "Done · Oct 23, 14:20" another day; local time. */
export function doneLabel(doneAt: number | null | undefined, now: number = Date.now()): string {
  if (!doneAt) return 'Done';
  const d = new Date(doneAt);
  const hm = `${String(d.getHours()).padStart(2, '0')}:${String(d.getMinutes()).padStart(2, '0')}`;
  const n = new Date(now);
  const sameDay = d.getFullYear() === n.getFullYear() && d.getMonth() === n.getMonth() && d.getDate() === n.getDate();
  return sameDay ? `Done · ${hm}` : `Done · ${MONTHS[d.getMonth()]} ${d.getDate()}, ${hm}`;
}

/** The notice when a Hive moves to done, or null for any other move. */
export function doneNotice(before: string | null | undefined, after: string, name: string): { title: string; body: string } | null {
  if (after !== 'done' || before === 'done' || before == null) return null;
  return { title: `Hive done: ${name}`, body: 'Its coordinator finished the goal. The report is in the Hive header.' };
}
