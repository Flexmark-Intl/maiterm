/**
 * Open a file in an editor tab, reusing the tab that already shows it (docs/file-tree.md §10.5).
 *
 * `createEditorTab` never dedupes, so every caller that opened files used to either repeat the
 * search (the agent's `openFile`) or skip it and stack a second tab on the same file (a click
 * on a path in the terminal). Both go through here now; Files will too.
 */
import type { EditorFileInfo } from '$lib/tauri/types';
import { workspacesStore } from '$lib/stores/workspaces.svelte';
import { terminalsStore } from '$lib/stores/terminals.svelte';
import { getPtyInfo, sshArgsOf } from '$lib/tauri/commands';
import { detectLanguageFromPath } from './languageDetect';
import { findOpenEditor } from './editorDedupe';

export interface OpenInEditorArgs {
  /** The file: a local absolute path, or the path on the ssh host when `remote` is set. */
  abs: string;
  /** The file lives on an ssh host; the command that reaches it (as a tab reports it). */
  remote?: { sshCommand: string } | null;
  /** Where a NEW tab goes. An existing tab is shown where it already is. */
  workspaceId: string;
  paneId: string;
  /** Insert a new tab after this one (default: after the pane's active tab). */
  afterTabId?: string;
}

export interface OpenInEditorResult {
  tabId: string;
  /** True when an editor tab already showed the file and was brought forward. */
  existing: boolean;
}

/**
 * The ssh command a terminal tab's files live behind, or null for a local tab.
 * Primary: the live foreground process (pass `foreground` when the caller already probed it).
 * Fallback: the tab's persisted ssh command, for when the process walk misses (background jobs
 * can confuse child ordering).
 */
export async function sshCommandOfTab(tabId: string, foreground?: string | null): Promise<string | null> {
  let live = foreground ?? null;
  if (foreground === undefined) {
    const instance = terminalsStore.get(tabId);
    if (instance) {
      live = await getPtyInfo(instance.ptyId).then((i) => i.foreground_command ?? null).catch(() => null);
    }
  }
  if (live) return live;
  const tab = workspacesStore._locateTab(tabId)?.tab;
  return tab?.restore_ssh_command ?? tab?.auto_resume_ssh_command ?? null;
}

/** The `EditorFileInfo` a tab for this file carries. */
export function editorFileInfo(abs: string, remote?: { sshCommand: string } | null): EditorFileInfo {
  return {
    file_path: abs,
    is_remote: !!remote,
    remote_ssh_command: remote ? sshArgsOf(remote.sshCommand) : null,
    remote_path: remote ? abs : null,
    language: detectLanguageFromPath(abs),
  };
}

export async function openInEditor(args: OpenInEditorArgs): Promise<OpenInEditorResult> {
  const fileInfo = editorFileInfo(args.abs, args.remote);
  const hit = findOpenEditor(workspacesStore.workspaces, fileInfo, {
    workspaceId: args.workspaceId,
    paneId: args.paneId,
  });
  if (hit) {
    await workspacesStore.setActiveTab(hit.workspaceId, hit.paneId, hit.tabId);
    return { tabId: hit.tabId, existing: true };
  }
  const name = args.abs.split('/').pop() || args.abs;
  // EditorPane shows its own loading state and errors, so the tab is created at once.
  const tab = await workspacesStore.createEditorTab(args.workspaceId, args.paneId, name, fileInfo, args.afterTabId);
  return { tabId: tab.id, existing: false };
}
