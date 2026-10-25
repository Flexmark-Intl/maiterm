/** The delete sheet's words (docs/hive.md §11.2), as pure functions over what Rust answered. */

import type { HiveDeletePreview, HiveDeleteResult } from '$lib/tauri/types';

export function formatBytes(n: number): string {
  if (n < 1024) return `${n} B`;
  const units = ['KB', 'MB', 'GB', 'TB'];
  let v = n / 1024;
  let i = 0;
  while (v >= 1024 && i < units.length - 1) {
    v /= 1024;
    i++;
  }
  return `${v >= 10 ? Math.round(v) : v.toFixed(1)} ${units[i]}`;
}

const plural = (n: number, one: string, many = `${one}s`) => `${n} ${n === 1 ? one : many}`;

/** Why the delete can't go ahead now, or null. */
export function deleteRefusal(p: HiveDeletePreview | null): string | null {
  if (!p) return null;
  return p.lands_in_flight.length ? `${p.lands_in_flight.join(', ')} ${p.lands_in_flight.length === 1 ? 'is' : 'are'} landing: wait for it to finish or be held.` : null;
}

/** The sheet's "Kept on disk": worktrees and scratch holding work, and the memory folder unless
 *  its box is ticked (memory is work, the human's own files in it included). */
export function keptLines(p: HiveDeletePreview, deleteMemory: boolean): string[] {
  const out = [
    ...p.dirty,
    ...p.scratch.map((s) => `Scratch of ${s.worker}: ${s.path} (${plural(s.files, 'file')}${s.reported ? `, ${s.reported} reported` : ''})`),
  ];
  if (p.memory && !deleteMemory) out.push(`Shared memory: ${p.memory.path} (${plural(p.memory.files, 'file')}, ${formatBytes(p.memory.bytes)})`);
  return out;
}

/** The sheet's "Removed": what goes that isn't work, and the gallery and memory when ticked. */
export function removedLines(p: HiveDeletePreview, deleteGallery: boolean, deleteMemory: boolean): string[] {
  const out = [...(p.removed ?? [])];
  if (deleteMemory && p.memory) out.unshift(`Shared memory: ${p.memory.path} (${plural(p.memory.files, 'file')})`);
  if (deleteGallery && p.gallery_files) out.unshift(`The gallery: ${plural(p.gallery_files, 'asset')}, ${formatBytes(p.gallery_bytes)}`);
  return out;
}

/** The toast after a delete: what went, and what stayed. */
export function deleteSummary(r: HiveDeleteResult): string {
  const parts = [`${plural(r.worktrees_removed, 'clean worktree')} removed`];
  if (r.kept.length) parts.push(`${plural(r.kept.length, 'item')} kept on disk (work in it)`);
  if (r.branches.length) parts.push(`${plural(r.branches.length, 'branch', 'branches')} kept`);
  parts.push(r.gallery_kept ? 'the gallery kept' : 'no gallery kept');
  if (r.memory_kept) parts.push('the memory kept');
  const where = r.folder_left ? ` What stayed is in ${r.folder_left}.` : '';
  return `${parts.join(', ')}.${where}`;
}
