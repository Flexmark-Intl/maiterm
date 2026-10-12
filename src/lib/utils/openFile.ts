import type { EditorFileInfo } from '$lib/tauri/types';
import { detectLanguageFromPath } from './languageDetect';
import { terminalsStore } from '$lib/stores/terminals.svelte';
import { workspacesStore } from '$lib/stores/workspaces.svelte';
import { getPtyInfo, sshArgsOf } from '$lib/tauri/commands';
import { error as logError } from '@tauri-apps/plugin-log';

/**
 * Detect SSH command for a terminal tab.
 * Primary: process tree via getPtyInfo (detects live SSH sessions).
 * Fallback: tab's persisted restore_ssh_command (covers cases where
 * process tree walk fails, e.g. background jobs confuse child ordering).
 */
function detectSshCommand(ptyInfo: { foreground_command: string | null }, tabId: string): string | null {
  if (ptyInfo.foreground_command) return ptyInfo.foreground_command;

  // Fallback: check persisted SSH command on the tab
  for (const ws of workspacesStore.workspaces) {
    for (const pane of ws.panes) {
      const tab = pane.tabs.find(t => t.id === tabId);
      if (tab) return tab.restore_ssh_command ?? tab.auto_resume_ssh_command ?? null;
    }
  }
  return null;
}

/**
 * Open a file from a terminal context.
 * Creates the editor tab immediately — EditorPane handles loading and errors.
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
    const sshCommand = detectSshCommand(ptyInfo, tabId);
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

    const language = detectLanguageFromPath(resolvedPath);
    const fileName = resolvedPath.split('/').pop() ?? resolvedPath;

    const fileInfo: EditorFileInfo = {
      file_path: resolvedPath,
      is_remote: isRemote,
      remote_ssh_command: isRemote ? sshArgsOf(sshCommand!) : null,
      remote_path: isRemote ? resolvedPath : null,
      language,
    };

    // Create tab immediately — EditorPane shows loading state and handles errors
    await workspacesStore.createEditorTab(workspaceId, paneId, fileName, fileInfo);
  } catch (e) {
    logError(`Failed to open file: ${e}`);
  }
}
