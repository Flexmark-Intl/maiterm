/**
 * Read-only diff tabs against git (docs/file-tree.md §7.4).
 *
 * `openReadOnlyDiff` is the agent's `showDiff` flow lifted out of `claudeCode.svelte.ts`: a
 * diff tab with an empty `request_id`, which no one answers and nothing blocks on.
 * `openGitDiff` feeds it from the Files tree: the base comes from `ftDiffBase` (Rust runs
 * `git cat-file` against the tree's root, so a root inside a larger repo and Windows paths
 * just work), the other side is the file as it is on disk now.
 */
import type { DiffContext } from '$lib/tauri/types';
import * as commands from '$lib/tauri/commands';
import { workspacesStore } from '$lib/stores/workspaces.svelte';
import { basenameOf } from './pathForm';

export interface ReadOnlyDiffArgs {
  filePath: string;
  oldContent: string;
  newContent: string;
  tabName: string;
  workspaceId: string;
  paneId: string;
  /** Insert after this tab (default: the pane's active tab). */
  afterTabId?: string | null;
}

/** Open a diff tab nobody answers: the old side against the new, read-only. */
export async function openReadOnlyDiff(a: ReadOnlyDiffArgs): Promise<void> {
  const diffContext: DiffContext = {
    request_id: '', // Empty = read-only, non-blocking
    file_path: a.filePath,
    old_content: a.oldContent,
    new_content: a.newContent,
    tab_name: a.tabName,
  };
  await workspacesStore.createDiffTab(a.workspaceId, a.paneId, a.tabName, diffContext, a.afterTabId ?? null);
}

/** The diff tab's title: "Diff: store.ts (HEAD)". */
export function gitDiffTabName(rel: string, ref: 'HEAD' | 'index'): string {
  // Rels are POSIX on every OS.
  return `Diff: ${basenameOf(rel, false)} (${ref})`;
}

/** readFile's refusal in words: its too-large error is a code the editor spells out too. */
function readFailure(raw: string): string {
  const big = /FILE_TOO_LARGE:([\d.]+)/.exec(raw);
  return big ? `The file is too large to compare (${big[1]} MB)` : raw.replace(/^Error:\s*/, '');
}

/**
 * "Compare with HEAD" (or the index) for a file in the Files tree: `subId` is the tree's
 * subscription, `abs` the file's native path (for reading the working copy and naming the
 * tab), `rel` its root-relative path. A file new since `ref` diffs against empty; a deleted
 * one against empty on the right. Throws with git's reason (binary, too large, no repo), or
 * with the reason a file still on disk couldn't be read.
 */
export async function openGitDiff(target: { subId: number; abs: string }, rel: string, ref: 'HEAD' | 'index' = 'HEAD'): Promise<void> {
  const ws = workspacesStore.activeWorkspace;
  const pane = workspacesStore.activePane;
  if (!ws || !pane) throw new Error('No active pane');
  const base = await commands.ftDiffBase(target.subId, rel, ref);
  let current = '';
  try {
    current = (await commands.readFile(target.abs)).content;
  } catch (e) {
    // Only a file that is gone diffs against empty. readFile refuses more than the base side
    // does (over 2 MB, or not UTF-8), and showing such a file as wholly deleted would be a
    // deletion that never happened.
    const exists = await commands.ftStat(target.subId, rel).then(() => true, () => false);
    if (exists) throw new Error(readFailure(String(e)));
  }
  await openReadOnlyDiff({
    filePath: target.abs,
    oldContent: base ?? '',
    newContent: current,
    tabName: gitDiffTabName(rel, ref),
    workspaceId: ws.id,
    paneId: pane.id,
    afterTabId: pane.active_tab_id,
  });
}
