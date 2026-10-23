/**
 * The active file: the file the tab in front shows, which the tree highlights (the
 * `active-file` row) and which "Reveal Active File" reveals (docs/file-tree.md §4.2).
 *
 * The highlight must always equal the active tab's file, or nothing. It is computed from the
 * tab itself (type and path), never from the edge that last re-resolved the root: a preview
 * tab swaps its file under the same tab id, and a terminal tab in front has no file at all,
 * and neither is a root edge.
 */
import type { Tab } from '$lib/tauri/types';

export interface ActiveFileTarget {
  /** The native absolute path, which Rust maps onto a row (`ftLocate`). */
  path: string;
  /** Outside the root, the header offers "Outside root · Show" (pin its folder). Only a
   *  local editor's file earns it: a diff tab's path may be an ssh agent's file, whose
   *  folder doesn't exist here. */
  offerShow: boolean;
}

/** What the tree should highlight for this tab: a local editor's file, a diff tab's file,
 *  or nothing (terminal, board, a remote editor). */
export function activeFileOf(tab: Pick<Tab, 'tab_type' | 'editor_file' | 'diff_context'> | null | undefined): ActiveFileTarget | null {
  if (!tab) return null;
  if (tab.tab_type === 'editor') {
    const f = tab.editor_file;
    return f && !f.is_remote && f.file_path ? { path: f.file_path, offerShow: true } : null;
  }
  if (tab.tab_type === 'diff') {
    const p = tab.diff_context?.file_path;
    return p ? { path: p, offerShow: false } : null;
  }
  return null;
}

export interface ActiveFileState {
  /** The row to highlight (root-relative), or null. '' is the root itself. */
  active: string | null;
  /** The editor's file when it is outside the root (the header's "Show"), or null. */
  outside: string | null;
}

/** Map a target onto the tree. `locate` is `ftLocate` for one path: the rel under the root,
 *  or null when the path isn't under it (or the lookup failed). */
export async function settleActiveFile(
  target: ActiveFileTarget | null,
  locate: ((abs: string) => Promise<string | null>) | null,
): Promise<ActiveFileState> {
  if (!target || !locate) return { active: null, outside: null };
  const rel = await locate(target.path).catch(() => null);
  if (rel == null) return { active: null, outside: target.offerShow ? target.path : null };
  return { active: rel, outside: null };
}
