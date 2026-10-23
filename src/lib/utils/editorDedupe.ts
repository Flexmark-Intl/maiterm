/**
 * Which open editor tab already shows a file — the pure half of `openInEditor`, kept free of
 * stores so Vitest can run it.
 *
 * A file is the same file when its path matches AND it lives on the same machine: a local
 * `/home/me/x` and the same path on an ssh host are two files, and so are one path on two
 * hosts. (The dedupe this replaces compared the path alone.)
 */
import type { EditorFileInfo, Workspace } from '$lib/tauri/types';

export interface EditorHit {
  workspaceId: string;
  paneId: string;
  tabId: string;
}

type FileKey = Pick<EditorFileInfo, 'file_path' | 'is_remote' | 'remote_ssh_command'>;

export function sameEditorFile(a: FileKey, b: FileKey): boolean {
  if (a.file_path !== b.file_path) return false;
  if (!!a.is_remote !== !!b.is_remote) return false;
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
): EditorHit | null {
  let inWorkspace: EditorHit | null = null;
  let anywhere: EditorHit | null = null;
  for (const ws of workspaces) {
    for (const pane of ws.panes) {
      for (const tab of pane.tabs) {
        if (tab.tab_type !== 'editor' || !tab.editor_file || !sameEditorFile(tab.editor_file, file)) continue;
        const hit = { workspaceId: ws.id, paneId: pane.id, tabId: tab.id };
        if (prefer.paneId && pane.id === prefer.paneId) return hit;
        if (prefer.workspaceId && ws.id === prefer.workspaceId) inWorkspace ??= hit;
        anywhere ??= hit;
      }
    }
  }
  return inWorkspace ?? anywhere;
}
