/** Hive settings → Tools (docs/hive.md §3.4, docs/hive-media.md §7): the pure half of changing a
 *  Hive's tools after Create. Rust checks every id again (hive/tool_change.rs); this only decides
 *  what the sheet shows and sends. */
import type { HiveDetectedTool } from '$lib/tauri/types';
import { isSetupOnly, toolLabel } from './media';

/** The picks the sheet starts from: the Hive's tools, without a setup-only program a Hive was
 *  saved with (it is not a tool, and saving drops it). */
export function seedPicked(hiveTools: readonly string[]): Set<string> {
  return new Set(hiveTools.filter((id) => !isSetupOnly(id)));
}

/** The Hive's tools this Mac no longer detects (a server dropped from the config). Kept unless the
 *  human unticks them: the picker can't show them, so the sheet lists them on their own. */
export function undetected(hiveTools: readonly string[], detected: readonly Pick<HiveDetectedTool, 'id'>[]): string[] {
  return hiveTools.filter((id) => !isSetupOnly(id) && !detected.some((t) => t.id === id));
}

/** What Save sends: the Hive's own order for what it keeps, then the new picks in the picker's
 *  order, then any pick neither knows (none, normally). */
export function nextTools(hiveTools: readonly string[], picked: ReadonlySet<string>, detected: readonly Pick<HiveDetectedTool, 'id'>[]): string[] {
  const out: string[] = [];
  const add = (id: string) => {
    if (picked.has(id) && !isSetupOnly(id) && !out.includes(id)) out.push(id);
  };
  hiveTools.forEach(add);
  detected.forEach((t) => add(t.id));
  picked.forEach(add);
  return out;
}

/** Whether `next` differs from the Hive's tools, order aside. */
export function toolsChanged(hiveTools: readonly string[], next: readonly string[]): boolean {
  const had = new Set(hiveTools.filter((id) => !isSetupOnly(id)));
  return had.size !== next.length || next.some((id) => !had.has(id)) || hiveTools.some(isSetupOnly);
}

/** A tool as the human reads it: its detected label, else its id without the kind prefix. */
export function labelOf(id: string, detected: readonly Pick<HiveDetectedTool, 'id' | 'label'>[]): string {
  return detected.find((t) => t.id === id)?.label || toolLabel(id);
}

/** The offered hosts still ticked: every one the human hasn't unticked. A host offered anew starts
 *  ticked; one the human unticked stays so while it is offered. */
export function tickedHosts(offered: readonly string[], unticked: ReadonlySet<string>): string[] {
  return offered.filter((h) => !unticked.has(h));
}

/** The inline confirm's sentence: "Adds fal, browse. Removes ffmpeg. Media workers may also reach
 *  2 hosts." Empty parts are left out. */
export function confirmLine(added: readonly string[], removed: readonly string[], hosts: number, label: (id: string) => string, folders = 0): string {
  const names = (ids: readonly string[]) => ids.map(label).join(', ');
  const parts: string[] = [];
  if (added.length) parts.push(`Adds ${names(added)}.`);
  if (removed.length) parts.push(`Removes ${names(removed)}.`);
  if (hosts) parts.push(`Media workers may also reach ${hosts} ${hosts === 1 ? 'host' : 'hosts'}.`);
  if (folders) parts.push(`galleryAdd also takes files from ${folders === 1 ? 'one more folder' : `${folders} more folders`}.`);
  return parts.join(' ');
}
