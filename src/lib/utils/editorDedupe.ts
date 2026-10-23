/**
 * Which open editor tab already shows a file — the pure half of `openInEditor`, kept free of
 * stores so Vitest can run it.
 *
 * A file is the same file when its path matches AND it lives on the same machine: a local
 * `/home/me/x` and the same path on an ssh host are two files, and so are one path on two
 * hosts. (The dedupe this replaces compared the path alone.)
 */
import type { EditorFileInfo, Workspace } from '$lib/tauri/types';
import { pathKey } from './pathForm';

export interface EditorHit {
  workspaceId: string;
  paneId: string;
  tabId: string;
}

type FileKey = Pick<EditorFileInfo, 'file_path' | 'is_remote' | 'remote_ssh_command'>;

/**
 * Paths are compared in `pathKey` form, so a click's `/repo/./src/a.ts` (cwd + typed) matches
 * the agent's `/repo/src/a.ts`, and on Windows `C:\repo/src` matches `c:\repo\src`. A remote path is always
 * read as POSIX: `windows` describes this machine, not the ssh host.
 */
export function sameEditorFile(a: FileKey, b: FileKey, windows = false): boolean {
  if (!!a.is_remote !== !!b.is_remote) return false;
  const win = windows && !a.is_remote;
  if (pathKey(a.file_path, win) !== pathKey(b.file_path, win)) return false;
  return !a.is_remote || (a.remote_ssh_command ?? '') === (b.remote_ssh_command ?? '');
}

/**
 * The editor tab already showing `file`, across every workspace in the window. When more than
 * one does, the one nearest the caller wins: the pane it named, then its workspace, then
 * anywhere.
 */
export function findOpenEditor(
  workspaces: readonly Workspace[],
  file: FileKey,
  prefer: { workspaceId?: string | null; paneId?: string | null } = {},
  windows = false,
): EditorHit | null {
  let inWorkspace: EditorHit | null = null;
  let anywhere: EditorHit | null = null;
  for (const ws of workspaces) {
    for (const pane of ws.panes) {
      for (const tab of pane.tabs) {
        if (tab.tab_type !== 'editor' || !tab.editor_file || !sameEditorFile(tab.editor_file, file, windows)) continue;
        const hit = { workspaceId: ws.id, paneId: pane.id, tabId: tab.id };
        if (prefer.paneId && pane.id === prefer.paneId) return hit;
        if (prefer.workspaceId && ws.id === prefer.workspaceId) inWorkspace ??= hit;
        anywhere ??= hit;
      }
    }
  }
  return inWorkspace ?? anywhere;
}

/**
 * Whether bringing `hit` forward must switch the window to its workspace.
 *
 * Only when the request came from the workspace on screen and the hit is in another one: the
 * human clicked a path (or asked Quick Open) where they are looking, and a new tab would have
 * appeared there, so the existing tab has to be SHOWN — making it active in a hidden pane does
 * nothing visible. A request from a workspace that isn't on screen (an agent working in the
 * background) never pulls the window away from what the human is looking at.
 */
export function revealsWorkspace(hit: EditorHit, callerWorkspaceId: string, activeWorkspaceId: string | null): boolean {
  return hit.workspaceId !== activeWorkspaceId && callerWorkspaceId === activeWorkspaceId;
}
