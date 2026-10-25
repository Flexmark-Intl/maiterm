import { Channel, invoke } from '@tauri-apps/api/core';
import type { AgentRuntime } from '$lib/agents/types';
import type { AgentMarkAbs, Baseline, BaselineStat, DeleteTicket, DirReq, FileOp, LedgerEntry, OpResult, OpenAck, PlaceHints, StatInfo, TabTurn, TreeEnvelope, TreePlace, TreeSearchResult } from './types';
import type { MissingRoot, RelocateOutcome, RelocatePreview } from './types';
import type { StackHeldAction, StackLeaseState } from './types';
import type { Hive, HiveBoard, HiveCreated, HiveDeletePreview, HiveDeleteResult, HiveDetectedTool, HiveDiffStat, HiveFolderInfo, HiveLandLog, HivePreflight, HiveSpec, HiveUndoPreview, RunQueueSettings } from './types';
import type { HiveMemoryFileText, HiveMemoryView } from './types';
import type { HiveDecisionView, HivePrecedentsView } from './types';
import type { AutonomySettings, HiveBlockerAnswerArg, HivePromptRoute, TaskBlocker } from './types';
import type { GallerySettingsChange } from './types';
import type { HiveFinishRecord } from './types';
import type {
  GalleryAddResult,
  GalleryAssetDetail,
  GalleryAssetRow,
  GalleryChip,
  GalleryCollectChange,
  GalleryCollection,
  GalleryExported,
  GalleryListFilter,
  GalleryTagChange,
} from './types';
// A tab that opened in home because its folder was missing saves the folder it WANTED, through
// every command below that saves a tab's cwd (docs/relocate.md §4).
import { savedCwd } from '$lib/stores/relocateFallback';
import type { AgentBridge, AgentRequest, AppData, BotChannel, OverlordLedgerEntry, CommsMonitorChannel, DiffContext, DuplicateWorkspaceResult, EditorFileInfo, FollowUp, ClaudeSetupReport, MailinkDevice, MailinkPairingPayload, MeshTopic, MoveTargetWindow, Pane, Preferences, ScrollInfo, SearchResult, Service, ShellInfo, SplitDirection, Tab, Task, Workstream, FrameMeta, WatchStatus, WindowData, Workspace, WorkspaceNote } from './types';

// Terminal commands
export async function spawnTerminal(ptyId: string, tabId: string, cols: number, rows: number, cwd?: string | null): Promise<void> {
  return invoke('spawn_terminal', { ptyId, tabId, cols, rows, cwd: cwd ?? null });
}

export interface PtyInfo {
  cwd: string | null;
  foreground_command: string | null;
}

/**
 * The program a command line starts with, when it is ssh by another name: a full path to ssh
 * (`/opt/homebrew/bin/ssh`) or autossh, bare or by path. maiTerm itself always types `ssh`, so
 * one of these on a line maiTerm built means the user's shell ran it through an alias or a
 * wrapper — and `ps` shows that program, followed by maiTerm's remote command with its
 * MAITERM_AUTH. Plain `ssh` is not "another name": it is stored without a program, as always.
 */
const SSH_PROGRAM = /^(?:\S*\/)?(?:auto)?ssh(?=\s|$)/;

/** The program to keep in a stored ssh command, or null for plain `ssh` / no program. */
function sshProgramOf(cmd: string): string | null {
  const m = cmd.match(SSH_PROGRAM);
  return m && m[0] !== 'ssh' ? m[0] : null;
}

/**
 * The ssh ARGUMENTS of a stored (cleaned) command — what `ssh` itself would be given — for
 * everything that reads a host out of it or hands it to maiTerm's own `ssh` (the MCP bridge's
 * tunnel, remote files, a share file). A stored command keeps the user's ssh program in front
 * when it isn't plain `ssh` (`cleanSshCommand`); this drops it, and autossh's own options with
 * it — ssh reads `-M` as master mode, so `-M 0` would make `0` the host.
 */
export function sshArgsOf(cmd: string): string {
  const program = cmd.match(SSH_PROGRAM)?.[0];
  if (!program) return cmd.trim();
  let rest = cmd.slice(program.length).trim();
  if (/(?:^|\/)autossh$/.test(program)) {
    rest = rest.replace(/(?:^|\s)-M\s*\S+/g, '').replace(/(?:^|\s)-f(?=\s|$)/g, '').trim();
  }
  return rest;
}

/** A stored (cleaned) ssh command as the human would type it: `ssh ews@nova`, or with the
 *  program it kept (`autossh -M 0 ews@nova`). For display. */
export function sshCommandLine(cmd: string): string {
  return SSH_PROGRAM.test(cmd) ? cmd : `ssh ${cmd}`;
}

/**
 * Strip previously-injected flags and remote commands from an SSH command
 * retrieved from the process tree, then normalize to just the user@host
 * portion (with any non-standard flags). Strips `ssh` prefix, `-t`,
 * `-o ControlMaster=...`, and `cd ... && exec $SHELL -l` suffixes.
 *
 * A program other than plain `ssh` (a full path, autossh) is KEPT in front — `/usr/bin/ssh
 * ews@nova`, `autossh -M 0 ews@nova` — so a clone or reconnect runs the user's program
 * (`buildSshCommand` reads it back), while everything after it is cleaned the same way.
 * Until this recognised only `ssh …`, such a line came back raw, maiTerm's export with
 * MAITERM_AUTH included, and was retyped into every clone and onto a fork request's card.
 * Read the host through `sshArgsOf`, never from this directly.
 */
