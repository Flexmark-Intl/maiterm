import { invoke } from '@tauri-apps/api/core';
import type { AgentBridge, AppData, DiffContext, DuplicateWorkspaceResult, EditorFileInfo, MailinkDevice, MailinkPairingPayload, MeshTopic, Pane, Preferences, ScrollInfo, SearchResult, ShellInfo, SplitDirection, Tab, TerminalFrame, WindowData, Workspace, WorkspaceNote } from './types';

// Terminal commands
export async function spawnTerminal(ptyId: string, tabId: string, cols: number, rows: number, cwd?: string | null): Promise<void> {
  return invoke('spawn_terminal', { ptyId, tabId, cols, rows, cwd: cwd ?? null });
}

export interface PtyInfo {
  cwd: string | null;
  foreground_command: string | null;
}

/**
 * Strip previously-injected flags and remote commands from an SSH command
 * retrieved from the process tree, then normalize to just the user@host
 * portion (with any non-standard flags). Strips `ssh` prefix, `-t`,
 * `-o ControlMaster=...`, and `cd ... && exec $SHELL -l` suffixes.
 */
export function cleanSshCommand(cmd: string): string {
  if (!cmd.match(/^ssh\s/)) return cmd;
  // Remove our injected remote command (unquoted form from ps output)
  let cleaned = cmd.replace(/\s+cd\s+.*?&&\s+exec\s+\$?SHELL\s+-l\s*$/, '');
  // Also handle the single-quoted form
  cleaned = cleaned.replace(/\s+'cd\s+.*?&&\s+exec\s+\$?SHELL\s+-l'\s*$/, '');
  // Remove only flags that buildSshCommand re-injects
  cleaned = cleaned.replace(/\s+-t(?=\s|$)/g, '');
  cleaned = cleaned.replace(/\s+-o\s+ControlMaster=\S+/g, '');
  // Remove any bare ControlMaster=... leftover (malformed from previous cycles)
  cleaned = cleaned.replace(/\s+ControlMaster=\S+/g, '');
  // Strip the `ssh` prefix — we store just user@host with any remaining flags
  cleaned = cleaned.replace(/^ssh\s+/, '');
  // Deduplicate single-letter flags (e.g. -x -C -x -C → -x -C)
  const parts = cleaned.split(/\s+/);
  const seen = new Set<string>();
  const deduped: string[] = [];
  for (const part of parts) {
    if (/^-[a-zA-Z]$/.test(part)) {
      if (seen.has(part)) continue;
      seen.add(part);
    }
    deduped.push(part);
  }
  return deduped.join(' ');
}

/**
 * Normalize SSH input from the user: accept either "ssh user@host ..."
 * or just "user@host ...", strip standard flags we re-inject (-t, -o ControlMaster),
 * and return just the user@host portion with any non-standard flags.
 */
