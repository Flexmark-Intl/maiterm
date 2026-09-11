import { invoke } from '@tauri-apps/api/core';
import type { AgentRuntime } from '$lib/agents/types';
import type { AgentBridge, AppData, BotChannel, OverlordLedgerEntry, CommsMonitorChannel, DiffContext, DuplicateWorkspaceResult, EditorFileInfo, MailinkDevice, MailinkPairingPayload, MeshTopic, Pane, Preferences, ScrollInfo, SearchResult, Service, ShellInfo, SplitDirection, Tab, Task, Workstream, FrameMeta, WindowData, Workspace, WorkspaceNote } from './types';

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
  // Remove our injected remote command. The MAITERM_TAB_ID forms MUST be stripped first:
  // the plain patterns below would match from ` cd …` onwards and leave a dangling
  // `'export MAITERM_TAB_ID=…;` behind, which then accumulates on every clone/restore
  // round-trip — the same flag-accumulation this function exists to prevent.
  // Match to the `;` rather than to the first whitespace: the export carries several
  // variables now (MAITERM_PORT, MAITERM_AUTH), so a `\S+?` stops at the first space and
  // recognises nothing.
  let cleaned = cmd.replace(/\s+'export\s+MAITERM_TAB_ID=[^';]*;\s+(?:cd\s+.*?&&\s+)?exec\s+\$?SHELL\s+-l'\s*$/, '');
  cleaned = cleaned.replace(/\s+export\s+MAITERM_TAB_ID=[^';]*;\s+(?:cd\s+.*?&&\s+)?exec\s+\$?SHELL\s+-l\s*$/, '');
  // Pre-export forms (stored commands from earlier builds, and ps output for them).
  cleaned = cleaned.replace(/\s+cd\s+.*?&&\s+exec\s+\$?SHELL\s+-l\s*$/, '');
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
 *
 * `tabId` bakes `export MAITERM_TAB_ID=<tab>` into that remote command, which is how a
 * maiTerm-initiated session gets a PER-TAB identity on a host where several tabs share one
 * account. The alternative — `~/.aiterm` — is per-ACCOUNT and is deliberately deleted on
 * shared hosts precisely because it would hand an agent a sibling's tab id; and the other
 * alternative, typing the export into the live PTY, lands in the agent's chat when the remote
 * shell is already running one. Baking it into the command we are already sending costs
 * nothing and cannot be mistyped into anything.
 *
 * `bridge` adds `MAITERM_PORT` and `MAITERM_AUTH` — which maiTerm this tab's agent should
 * talk to, and with what. They ride here rather than being written into the remote's
 * per-account config because that config is shared by every maiTerm bridging to the account:
 * whoever writes last owns it, and the others' tabs go dead. In the environment the value is
 * per-tab and nobody can overwrite it. The port is a PREDICTION (see get_remote_bridge_env) —
 * this maiTerm's usual port on that host, which is wrong only if a collision has moved it
 * since, and self-corrects on the bridge's own env injection.
 */
export function buildSshCommand(
  sshCmd: string | null,
  remoteCwd: string | null,
  tabId?: string | null,
  bridge?: { port: number; auth: string } | null,
): string {
  if (!sshCmd) return '';
  const fullCmd = sshCmd.match(/^ssh\s/) ? sshCmd : `ssh ${sshCmd}`;
  // Tab ids are UUIDs; anything else is not ours to interpolate into a shell command.
  const exports: string[] = [];
  if (tabId && /^[A-Za-z0-9-]+$/.test(tabId)) exports.push(`MAITERM_TAB_ID=${tabId}`);
  // Same rule for the token: it reaches a remote shell, so anything that isn't plainly
  // safe to paste into one is dropped rather than quoted around.
  if (bridge && bridge.port > 0 && /^[A-Za-z0-9._-]+$/.test(bridge.auth)) {
    exports.push(`MAITERM_PORT=${bridge.port}`, `MAITERM_AUTH=${bridge.auth}`);
  }
  const exportPrefix = exports.length ? `export ${exports.join(' ')}; ` : '';
  const rest = fullCmd.replace(/^ssh\s+/, '');
  if (!remoteCwd) {
    if (!exportPrefix) {
      return fullCmd.replace(/^ssh\s+/, 'ssh -o ControlMaster=no ');
    }
    return `ssh -t -o ControlMaster=no ${rest} '${exportPrefix}exec $SHELL -l'`;
  }
  const cdPath = shellEscapePath(remoteCwd);
  return `ssh -t -o ControlMaster=no ${rest} '${exportPrefix}cd ${cdPath} && exec $SHELL -l'`;
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

/** Foreground command only — skips the lsof cwd lookup `getPtyInfo` also does.
 *  Used on the SSH-bridge env-injection hot path where cwd is irrelevant and the
 *  export races the user's first keystrokes.
 *
 *  `fresh` bypasses the backend's 800ms process-snapshot cache. Pass it for any
 *  EDGE-triggered ssh transition (a title event: "did ssh just start/exit here?").
 *  That cache is for polling; on an edge a stale answer is unrecoverable, because
 *  the title event is the only detection opportunity that session. */
export async function getPtyForeground(ptyId: string, fresh = false): Promise<string | null> {
  const cmd: string | null = await invoke('get_pty_foreground', { ptyId, fresh });
  return cmd ? cleanSshCommand(cmd) : null;
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
export async function scrollTerminal(ptyId: string, delta: number): Promise<FrameMeta> {
  return invoke('scroll_terminal', { ptyId, delta });
}

export async function scrollTerminalTo(ptyId: string, offset: number): Promise<FrameMeta> {
  return invoke('scroll_terminal_to', { ptyId, offset });
}

/** Hidden tabs get no frames from Rust; showing one triggers a full repaint. */
export async function setTerminalVisible(ptyId: string, visible: boolean): Promise<void> {
  return invoke('set_terminal_visible', { ptyId, visible });
}

/** xterm's buffer changed under Rust (a resize reflow) — ask for a full repaint
 *  so subsequent delta frames have a true baseline. */
export async function refreshTerminalFrame(ptyId: string, cols: number, rows: number): Promise<void> {
  return invoke('refresh_terminal_frame', { ptyId, cols, rows });
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

/** What holds a PTY's terminal right now — the stack store's pre-write guard
 *  (docs/stack.md §4). `shell_at_prompt` null means "cannot tell" and must not be read
 *  as either answer. `pid` is the foreground job leader; the store records it after a
 *  start and compares before a stop, since the executable name of `npm run dev` varies. */
export interface PtyForeground {
  shell_at_prompt: boolean | null;
  executable: string | null;
  command: string | null;
  pid: number | null;
}
/** Every stack write is an edge, so this defaults to a fresh process sweep (the backend's
 *  50ms floor collapses a burst of checks into one). */
export async function getPtyForegroundJob(ptyId: string, fresh = true): Promise<PtyForeground> {
  return invoke('get_pty_foreground_job', { ptyId, fresh });
}
/** Signal a PTY's foreground job — only if `pid` is still that job (re-checked with a
 *  fresh sweep). TERM by default, KILL with `force`. Resolves to whether anything was sent. */
export async function killPtyForegroundJob(ptyId: string, pid: number, force = false): Promise<boolean> {
  return invoke('kill_pty_foreground_job', { ptyId, pid, force });
}

/** Liveness for many PTYs in one pass — PTYs that no longer exist are simply absent. */
export async function getAgentLivenessBatch(ptyIds: string[]): Promise<Record<string, AgentLiveness>> {
  return invoke('get_agent_liveness_batch', { ptyIds });
}

/** Per-tab agent facts for the Overlord engine (docs/overlord.md §5) — cheap cached signals
 *  from the transcript tail-facts cache. All fields optional: a tab may resolve a session
 *  before its first assistant turn (no meta yet), and Gemini has no transcript source. */
export interface OverlordTabFacts {
  runtime: 'claude' | 'codex' | 'gemini';
  session_id: string;
  context_used?: number;
  context_limit?: number;
  context_pct?: number;
  model?: string;
  /** Unix-ms timestamp of the session's last REAL turn. */
  last_turn_ts?: number;
  /** Unix-ms of the newest `git commit` Bash tool call (Claude-only today). */
  last_commit_ts?: number;
  /** Unix-ms of the newest compaction boundary (isCompactSummary / compact_boundary). */
  last_compact_ts?: number;
  /** Whether this session has a task list at all. Distinguishes "completed everything"
   *  (tracked, empty todos — Claude Code sweeps the files once all tasks are done) from
   *  "never tracked anything" (absent), which otherwise look identical. */
  tracked?: boolean;
  /** The tab's task list (Claude-only). Read from Claude Code's own task store when
   *  available — the complete current list — else reconstructed from the transcript tail.
   *  Empty with `tracked: true` means every task was finished and swept. */
  todos?: OverlordTodoItem[];
  /** Where `todos` came from: 'store' is authoritative and complete; 'transcript' is
   *  whatever fit in the tail window (SSH tabs, whose store lives on the remote host). */
  todos_source?: 'store' | 'transcript';
  todos_ts?: number;
}

/** One TodoWrite item as recorded in the transcript. */
export interface OverlordTodoItem {
  content: string;
  status: 'pending' | 'in_progress' | 'completed';
  activeForm?: string;
  /** Task store only: waiting on an unfinished dependency (`blockedBy`). */
  blocked?: boolean;
}
/** Batched facts poll; tabs with no resolvable agent session are absent from the map. */
export async function getOverlordTabFacts(tabIds: string[]): Promise<Record<string, OverlordTabFacts>> {
  return invoke('get_overlord_tab_facts', { tabIds });
}

/** What a tab's agent has said since `sinceMs`, joined oldest-first — how Overlord reads
 *  the answer to a directive it typed. `sinceMs` MUST come from the transcript's own clock
 *  (the tab's previous `last_turn_ts`): an SSH tab's transcript is written on the remote
 *  host, so comparing against this machine's wall clock skews. */
export async function getAgentReplySince(tabId: string, sinceMs: number): Promise<string | null> {
  return invoke('get_agent_reply_since', { tabId, sinceMs });
}

/** One question's answer: chosen option labels, and/or free text via the Other row. */
export interface PromptAnswer {
  selected?: string[];
  other?: string | null;
}

/** What is currently blocking a tab. `kind: 'permission'` is a tool gate (carries `tool` and
 *  `detail`); `kind: 'question'` is an AskUserQuestion (carries `questions`). */
export interface TabPrompt {
  kind: 'permission' | 'question';
  prompt_id: string;
  runtime: string;
  tool?: string;
  detail?: string;
  questions?: unknown;
  asked_at?: number;
}

export async function getTabPrompt(tabId: string): Promise<TabPrompt | null> {
  return invoke('get_tab_prompt', { tabId });
}

/** Answer a tab's open prompt through the same hardened path the phone uses. Pass the
 *  `prompt_id` from `getTabPrompt` — it is the stale-guard against answering a prompt that
 *  opened while the decision was being made. */
export async function answerTabPrompt(
  tabId: string,
  prompt_id?: string | null,
  choice?: string | null,
  answers?: PromptAnswer[] | null,
): Promise<{ ok: boolean; reason?: string; detail?: string }> {
  return invoke('answer_tab_prompt', {
    tabId,
    promptId: prompt_id ?? null,
    choice: choice ?? null,
    answers: answers ?? null,
  });
}

/** Append entries to this window's Overlord ledger (ring-buffered backend-side). */
export async function appendOverlordLedger(entries: OverlordLedgerEntry[]): Promise<void> {
  return invoke('append_overlord_ledger', { entries });
}

/** This window's Overlord ledger, oldest first. */
export async function getOverlordLedger(): Promise<OverlordLedgerEntry[]> {
  return invoke('get_overlord_ledger');
}

/** Publish this window's Overlord engine snapshot for maiLink (docs/mailink-protocol.md §13).
 *  Rust gates it on tab designation, stamps it, and serves it to the phone — so the phone never
 *  has to ask a webview that may be asleep. */
export async function publishOverlordSnapshot(snapshot: Record<string, unknown>): Promise<void> {
  return invoke('publish_overlord_snapshot', { snapshot });
}

/** Replace one workspace's task list (docs/tasks.md). Whole-list persistence, like
 *  setWorkspaceMeshTopics; Rust recomputes `normalized_title` on the way in. */
export async function setWorkspaceTasks(
  workspaceId: string,
  tasks: Task[],
  workstreams: Workstream[],
): Promise<void> {
  return invoke('set_workspace_tasks', { workspaceId, tasks, workstreams });
}

/** Every task in this window as [workspaceId, tasks, workstreams] triples, in workspace order. */
export async function getWindowTasks(): Promise<[string, Task[], Workstream[]][]> {
  return invoke('get_window_tasks');
}

/** Flag/unflag a workspace as this window's Overlord workspace (at most one per window). */
export async function setWorkspaceOverlord(workspaceId: string, enabled: boolean): Promise<void> {
  return invoke('set_workspace_overlord', { workspaceId, enabled });
}

/** Create (or return the existing) Overlord workspace for this window: board tab + agent tab. */
export async function createOverlordWorkspace(): Promise<Workspace> {
  return invoke('create_overlord_workspace');
}

export async function serializeTerminal(ptyId: string): Promise<number[]> {
  return invoke('serialize_terminal', { ptyId });
}

export async function restoreTerminalScrollback(ptyId: string, scrollback: number[]): Promise<void> {
  return invoke('restore_terminal_scrollback', { ptyId, scrollback });
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

export async function startSelection(ptyId: string, col: number, row: number, side: string, selectionType: string): Promise<FrameMeta> {
  return invoke('start_selection', { ptyId, col, row, side, selectionType });
}

export async function updateSelection(ptyId: string, col: number, row: number, side: string): Promise<FrameMeta> {
  return invoke('update_selection', { ptyId, col, row, side });
}

export async function clearSelection(ptyId: string): Promise<FrameMeta> {
  return invoke('clear_selection', { ptyId });
}

export async function copySelection(ptyId: string): Promise<string | null> {
  return invoke('copy_selection', { ptyId });
}

export async function selectAll(ptyId: string): Promise<FrameMeta> {
  return invoke('select_all', { ptyId });
}

export async function scrollSelection(ptyId: string, delta: number, col: number): Promise<FrameMeta> {
  return invoke('scroll_selection', { ptyId, delta, col });
}

export async function saveTerminalScrollback(ptyId: string, tabId: string): Promise<void> {
  return invoke('save_terminal_scrollback', { ptyId, tabId });
}

/** Flush scrollback for every live terminal across ALL windows (Rust owns the
 *  buffers). Returns the number saved. Used before an update relaunch so
 *  secondary windows don't come back with blank terminals. */
export async function saveAllScrollback(): Promise<number> {
  return invoke('save_all_scrollback');
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

/** How many live tabs (any window) claim this session id via their runtime's session-id
 *  trigger var. >1 ⇒ contested (a duplicated tab still holds a copy) — auto-resume forks
 *  instead of plain-resuming. */
export async function countSessionIdClaimants(sessionId: string): Promise<number> {
  return invoke('count_session_id_claimants', { sessionId });
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

/** Clears the workspace's suspended flag; returns the ids of the tabs that were
 *  live when it was suspended (their wake_on_resume flags are consumed). */
export async function resumeWorkspace(workspaceId: string): Promise<string[]> {
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

export async function setTabTasksOpen(workspaceId: string, paneId: string, tabId: string, open: boolean): Promise<void> {
  return invoke('set_tab_tasks_open', { workspaceId, paneId, tabId, open });
}

export async function setTabOverlordExempt(workspaceId: string, paneId: string, tabId: string, exempt: boolean): Promise<void> {
  return invoke('set_tab_overlord_exempt', { workspaceId, paneId, tabId, exempt });
}

export async function setWorkspaceOverlordExempt(workspaceId: string, exempt: boolean): Promise<void> {
  return invoke('set_workspace_overlord_exempt', { workspaceId, exempt });
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

/** Declare a tab's agent runtime before any agent has registered — see the Rust command. Used
 *  when we deliberately launch an agent into a fresh tab, so it's maiLink-visible immediately
 *  instead of only once initSession lands (and visibly dormant if the launch fails). */
export async function setTabRuntime(workspaceId: string, paneId: string, tabId: string, runtime: AgentRuntime): Promise<void> {
  return invoke('set_tab_runtime', { workspaceId, paneId, tabId, runtime });
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

/** Operator kill switch: clear a tab's comms thread binding(s). Omit rootId to clear all. */
export async function clearTabCommsBinding(workspaceId: string, paneId: string, tabId: string, rootId?: string): Promise<void> {
  return invoke('clear_tab_comms_binding', { workspaceId, paneId, tabId, rootId: rootId ?? null });
}

/** Channels the configured comms bot is a member of (for the chat-monitoring picker). */
export async function commsListBotChannels(): Promise<BotChannel[]> {
  return invoke('comms_list_bot_channels');
}

/** Enable/update (channels) or disable (null) chat monitoring on a tab. */
export async function setTabCommsMonitor(
  workspaceId: string,
  paneId: string,
  tabId: string,
  channels: CommsMonitorChannel[] | null
): Promise<void> {
  return invoke('set_tab_comms_monitor', { workspaceId, paneId, tabId, channels });
}

/**
 * Hand a reload's replacement tab everything the original was responsible for.
 *
 * Carries the WHOLE persisted tab record except the replacement's own identity, PTY and
 * freshly-captured scrollback — an inverted list, so a field added to `Tab` survives a
 * reload by default instead of being silently dropped until someone remembers it. The
 * outward-facing claims (bound threads, chat monitoring) are MOVED, released on the
 * original in the same write so the comms watcher never sees both tabs holding them.
 */
export async function carryTabStateOnReload(
  workspaceId: string,
  paneId: string,
  fromTabId: string,
  toTabId: string
): Promise<void> {
  return invoke('carry_tab_state_on_reload', { workspaceId, paneId, fromTabId, toTabId });
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

/** Test a comms (Mattermost) server URL + bot token before saving them. */
export async function commsTestConnection(
  serverUrl: string,
  botToken: string
): Promise<{ ok: boolean; bot_username: string }> {
  return invoke('comms_test_connection', { serverUrl, botToken });
}

export async function setWorkspaceMeshTopics(workspaceId: string, topics: MeshTopic[]): Promise<void> {
  return invoke('set_workspace_mesh_topics', { workspaceId, topics });
}

/** Coarse whole-list replace of a workspace's stack definitions (docs/stack.md §3);
 *  Rust recomputes `normalized_name` on the way in. Bindings are separate — see
 *  `setTabServiceId`. */
export async function setWorkspaceStack(workspaceId: string, stack: Service[]): Promise<void> {
  return invoke('set_workspace_stack', { workspaceId, stack });
}

/** A service the project's own files say it runs (docs/stack.md §8). */
export interface StackSuggestion {
  name: string;
  command: string;
  cwd: string;
  source: 'package.json' | 'Procfile' | 'compose' | 'justfile' | 'Makefile' | string;
  recommended: boolean;
}
/** Scan a directory for services to import: package.json scripts, Procfile, compose, justfile, Makefile. */
export async function suggestStack(cwd: string): Promise<StackSuggestion[]> {
  return invoke('suggest_stack', { cwd });
}

/** One service's live status, as published to Rust for the SessionStart priming. */
export interface StackRuntimeRow {
  service_id: string;
  status: string;
  note?: string | null;
  since_ms?: number | null;
}
/** Upsert the stack store's runtime snapshot in Rust; `removed` drops ids that no longer exist. */
export async function publishStackRuntime(rows: StackRuntimeRow[], removed: string[] = []): Promise<void> {
  return invoke('publish_stack_runtime', { rows, removed });
}

/** Bind a tab to a stack service (or clear with null). Rust clears the same service from
 *  any other tab in the workspace in the same write — one tab per service. */
export async function setTabServiceId(
  workspaceId: string,
  paneId: string,
  tabId: string,
  serviceId: string | null
): Promise<void> {
  return invoke('set_tab_service_id', { workspaceId, paneId, tabId, serviceId });
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

/** Name this window, or pass null/blank to clear the name and fall back to derived text. */
export async function setWindowName(name: string | null): Promise<void> {
  return invoke('set_window_name', { name });
}

export async function saveWindowGeometry(monitorCount: number): Promise<void> {
  return invoke('save_window_geometry', { monitorCount });
}

/** Paint this window's native background (what shows before WebKit's first frame) in the theme bg. */
export async function setWindowBackground(hex: string): Promise<void> {
  return invoke('set_window_background', { hex });
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

/** SCP a remote file to a stable local temp path (on demand); returns that path. */
export async function stageRemoteFileTemp(sshCommand: string, remotePath: string): Promise<string> {
  return invoke('stage_remote_file_temp', { sshCommand, remotePath });
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

/**
 * The bridge values to export in a tab's ssh command, for the host that command targets.
 * Answerable before the tunnel exists — see buildSshCommand. Null while the MCP server
 * is still coming up, in which case the command is built without them, exactly as before.
 */
export async function getRemoteBridgeEnv(hostKey: string): Promise<{ port: number; auth: string } | null> {
  return invoke('get_remote_bridge_env', { hostKey });
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
export async function buildCodexSetupScript(remotePort: number, auth: string, tabId: string, hooks: boolean): Promise<string> {
  return invoke('build_codex_setup_script', { remotePort, auth, tabId, hooks });
}

export async function checkFullDiskAccess(): Promise<boolean> {
  return invoke('check_full_disk_access');
}

export async function openFullDiskAccessSettings(): Promise<void> {
  return invoke('open_full_disk_access_settings');
}

// ─── Deshittification ──────────────────────────────────────────────────────
// Rule state lives outside maiTerm's preferences (~/.claude/settings.json, the
// user's global git config), so every call answers with what is actually on
// disk — that IS the toggle position.

export interface DeshittifyRuleStatus {
  id: string;
  applied: boolean;
  /** Something outside maiTerm owns this rule's state, so it can't be applied.
   *  Left out of the group's "all applied?" arithmetic — see groupState(). */
  blocked: boolean;
  detail?: string;
}

export interface DeshittifyStatus {
  rules: DeshittifyRuleStatus[];
}

export async function deshittifyStatus(): Promise<DeshittifyStatus> {
  return invoke('deshittify_status');
}

export async function deshittifySetRule(id: string, enabled: boolean): Promise<DeshittifyStatus> {
  return invoke('deshittify_set_rule', { id, enabled });
}

/** Section switch: applies/reverts a whole group of rules. Returns the resulting
 *  status plus any per-rule failures (one refusing rule doesn't block the others). */
export async function deshittifySetRules(ids: string[], enabled: boolean): Promise<[DeshittifyStatus, string[]]> {
  return invoke('deshittify_set_rules', { ids, enabled });
}

/** Shell script that makes a bridged SSH host's Claude Code settings and git
 *  hooks match this machine's deshittification rules — including removing them
 *  again when a rule is switched off here. Reads only when nothing is enabled. */
export async function buildDeshittifySetupScript(): Promise<string> {
  return invoke('build_deshittify_setup_script');
}
