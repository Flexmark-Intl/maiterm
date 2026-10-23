import { terminalsStore } from '$lib/stores/terminals.svelte';
import { canonicalEditorPath, getPtyInfo } from '$lib/tauri/commands';
import { error as logError } from '@tauri-apps/plugin-log';
import { openInEditor, sshCommandOfTab } from './openInEditor';

/**
 * Open a file from a terminal context.
 * Reuses an editor tab already showing the file; otherwise creates one at once —
 * EditorPane handles loading and errors.
 */
export async function openFileFromTerminal(
  workspaceId: string,
  paneId: string,
  tabId: string,
  filePath: string,
) {
  try {
    const instance = terminalsStore.get(tabId);
    if (!instance) return;

    // Get PTY info for SSH detection and local cwd
    const ptyInfo = await getPtyInfo(instance.ptyId);
    const sshCommand = await sshCommandOfTab(tabId, ptyInfo.foreground_command ?? null);
    const isRemote = !!sshCommand;

    // Resolve relative paths
    let resolvedPath = filePath;
    if (!filePath.startsWith('/') && !filePath.startsWith('~')) {
      if (isRemote) {
        const oscState = terminalsStore.getOsc(tabId);
        const remoteCwd = oscState?.cwd ?? oscState?.promptCwd;
        if (remoteCwd) {
          resolvedPath = remoteCwd.endsWith('/') ? remoteCwd + filePath : remoteCwd + '/' + filePath;
        }
      } else {
        if (ptyInfo.cwd) {
          resolvedPath = ptyInfo.cwd.endsWith('/') ? ptyInfo.cwd + filePath : ptyInfo.cwd + '/' + filePath;
        }
      }
    }
    // A click has no spelling worth keeping (`cwd/./src/a.ts`, `cwd/../x`, `~/notes`), so a new
    // tab gets the canonical one: folder resolved in Rust, file name as typed. A remote path
    // can't be resolved from here and stays as joined.
    if (!isRemote) {
      resolvedPath = await canonicalEditorPath(resolvedPath).catch(() => resolvedPath);
    }

    await openInEditor({
      abs: resolvedPath,
      remote: sshCommand ? { sshCommand } : null,
      workspaceId,
      paneId,
    });
  } catch (e) {
    logError(`Failed to open file: ${e}`);
  }
}
