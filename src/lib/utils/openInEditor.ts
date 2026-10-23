/**
 * Open a file in an editor tab, reusing the tab that already shows it (docs/file-tree.md §10.5).
 *
 * `createEditorTab` never dedupes, so every caller that opened files used to either repeat the
 * search (the agent's `openFile`) or skip it and stack a second tab on the same file (a click
 * on a path in the terminal). Both go through here now; Files will too.
 */
import type { EditorFileInfo } from '$lib/tauri/types';
import { workspacesStore, navigateToTab } from '$lib/stores/workspaces.svelte';
import { terminalsStore } from '$lib/stores/terminals.svelte';
import { editorFileIdentities, getPtyInfo, sshArgsOf } from '$lib/tauri/commands';
import { detectLanguageFromPath } from './languageDetect';
import { findOpenEditor, localEditorPaths, revealsWorkspace } from './editorDedupe';
import { basenameOf } from './pathForm';
import { isWindows } from './platform';

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

/**
 * What each local path is on disk, keyed by the path, for `sameEditorFile`. Undefined when Rust
 * can't say, which leaves the comparison to its lexical fallback. A path missing from the map is
 * one whose stat didn't answer in time (Rust waits about a second, so one tab on a dead network
 * share can't hold up every open); that tab compares by spelling.
 */
export async function localFileIdentities(paths: string[]): Promise<Map<string, string> | undefined> {
  const unique = [...new Set(paths)];
  if (unique.length === 0) return new Map();
  const ids = await editorFileIdentities(unique).catch(() => null);
  if (!ids || ids.length !== unique.length) return undefined;
  const out = new Map<string, string>();
  unique.forEach((p, i) => {
    const id = ids[i];
    if (id != null) out.set(p, id);
  });
  return out;
}

export async function openInEditor(args: OpenInEditorArgs): Promise<OpenInEditorResult> {
  // The tab keeps the caller's spelling: the agent's later saveDocument/checkDocumentDirty and
  // getOpenEditors speak in it. Whether an open tab already shows the file is decided by what
  // the file IS on disk (Rust), not by spelling, because open tabs carry whatever spelling they
  // were opened with — the agent's /tmp/…, a Cmd+O pick, one saved by an older build — and a
  // click's /private/tmp/… or wrong-case name is the same file.
  const abs = args.abs;
  const fileInfo = editorFileInfo(abs, args.remote);
  const identities = args.remote
    ? undefined
    : await localFileIdentities([abs, ...localEditorPaths(workspacesStore.workspaces)]);
  const hit = findOpenEditor(
    workspacesStore.workspaces,
    fileInfo,
    { workspaceId: args.workspaceId, paneId: args.paneId },
    isWindows(),
    identities,
  );
  if (hit) {
    if (revealsWorkspace(hit, args.workspaceId, workspacesStore.activeWorkspaceId)) {
      await navigateToTab(hit.tabId);
    } else {
      await workspacesStore.setActiveTab(hit.workspaceId, hit.paneId, hit.tabId);
    }
    return { tabId: hit.tabId, existing: true };
  }
  const name = args.remote ? abs.split('/').pop() || abs : basenameOf(abs, isWindows());
  // EditorPane shows its own loading state and errors, so the tab is created at once.
  const tab = await workspacesStore.createEditorTab(args.workspaceId, args.paneId, name, fileInfo, args.afterTabId);
  return { tabId: tab.id, existing: false };
}
