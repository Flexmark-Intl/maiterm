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

/** Local path → what the file is on disk (`editorFileIdentities`), when Rust could say. */
export type FileIdentities = ReadonlyMap<string, string>;

/**
 * Two local files are the same when Rust gave both the same identity: that sees through a
 * symlinked folder, a symlink and a wrong-case name, whichever spelling each tab stored. Without
 * identities they compare in `pathKey` form, so a click's `/repo/./src/a.ts` (cwd + typed) still
 * matches `/repo/src/a.ts`, and on Windows `C:\repo/src` matches `c:\repo\src`.
 *
 * A remote path is always read as POSIX (`windows` describes this machine, not the ssh host), and
 * its `..` is never folded: nothing here can see the host's symlinks, and a lexical fold there
 * could take a different file for the one clicked.
 */
export function sameEditorFile(a: FileKey, b: FileKey, windows = false, identities?: FileIdentities): boolean {
  if (!!a.is_remote !== !!b.is_remote) return false;
  if (a.is_remote) {
    return (
      pathKey(a.file_path, false, false) === pathKey(b.file_path, false, false) &&
      (a.remote_ssh_command ?? '') === (b.remote_ssh_command ?? '')
    );
  }
  const ia = identities?.get(a.file_path);
  const ib = identities?.get(b.file_path);
  if (ia !== undefined && ib !== undefined) return ia === ib;
  return pathKey(a.file_path, windows) === pathKey(b.file_path, windows);
}

/** Every local editor tab's stored path in the window: what `findOpenEditor` needs identities for. */
export function localEditorPaths(workspaces: readonly Workspace[]): string[] {
  const out = new Set<string>();
  for (const ws of workspaces) {
    for (const pane of ws.panes) {
      for (const tab of pane.tabs) {
        if (tab.tab_type === 'editor' && tab.editor_file && !tab.editor_file.is_remote) out.add(tab.editor_file.file_path);
      }
    }
  }
  return [...out];
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
  identities?: FileIdentities,
): EditorHit | null {
  let inWorkspace: EditorHit | null = null;
  let anywhere: EditorHit | null = null;
  for (const ws of workspaces) {
    for (const pane of ws.panes) {
      for (const tab of pane.tabs) {
        if (tab.tab_type !== 'editor' || !tab.editor_file) continue;
        if (!sameEditorFile(tab.editor_file, file, windows, identities)) continue;
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