export function cleanSshCommand(cmd: string): string {
  const program = sshProgramOf(cmd);
  if (program) {
    const rest = cmd.slice(program.length).trim();
    const cleaned = rest ? cleanSshCommand(`ssh ${rest}`) : '';
    return cleaned ? `${program} ${cleaned}` : program;
  }
  if (!cmd.match(/^ssh\s/)) return cmd;
  // Remove our injected remote command. The MAITERM_TAB_ID forms MUST be stripped first:
  // the plain patterns below would match from ` cd …` onwards and leave a dangling
  // `'export MAITERM_TAB_ID=…;` behind, which then accumulates on every clone/restore
  // round-trip — the same flag-accumulation this function exists to prevent.
  // Match to the `;` rather than to the first whitespace: the export carries several
  // variables now (MAITERM_PORT, MAITERM_AUTH), so a `\S+?` stops at the first space and
  // recognises nothing.
  //
  // Matched by the SHAPE of the remote command — a trailing `exec $SHELL -l` — rather than by
  // enumerating what precedes it. The enumerating version broke the moment §6 added a second
  // statement between the export and the `cd`: each new clause needed its own alternative here,
  // and forgetting one does not fail loudly, it silently stores the whole remote command as part
  // of the host string and accumulates it on every round trip.
  //
  // Greedy to the LAST `exec $SHELL -l'`, not lazy to the first quote: `shellEscapePath` quotes
  // the cwd, so the remote command legitimately contains single quotes in the middle
  // (`'…; cd '/srv/app' && exec $SHELL -l'` relies on adjacent-quote concatenation). Anchored on
  // one of the three things maiTerm itself starts that command with, so the `.*` cannot run
  // backwards over a user's own flags.
  let cleaned = cmd.replace(
    /\s+'(?:export\s+MAITERM_TAB_ID=|\[ -r ~\/\.maiterm\/|cd\s).*exec\s+\$?SHELL\s+-l'\s*$/,
    '',
  );
  // `ps` reports argv joined by spaces, so the same command comes back from the process table
  // with its quotes gone. Anchored on our own export so a greedy `.*` cannot eat a user's flags.
  // `[ -r ~/.maiterm/` opens the §6 account fragment (`accounts::remote`), listed here as well
  // as the export because the two are independently optional.
  cleaned = cleaned.replace(/\s+(?:export\s+MAITERM_TAB_ID=|\[ -r ~\/\.maiterm\/).*exec\s+\$?SHELL\s+-l\s*$/, '');
  // Pre-export form (stored commands from earlier builds, and ps output for them).
  cleaned = cleaned.replace(/\s+cd\s+.*?&&\s+exec\s+\$?SHELL\s+-l\s*$/, '');
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
 * Points a remote shell's `claude` at the maiterm-tab mod the ssh bridge copies to the host
 * (sshMcpBridge.svelte.ts `claudeModFiles`), as a local tab's shell is pointed at its own copy
 * (pty/manager.rs). Set beside MAITERM_PORT/AUTH, which the mod needs to reach maiTerm; `$HOME`
 * expands on the remote. It replaces a remote CLAUDE_CODE_PLUGIN_DIRS of the user's own in that
 * shell: prepending would add the folder again on every reconnect, and Claude loads a plugin
 * named twice once, warning about the other in every session. A missing folder (a host the
 * bridge hasn't set up yet) loads nothing, and a Claude too old for mods ignores the variable.
 */
export const REMOTE_CLAUDE_MOD_ENV = 'CLAUDE_CODE_PLUGIN_DIRS=$HOME/.maiterm/claude-mod/maiterm-tab';

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
 *
 * `accountExport` is the §6 managed-account fragment from `remoteAccountExport` — a statement of
 * its own rather than another `NAME=value`, because it does not carry a value. **The token is
 * deliberately not here**: this whole string is typed into the user's local shell, so it lands in
 * their scrollback (and from there in `aiterm-state.json`), their history and `ps` on both
 * machines. What rides here is the expression that reads a file Rust already pushed over its own
 * connection; see `accounts::remote`. Pass it through verbatim — it must not be quoted or
 * escaped, and it contains no apostrophe so the single quotes below still close where they look
 * like they do.
 */
/** A remote folder no shell treats specially, which `buildSshCommand` can quote once. */
const SAFE_REMOTE_PATH = /^[A-Za-z0-9_@%+=:,./~-]+$/;

export function buildSshCommand(
  sshCmd: string | null,
  remoteCwd: string | null,
  tabId?: string | null,
  bridge?: { port: number; auth: string } | null,
  accountExport?: string | null,
): string {
  if (!sshCmd) return '';
  // The user's own ssh program when the stored command kept one (cleanSshCommand), else `ssh`.
  const m = sshCmd.match(SSH_PROGRAM);
  const program = m?.[0] ?? 'ssh';
  const rest = (m ? sshCmd.slice(program.length) : sshCmd).trim();
  // Tab ids are UUIDs; anything else is not ours to interpolate into a shell command.
  const exports: string[] = [];
  if (tabId && /^[A-Za-z0-9-]+$/.test(tabId)) exports.push(`MAITERM_TAB_ID=${tabId}`);
  // Same rule for the token: it reaches a remote shell, so anything that isn't plainly
  // safe to paste into one is dropped rather than quoted around.
  if (bridge && bridge.port > 0 && /^[A-Za-z0-9._-]+$/.test(bridge.auth)) {
    exports.push(`MAITERM_PORT=${bridge.port}`, `MAITERM_AUTH=${bridge.auth}`, REMOTE_CLAUDE_MOD_ENV);
  }
  let prelude = exports.length ? `export ${exports.join(' ')}; ` : '';
  // Refused rather than escaped if it could break out of the quoting below. Rust builds this
  // from a validated tab id and nothing else, so a quote here means something is wrong upstream
  // and the right answer is to send no token, not to send a repaired one.
  if (accountExport && !accountExport.includes("'")) prelude += `${accountExport}; `;
  if (!remoteCwd) {
    if (!prelude) {
      return `${program} -o ControlMaster=no ${rest}`;
    }
    return `${program} -t -o ControlMaster=no ${rest} '${prelude}exec $SHELL -l'`;
  }
  // `shellEscapePath` quotes for the REMOTE shell, but the line is typed into the local one,
  // inside its own single quotes. A plain path passes through both unharmed; one with a space,
  // a quote or a `$` must be escaped again for the local shell, or it splits there (and the
  // remote `cd` fails, ending the session) or expands on this computer.
  const cdPath = SAFE_REMOTE_PATH.test(remoteCwd) ? shellEscapePath(remoteCwd) : shellEscapePath(remoteCwd).replace(/'/g, "'\\''");
  return `${program} -t -o ControlMaster=no ${rest} '${prelude}cd ${cdPath} && exec $SHELL -l'`;
}

export function normalizeSshInput(input: string): string {
  const trimmed = input.trim();
  if (!trimmed) return '';
  // A full command (`ssh …`, or ssh by path / autossh) goes through cleanSshCommand
  if (trimmed.match(SSH_PROGRAM)) return cleanSshCommand(trimmed);
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

/** `human`: the human typed this (not an answer to a permission prompt). Rust holds what maiTerm
 *  types for others behind it — the last key must be stamped where the gate reads it. */
export async function writeTerminal(ptyId: string, data: number[], human = false): Promise<void> {
  return invoke('write_terminal', { ptyId, data, human });
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

export async function detectClaudeSetup(): Promise<ClaudeSetupReport> {
  return invoke('detect_claude_setup');
}

export async function claudeSetupAddToPath(): Promise<void> {
  return invoke('claude_setup_add_to_path');
}

/** Opens the Claude setup wizard in the terminal window the user was last in. */
export async function openClaudeSetup(): Promise<void> {
  return invoke('open_claude_setup');
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

/** Per-tab last activity (unix ms) by the phone's rule — every requested tab is present. */
export async function getTabsLastActivity(tabIds: string[]): Promise<Record<string, number>> {
  return invoke('get_tabs_last_activity', { tabIds });
}

/** What a tab's agent has said since `sinceMs`, joined oldest-first — how Overlord reads
 *  the answer to a directive it typed. `sinceMs` MUST come from the transcript's own clock
 *  (the tab's previous `last_turn_ts`): an SSH tab's transcript is written on the remote
 *  host, so comparing against this machine's wall clock skews. */
export async function getAgentReplySince(tabId: string, sinceMs: number): Promise<string | null> {
  return invoke('get_agent_reply_since', { tabId, sinceMs });
}

/** The answer to a directive, anchored on the directive's own turn in the transcript (it may have
 *  been typed into a busy agent): `prompt_ts` null until it shows up, `reply` null until the agent
 *  says something after it. */
export async function getAgentReplyAfterPrompt(
  tabId: string,
  prompt: string,
  sinceMs: number,
): Promise<{ prompt_ts: number | null; reply: string | null }> {
  return invoke('get_agent_reply_after_prompt', { tabId, prompt, sinceMs });
}

/** One question's answer: chosen option labels, and/or free text via the Other row. */
export interface PromptAnswer {
  selected?: string[];
  other?: string | null;
}

/** What is currently blocking a tab. `kind: 'permission'` is a tool gate (carries `tool` and
 *  `detail`); `kind: 'question'` is an AskUserQuestion (carries `questions`); `kind: 'trust'` is
 *  Claude's workspace-trust dialog before any session exists (carries `path`, `options` and a
 *  `note` that it is the human's decision). */
export interface TabPrompt {
  kind: 'permission' | 'question' | 'trust';
  prompt_id: string;
  runtime: string;
  tool?: string;
  detail?: string;
  questions?: unknown;
  asked_at?: number;
  path?: string;
  options?: string[];
  note?: string;
  /** A Hive worker's routed prompt (docs/hive.md §4.7): who answers it, "coordinator" | "human". */
  owner?: string;
  /** Why the human holds it: "ask", "timeout", "take_over", or the allowlist's reason. */
  owner_why?: string;
  /** When the coordinator's time runs out (ms); 0 when the human holds it. */
  due_at?: number;
  timed_out?: boolean;
}

export async function getTabPrompt(tabId: string): Promise<TabPrompt | null> {
  return invoke('get_tab_prompt', { tabId });
}

/** What the agent's input box on this tab holds, read off the screen (mailink/input_box.rs):
 *  'unknown' when maiTerm doesn't recognise the layout — never read that as empty. */
export async function agentInputBox(tabId: string): Promise<'empty' | 'has_text' | 'unknown'> {
  return invoke('agent_input_box', { tabId });
}

/** Whether a prompt for this tab goes to its maiterm-tab mod now rather than typed: then a
 *  draft in the agent's box doesn't hold it (claude_code/mod_inbox.rs `takes_now`). */
export async function agentTakesPrompt(tabId: string): Promise<boolean> {
  return invoke('agent_takes_prompt', { tabId });
}

/** Hands a prompt to this tab's maiterm-tab mod to submit, when it takes one now. False means
 *  nothing was sent: type it, under the caller's own checks. Use `deliverPrompt`. */
export async function submitPromptToAgent(tabId: string, text: string): Promise<boolean> {
  return invoke('submit_prompt_to_agent', { tabId, text });
}

/** What `deliverToAgent` did. `held`: waiting for a dialog, a draft or the human's typing to
 *  clear — never for a busy agent; its outcome follows as an `agent-inbox-outcome` event keyed by
 *  `msg_id`. `refused`: nothing there can take it (`no_agent`, `no_terminal`, `type_failed`). */
export type AgentDelivery =
  | { status: 'delivered'; via: 'agent' | 'keys' }
  | { status: 'held'; msg_id: string; reason: string }
  | { status: 'refused'; reason: string };

/** `agent-inbox-outcome`: what became of a held desktop message. */
export interface AgentInboxOutcome {
  tab_id: string;
  msg_id: string;
  outcome: 'typed' | 'dropped';
  why: string | null;
}

/** A dialog is open in this tab, read off the screen as well as the hooks (whose permission
 *  state lags the dialog by ~6 s). Anything typed into a busy agent must ask this first: a
 *  paste's CR at a permission dialog approves it. */
export async function agentDialogOpen(tabId: string): Promise<boolean> {
  return invoke('agent_dialog_open', { tabId });
}

/** Deliver a message to a tab's agent — the one gate for everything the desktop types for someone
 *  else (src-tauri/src/mailink/draft_hold.rs). `what` labels the log: answer, start, notice,
 *  directive, request, follow-up, rule. */
export async function deliverToAgent(tabId: string, text: string, what: string): Promise<AgentDelivery> {
  return invoke('deliver_to_agent', { tabId, text, what });
}

/** Whether Claude's workspace-trust dialog is open on this tab. Every automated path that types
 *  into a tab must ask first: an Enter there confirms "No, exit" and Claude quits. */
export async function trustDialogOpen(tabId: string): Promise<boolean> {
  return invoke('trust_dialog_open', { tabId });
}

/** One distilled transcript turn, the same shape the phone's thread receives
 *  (docs/mailink-protocol.md §4.3). `tool` turns carry a one-line marker like
 *  `Bash(npm test)`. A `terminal_snapshot` is a raw live-screen scrape for a tab without a
 *  readable transcript: render it preformatted and never append it as history. */
export interface ChatTurn {
  msg_id: string;
  role: 'agent' | 'user' | 'system' | 'tool';
  /** `session_divider`: between two sessions of a retired Hive worker's chat. */
  kind?: 'terminal_snapshot' | 'peer_message' | 'goal_status' | 'asset' | 'session_divider';
  text: string;
  /** unix ms */
  ts: number;
  goal?: { event: 'set' | 'blocked' | 'met' | 'failed' | 'cleared'; condition?: string };
  peer?: { direction: 'in' | 'out'; name?: string; topic?: string };
  /** A user turn maiTerm typed rather than the human (protocol 0.14): an Overlord directive,
   *  or a message sent for the human. */
  typedBy?: { by: 'overlord' | 'maiterm'; rule?: string };
  /** A Hive agent's `galleryAdd` step: what it filed (docs/hive.md §4.2), desktop only. */
  gallery_assets?: GalleryChip[];
}

/** A tab's chat as the phone sees it, for the Workstream Loom's Focus view. */
export async function getTabTranscript(tabId: string): Promise<ChatTurn[]> {
  return invoke('get_tab_transcript', { tabId });
}

/** A chat's telemetry, as the phone's thread header reads it (mailink-protocol §12.1 `meta`). */
export interface TabMeta {
  runtime: 'claude' | 'codex' | 'gemini';
  /** Display name ("Opus 5"); absent before the first reply. */
  model?: string;
  /** Claude only, and absent for good once a session is resumed: unknown, never a value. */
  effort?: string;
  contextPct?: number;
  contextUsed?: number;
  contextLimit?: number;
  /** Claude only: messages typed while the agent was busy and not yet taken, in order. */
  queued?: { text: string; queuedAt: number }[];
}
export async function getTabMeta(tabId: string): Promise<TabMeta | null> {
  return invoke('get_tab_meta', { tabId });
}

/** A row the model picker offers (mailink-protocol `ModelOption`). `value` is what follows
 *  `/model`; `display` is what `TabMeta.model` reads on it. */
export interface ModelOption {
  value: string;
  name: string;
  display: string;
  family: string;
  ambiguous: boolean;
  note: string;
  source: 'account' | 'builtin';
}
export async function listTabModels(tabId: string): Promise<ModelOption[]> {
  return invoke('list_tab_models', { tabId });
}

/** An image an agent showed by its path, for the Focus chat (the phone's 0.18 rules). */
export type ChatImage = { ok: true; url: string } | { ok: false; reason: string };
export async function getChatImage(tabId: string, path: string): Promise<ChatImage> {
  return invoke('get_chat_image', { tabId, path });
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

/** The HUMAN answering a tab's prompt from the Loom. Same responder as `answerTabPrompt`, but
 *  it may answer Claude's workspace-trust dialog, which the Overlord agent's path refuses. Never
 *  call it from an agent's tool handler. */
export async function answerTabPromptAsHuman(
  tabId: string,
  prompt_id: string,
  choice?: string | null,
  answers?: PromptAnswer[] | null,
): Promise<{ ok: boolean; reason?: string; detail?: string }> {
  return invoke('answer_tab_prompt_as_human', {
    tabId,
    promptId: prompt_id,
    choice: choice ?? null,
    answers: answers ?? null,
  });
}

/** What happened to a message typed from the Loom's composer. */
export interface SendResult {
  status: 'delivered' | 'unreachable' | 'prompt_open' | 'draft' | 'failed' | 'empty';
  reason?: string;
  detail?: string;
  /** The wake it took ('init' | 'resume'), when the tab's agent had to be brought back first;
   *  'hive' when the message woke a Done Hive (docs/hive.md §11.4), whose coordinator gets it
   *  once it's back. */
  woke?: 'init' | 'resume' | 'hive' | null;
}

/** Type a message into a tab's agent, under the phone's rules: an unregistered tab is woken
 *  first, the trust dialog is never typed at, and nothing is sent while a prompt is open.
 *  `files`: local paths typed ahead of the text (an SSH tab gets copies staged on its host). */
export async function sendTabMessage(tabId: string, text: string, files: string[] = []): Promise<SendResult> {
  return invoke('send_tab_message', { tabId, text, files });
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

/** The process is about to end: Rust refuses tab/pane deletes from here on and tells EVERY
 *  window (`app-shutting-down`) to stop acting on its dying shells' pty-close. */
export async function beginShutdown(): Promise<void> {
  return invoke('begin_shutdown');
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

/** `hiveCopyOf`: a reload's copy of that Hive tab, the one tab a Hive workspace takes (never
 *  made active there). */
/** `serviceId`/`serviceTarget`: the tab is born bound to that stack row, hidden and never active
 *  (a service copy or a run-queue job; the only new tab a Hive takes). */
export async function createTab(workspaceId: string, paneId: string, name: string, afterTabId?: string, hiveCopyOf?: string, serviceId?: string, serviceTarget?: string): Promise<Tab> {
  return invoke('create_tab', { workspaceId, paneId, name, afterTabId, hiveCopyOf, serviceId, serviceTarget });
}

/** `replacedBy`: the tab that replaces this one (a reload), already saved — lets the delete
 *  through even while the app is shutting down, since nothing is lost by it. */
export async function deleteTab(workspaceId: string, paneId: string, tabId: string, replacedBy?: string): Promise<void> {
  return invoke('delete_tab', { workspaceId, paneId, tabId, replacedBy: replacedBy ?? null });
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

/** Select a tab the human can actually see in this pane, or nothing when there is none
 *  (docs/stack.md §7). Returns the tab it settled on. */
export async function healPaneActiveTab(workspaceId: string, paneId: string): Promise<string | null> {
  return invoke('heal_pane_active_tab', { workspaceId, paneId });
}

export async function setTabPtyId(workspaceId: string, paneId: string, tabId: string, ptyId: string): Promise<void> {
  return invoke('set_tab_pty_id', { workspaceId, paneId, tabId, ptyId });
}

export async function suspendTab(workspaceId: string, paneId: string, tabId: string, cwd: string | null, sshCommand: string | null, remoteCwd: string | null): Promise<void> {
  return invoke('suspend_tab', { workspaceId, paneId, tabId, cwd: savedCwd(tabId, cwd), sshCommand, remoteCwd });
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

/** Open/close this window's Files column (docs/file-tree.md §3.1). Only from the human's toggle:
 *  a width change at a streaming agent duplicates its scrollback. */
export async function setFileTreeOpen(open: boolean): Promise<void> {
  return invoke('set_file_tree_open', { open });
}

/** Commit the Files column width once, on pointerup of the ghost resize. Clamped 200–560 in Rust. */
export async function setFileTreeWidth(width: number): Promise<void> {
  return invoke('set_file_tree_width', { width });
}

/** Pin the Files root for a workspace (a `watch::place`), or unpin with null (§4.2). */
export async function setWorkspaceFileTreePin(workspaceId: string, place: string | null): Promise<void> {
  return invoke('set_workspace_file_tree_pin', { workspaceId, place });
}

// ── Files tree (docs/file-tree.md §5.2) ──────────────────────────────────────────────────
// Paths in and out are root-relative POSIX ('' = the root). A subscription is one webview's
// Channel; a page reload drops this window's subscriptions in Rust, so a reloaded page just
// opens again.

/** Which folder a tab's tree shows. Call on tab activation, OSC 133 A, OSC 7/title change and
 *  the tab's agent SessionStart, debounced 150 ms; never on a timer. Rejects for a stack
 *  service tab, a tab with no known folder, and an ssh tab whose host is unknown. A pinned
 *  workspace answers with its pin (`source: 'pin'`). */
export async function ftResolvePlace(tabId: string, hints: PlaceHints): Promise<TreePlace> {
  return invoke('ft_resolve_place', { tabId, hints });
}

/** Subscribe to a local root. `restore` re-opens folders with the generations held (the root is
 *  always included). Set `channel.onmessage` BEFORE calling: events can arrive before the
 *  promise resolves. Rejects for a remote or missing place. */
export async function ftOpen(place: string, restore: DirReq[], channel: Channel<TreeEnvelope>): Promise<OpenAck> {
  return invoke('ft_open', { place, restore, onEvent: channel });
}

/** End a subscription: closing the column or switching roots (not pause). The hub stays warm
 *  for 30 s, so coming back is cheap. */
export async function ftClose(subId: number): Promise<void> {
  return invoke('ft_close', { subId });
}

/** `document.visibilitychange`: true while hidden. Changed folders arrive whole on resume. */
export async function ftPause(subId: number, paused: boolean): Promise<void> {
  return invoke('ft_pause', { subId, paused });
}

/** Everything up to `seq` is applied. Called from inside `onmessage` (see `TreeEnvelope`). */
export async function ftAck(subId: number, seq: number): Promise<void> {
  return invoke('ft_ack', { subId, seq });
}

/** Open folders: answers arrive as `dir` (chunked) or `not_modified`. */
export async function ftExpand(subId: number, dirs: DirReq[]): Promise<void> {
  return invoke('ft_expand', { subId, dirs });
}

/** Close folders, so Rust stops tracking them. */
export async function ftCollapse(subId: number, dirs: string[]): Promise<void> {
  return invoke('ft_collapse', { subId, dirs });
}

/** A whole `dir` for `rel` (null = every open folder): after a `delta` whose `from` isn't held,
 *  a broken chunk sequence, or the human's Refresh. */
export async function ftRefresh(subId: number, rel: string | null): Promise<void> {
  return invoke('ft_refresh', { subId, rel });
}

/** A remote place whose open failed: its host's Files helper tries again now (Retry). */
export async function ftRetryHost(place: string): Promise<void> {
  return invoke('ft_retry_host', { place });
}

/** This computer's name, for the remote root's wrong-host warning. */
export async function ftLocalHostname(): Promise<string> {
  return invoke('ft_local_hostname');
}

/** Absolute paths (an editor's file, a hook path) to rels spelled as the tree keys them; null =
 *  outside the root. The only abs → rel conversion: never strip prefixes in JS. */
export async function ftLocate(subId: number, absPaths: string[]): Promise<(string | null)[]> {
  return invoke('ft_locate', { subId, absPaths });
}

/** Whether each absolute path is on disk: on the subscription's root's machine (an ssh host
 *  is asked, for a path under the root), or on this computer with `subId` null. Null = can't
 *  tell (refused, a host not answering, outside a remote root). */
export async function ftExists(subId: number | null, absPaths: string[]): Promise<(boolean | null)[]> {
  return invoke('ft_exists', { subId, absPaths });
}

/** Size, mtime, exec bit, link target and (a changed file's) line counts for the hover tooltip. */
export async function ftStat(subId: number, rel: string): Promise<StatInfo> {
  return invoke('ft_stat', { subId, rel });
}

/** Find (§8): the best `limit` (≤500) paths for `query`, with matched character positions.
 *  `seq` must increase per query: an older answer arriving late is the caller's to drop.
 *  `indexShallow`: the human asked to index a home or volume folder. */
export async function ftSearch(subId: number, query: string, limit: number, seq: number, indexShallow = false): Promise<TreeSearchResult> {
  return invoke('ft_search', { subId, query, limit, seq, indexShallow });
}

/** A file's content at 'HEAD' or in the 'index' (Compare with HEAD); null = new since then. */
export async function ftDiffBase(subId: number, rel: string, base: 'HEAD' | 'index'): Promise<string | null> {
  return invoke('ft_diff_base', { subId, rel, base });
}

/** A file operation on the subscription's root (§10). Rejects, with nothing changed, for a path
 *  outside the root, the root itself or a bad delete ticket; a decision for the human comes
 *  back as `needs`. The view updates through the Channel as usual. */
export async function ftOp(subId: number, op: FileOp): Promise<OpResult> {
  return invoke('ft_op', { subId, op });
}

/** Count what deleting `rels` permanently removes and get the ticket `ftOp({kind: 'delete'})`
 *  needs: single use, 60 s, exactly this set. */
export async function ftPrepareDelete(subId: number, rels: string[]): Promise<DeleteTicket> {
  return invoke('ft_prepare_delete', { subId, rels });
}

/** A tab's agent ledger (§9.1): 'turn' (its current or last turn) or 'session'. Reads and
 *  searches only with `includeReads`. Empty for a tab no agent has worked in. */
export async function agentFilesActivity(tabId: string, scope: 'turn' | 'session', includeReads = false): Promise<LedgerEntry[]> {
  return invoke('agent_files_activity', { tabId, scope, includeReads });
}

/** The agent marks still standing for this window's tabs, with absolute paths: what the
 *  column draws on mount, before any `agent` event. */
export async function agentFilesMarks(): Promise<AgentMarkAbs[]> {
  return invoke('agent_files_marks');
}

/** The current turn of each of this window's tabs, as the agent ledger counts them: what a
 *  Files column that was closed starts its turn tracking from. */
export async function agentFilesTurns(): Promise<TabTurn[]> {
  return invoke('agent_files_turns');
}

/** What `pathAbs` held before `tabId`'s agent first edited it, this 'turn' (its current or
 *  last) or this 'session' (§14). Serve an agent only its OWN tab's. */
export async function agentFilesBaseline(tabId: string, pathAbs: string, scope: 'turn' | 'session'): Promise<Baseline> {
  return invoke('agent_files_baseline', { tabId, pathAbs, scope });
}

/** Lines added and removed in `pathAbs` since that baseline (the Turn lens tooltip). */
export async function agentFilesBaselineStat(tabId: string, pathAbs: string, scope: 'turn' | 'session'): Promise<BaselineStat> {
  return invoke('agent_files_baseline_stat', { tabId, pathAbs, scope });
}

/** A reload replaced `oldTabId` with `newTabId`: the agent ledger moves with it. */
export async function agentFilesRemapTab(oldTabId: string, newTabId: string): Promise<void> {
  return invoke('agent_files_remap_tab', { oldTabId, newTabId });
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

export async function setTabMuted(workspaceId: string, paneId: string, tabId: string, muted: boolean): Promise<void> {
  return invoke('set_tab_muted', { workspaceId, paneId, tabId, muted });
}

export async function setWorkspaceMuted(workspaceId: string, muted: boolean): Promise<void> {
  return invoke('set_workspace_muted', { workspaceId, muted });
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
  return invoke('set_tab_restore_context', { workspaceId, paneId, tabId, cwd: savedCwd(tabId, cwd), sshCommand, remoteCwd });
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

export async function setWorkspaceMeshGroup(workspaceId: string, group: string | null): Promise<void> {
  return invoke('set_workspace_mesh_group', { workspaceId, group });
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
/** Returns the rows as Rust stored them — names normalized, `~` in cwd expanded. */
export async function setWorkspaceStack(workspaceId: string, stack: Service[]): Promise<Service[]> {
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

/** One copy's live status, as published to Rust for the SessionStart priming. Keyed in Rust
 *  by copy key (`service_id`, or `service_id@target`). */
export interface StackRuntimeRow {
  service_id: string;
  status: string;
  note?: string | null;
  since_ms?: number | null;
  /** Which copy (docs/hive.md §6.9); absent/'' for the service's own folder. */
  target?: string;
  /** The folder the copy runs in (`ResolvedCopy.cwd`); absent means the service's own cwd. */
  cwd?: string;
  /** What to call the copy (`ResolvedCopy.label`). */
  label?: string;
}
/** Upsert the stack store's runtime snapshot in Rust; `removed` drops copy keys that no longer exist. */
export async function publishStackRuntime(rows: StackRuntimeRow[], removed: string[] = []): Promise<void> {
  return invoke('publish_stack_runtime', { rows, removed });
}

/** Bind a tab to a copy of a stack service (or clear with null). Rust clears the same copy
 *  from any other tab in the workspace in the same write — one tab per `(service, target)`.
 *  No target is the service's own folder. */
export async function setTabServiceId(
  workspaceId: string,
  paneId: string,
  tabId: string,
  serviceId: string | null,
  serviceTarget: string | null = null
): Promise<void> {
  return invoke('set_tab_service_id', { workspaceId, paneId, tabId, serviceId, serviceTarget });
}

/** What to type for one copy of a service (docs/hive.md §6.9): Rust's re-rooted cwd and the
 *  command/env with placeholders filled. A service on its own folder with no placeholders comes
 *  back exactly as defined. */
export interface ResolvedCopy {
  copy_key: string;
  cwd: string;
  command: string;
  env: [string, string][];
  /** Ports maiTerm booked for `{port}`/`{port2}`; null when it booked none. */
  port: number | null;
  port2: number | null;
  branch: string | null;
  /** Short commit the checkout is at. */
  commit: string | null;
  /** The checkout's root; null on the own folder. */
  checkout: string | null;
  /** The worker's slug when it runs in a worker's worktree. */
  worker: string | null;
  /** What a row calls the copy; '' for the own folder. */
  label: string;
  /** No `{port}`: only one copy runs at a time. */
  one_copy: boolean;
}
/** Resolve a copy; target '' (default) is the own folder, else `checkout:<abs>` / `branch:<name>`. */
export async function stackResolveCopy(workspaceId: string, serviceId: string, target = ''): Promise<ResolvedCopy> {
  return invoke('stack_resolve_copy', { workspaceId, serviceId, target });
}
/** Free a stopped copy's ports. False if it held none. */
export async function stackReleaseCopy(copyKey: string): Promise<boolean> {
  return invoke('stack_release_copy', { copyKey });
}
/** One "Run on ▸" choice. */
export interface CopyTarget {
  target: string;
  kind: 'own' | 'worktree' | 'branch' | (string & {});
  label: string;
  branch?: string;
  worker?: string;
  path?: string;
}
/** The own folder, the repo's other worktrees (workers named), and up to 50 branches. */
export async function stackCopyTargets(workspaceId: string, serviceId: string): Promise<CopyTarget[]> {
  return invoke('stack_copy_targets', { workspaceId, serviceId });
}
/** An agent's `on` as a target: "main" → '', a worker (badge or name) → its worktree, else a branch. */
export async function stackTargetFor(workspaceId: string, serviceId: string, on: string): Promise<string> {
  return invoke('stack_target_for', { workspaceId, serviceId, on });
}
/** Remove maiTerm's branch checkouts whose branch is gone and that nothing targets. */
export async function stackPruneWorktrees(): Promise<number> {
  return invoke('stack_prune_worktrees');
}
/** A `shared_from` service's source as the Rust mirror holds it (docs/hive.md §6.8): any
 *  window's workspace. */
export interface StackSharedRow {
  workspace_name: string;
  service_name: string;
  /** The source's own folder copy: "stopped" when nothing published one. */
  status: string;
  note?: string | null;
  since_ms?: number | null;
  port?: number | null;
  url?: string | null;
}
/** The source a `shared_from` names; null when that workspace or service is gone. */
export async function getStackRuntimeRow(workspaceId: string, serviceId: string): Promise<StackSharedRow | null> {
  return invoke('get_stack_runtime_row', { workspaceId, serviceId });
}

/** The window has a shell at its prompt for a queued job and is about to type its line
 *  (docs/hive.md §6.8). Refused once Rust gave the start up, so nothing is typed then. */
export async function stackJobArm(jobId: string, tabId: string, ptyId: string): Promise<void> {
  return invoke('stack_job_arm', { jobId, tabId, ptyId });
}
/** Which of these copies a running job leases now: checked before every stop and retarget. */
export async function stackLeaseState(copyKeys: string[]): Promise<StackLeaseState[]> {
  return invoke('stack_lease_state', { copyKeys });
}
/** Run when free: the stop or retarget waits in Rust until the copy's lease ends. */
export async function stackLeaseHold(workspaceId: string, copyKey: string, action: 'stop' | 'restart' | 'retarget', to: string | null, by: 'human' | 'agent'): Promise<StackHeldAction> {
  return invoke('stack_lease_hold', { workspaceId, copyKey, action, to, by });
}
/** What waited on a copy (Run when free, Stop anyway) has run: the copy may be leased again. */
export async function stackLeaseDone(copyKey: string): Promise<void> {
  return invoke('stack_lease_done', { copyKey });
}
/** Stop anyway: the jobs leasing the copy are interrupted (never a receipt). Their ids. */
export async function hiveLeaseOverride(copyKey: string): Promise<string[]> {
  return invoke('hive_lease_override', { copyKey });
}
/** A Hive is about to be suspended: its runs are released before its PTYs die. How many went. */
export async function hiveSuspendRuns(workspaceId: string): Promise<number> {
  return invoke('hive_suspend_runs', { workspaceId });
}

/** Add one follow-up to a tab (docs/follow-ups.md). Returns the tab's list as it now stands. */
export async function addTabFollowUp(workspaceId: string, tabId: string, followUp: FollowUp): Promise<FollowUp[]> {
  return invoke('add_tab_follow_up', { workspaceId, tabId, followUp });
}

/** Take one follow-up off a tab — atomically, and only if the tab still holds it. `null` means
 *  it wasn't there (cancelled, or moved by a reload): don't act on it. Otherwise the new list. */
export async function takeTabFollowUp(workspaceId: string, tabId: string, followUpId: string): Promise<FollowUp[] | null> {
  return invoke('take_tab_follow_up', { workspaceId, tabId, followUpId });
}

/** Ask the shell in a terminal how much is on its command line (maiTerm's zsh integration answers
 *  an empty bracketed paste). Probes only the tab's own shell, when it announced the probe and
 *  holds the terminal right now. `null` = can't tell — never read it as empty. */
export async function probeShellLine(ptyId: string): Promise<{ pid: number; len: number; shell: 'zsh' | 'bash' } | null> {
  return invoke('probe_shell_line', { ptyId });
}

/** Whether an agent session's transcript is on this machine (false for one recorded over ssh). */
export async function agentSessionIsLocal(runtime: string, sessionId: string): Promise<boolean> {
  return invoke('agent_session_is_local', { runtime, sessionId });
}

/** Mark an event follow-up's condition met — once; the first observation stands. `null` when
 *  nothing changed (gone, already met, or a time follow-up). Otherwise the new list. */
export async function meetTabFollowUp(workspaceId: string, tabId: string, followUpId: string, metAt: string, outcome: string): Promise<FollowUp[] | null> {
  return invoke('meet_tab_follow_up', { workspaceId, tabId, followUpId, metAt, outcome });
}

/** The human approved a watch script (docs/follow-ups.md §5.1). Null: not there, or not a script. */
export async function approveTabFollowUpScript(workspaceId: string, tabId: string, followUpId: string): Promise<FollowUp[] | null> {
  return invoke('approve_tab_follow_up_script', { workspaceId, tabId, followUpId });
}

export async function declineTabFollowUpScript(workspaceId: string, tabId: string, followUpId: string): Promise<FollowUp[] | null> {
  return invoke('decline_tab_follow_up_script', { workspaceId, tabId, followUpId });
}

/** An ssh tab's watch-script home: the bridged host and its agent's folder there (§5.1). */
export async function followUpRemoteHome(tabId: string): Promise<{ host: string | null; cwd: string | null }> {
  return invoke('follow_up_remote_home', { tabId });
}

export async function followUpWatchStatus(): Promise<Record<string, WatchStatus>> {
  return invoke('follow_up_watch_status');
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

export interface DuplicatedWindow {
  label: string;
  /** Names of the Hive workspaces left out of the copy (docs/hive.md §11.2). */
  skipped_hives: string[];
}

export async function duplicateWindow(tabContexts: TabContext[]): Promise<DuplicatedWindow> {
  return invoke('duplicate_window', { tabContexts });
}

export async function closeWindow(): Promise<void> {
  return invoke('close_window');
}

export async function listMoveTargets(): Promise<MoveTargetWindow[]> {
  return invoke('list_move_targets');
}

export async function moveWorkspaceToWindow(workspaceId: string, targetLabel: string): Promise<void> {
  return invoke('move_workspace_to_window', { workspaceId, targetLabel });
}

// ── Agent requests only the human approves (src-tauri/src/agent_requests.rs) ──
// `decideAgentRequest` is the human's click and nothing else may call it: no tool handler, no
// rule, no agent-facing path. That is the whole security of the feature.

export async function fileAgentRequest(
  tabId: string,
  kind: AgentRequest['kind'],
  args: Record<string, unknown>,
  summary: string,
  effects: string[],
  reason: string | null,
): Promise<AgentRequest> {
  return invoke('file_agent_request', { tabId, kind, args, summary, effects, reason });
}

export async function listAgentRequests(): Promise<AgentRequest[]> {
  return invoke('list_agent_requests');
}

export async function decideAgentRequest(id: string, approve: boolean): Promise<AgentRequest | null> {
  return invoke('decide_agent_request', { id, approve });
}

export async function settleAgentRequest(id: string, ok: boolean, outcome: string): Promise<AgentRequest | null> {
  return invoke('settle_agent_request', { id, ok, outcome });
}

export async function moveTabToWindow(
  sourceWorkspaceId: string,
  sourcePaneId: string,
  tabId: string,
  targetLabel: string,
  targetWorkspaceId: string,
): Promise<void> {
  return invoke('move_tab_to_window', { sourceWorkspaceId, sourcePaneId, tabId, targetLabel, targetWorkspaceId });
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

/** Reload another app window's webview (its PTYs live on). */
export async function reloadWindow(label: string): Promise<void> {
  return invoke('reload_window', { label });
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

/** Open a local file with the OS's default app for it (Files "Open With Default App"). */
export async function openWithDefaultApp(path: string): Promise<void> {
  return invoke('open_with_default_app', { path });
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

/** The one spelling an editor tab keeps for a local file: folder canonicalized, name kept. */
export async function canonicalEditorPath(path: string): Promise<string> {
  return invoke('canonical_editor_path', { path });
}

/**
 * What each local file IS (device + inode, or a canonical path), in order: two spellings of one
 * file — a symlinked folder, a symlink, a wrong-case name — get the same string. Null for a
 * path whose stat didn't answer within a second (a tab on a network share that went away).
 */
export async function editorFileIdentities(paths: string[]): Promise<(string | null)[]> {
  return invoke('editor_file_identities', { paths });
}

/** A path clicked in an ssh tab with its folder resolved on the host (symlinks, `.`, `..`). */
export async function resolveRemoteEditorPath(sshCommand: string, path: string): Promise<string> {
  return invoke('resolve_remote_editor_path', { sshCommand, path });
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
  return invoke('archive_tab', { workspaceId, paneId, tabId, displayName, scrollback, cwd: savedCwd(tabId, cwd), sshCommand, remoteCwd });
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

// Workspace Share (docs/workspace-share.md) — mirrors src-tauri/src/share/

export type ShareRootKind = 'git' | 'plain';

export interface SharedRoot {
  id: string;
  path: string;
  kind: ShareRootKind;
  remotes: Record<string, string>;
  branch?: string | null;
}

export interface ShareTabContext {
  tab_id: string;
  cwd: string | null;
  ssh_command: string | null;
  remote_cwd: string | null;
  live_agent: { runtime: AgentRuntime; session_id: string | null } | null;
}

export interface ShareExportPreview {
  name: string;
  roots: (SharedRoot & { warning: string | null; tab_count: number })[];
  tabs: {
    id: string;
    name: string;
    kind: 'local' | 'remote' | 'editor' | 'remote_editor' | 'dropped';
    root_id: string | null;
    place: string | null;
    agent: { runtime: AgentRuntime; reason: string; forkable: boolean } | null;
    dropped_reason: string | null;
  }[];
  services: { id: string; name: string; env_names: string[] }[];
  note_count: number;
  task_count: number;
}

export interface ShareExportOptions {
  include_notes: boolean;
  include_tasks: boolean;
  services: { id: string; env_values: string[] }[];
  agent_tab_ids: string[];
}

export type ShareDirVerdict =
  | { verdict: 'clone' }
  | { verdict: 'use' }
  | { verdict: 'reject'; reason: string };

export type ShareProbe =
  | { outcome: 'reachable'; branch_exists: boolean | null }
  | { outcome: 'denied'; message: string }
  | { outcome: 'unverified'; message: string };

export interface ShareFileService {
  name: string;
  command: string;
  location?: { root_id: string; subpath: string } | null;
  env: { name: string; value?: string | null }[];
  auto_start: boolean;
  restart: string;
}

export interface ShareFile {
  format: string;
  version: number;
  exported_at: string;
  maiterm_version: string;
  workspace: { name: string; mesh?: boolean; panes: { id: string; name: string; tabs: SharedTab[] }[] };
  roots: SharedRoot[];
  services: ShareFileService[];
  notes?: { tabs: Record<string, unknown>; workspace: unknown[] } | null;
  tasks?: { tasks: unknown[] } | null;
  /** What the import took out of the file as unsafe to run (docs/workspace-share.md §4.2). */
  refused?: string[];
}

export type SharedLoc = { root_id: string; subpath: string };

export interface SharedTab {
  id: string;
  name: string;
  kind:
    | { type: 'local'; location?: SharedLoc | null }
    | { type: 'remote'; ssh_command: string; remote_cwd?: string | null }
    | { type: 'editor'; location: SharedLoc }
    | { type: 'remote_editor'; ssh_command: string; remote_path: string; file_path: string };
  auto_resume?: { enabled: boolean; command?: string | null } | null;
  agent?: { runtime: AgentRuntime; session_id?: string | null } | null;
  mesh_purpose?: string | null;
}

export type ShareRootCheck = { root_id: string; local_path: string; repo_name: string | null } & ShareDirVerdict;

export interface ShareImportPreview {
  file: ShareFile;
  roots: ShareRootCheck[];
  digest: string;
}

export interface ShareImportOptions {
  mapping: Record<string, string>;
  include_notes: boolean;
  include_tasks: boolean;
  services: number[];
  env_values: Record<number, Record<string, string>>;
  /** Review keys (`tab:<id>:resume|ssh|agent`, `service:<i>:start`) — nothing else runs. */
  approved: string[];
  /** The preview's digest: the build refuses a file that changed since the review. */
  digest: string;
}

export interface ShareAgentLaunch {
  tab_id: string;
  runtime: AgentRuntime;
  cwd: string | null;
  ssh_command: string | null;
  remote_cwd: string | null;
  fork_session_id: string | null;
}

export interface ShareImportResult {
  workspace: Workspace;
  launches: ShareAgentLaunch[];
  notices: string[];
}

export async function shareExportPreview(workspaceId: string, contexts: ShareTabContext[]): Promise<ShareExportPreview> {
  return invoke('share_export_preview', { workspaceId, contexts });
}

export async function shareExportWrite(workspaceId: string, contexts: ShareTabContext[], options: ShareExportOptions, path: string): Promise<void> {
  return invoke('share_export_write', { workspaceId, contexts, options, path });
}

export async function shareReadFile(path: string): Promise<ShareImportPreview> {
  return invoke('share_read_file', { path });
}

export async function shareCheckDirs(checks: { root: SharedRoot; dir: string }[]): Promise<ShareDirVerdict[]> {
  return invoke('share_check_dirs', { checks });
}

export async function shareProbe(requests: { url: string; branch: string | null }[]): Promise<ShareProbe[]> {
  return invoke('share_probe', { requests });
}

export async function shareCloneCommand(url: string, dest: string, branch: string | null): Promise<string> {
  return invoke('share_clone_command', { url, dest, branch });
}

export async function shareImportBuild(path: string, options: ShareImportOptions): Promise<ShareImportResult> {
  return invoke('share_import_build', { path, options });
}

export async function takePendingShareOpens(): Promise<string[]> {
  return invoke('take_pending_share_opens');
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
  /** The maiterm-tab mod's files: [path inside its folder, contents]. */
  claude_mod: [string, string][];
  /** Proxied MCP servers' remote entries (docs/mcp-proxy.md §6): written by name, never deleted. */
  mcp_proxy: { set: Record<string, unknown> };
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

/** Why the SSH MCP bridge is off in this launch (an isolated test instance), or null. */
export async function getSshBridgeRefusal(): Promise<string | null> {
  return invoke('get_ssh_bridge_refusal');
}

/**
 * The bridge values to export in a tab's ssh command, for the host that command targets.
 * Answerable before the tunnel exists — see buildSshCommand. Null while the MCP server
 * is still coming up, in which case the command is built without them, exactly as before.
 */
export async function getRemoteBridgeEnv(hostKey: string): Promise<{ port: number; auth: string } | null> {
  // A stored command may keep the user's ssh program (cleanSshCommand); Rust keys on the host.
  return invoke('get_remote_bridge_env', { hostKey: sshArgsOf(hostKey) });
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

// ---------------------------------------------------------------------------
// Managed agent accounts (docs/login.md)
//
// Nothing in this section carries credential material. An account is named by id; what comes
// back is identity metadata the Accounts pane displays and the duplicate guard compares.
// ---------------------------------------------------------------------------

/** One row of the runtime registry. */
export interface AccountRuntimeInfo {
  slug: string;
  label: string;
  config_env: string;
  /** False until the runtime has had the §5.4 verification Claude has had. Shown as not yet
   *  available rather than hidden — a runtime that silently half-works is the bad outcome. */
  supported: boolean;
}

/** What `reconcileAccount` did. `displaced` names real files renamed aside, never deleted. */
export interface AccountReconciled {
  linked: string[];
  absent: string[];
  displaced: string[];
}

/** What a runtime reports about the account in one config root.
 *
 *  **Branch on `is_account_login`, never on `logged_in`.** Credential precedence is a
 *  fall-through, so a root holding none of our credentials still reports `logged_in: true` from
 *  another rung — and every such answer arrives with no email and no org id. Treating that as an
 *  identity makes every account look identical: the duplicate guard then fires on every new
 *  account and discards a valid login, and the §6.1 verification can never fail.
 *
 *  Optional fields are Rust `Option<String>` with no `skip_serializing_if`, so they are always
 *  present and `null` when unset — not `undefined`. */
export interface AccountIdentity {
  /** True only when this root's own claude.ai login answered AND carried an email. The only
   *  field you may branch on before using `email`/`org_id`. */
  is_account_login: boolean;
  /** What answered instead, when `is_account_login` is false — the "resolved source" to show
   *  rather than a green "logged in". */
  shadowed_by: string | null;
  logged_in: boolean;
  auth_method: string | null;
  api_key_source: string | null;
  /** `firstParty`, `bedrock`, … — set when a cloud provider answered. */
  api_provider: string | null;
  email: string | null;
  org_id: string | null;
  org_name: string | null;
  plan: string | null;
}

/** Variables to set AND to remove when spawning under an account. The removals are not
 *  optional: a leftover `CLAUDE_SECURESTORAGE_CONFIG_DIR` collapses every account onto one
 *  credential, silently. */
export interface AccountSpawnEnv {
  set: [string, string][];
  unset: string[];
}

/** The runtimes maiTerm knows about, supported or not. */
export async function listAccountRuntimes(): Promise<AccountRuntimeInfo[]> {
  return invoke('list_account_runtimes', {});
}

/** Create or repair an account's config root. Safe on every spawn, and meant to be. */
export async function reconcileAccount(runtime: string, accountId: string): Promise<AccountReconciled> {
  return invoke('reconcile_account', { runtime, accountId });
}

/** Ask the runtime who it thinks it is inside one account root. */
export async function readAccountIdentity(runtime: string, accountId: string): Promise<AccountIdentity> {
  return invoke('read_account_identity', { runtime, accountId });
}

export async function accountSpawnEnv(runtime: string, accountId: string): Promise<AccountSpawnEnv> {
  return invoke('account_spawn_env', { runtime, accountId });
}

/** The result of an add-account attempt. If the caller rejects the account — a §5.1 duplicate,
 *  or the user changed their mind — it MUST call `discardAccountRoot`, or the directory leaks. */
export interface NewAccount {
  account_id: string;
  identity: AccountIdentity;
}

/** Run a runtime's interactive sign-in against a fresh account root.
 *
 *  This is the §5.2 *fallback* path: the runtime opens the default browser, which may already
 *  hold a session and silently return the account you already have. That is not prevented here —
 *  the §5.1 duplicate check on `identity` is what catches it. The in-app incognito webview that
 *  avoids the problem is a later upgrade to this same command. */
export async function beginAccountLogin(
  runtime: string,
  accountId: string,
  opts?: {
    timeoutSecs?: number;
    /** Where the sign-in link opens: `'default'` for the normal browser, a
     *  {@link PrivateBrowserInfo} id for a private window, or omitted to open nothing (the
     *  copy-to-clipboard path).
     *
     *  **maiTerm always decides this, never the runtime.** Its own launcher is shadowed either
     *  way — partly so a private sign-in does not also get a window signed in to the account
     *  being avoided, and partly because shadowing it is the only way to see the URL it really
     *  uses. The URL it *prints* redirects to a hosted page that shows a code to paste, which is
     *  a dead end when the child has no stdin. */
    openWith?: string;
  },
): Promise<NewAccount> {
  return invoke('begin_account_login', {
    runtime,
    accountId,
    timeoutSecs: opts?.timeoutSecs ?? null,
    openWith: opts?.openWith ?? null,
  });
}

/** Emitted once per sign-in, as soon as the runtime prints its authorization URL.
 *
 *  Broadcasts to every window, so a listener MUST match `account_id` against the attempt it
 *  started — otherwise a second window signing in at the same time overwrites the link. */
export const ACCOUNT_LOGIN_URL_EVENT = 'account-login-url';

/** The payload of {@link ACCOUNT_LOGIN_URL_EVENT}.
 *
 *  `url` is not credential material: it is the *start* of an OAuth flow, and whoever opens it
 *  still has to authenticate. It is the same link the runtime prints on its own output, and is
 *  safe to put on the clipboard — which is the point, since pasting it into a private window is
 *  how you sign in as a second account without logging the first one out (§5.1). */
export interface AccountLoginUrl {
  account_id: string;
  url: string;
  /** Whether maiTerm actually launched a browser for this URL. **False does not mean "failed"** —
   *  it is also false when the caller asked for no browser (copy-to-clipboard) and when the
   *  runtime opened its own because no shim could be installed. `open_error` distinguishes a
   *  failure from a deliberate no-op; the UI must not claim a window opened on either. */
  opened: boolean;
  open_error: string | null;
  /** True when this is the link the runtime PRINTED rather than the one it opened — the
   *  paste-a-code variant, which cannot complete because the CLI's stdin is null. The UI must
   *  not offer to open it: doing so walks the user into a dead end that looks like success. */
  paste_code: boolean;
  /** True for a mint: offer the code field as a FALLBACK.
   *
   *  **Not the inverse of `paste_code`, and not a statement that a code is required.**
   *  `setup-token` runs its own localhost callback and normally finishes in the browser with
   *  nothing typed; it also builds a manual URL whose page shows a code, and some browsers land
   *  there. So this means "a code may appear — have somewhere to put it", where `paste_code`
   *  means "this link cannot finish at all". The UI must not caption it as the next step: doing
   *  so sent a user hunting for a code that did not exist. */
  needs_code: boolean;
}

/** Hand a running mint the authorization code the browser showed.
 *
 *  A fallback. `claude setup-token` runs its own `127.0.0.1` callback and usually finishes in the
 *  browser, but it also builds a manual URL whose page shows a code instead, and some browsers
 *  land there. maiTerm runs the mint on a PTY — required anyway, since it is an ink TUI that
 *  prints nothing to a pipe — so there is something to type the code into. */
export async function submitAccountCode(accountId: string, code: string): Promise<void> {
  return invoke('submit_account_code', { accountId, code });
}

/** Offer the code field again after a code the agent would not take.
 *
 *  The agent validates the `<code>#<state>` shape itself and re-prompts on a bad one. Without
 *  this, one mistyped character left the dialog waiting on a mint that was waiting on it. */
export async function resetAccountCode(accountId: string): Promise<void> {
  return invoke('reset_account_code', { accountId });
}

/** A workspace that has tabs still running under the previous account. */
export interface AccountReloadWorkspace {
  id: string;
  name: string;
  /** Tabs with a live PTY — the only ones worth reloading. A suspended or never-opened tab
   *  picks the new account up whenever it next starts. */
  live_tabs: number;
}

/** A window, for the offer made after switching accounts. */
export interface AccountReloadWindow {
  window_id: string;
  /** The Tauri label — what {@link requestAccountReload} addresses. */
  label: string;
  name: string | null;
  workspaces: AccountReloadWorkspace[];
}

/** Emitted to ONE window, asking it to reload tabs still on the previous account. */
export const ACCOUNT_RELOAD_TABS_EVENT = 'accounts-reload-tabs';

export interface AccountReloadRequest {
  /** `null` means every workspace in the window. */
  workspace_ids: string[] | null;
  /** Window label to report completion to. */
  reply_to: string;
  /** Echoed back, so a caller with several requests in flight knows which finished. */
  request_id: string;
}

/** Emitted back to the requester once a window has finished reloading. */
export const ACCOUNT_RELOAD_DONE_EVENT = 'accounts-reload-done';

export interface AccountReloadDone {
  request_id: string;
  reloaded: number;
}

/** Report that this window finished reloading.
 *
 *  There is no other completion signal: the request is one-way, and the live tab counts look
 *  identical afterwards because the replacements are live too. Without this the dialog claims
 *  "Reloading…" forever. */
export async function reportAccountReloadDone(
  replyTo: string,
  requestId: string,
  reloaded: number,
): Promise<void> {
  return invoke('report_account_reload_done', { replyTo, requestId, reloaded });
}

/** Which windows and workspaces still have tabs running under the previous account.
 *
 *  Switching accounts cannot move a running tab: the config dir is in the environment its shell
 *  was exec'd with. This is what lets the UI offer the only real remedy — respawning them. */
export async function accountReloadTargets(): Promise<AccountReloadWindow[]> {
  return invoke('account_reload_targets', {});
}

/** Ask one window to reload its running tabs so they come back under the active account.
 *
 *  Addressed by window label, never broadcast — every window would otherwise reload its own
 *  tabs, turning "reload this workspace" into "reload everything". */
export async function requestAccountReload(
  label: string,
  workspaceIds: string[] | undefined,
  replyTo: string,
  requestId: string,
): Promise<void> {
  return invoke('request_account_reload', {
    label,
    workspaceIds: workspaceIds ?? null,
    replyTo,
    requestId,
  });
}

/** A browser on this machine that can be told to open a private window. */
export interface PrivateBrowserInfo {
  id: string;
  label: string;
}

/** Browsers that can open a private window, in preference order.
 *
 *  **An empty list is a normal answer.** Safari has no command-line switch for a private window,
 *  and neither do several others — callers must keep offering the link itself. */
export async function listPrivateBrowsers(): Promise<PrivateBrowserInfo[]> {
  return invoke('list_private_browsers', {});
}

/** Open a sign-in link in a private window, so it can be completed as a different account
 *  without signing the current one out of the browser (§5.1). */
export async function openPrivateWindow(browserId: string, url: string): Promise<void> {
  return invoke('open_private_window', { browserId, url });
}

/** Stop a sign-in that is still running. The child process is killed, so the browser flow
 *  cannot complete later and strand an authenticated root nothing knows about.
 *
 *  The caller supplies the account id to `beginAccountLogin` precisely so it has something to
 *  pass here while that call is still outstanding. */
export async function cancelAccountLogin(accountId: string): Promise<void> {
  return invoke('cancel_account_login', { accountId });
}

/** What a mint reports back. **Never the token** — docs/login.md §9.3 makes the surface
 *  status-only, and a token that crossed IPC would be one webview bug from a log file. */
export interface TokenMint {
  /** Unix seconds. Expiry is `minted_at + 1 year` and is derived from this alone — §7 forbids
   *  parsing the credential, which is a private format `auth status` does not expose anyway. */
  minted_at: number;
  /** Who the token ACTUALLY resolved as, read back before it was stored. Shown so the user can
   *  see the mint landed on the account they picked — the browser-session trap (§5.1) applies
   *  to minting exactly as it does to signing in. */
  identity: AccountIdentity;
}

/** Mint a long-lived `setup-token` for an account and keep it in the OS keychain (§6 step 1).
 *
 *  Opens a browser, same as a sign-in, and takes the same `openWith` — so the private-window
 *  and copy-link paths work here too. They matter MORE here: if the browser is still signed in
 *  to another account, the mint can land on that one instead.
 *
 *  Verified before it is stored: maiTerm asks what the token resolves to in an empty config
 *  root, where nothing else can answer. A token that came back as the wrong account, or as no
 *  account, is discarded rather than stored. */
export async function mintAccountToken(
  runtime: string,
  accountId: string,
  opts?: { timeoutSecs?: number; openWith?: string | null },
): Promise<TokenMint> {
  return invoke('mint_account_token', {
    runtime,
    accountId,
    timeoutSecs: opts?.timeoutSecs ?? null,
    openWith: opts?.openWith ?? null,
  });
}

/** Forget an account's remote token.
 *
 *  **This does not revoke it.** It stops maiTerm handing the token out; a host that already has
 *  it keeps working until the token expires (§9.4 — we build as if `auth logout` does not reach
 *  these, because nothing local does). Any UI calling this has to say so. */
export async function forgetAccountToken(accountId: string): Promise<void> {
  return invoke('forget_account_token', { accountId });
}

/** Does the keychain actually hold a token for this account?
 *
 *  **Ask this rather than trusting `token_minted_at`.** Rust stores the token before the
 *  frontend records the metadata, so every way that second step can fail — the two-instance
 *  save guard refusing, the window closing mid-mint, a reply landing in a destroyed webview —
 *  leaves a live one-year credential with nothing in state pointing at it. A UI keyed on the
 *  metadata then hides the only control that could remove it. */
export async function hasAccountToken(accountId: string): Promise<boolean> {
  return invoke('has_account_token', { accountId });
}

/** What `prepareRemoteAccountToken` decided about one tab's ssh session.
 *
 *  **`export` is a shell fragment naming a path, never a credential.** Rust reads the vault,
 *  pushes the token to the host on the stdin of its own ssh connection, and hands back only the
 *  expression the remote shell runs to pick it up. Nothing here has ever held token bytes, which
 *  is what keeps §9.3 true — agents can reach the frontend. */
export interface RemoteTokenPrep {
  status: 'not_applicable' | 'ready' | 'missing_credential' | 'failed';
  export: string | null;
  account_label: string | null;
  host: string | null;
  detail: string | null;
  /** This handoff's own name — a file name, not a credential. What `discardRemoteAccountToken`
   *  takes back. */
  handle: string | null;
}

/** Put the active account's remote token on this tab's ssh host, if that host is covered.
 *
 *  Returns `not_applicable` **without opening a connection** when the feature is off, no account
 *  is active, it has no token, or this host is not one it may reach — so the cost on the spawn
 *  path of not using this feature is a state read.
 *
 *  Callers should prefer `remoteAccountExport`, which also reports the states that need saying
 *  out loud. */
export async function prepareRemoteAccountToken(
  tabId: string,
  sshArgs: string,
  /** The fragment will be typed into an ssh that is ALREADY running (the bridge's typed-ssh
   *  path, the manual inject), so Rust binds the handoff to that process now. Leave false where
   *  maiTerm types a NEW ssh carrying the fragment in its argv — that one is recognised by it. */
  bindNow = false,
): Promise<RemoteTokenPrep> {
  // Without the user's ssh program: Rust finds the account by the host, and a program in front
  // read as the host matches none — the tab would come up as the host's own login (docs/login.md
  // §6.1), with nothing said.
  return invoke('prepare_remote_account_token', { tabId, sshArgs: sshArgsOf(sshArgs), bindNow });
}

/** Take back a handoff file that is not going to be used.
 *
 *  The push runs in parallel with the decision about whether to inject, so a caller that changes
 *  its mind has already put a standing one-year credential on that host — and nothing local
 *  revokes it (§9.4). Prefer `beginRemoteAccount`, which pairs this with the announcement. */
/** Take back ONE handoff — the one `handle` names (the prep's own), never whatever the tab's
 *  record holds now, which a newer prep may have replaced. */
export async function discardRemoteAccountToken(
  tabId: string,
  sshArgs: string,
  handle: string,
): Promise<void> {
  return invoke('discard_remote_account_token', { tabId, sshArgs, handle });
}

/** The ssh maiTerm typed for this tab has come up: bind its account handoff to that process
 *  (maiLink §14). Call from the ssh-up poll. Returns whether a handoff was bound. */
export async function bindRemoteAccount(tabId: string): Promise<boolean> {
  return invoke('bind_remote_account', { tabId });
}

/** Sign an account out and delete its config root — duplicate, cancelled sign-in, Remove, or
 *  "Clear setup".
 *
 *  The sign-out is not optional: deleting the directory does not touch the credential, which
 *  lives in a Keychain item keyed by the config-dir path, so without it every "removed" account
 *  leaves a valid credential behind. Best effort — a failure to sign out still removes the root,
 *  because leaving that behind as well would be worse.
 *
 *  Unlinks symlinks without following them; the root points at the user's real transcripts. */
export async function discardAccountRoot(runtime: string, accountId: string): Promise<void> {
  return invoke('discard_account_root', { runtime, accountId });
}

// Move project (docs/relocate.md)
export async function findMissingFolders(): Promise<MissingRoot[]> {
  return invoke('find_missing_folders');
}

export async function previewRelocation(old: string, newPath: string, moveFolder: boolean): Promise<RelocatePreview> {
  return invoke('preview_relocation', { old, new: newPath, moveFolder });
}

export async function relocateProject(old: string, newPath: string, moveFolder: boolean): Promise<RelocateOutcome> {
  return invoke('relocate_project', { old, new: newPath, moveFolder });
}

export async function folderExists(path: string): Promise<boolean> {
  return invoke('folder_exists', { path });
}

export async function projectRootOf(path: string): Promise<string> {
  return invoke('project_root_of', { path });
}

export async function pickFolder(startIn?: string | null): Promise<string | null> {
  return invoke('pick_folder', { startIn: startIn ?? null });
}

// MCP servers through maiTerm (docs/mcp-proxy.md)
export interface McpServerStatus {
  /** `connecting` | `connected` | `needs_sign_in` | `unreachable` | `error` | `disabled` */
  state: string;
  detail: string;
  since: number;
}

export interface McpServerView {
  id: string;
  url: string;
  /** `none` | `token` | `oauth` */
  auth: string;
  enabled: boolean;
  /** Empty: every workspace. */
  workspaces: string[];
  status: McpServerStatus;
  tools: string[];
  fetched_at: number | null;
  added_at: number;
  added_by: string;
  /** The name agents see it under (`<id>-dev` from a dev build). */
  entry_name: string;
  /** Runtimes where an entry of the human's own already holds that name. */
  collisions: string[];
  /** Entries of the human's own reaching the same URL: "Claude: linear-native". */
  duplicates: string[];
}

export interface McpProbe {
  auth: 'none' | 'token' | 'oauth';
  detail: string;
  issuer?: string;
  can_register: boolean;
  /** Entries of the human's own reaching the same URL: "Claude: linear". */
  duplicates: string[];
}

export async function mcpServersList(): Promise<McpServerView[]> {
  return invoke('mcp_servers_list');
}

export async function mcpServerProbe(url: string): Promise<McpProbe> {
  return invoke('mcp_server_probe', { url });
}

export async function mcpServerAdd(
  id: string,
  url: string,
  auth: string,
  opts: { token?: string | null; clientId?: string | null; clientSecret?: string | null; requestId?: string | null } = {},
): Promise<McpServerView> {
  return invoke('mcp_server_add', {
    id, url, auth,
    token: opts.token ?? null,
    clientId: opts.clientId ?? null,
    clientSecret: opts.clientSecret ?? null,
    requestId: opts.requestId ?? null,
  });
}

export async function mcpServerRemove(id: string): Promise<void> {
  return invoke('mcp_server_remove', { id });
}

export async function mcpServerSetToken(id: string, token: string): Promise<void> {
  return invoke('mcp_server_set_token', { id, token });
}

export async function mcpServerSetEnabled(id: string, enabled: boolean): Promise<McpServerView> {
  return invoke('mcp_server_set_enabled', { id, enabled });
}

export async function mcpServerSetWorkspaces(id: string, workspaces: string[]): Promise<McpServerView> {
  return invoke('mcp_server_set_workspaces', { id, workspaces });
}

export async function mcpServerRefresh(id: string): Promise<McpServerView> {
  return invoke('mcp_server_refresh', { id });
}

/** Names held by the human's own MCP entries in Claude's and Codex's configs. */
export async function mcpNativeNames(): Promise<string[]> {
  return invoke('mcp_native_names');
}

/** Start a sign-in: the URL to open in the browser. */
export async function mcpServerSignIn(id: string): Promise<string> {
  return invoke('mcp_server_sign_in', { id });
}

/** Remove the human's own entries for this server (same name or URL). Returns what went. */
export async function mcpServerAdoptNative(id: string): Promise<string[]> {
  return invoke('mcp_server_adopt_native', { id });
}

// ── Browser link: the maiTerm Chrome extension (docs/browser-link.md) ──

export interface BrowserLinkView {
  maiterm_tab_id: string;
  chrome_tab_id: number;
  /** What the agent calls this Chrome tab: b1, b2… (an agent tab can hold several). */
  handle: string;
  /** The handle of the linked tab that opened this one, when it joined that way. */
  opened_by: string | null;
  browser: string;
  title: string;
  url: string;
  /** null when the maiTerm tab is gone (it is pruned on the next read). */
  tab_name: string | null;
}

export interface BrowserLinkStatus {
  enabled: boolean;
  /** One per connected browser profile. */
  connections: { browser: string; version: string }[];
  links: BrowserLinkView[];
  /** Browsers whose native-messaging folder holds maiTerm's host manifest. */
  installed_in: string[];
  /** The folder "Load unpacked" points the browser at. */
  extension_dir: string | null;
  extension_id: string;
}

/** Bring a linked Chrome tab to the front. */
export async function browserLinkReveal(maitermTabId: string, handle: string): Promise<void> {
  return invoke('browser_link_reveal', { maitermTabId, handle });
}

/** The live links alone (the tab strip's browser indicator); no manifest scan. */
export async function browserLinkLinks(): Promise<BrowserLinkView[]> {
  return invoke('browser_link_links');
}

export async function browserLinkStatus(): Promise<BrowserLinkStatus> {
  return invoke('browser_link_status');
}

/** Write or remove the host manifests to match the saved preference. */
export async function browserLinkSync(): Promise<void> {
  return invoke('browser_link_sync');
}

/** Unlink one of a tab's Chrome tabs by handle, or all of them when none is given. */
export async function browserLinkUnlink(maitermTabId: string, handle?: string): Promise<boolean> {
  return invoke('browser_link_unlink', { maitermTabId, handle: handle ?? null });
}

/** Refresh the unpacked extension folder; returns its path. */
export async function browserLinkPrepareUnpacked(): Promise<string> {
  return invoke('browser_link_prepare_unpacked');
}

export async function browserLinkOpenExtensionsPage(): Promise<boolean> {
  return invoke('browser_link_open_extensions_page');
}

/** The line a Hive tab's shell types to start or resume its agent (docs/hive.md §11.1). Type
 * it as is, never through trigger interpolation; on an error type nothing. */
export async function hiveLaunchCommand(tabId: string): Promise<string> {
  return invoke('hive_launch_command', { tabId });
}

/** Can this workspace become a Hive, and what the Create sheet starts from (docs/hive.md §3).
 *  `fromTabId` is the tab the human was last looking at: its folder becomes the default Primary. */
/** `fromTabIds`: the tabs whose folder may become the default Primary, first match wins. */
export async function hivePreflight(workspaceId: string, fromTabIds: string[]): Promise<HivePreflight> {
  return invoke('hive_preflight', { workspaceId, fromTabIds });
}

export async function hiveDetectFolder(path: string): Promise<HiveFolderInfo> {
  return invoke('hive_detect_folder', { path });
}

export async function hiveDetectTools(): Promise<HiveDetectedTool[]> {
  return invoke('hive_detect_tools');
}

/** Lay the Hive out on disk and install it, with its hidden coordinator tab, in one write. */
export async function hiveCreate(workspaceId: string, spec: HiveSpec): Promise<HiveCreated> {
  return invoke('hive_create', { workspaceId, spec });
}

/** The worker tabs a Hive view mounts: those whose worker is in a live phase (docs/hive.md §11.2).
 *  A stopped or failed worker stays down until its coordinator restarts it. */
export async function hiveLiveWorkerTabs(workspaceId: string): Promise<string[]> {
  return invoke('hive_live_worker_tabs', { workspaceId });
}

/** The Hive board's cards and headroom (docs/hive.md §4.3). Fetched on `hive-workers-changed` and
 *  task changes, never polled. */
export async function hiveWorkers(workspaceId: string): Promise<HiveBoard> {
  return invoke('hive_workers', { workspaceId });
}

/** Every worker's diff stat for the Hive view's mount (docs/hive.md §4.4): cached, or measured once
 *  now. After this the view keeps up through `hive-diff-stat`. */
export async function hiveDiffStats(workspaceId: string): Promise<HiveDiffStat[]> {
  return invoke('hive_diff_stats', { workspaceId });
}

/** A worker tab's meta: {@link TabMeta} without `queued`. */
export interface HiveWorkerMeta {
  tab_id: string;
  meta: TabMeta | null;
}

/** `hive-worker-meta`, sent to the Hive's window only. */
export interface HiveWorkerMetaEvent extends HiveWorkerMeta {
  workspace_id: string;
}

/** Every worker tab's meta for the Hive view's mount, read once for any not cached. After this the
 *  view keeps up through `hive-worker-meta`; nothing polls. */
export async function hiveWorkerMetas(workspaceId: string): Promise<HiveWorkerMeta[]> {
  return invoke('hive_worker_metas', { workspaceId });
}

/** A Hive paused by a usage limit it couldn't wait out starts workers again (docs/hive.md §7.8).
 *  The machine-wide breaker is timed and not lifted by this. */
export async function hiveResume(workspaceId: string): Promise<Hive> {
  return invoke('hive_resume', { workspaceId });
}

/** The Hive's last accepted `hiveFinish` report (docs/hive.md §11.4), or null before one. */
export async function hiveFinishReport(workspaceId: string): Promise<HiveFinishRecord | null> {
  return invoke('hive_finish_report', { workspaceId });
}

/** The human's queued commands and resource limits (docs/hive.md §6.8), checked and saved in
 *  Rust. Answers the Hive as saved; the window's mirror also follows `hive-changed`. */
export async function hiveSetRunQueue(workspaceId: string, runQueue: RunQueueSettings): Promise<Hive> {
  return invoke('hive_set_run_queue', { workspaceId, runQueue });
}

/** The checkpoints (each "coordinator" | "ask"), the prompt timeout and the daily digest
 *  (docs/hive.md §7.4), checked and saved in Rust; the coordinator is told what changed. */
export async function hiveSetAutonomy(workspaceId: string, checkpoints: Record<string, string>, autonomy: AutonomySettings): Promise<Hive> {
  return invoke('hive_set_autonomy', { workspaceId, checkpoints, autonomy });
}

/** Hive header → Budget (docs/hive.md §10.3, §14.16): the budget, output folders and generator
 *  hosts, checked and saved in Rust. A raise past the spend closes the budget row. Answers the Hive
 *  as saved; the window's mirror also follows `hive-changed`. */
export async function hiveSetGallerySettings(workspaceId: string, settings: GallerySettingsChange): Promise<Hive> {
  return invoke('hive_set_gallery_settings', { workspaceId, settings });
}

/** The hosts the Hive's chosen generators need now, from a fresh detection; nothing is saved. */
export async function hiveDetectGeneratorHosts(workspaceId: string): Promise<string[]> {
  return invoke('hive_detect_generator_hosts', { workspaceId });
}

/** The human takes one of a worker's prompts over from the coordinator (docs/hive.md §7.4). */
export async function hiveTakeOver(workspaceId: string, tabId: string, key: string): Promise<void> {
  return invoke('hive_take_over', { workspaceId, tabId, key });
}

/** Who holds each open worker prompt of a Hive in this window; it then follows `hive-prompts-changed`. */
export async function hivePromptRoutes(workspaceId: string): Promise<HivePromptRoute[]> {
  return invoke('hive_prompt_routes', { workspaceId });
}

/** The human answered a blocker in a Hive (docs/hive.md §7.6): Rust's decision log takes it, the
 *  route goes, and the coordinator hears of a worker's. `blocker` is the store's copy from before
 *  the answer cleared it. Answers the decision id, or null when nothing was written (not a Hive,
 *  already logged). Called only by `overlordStore.answerBlocker`. */
export async function hiveBlockerAnswered(workspaceId: string, taskId: string, blocker: TaskBlocker, answer: HiveBlockerAnswerArg): Promise<string | null> {
  return invoke('hive_blocker_answered', { workspaceId, taskId, blocker, answer });
}

/** A retired Hive worker's chat (docs/hive.md §4.2, §8.4): the sessions Rust recorded for its task,
 *  in order, a `session_divider` turn between them. Never a path from here. */
export async function getTaskTranscript(hiveId: string, taskId: string): Promise<ChatTurn[]> {
  return invoke('get_task_transcript', { hiveId, taskId });
}

/** The gallery view's rows (docs/hive.md §10.4), newest first; the trash only with `include_trash`. */
export async function galleryList(workspaceId: string, filter: GalleryListFilter): Promise<GalleryAssetRow[]> {
  return invoke('gallery_list', { workspaceId, filter });
}

/** One asset whole: recipe, lineage and its stored path. */
export async function galleryGet(workspaceId: string, id: string): Promise<GalleryAssetDetail> {
  return invoke('gallery_get', { workspaceId, id });
}

export async function galleryCollections(workspaceId: string): Promise<GalleryCollection[]> {
  return invoke('gallery_collections', { workspaceId });
}

/** Files dropped on the gallery, filed as the human's (docs/hive.md §10.3). */
export async function galleryAddFiles(workspaceId: string, paths: string[]): Promise<GalleryAddResult> {
  return invoke('gallery_add_files', { workspaceId, paths });
}

/** The human's tags, star and any status (docs/hive.md §10.4). A pick on an asset the coordinator
 *  recommended answers it; one that changes the coordinator's own finals call overrides it. */
export async function galleryTag(workspaceId: string, ids: string[], change: GalleryTagChange): Promise<Omit<GalleryAssetDetail, 'path' | 'parent_chips' | 'child_chips'>[]> {
  return invoke('gallery_tag', { workspaceId, ids, change });
}

export async function galleryCollect(workspaceId: string, change: GalleryCollectChange): Promise<GalleryCollection> {
  return invoke('gallery_collect', { workspaceId, change });
}

export async function galleryRenameCollection(workspaceId: string, collection: string, name: string): Promise<GalleryCollection> {
  return invoke('gallery_rename_collection', { workspaceId, collection, name });
}

/** The grouping goes; its assets stay. */
export async function galleryDeleteCollection(workspaceId: string, collection: string): Promise<GalleryCollection> {
  return invoke('gallery_delete_collection', { workspaceId, collection });
}

/** Into the gallery's trash (docs/hive.md §10.7): hidden, the file kept until the trash is emptied. */
export async function galleryTrash(workspaceId: string, ids: string[]): Promise<number> {
  return invoke('gallery_trash', { workspaceId, ids });
}

export async function galleryRestore(workspaceId: string, ids: string[]): Promise<number> {
  return invoke('gallery_restore', { workspaceId, ids });
}

/** Files and thumbnails go; each record stays, so lineage and the ledger hold. */
export async function galleryEmptyTrash(workspaceId: string): Promise<number> {
  return invoke('gallery_empty_trash', { workspaceId });
}

/** The whole trash's count, whatever the view's filters: what emptying it deletes. */
export async function galleryTrashCount(workspaceId: string): Promise<number> {
  return invoke('gallery_trash_count', { workspaceId });
}

/** The human's Export… (docs/hive.md §10.6): assets, or a collection in order, into any folder;
 *  new files with suffixes, or with `replace` over files of the same name. */
export async function galleryExport(workspaceId: string, ids: string[], collection: string | null, folder: string, replace: boolean): Promise<GalleryExported> {
  return invoke('gallery_export', { workspaceId, ids, collection, folder, replace });
}

/** Stored paths of assets out of the trash: what a composer attaches. */
export async function galleryPaths(workspaceId: string, ids: string[]): Promise<string[]> {
  return invoke('gallery_paths', { workspaceId, ids });
}

/** The coordinator's finals recommendations waiting on the human: asset id → status. */
export async function galleryRecommendations(workspaceId: string): Promise<Record<string, string>> {
  return invoke('gallery_recommendations', { workspaceId });
}

/** What deleting the Hive would keep and stop (docs/hive.md §11.2), before anything is touched. */
export async function hiveDeletePreview(workspaceId: string): Promise<HiveDeletePreview> {
  return invoke('hive_delete_preview', { workspaceId });
}

/** Delete the Hive: the human's click on the delete sheet. Clean worktrees go, dirty ones and every
 *  branch stay, the gallery stays unless `deleteGallery`. */
export async function hiveDelete(workspaceId: string, deleteGallery: boolean): Promise<HiveDeleteResult> {
  return invoke('hive_delete', { workspaceId, deleteGallery });
}

/** The Hive's land log, folded: undone and suspect lines, each repo's standing, open undos
 *  (docs/hive.md §8.6). */
export async function hiveLandLog(workspaceId: string): Promise<HiveLandLog> {
  return invoke('hive_land_log', { workspaceId });
}

/** The Hive's Memory view (docs/hive.md §9): the folder rebuilt first, then listed. */
export async function hiveMemoryList(workspaceId: string): Promise<HiveMemoryView> {
  return invoke('hive_memory_list', { workspaceId });
}

/** A memory file's text and the hash a later write or delete must name. */
export async function hiveMemoryRead(workspaceId: string, name: string): Promise<HiveMemoryFileText> {
  return invoke('hive_memory_read', { workspaceId, name });
}

/** The human's create (`expectSha` null) or edit of a memory file: provenance `human`. */
export async function hiveMemoryWrite(workspaceId: string, name: string, text: string, expectSha: string | null): Promise<HiveMemoryView> {
  return invoke('hive_memory_write', { workspaceId, name, text, expectSha });
}

export async function hiveMemoryDelete(workspaceId: string, name: string, expectSha: string): Promise<HiveMemoryView> {
  return invoke('hive_memory_delete', { workspaceId, name, expectSha });
}

/** The kept block: `set` the human's lines, `accept` the block on disk, `restore` the recorded one.
 *  `seen` is the block the view showed when they acted: refused if it changed since. */
export async function hiveMemoryKept(workspaceId: string, action: 'set' | 'accept' | 'restore', lines: string[] | null, seen: string[]): Promise<HiveMemoryView> {
  return invoke('hive_memory_kept', { workspaceId, action, lines, seen });
}

/** A file's index line into the kept block. */
export async function hiveMemoryPin(workspaceId: string, name: string): Promise<HiveMemoryView> {
  return invoke('hive_memory_pin', { workspaceId, name });
}

export async function hiveMemoryUnfiled(workspaceId: string, line: string, action: 'keep' | 'pin' | 'delete'): Promise<HiveMemoryView> {
  return invoke('hive_memory_unfiled', { workspaceId, line, action });
}

/** Memory → Decisions (docs/hive.md §7.6): every decision, newest first. It follows
 *  `hive-decisions-changed`. */
export async function hiveDecisionLog(workspaceId: string): Promise<HiveDecisionView[]> {
  return invoke('hive_decision_log', { workspaceId });
}

/** The human's Override… on one of the coordinator's decisions: logged, counted against the
 *  precedent it cited, and told to the coordinator with `note`. */
export async function hiveOverrideDecision(workspaceId: string, decisionId: string, note: string): Promise<HiveDecisionView> {
  return invoke('hive_override_decision', { workspaceId, decisionId, note });
}

/** Memory → Precedents (docs/hive.md §7.5): this Hive's and the global ones. */
export async function hivePrecedents(workspaceId: string): Promise<HivePrecedentsView> {
  return invoke('hive_precedents', { workspaceId });
}

/** The human's rewording of a precedent's rule (theirs from then on), this Hive's or a global one. */
export async function hivePrecedentEdit(workspaceId: string, id: string, rule: string): Promise<HivePrecedentsView> {
  return invoke('hive_precedent_edit', { workspaceId, id, rule });
}

export async function hivePrecedentDelete(workspaceId: string, id: string): Promise<HivePrecedentsView> {
  return invoke('hive_precedent_delete', { workspaceId, id });
}

/** Make global: only ever the human's click, never a tool. */
export async function hivePrecedentPromote(workspaceId: string, id: string): Promise<HivePrecedentsView> {
  return invoke('hive_precedent_promote', { workspaceId, id });
}

/** What undoing land log line `id` would write, for the inline confirmation. */
export async function hiveUndoPreview(workspaceId: string, id: string): Promise<HiveUndoPreview> {
  return invoke('hive_undo_preview', { workspaceId, id });
}

/** The human's Undo: their confirmation is the approval. `configFp` is what it showed, so a revert
 *  that has come to change other config is refused rather than written. Answers the result. */
export async function hiveUndoLand(workspaceId: string, id: string, configFp: string | null): Promise<{ state: string; result?: string }> {
  return invoke('hive_undo_land', { workspaceId, id, configFp });
}

/** Retry a held land with its own commits (the worker column's menu). Answers its result. */
export async function hiveRetryLand(workspaceId: string, taskId: string): Promise<{ state: string; result?: string; note?: string }> {
  return invoke('hive_retry_land', { workspaceId, taskId });
}