export function shellEscapePath(path: string): string {
  if (path === '~') return '~';
  if (path.startsWith('~/')) {
    const rest = path.slice(2).replace(/'/g, "'\\''");
    return `~/'${rest}'`;
  }
  const escaped = path.replace(/'/g, "'\\''");
  return `'${escaped}'`;
}

/**
 * Build the SSH command for split cloning / auto-resume.
 * Stored SSH values are bare "user@host" (possibly with flags).
 * Reconstructs full "ssh -t -o ControlMaster=no user@host" and
 * appends 'cd <path> && exec $SHELL -l' if remoteCwd is given.
 */
export function buildSshCommand(sshCmd: string | null, remoteCwd: string | null): string {
  if (!sshCmd) return '';
  const fullCmd = sshCmd.match(/^ssh\s/) ? sshCmd : `ssh ${sshCmd}`;
  if (!remoteCwd) {
    return fullCmd.replace(/^ssh\s+/, 'ssh -o ControlMaster=no ');
  }
  const cdPath = shellEscapePath(remoteCwd);
  const rest = fullCmd.replace(/^ssh\s+/, '');
  return `ssh -t -o ControlMaster=no ${rest} 'cd ${cdPath} && exec $SHELL -l'`;
}

export function normalizeSshInput(input: string): string {
  const trimmed = input.trim();
  if (!trimmed) return '';
  // If it starts with "ssh ", run through cleanSshCommand which handles full commands
  if (trimmed.match(/^ssh\s/)) return cleanSshCommand(trimmed);
  // Already bare user@host (possibly with flags) — just return as-is
  return trimmed;
}

export async function getPtyInfo(ptyId: string): Promise<PtyInfo> {
  const info: PtyInfo = await invoke('get_pty_info', { ptyId });
  if (info.foreground_command) {
    info.foreground_command = cleanSshCommand(info.foreground_command);
  }
  return info;
}

export async function writeTerminal(ptyId: string, data: number[]): Promise<void> {
  return invoke('write_terminal', { ptyId, data });
}

export async function resizeTerminal(ptyId: string, cols: number, rows: number): Promise<void> {
  return invoke('resize_terminal', { ptyId, cols, rows });
}

export async function killTerminal(ptyId: string): Promise<void> {
  return invoke('kill_terminal', { ptyId });
}

/** PTY IDs still alive in the backend registry. Empty after a full app restart;
 *  populated after a window reload (used to reattach instead of respawning). */
export async function listLivePtys(): Promise<string[]> {
  return invoke('list_live_ptys');
}

export async function readClipboardFilePaths(): Promise<string[]> {
  return invoke('read_clipboard_file_paths');
}

export async function detectWindowsShells(): Promise<ShellInfo[]> {
  return invoke('detect_windows_shells');
}

// Terminal backend commands (alacritty_terminal)
export async function scrollTerminal(ptyId: string, delta: number): Promise<TerminalFrame> {
  return invoke('scroll_terminal', { ptyId, delta });
}

export async function scrollTerminalTo(ptyId: string, offset: number): Promise<TerminalFrame> {
  return invoke('scroll_terminal_to', { ptyId, offset });
}

export async function getTerminalScrollbackInfo(ptyId: string): Promise<ScrollInfo> {
  return invoke('get_terminal_scrollback_info', { ptyId });
}

export async function searchTerminal(ptyId: string, query: string, caseSensitive: boolean): Promise<SearchResult> {
  return invoke('search_terminal', { ptyId, query, caseSensitive });
}

export async function terminalBracketedPaste(ptyId: string): Promise<boolean> {
  return invoke('terminal_bracketed_paste', { ptyId });
}

/** Liveness signals for the mesh readiness check. `agent_running` = a claude/codex/gemini
 *  process is alive in the tab's LOCAL process tree (ground truth for local agents).
 *  `ssh_foreground` = the tty's foreground job is an ssh session (a live remote session —
 *  the remote agent lives past the hop, so this stands in for it). Either being true means
 *  the tab needs only `/maiterm init`, not a full ssh+resume replay. */
export interface AgentLiveness {
  agent_running: boolean;
  ssh_foreground: boolean;
}
export async function getAgentLiveness(ptyId: string): Promise<AgentLiveness> {
  return invoke('get_agent_liveness', { ptyId });
}

export async function serializeTerminal(ptyId: string): Promise<number[]> {
  return invoke('serialize_terminal', { ptyId });
}

export async function restoreTerminalScrollback(ptyId: string, scrollback: number[]): Promise<void> {
  return invoke('restore_terminal_scrollback', { ptyId, scrollback });
}

export async function resizeTerminalGrid(ptyId: string, cols: number, rows: number): Promise<void> {
  return invoke('resize_terminal_grid', { ptyId, cols, rows });
}

export async function clearTerminalScrollback(ptyId: string): Promise<void> {
  return invoke('clear_terminal_scrollback', { ptyId });
}

export async function getTerminalSelectionText(ptyId: string, startX: number, startY: number, endX: number, endY: number): Promise<string> {
  return invoke('get_terminal_selection_text', { ptyId, startX, startY, endX, endY });
}

export async function getTerminalRecentText(ptyId: string, lineCount: number): Promise<string> {
  return invoke('get_terminal_recent_text', { ptyId, lineCount });
}

export async function startSelection(ptyId: string, col: number, row: number, side: string, selectionType: string): Promise<TerminalFrame> {
  return invoke('start_selection', { ptyId, col, row, side, selectionType });
}

export async function updateSelection(ptyId: string, col: number, row: number, side: string): Promise<TerminalFrame> {
  return invoke('update_selection', { ptyId, col, row, side });
}

export async function clearSelection(ptyId: string): Promise<TerminalFrame> {
  return invoke('clear_selection', { ptyId });
}

export async function copySelection(ptyId: string): Promise<string | null> {
  return invoke('copy_selection', { ptyId });
}

export async function selectAll(ptyId: string): Promise<TerminalFrame> {
  return invoke('select_all', { ptyId });
}

export async function scrollSelection(ptyId: string, delta: number, col: number): Promise<TerminalFrame> {
  return invoke('scroll_selection', { ptyId, delta, col });
}

export async function saveTerminalScrollback(ptyId: string, tabId: string): Promise<void> {
  return invoke('save_terminal_scrollback', { ptyId, tabId });
}

export async function restoreTerminalFromSaved(ptyId: string, tabId: string): Promise<void> {
  return invoke('restore_terminal_from_saved', { ptyId, tabId });
}

export async function hasSavedScrollback(tabId: string): Promise<boolean> {
  return invoke('has_saved_scrollback', { tabId });
}

export async function getSavedScrollbackText(tabId: string, lineCount: number): Promise<string | null> {
  return invoke('get_saved_scrollback_text', { tabId, lineCount });
}

export async function getSavedTerminalSize(tabId: string): Promise<[number, number] | null> {
  return invoke('get_saved_terminal_size', { tabId });
}

/** Bulk-mark terminal tabs as properly suspended (clears stale pty_id, stamps
 *  suspended_at). `suspendedAt` is the last-active time for the idle age; pass
 *  null to use now. Used by "suspend other tabs" so it can't leave a stale
 *  pty_id (which would relapse the live-tab high-watermark). */
export async function markTabsSuspended(updates: { tab_id: string; suspended_at: string | null }[]): Promise<void> {
  return invoke('mark_tabs_suspended', { updates });
}

// Workspace commands
export async function getAppData(): Promise<AppData> {
  return invoke('get_app_data');
}

export async function createWorkspace(name: string): Promise<Workspace> {
  return invoke('create_workspace', { name });
}

export async function deleteWorkspace(workspaceId: string): Promise<void> {
  return invoke('delete_workspace', { workspaceId });
}

export async function renameWorkspace(workspaceId: string, name: string): Promise<void> {
  return invoke('rename_workspace', { workspaceId, name });
}

export async function splitPane(workspaceId: string, targetPaneId: string, direction: SplitDirection, scrollback?: string | null, editorFile?: EditorFileInfo | null): Promise<Pane> {
  return invoke('split_pane', { workspaceId, targetPaneId, direction, scrollback: scrollback ?? null, editorFile: editorFile ?? null });
}

export async function deletePane(workspaceId: string, paneId: string): Promise<void> {
  return invoke('delete_pane', { workspaceId, paneId });
}

export async function renamePane(workspaceId: string, paneId: string, name: string): Promise<void> {
  return invoke('rename_pane', { workspaceId, paneId, name });
}

export async function createTab(workspaceId: string, paneId: string, name: string, afterTabId?: string): Promise<Tab> {
  return invoke('create_tab', { workspaceId, paneId, name, afterTabId });
}

export async function deleteTab(workspaceId: string, paneId: string, tabId: string): Promise<void> {
  return invoke('delete_tab', { workspaceId, paneId, tabId });
}

export async function moveTabToWorkspaceCmd(sourceWorkspaceId: string, sourcePaneId: string, tabId: string, targetWorkspaceId: string): Promise<void> {
  return invoke('move_tab_to_workspace', { sourceWorkspaceId, sourcePaneId, tabId, targetWorkspaceId });
}

export async function moveTabToPaneCmd(workspaceId: string, sourcePaneId: string, tabId: string, targetPaneId: string, insertBeforeTabId?: string | null): Promise<void> {
  return invoke('move_tab_to_pane', { workspaceId, sourcePaneId, tabId, targetPaneId, insertBeforeTabId: insertBeforeTabId ?? null });
}

export async function moveTabToSplitCmd(workspaceId: string, sourcePaneId: string, tabId: string, targetPaneId: string, direction: SplitDirection, before?: boolean): Promise<Pane> {
  return invoke('move_tab_to_split', { workspaceId, sourcePaneId, tabId, targetPaneId, direction, before: before ?? false });
}

export async function renameTab(workspaceId: string, paneId: string, tabId: string, name: string, customName?: boolean): Promise<void> {
  return invoke('rename_tab', { workspaceId, paneId, tabId, name, customName: customName ?? null });
}

export async function updateEditorTabFile(tabId: string, name: string, fileInfo: EditorFileInfo): Promise<void> {
  return invoke('update_editor_tab_file', { tabId, name, fileInfo });
}

export async function setActiveWorkspace(workspaceId: string): Promise<void> {
  return invoke('set_active_workspace', { workspaceId });
}

export async function suspendWorkspace(workspaceId: string): Promise<void> {
  return invoke('suspend_workspace', { workspaceId });
}

export async function resumeWorkspace(workspaceId: string): Promise<void> {
  return invoke('resume_workspace', { workspaceId });
}

export async function setActivePane(workspaceId: string, paneId: string): Promise<void> {
  return invoke('set_active_pane', { workspaceId, paneId });
}

export async function setActiveTab(workspaceId: string, paneId: string, tabId: string): Promise<void> {
  return invoke('set_active_tab', { workspaceId, paneId, tabId });
}

export async function setTabPtyId(workspaceId: string, paneId: string, tabId: string, ptyId: string): Promise<void> {
  return invoke('set_tab_pty_id', { workspaceId, paneId, tabId, ptyId });
}

export async function suspendTab(workspaceId: string, paneId: string, tabId: string, cwd: string | null, sshCommand: string | null, remoteCwd: string | null): Promise<void> {
  return invoke('suspend_tab', { workspaceId, paneId, tabId, cwd, sshCommand, remoteCwd });
}

export async function setTabPinned(workspaceId: string, paneId: string, tabId: string, pinned: boolean): Promise<void> {
  return invoke('set_tab_pinned', { workspaceId, paneId, tabId, pinned });
}

export async function setSidebarWidth(width: number): Promise<void> {
  return invoke('set_sidebar_width', { width });
}

export async function setSidebarCollapsed(collapsed: boolean): Promise<void> {
  return invoke('set_sidebar_collapsed', { collapsed });
}

export async function setSplitRatio(workspaceId: string, splitId: string, ratio: number): Promise<void> {
  return invoke('set_split_ratio', { workspaceId, splitId, ratio });
}

export async function setTabScrollback(tabId: string, scrollback: string | null): Promise<void> {
  return invoke('set_tab_scrollback', { tabId, scrollback });
}

export async function setTabNotes(workspaceId: string, paneId: string, tabId: string, notes: string | null): Promise<void> {
  return invoke('set_tab_notes', { workspaceId, paneId, tabId, notes });
}

export async function setTabNotesOpen(workspaceId: string, paneId: string, tabId: string, open: boolean): Promise<void> {
  return invoke('set_tab_notes_open', { workspaceId, paneId, tabId, open });
}

export async function setTabNotesMode(workspaceId: string, paneId: string, tabId: string, notesMode: string | null): Promise<void> {
  return invoke('set_tab_notes_mode', { workspaceId, paneId, tabId, notesMode });
}

export async function setTabComposerOpen(workspaceId: string, paneId: string, tabId: string, open: boolean | null): Promise<void> {
  return invoke('set_tab_composer_open', { workspaceId, paneId, tabId, open });
}

export async function setTabComposerDraft(workspaceId: string, paneId: string, tabId: string, draft: string | null): Promise<void> {
  return invoke('set_tab_composer_draft', { workspaceId, paneId, tabId, draft });
}

export async function setTabMeshPurpose(workspaceId: string, paneId: string, tabId: string, purpose: string | null): Promise<void> {
  return invoke('set_tab_mesh_purpose', { workspaceId, paneId, tabId, purpose });
}

export async function reorderTabs(workspaceId: string, paneId: string, tabIds: string[]): Promise<void> {
  return invoke('reorder_tabs', { workspaceId, paneId, tabIds });
}

export async function reorderWorkspaces(workspaceIds: string[]): Promise<void> {
  return invoke('reorder_workspaces', { workspaceIds });
}

export async function duplicateWorkspaceCmd(
  workspaceId: string,
  position: number,
  tabContexts: TabContext[],
): Promise<DuplicateWorkspaceResult> {
  return invoke('duplicate_workspace', { workspaceId, position, tabContexts });
}

export async function getPreferences(): Promise<Preferences> {
  return invoke('get_preferences');
}

export async function setPreferences(preferences: Preferences): Promise<void> {
  return invoke('set_preferences', { preferences });
}

/** Re-apply on-disk integration for non-Claude runtimes (install/unregister) after a
 *  preference toggle, using the live MCP port/auth — so enabling Codex configures
 *  ~/.codex immediately without a restart. */
export async function refreshAgentIntegrations(): Promise<void> {
  return invoke('refresh_agent_integrations');
}

export async function copyTabHistory(sourceTabId: string, destTabId: string): Promise<void> {
  return invoke('copy_tab_history', { sourceTabId, destTabId });
}

export async function setTabLastCwd(
  workspaceId: string,
  paneId: string,
  tabId: string,
  cwd: string | null,
): Promise<void> {
  return invoke('set_tab_last_cwd', { workspaceId, paneId, tabId, cwd });
}

export async function setTabRestoreContext(
  workspaceId: string,
  paneId: string,
  tabId: string,
  cwd: string | null,
  sshCommand: string | null,
  remoteCwd: string | null,
): Promise<void> {
  return invoke('set_tab_restore_context', { workspaceId, paneId, tabId, cwd, sshCommand, remoteCwd });
}

export async function setTabTriggerVariables(
  workspaceId: string,
  paneId: string,
  tabId: string,
  vars: Record<string, string>,
): Promise<void> {
  return invoke('set_tab_trigger_variables', { workspaceId, paneId, tabId, vars });
}

export async function getAllWorkspaces(): Promise<[string, string][]> {
  return invoke('get_all_workspaces');
}

export async function getAllTabs(): Promise<[string, string, string, string, boolean][]> {
  return invoke('get_all_tabs');
}

export async function setTabAutoResumeContext(
  workspaceId: string,
  paneId: string,
  tabId: string,
  cwd: string | null,
  sshCommand: string | null,
  remoteCwd: string | null,
  command: string | null,
  pinned?: boolean,
): Promise<void> {
  return invoke('set_tab_auto_resume_context', { workspaceId, paneId, tabId, cwd, sshCommand, remoteCwd, command, pinned: pinned ?? null });
}

export async function setTabAutoResumeEnabled(
  workspaceId: string,
  paneId: string,
  tabId: string,
  enabled: boolean,
): Promise<void> {
  return invoke('set_tab_auto_resume_enabled', { workspaceId, paneId, tabId, enabled });
}

export async function setTabAgentBridge(
  workspaceId: string,
  paneId: string,
  tabId: string,
  bridge: AgentBridge | null,
): Promise<void> {
  return invoke('set_tab_agent_bridge', { workspaceId, paneId, tabId, bridge });
}

// Workspace note commands
export async function addWorkspaceNote(workspaceId: string, content: string, mode: string | null): Promise<WorkspaceNote> {
  return invoke('add_workspace_note', { workspaceId, content, mode });
}

export async function updateWorkspaceNote(workspaceId: string, noteId: string, content: string, mode: string | null): Promise<void> {
  return invoke('update_workspace_note', { workspaceId, noteId, content, mode });
}

export async function deleteWorkspaceNote(workspaceId: string, noteId: string): Promise<void> {
  return invoke('delete_workspace_note', { workspaceId, noteId });
}

// Mesh workspace commands (docs/mesh-workspace.md)
export async function setWorkspaceBridgeAll(workspaceId: string, enabled: boolean): Promise<void> {
  return invoke('set_workspace_bridge_all', { workspaceId, enabled });
}

// maiLink companion commands (docs/mailink-protocol.md)
export async function setTabMailinkNative(workspaceId: string, paneId: string, tabId: string, mailinkNative: boolean): Promise<void> {
  return invoke('set_tab_mailink_native', { workspaceId, paneId, tabId, mailinkNative });
}

export async function setTabMailinkExcluded(workspaceId: string, paneId: string, tabId: string, excluded: boolean): Promise<void> {
  return invoke('set_tab_mailink_excluded', { workspaceId, paneId, tabId, excluded });
}

export async function setWorkspaceMailinkNative(workspaceId: string, enabled: boolean): Promise<void> {
  return invoke('set_workspace_mailink_native', { workspaceId, enabled });
}

/** Persist the maiLink bridge enable flag AND start/stop the live listener (no restart needed). */
export async function mailinkSetEnabled(enabled: boolean): Promise<void> {
  return invoke('mailink_set_enabled', { enabled });
}

/** Mint a one-time pairing code; returns the QR payload the phone scans (120s TTL, single use). */
export async function mailinkCreatePairing(): Promise<MailinkPairingPayload> {
  return invoke('mailink_create_pairing');
}

/** List paired maiLink devices (sanitized — no token hash / relay capability). */
export async function mailinkListDevices(): Promise<MailinkDevice[]> {
  return invoke('mailink_list_devices');
}

/** Unpair a device: its bearer token stops working and the doorbell stops ringing it. */
export async function mailinkRemoveDevice(deviceId: string): Promise<void> {
  return invoke('mailink_remove_device', { deviceId });
}

export async function setWorkspaceMeshTopics(workspaceId: string, topics: MeshTopic[]): Promise<void> {
  return invoke('set_workspace_mesh_topics', { workspaceId, topics });
}

// Sound commands
export async function listSystemSounds(): Promise<string[]> {
  return invoke('list_system_sounds');
}

export async function playSystemSound(name: string, volume: number): Promise<void> {
  return invoke('play_system_sound', { name, volume });
}

export async function playBellSound(): Promise<void> {
  return invoke('play_bell_sound');
}

// Window commands
export async function getWindowData(): Promise<WindowData> {
  return invoke('get_window_data');
}

export async function createNewWindow(): Promise<string> {
  return invoke('create_window');
}

export interface TabContext {
  tab_id: string;
  scrollback: string | null;
  cwd: string | null;
  ssh_command: string | null;
  remote_cwd: string | null;
}

export async function duplicateWindow(tabContexts: TabContext[]): Promise<string> {
  return invoke('duplicate_window', { tabContexts });
}

export async function closeWindow(): Promise<void> {
  return invoke('close_window');
}

export async function saveWindowGeometry(monitorCount: number): Promise<void> {
  return invoke('save_window_geometry', { monitorCount });
}

export async function getMonitorCount(): Promise<number> {
  return invoke('get_monitor_count');
}

export async function restoreWindowGeometry(monitorCount: number): Promise<boolean> {
  return invoke('restore_window_geometry', { monitorCount });
}

export async function resetWindow(): Promise<void> {
  return invoke('reset_window');
}

export async function getWindowCount(): Promise<number> {
  return invoke('get_window_count');
}

export async function openPreferencesWindow(): Promise<void> {
  return invoke('open_preferences_window');
}

export async function openHelpWindow(section?: string): Promise<void> {
  return invoke('open_help_window', { section: section ?? null });
}

// Editor commands
export interface ReadFileResult {
  content: string;
  size: number;
}

export async function readFile(path: string): Promise<ReadFileResult> {
  return invoke('read_file', { path });
}

export interface ReadFileBase64Result {
  data: string;
  size: number;
}

export async function gitShowFile(filePath: string, gitRef: string): Promise<string> {
  return invoke('git_show_file', { filePath, gitRef });
}

export async function readFileBase64(path: string): Promise<ReadFileBase64Result> {
  return invoke('read_file_base64', { path });
}

export async function scpReadFileBase64(sshCommand: string, remotePath: string): Promise<ReadFileBase64Result> {
  return invoke('scp_read_file_base64', { sshCommand, remotePath });
}

export async function writeFile(path: string, content: string): Promise<void> {
  return invoke('write_file', { path, content });
}

export async function scpReadFile(sshCommand: string, remotePath: string): Promise<ReadFileResult> {
  return invoke('scp_read_file', { sshCommand, remotePath });
}

export async function scpWriteFile(sshCommand: string, remotePath: string, content: string): Promise<void> {
  return invoke('scp_write_file', { sshCommand, remotePath, content });
}

export async function saveClipboardImage(dataBase64: string, ext?: string): Promise<string> {
  return invoke('save_clipboard_image', { dataBase64, ext });
}

/** Reveal a local file in the OS file manager (Finder/Explorer/file browser). */
export async function revealInFileManager(path: string): Promise<void> {
  return invoke('reveal_in_file_manager', { path });
}

/** SCP a remote file into the local Downloads directory; returns the saved path. */
export async function downloadRemoteFile(sshCommand: string, remotePath: string): Promise<string> {
  return invoke('download_remote_file', { sshCommand, remotePath });
}

export async function scpUploadFiles(sshCommand: string, localPaths: string[], remoteDir: string, uploadId: string): Promise<void> {
  return invoke('scp_upload_files', { sshCommand, localPaths, remoteDir, uploadId });
}

export async function cancelScpUpload(uploadId: string): Promise<void> {
  return invoke('cancel_scp_upload', { uploadId });
}

export async function isDirectory(path: string): Promise<boolean> {
  return invoke('is_directory', { path });
}

export async function sshIsDirectory(sshCommand: string, remotePath: string): Promise<boolean> {
  return invoke('ssh_is_directory', { sshCommand, remotePath });
}

export async function listFiles(path: string, maxFiles?: number, showHidden?: boolean, showIgnored?: boolean): Promise<string[]> {
  return invoke('list_files', { path, maxFiles: maxFiles ?? null, showHidden: showHidden ?? null, showIgnored: showIgnored ?? null });
}

export async function sshListFiles(sshCommand: string, remotePath: string, maxFiles?: number, showHidden?: boolean, showIgnored?: boolean): Promise<string[]> {
  return invoke('ssh_list_files', { sshCommand, remotePath, maxFiles: maxFiles ?? null, showHidden: showHidden ?? null, showIgnored: showIgnored ?? null });
}

export async function createEditorTab(workspaceId: string, paneId: string, name: string, fileInfo: EditorFileInfo, afterTabId?: string): Promise<Tab> {
  return invoke('create_editor_tab', { workspaceId, paneId, name, fileInfo, afterTabId: afterTabId ?? null });
}

export async function watchFile(tabId: string, path: string): Promise<void> {
  return invoke('watch_file', { tabId, path });
}

export async function unwatchFile(tabId: string): Promise<void> {
  return invoke('unwatch_file', { tabId });
}

export async function getFileMtime(path: string): Promise<number> {
  return invoke('get_file_mtime', { path });
}

export async function watchRemoteFile(tabId: string, sshCommand: string, remotePath: string): Promise<void> {
  return invoke('watch_remote_file', { tabId, sshCommand, remotePath });
}

export async function unwatchRemoteFile(tabId: string): Promise<void> {
  return invoke('unwatch_remote_file', { tabId });
}

export async function getRemoteFileMtime(sshCommand: string, remotePath: string): Promise<number> {
  return invoke('get_remote_file_mtime', { sshCommand, remotePath });
}

// Claude Code IDE integration commands
export async function claudeCodeRespond(requestId: string, result: unknown): Promise<void> {
  return invoke('claude_code_respond', { requestId, result });
}

export async function claudeCodeNotifySelection(payload: unknown): Promise<void> {
  return invoke('claude_code_notify_selection', { payload });
}

export async function createDiffTab(
  workspaceId: string,
  paneId: string,
  name: string,
  diffContext: DiffContext,
  afterTabId?: string | null,
): Promise<Tab> {
  return invoke('create_diff_tab', { workspaceId, paneId, name, diffContext, afterTabId: afterTabId ?? null });
}

// Archive tab commands
export async function archiveTab(
  workspaceId: string,
  paneId: string,
  tabId: string,
  displayName: string,
  scrollback: string | null,
  cwd: string | null,
  sshCommand: string | null,
  remoteCwd: string | null,
): Promise<void> {
  return invoke('archive_tab', { workspaceId, paneId, tabId, displayName, scrollback, cwd, sshCommand, remoteCwd });
}

export async function restoreArchivedTab(
  workspaceId: string,
  paneId: string,
  tabId: string,
): Promise<Tab> {
  return invoke('restore_archived_tab', { workspaceId, paneId, tabId });
}

export async function deleteArchivedTab(
  workspaceId: string,
  tabId: string,
): Promise<void> {
  return invoke('delete_archived_tab', { workspaceId, tabId });
}

/** Generate default backup filename: aiterm_backup_YYYYMMDD_HHMM.json.gz */
export function backupFilename(): string {
  const now = new Date();
  const pad = (n: number) => String(n).padStart(2, '0');
  const stamp = `${now.getFullYear()}${pad(now.getMonth() + 1)}${pad(now.getDate())}_${pad(now.getHours())}${pad(now.getMinutes())}`;
  return `aiterm_backup_${stamp}.json.gz`;
}

// State backup commands
export async function exportState(path: string, excludeScrollback: boolean = false): Promise<void> {
  return invoke('export_state', { path, excludeScrollback });
}

export async function importState(path: string): Promise<void> {
  return invoke('import_state', { path });
}

export interface ImportPreviewTab {
  id: string;
  name: string;
  tab_type: string;
  has_scrollback: boolean;
  has_notes: boolean;
  has_auto_resume: boolean;
  editor_file_path: string | null;
}

export interface ImportPreviewWorkspace {
  id: string;
  name: string;
  tab_count: number;
  tabs: ImportPreviewTab[];
  note_count: number;
  archived_count: number;
}

export interface ImportPreviewWindow {
  label: string;
  workspaces: ImportPreviewWorkspace[];
}

export interface ImportPreview {
  windows: ImportPreviewWindow[];
  file_size: number;
  has_preferences: boolean;
}

export interface ImportConfig {
  mode: 'overwrite' | 'merge';
  selected_workspace_ids: string[];
  import_preferences: boolean;
}

export async function previewImport(path: string): Promise<ImportPreview> {
  return invoke('preview_import', { path });
}

export async function importStateSelective(path: string, config: ImportConfig): Promise<void> {
  return invoke('import_state_selective', { path, config });
}

export async function runScheduledBackup(): Promise<string> {
  return invoke('run_scheduled_backup');
}

export async function trimOldBackups(): Promise<number> {
  return invoke('trim_old_backups');
}

export async function pickBackupDirectory(): Promise<string | null> {
  return invoke('pick_backup_directory');
}

export async function getAppDiagnostics(): Promise<Record<string, unknown>> {
  return invoke('get_app_diagnostics');
}

export async function readAppLogs(opts?: { lines?: number; level?: string; search?: string }): Promise<{ path: string; total_matching: number; lines: string[]; truncated: boolean }> {
  return invoke('read_app_logs', { lines: opts?.lines ?? null, level: opts?.level ?? null, search: opts?.search ?? null });
}

// SSH MCP tunnel commands
export interface SshTunnelInfo {
  tunnel_id: string;
  remote_port: number;
  host_key: string;
}

export interface MaitermSkillScripts {
  skill_md: string;
  setup_statusline: string;
  statusline_command: string;
}

export async function startSshTunnel(sshArgs: string, hostKey: string, tabId: string, localPort: number): Promise<SshTunnelInfo> {
  return invoke('start_ssh_tunnel', { sshArgs, hostKey, tabId, localPort });
}

export async function detachSshTunnel(hostKey: string, tabId: string): Promise<void> {
  return invoke('detach_ssh_tunnel', { hostKey, tabId });
}

export async function getSshTunnel(hostKey: string): Promise<SshTunnelInfo | null> {
  return invoke('get_ssh_tunnel', { hostKey });
}

export async function getMcpPort(): Promise<number | null> {
  return invoke('get_mcp_port');
}

export async function getMcpAuth(): Promise<string | null> {
  return invoke('get_mcp_auth');
}

export async function sshRunSetup(sshArgs: string, setupScript: string): Promise<void> {
  return invoke('ssh_run_setup', { sshArgs, setupScript });
}

export async function getMaitermSkillScripts(): Promise<MaitermSkillScripts> {
  return invoke('get_maiterm_skill_scripts');
}

/** Render the remote-Codex setup shell script (config.toml + hooks.json + shim + prompt),
 *  pointed at the SSH reverse-tunnel port. Run it via sshRunSetup. No-ops on hosts
 *  without the codex CLI. */
export async function buildCodexSetupScript(remotePort: number, auth: string, tabId: string): Promise<string> {
  return invoke('build_codex_setup_script', { remotePort, auth, tabId });
}

export async function checkFullDiskAccess(): Promise<boolean> {
  return invoke('check_full_disk_access');
}

export async function openFullDiskAccessSettings(): Promise<void> {
  return invoke('open_full_disk_access_settings');
}
