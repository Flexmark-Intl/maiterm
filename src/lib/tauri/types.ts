import type { AgentRuntime } from '$lib/agents/types';

export type TabType = 'terminal' | 'editor' | 'diff' | 'board';

export interface EditorFileInfo {
  file_path: string;
  is_remote: boolean;
  remote_ssh_command: string | null;
  remote_path: string | null;
  language: string | null;
}

export interface DiffContext {
  request_id: string;
  file_path: string;
  old_content: string;
  new_content: string;
  tab_name: string;
}

export interface AgentBridge {
  partner_tab_id: string;
  partner_label: string;
  partner_session_id?: string | null;
  /** "caller" | "fork" */
  role: string;
  turn: number;
}

export interface Tab {
  id: string;
  name: string;
  pty_id: string | null;
  scrollback: string | null;
  custom_name: boolean;
  /** Pinned tabs cluster at the front of the bar and are exempt from active/suspended regrouping. */
  pinned?: boolean;
  restore_cwd: string | null;
  restore_ssh_command: string | null;
  restore_remote_cwd: string | null;
  auto_resume_cwd: string | null;
  auto_resume_ssh_command: string | null;
  auto_resume_remote_cwd: string | null;
  auto_resume_command: string | null;
  auto_resume_remembered_command: string | null;
  auto_resume_pinned: boolean;
  auto_resume_enabled: boolean;
  notes: string | null;
  notes_mode: string | null;
  notes_open: boolean;
  /** Whether this tab's task panel is open (docs/tasks.md §6). */
  tasks_open?: boolean;
  /** Exempt from Overlord (docs/overlord.md §11): no rules, probes, proposals, cards or
   *  agent tools touch this tab. `Workspace.overlord_exempt` covers a whole workspace. */
  overlord_exempt?: boolean;
  /** Composer dock open state: null/absent = inherit composer_default_open pref. */
  composer_open?: boolean | null;
  /** Persisted in-progress composer draft text. */
  composer_draft?: string | null;
  /** Mesh Workspace one-line purpose for this agent (persisted across restarts). */
  mesh_purpose?: string | null;
  trigger_variables: Record<string, string>;
  last_cwd: string | null;
  archived_name: string | null;
  archived_at: string | null;
  /** ISO 8601 timestamp of when the tab was last suspended; null/absent while live. */
  suspended_at?: string | null;
  /** Task rows parked on this tab while it is ARCHIVED — moved out of `Workspace.tasks` by
   *  archive and moved back by restore. Always absent on a live tab. */
  archived_tasks?: Task[];
  /** True when this tab was live at the moment its workspace was suspended —
   *  resuming the workspace respawns exactly these tabs. */
  wake_on_resume?: boolean;
  tab_type: TabType;
  editor_file: EditorFileInfo | null;
  diff_context: DiffContext | null;
  import_highlight?: boolean;
  agent_bridge?: AgentBridge | null;
  /** Which AI agent runtime this tab is running; detected at initSession. */
  runtime?: AgentRuntime | null;
  /** maiLink: when true, this tab is exposed to the maiLink mobile companion as a chat. */
  mailink_native?: boolean;
  /** maiLink exception: when true, hold this tab back from maiLink even while the
   *  "make all tabs available" preference is on (ignored in designate-only mode). */
  mailink_excluded?: boolean;
  /** Comms thread bindings (/maiterm resolve + chat-monitor pickups): the watcher
   *  forwards each bound thread's @bot replies into this tab's agent session. */
  comms_bindings?: CommsBinding[];
  /** Chat monitoring: this tab picks up @bot summons from the listed channels. */
  comms_monitor?: CommsMonitor | null;
}

/** A tab's binding to an external chat thread (Mattermost) — see /maiterm resolve. */
export interface CommsBinding {
  provider: string;
  server_url: string;
  channel_id: string;
  root_id: string;
  permalink: string;
  last_seen_create_at: number;
  bound_at: number;
  /** True on threads the agent opened itself: every reply is delivered, not just @mentions. */
  deliver_all_replies?: boolean;
}

/** Chat-monitoring config for a tab (operator-designated pickup target). */
export interface CommsMonitor {
  channels: CommsMonitorChannel[];
}

export interface CommsMonitorChannel {
  id: string;
  name: string;
  team_name: string;
  last_seen_create_at: number;
}

/** A channel the bot is a member of (comms_list_bot_channels). */
export interface BotChannel {
  id: string;
  display_name: string;
  team_name: string;
  team_display_name: string;
}

export interface Pane {
  id: string;
  name: string;
  tabs: Tab[];
  active_tab_id: string | null;
}

export type SplitDirection = 'horizontal' | 'vertical';

export interface SplitLeaf {
  type: 'leaf';
  pane_id: string;
}

export interface SplitBranch {
  type: 'split';
  id: string;
  direction: SplitDirection;
  ratio: number;
  children: [SplitNode, SplitNode];
}

export type SplitNode = SplitLeaf | SplitBranch;

export interface WorkspaceNote {
  id: string;
  content: string;
  mode: string | null;
  created_at: string;
  updated_at: string;
}

export type MeshTopicState = 'open' | 'complete';

/** A first-class conversation thread in a Mesh Workspace (docs/mesh-workspace.md).
 *  Mirrors the Rust MeshTopic; persisted as a Vec on the workspace. */
export interface MeshTopic {
  id: string;
  label: string;
  /** Case/separator-normalized label used for dedup. */
  normalized_label: string;
  /** Tab id of the agent that started the topic (the completion authority). */
  owner_tab_id: string;
  state: MeshTopicState;
  participants: string[];
  /** Per-topic turn counter (soft cap + mesh-map edge weight). */
  turn: number;
  created_at: string;
  updated_at: string;
}

/** The six board lanes, in board order — `backlog` is LEFTMOST even though new tasks
 *  start in `todo`, because parking a task is a move BACKWARDS out of the active flow.
 *
 *  `backlog` is a parking lot, not a to-do list: next month, future ideas, low-priority.
 *  It is deliberately exempt from staleness signals — a parked item is *supposed* to sit
 *  untouched, and nagging about it would make the backlog a source of interruptions
 *  instead of the thing that protects focus. `todo` is the not-started-yet lane. */
export type TaskStatus = 'backlog' | 'todo' | 'active' | 'blocked' | 'review' | 'done' | 'dropped';

export type TaskOrigin = 'human' | 'agent' | 'overlord' | 'imported';

/** A unit of work owned by a workspace (docs/tasks.md). maiTerm is the source of truth
 *  for every writer — the side panel, agents over MCP, Overlord, and the Claude
 *  task-store importer. Mirrors the Rust `Task`. */
/** A named job inside a workspace (docs/tasks.md §4) — one agent tab is routinely asked
 *  to do two unrelated things, and this is how they stay apart. Mirrors the Rust
 *  `Workstream`. */
export interface Workstream {
  id: string;
  name: string;
  /** Case/whitespace-normalized name — the dedup key within a workspace. Recomputed by
   *  Rust on persist. */
  normalized_name: string;
  created_at: string;
  updated_at: string;
}

/** One line in a task's progress log. Mirrors the Rust `TaskNote`.
 *
 *  `detail` says what the task IS; this says what happened to it. They are separate
 *  because `detail` is a whole-field replace — recording why something is blocked by
 *  rewriting the spec destroys the spec. */
export interface TaskNote {
  at: string;
  text: string;
  /** Same vocabulary as `TaskOrigin`, minus 'imported' — a note is always written by
   *  someone present, never mirrored in. */
  by: 'human' | 'agent' | 'overlord';
}

export interface Task {
  id: string;
  title: string;
  /** Case/whitespace-normalized title — the dedup key within a tab. Recomputed by Rust
   *  on persist, so never hand-set it expecting it to survive. */
  normalized_title: string;
  /** Markdown body: acceptance criteria, links, what "done" means. The SPEC — the log
   *  lives in `notes`. */
  detail?: string | null;
  status: TaskStatus;
  /** Assignee tab; null = workspace backlog, unassigned. */
  tab_id?: string | null;
  /** Task ids that must finish first. */
  blocked_by?: string[];
  origin: TaskOrigin;
  created_at: string;
  updated_at: string;
  /** The named job this task belongs to; null = a loose task on the workspace. */
  workstream_id?: string | null;
  /** Append-only progress log, oldest first. Capped at `TASK_NOTE_CAP` on append and
   *  again by Rust before disk. */
  notes?: TaskNote[];
}

export interface Workspace {
  id: string;
  name: string;
  panes: Pane[];
  active_pane_id: string | null;
  split_root: SplitNode | null;
  workspace_notes: WorkspaceNote[];
  /** Mesh Workspace flag — every agent tab here is bridged N:M. */
  bridge_all?: boolean;
  /** maiLink flag — every agent tab in this workspace is exposed to maiLink as a chat. */
  mailink_native?: boolean;
  /** Topic threads (empty for normal workspaces). */
  mesh_topics?: MeshTopic[];
  /** This workspace's task list (docs/tasks.md). Order is the array order. */
  tasks?: Task[];
  /** Named task groups — one per distinct job in this workspace. */
  workstreams?: Workstream[];
  /** Overlord workspace flag — hosts the board + agent tab; at most one per window. */
  overlord?: boolean;
  /** Every tab in this workspace is exempt from Overlord (docs/overlord.md §11). */
  overlord_exempt?: boolean;
  archived_tabs: Tab[];
  import_highlight?: boolean;
  suspended?: boolean;
}

export type CursorStyle = 'block' | 'underline' | 'bar';

export type TriggerActionType = 'notify' | 'send_command' | 'set_tab_state' | 'enable_auto_resume' | 'replay_auto_resume';

export type MatchMode = 'regex' | 'plain_text' | 'variable';

export type TabStateName = 'alert' | 'question';

export interface TriggerActionEntry {
  action_type: TriggerActionType;
  command: string | null;
  title: string | null;
  message: string | null;
  tab_state: TabStateName | null;
}

export interface VariableMapping {
  name: string;
  group: number;
  template?: string;
}

export interface Trigger {
  id: string;
  name: string;
  description?: string | null;
  pattern: string;
  actions: TriggerActionEntry[];
  enabled: boolean;
  workspaces: string[];
  tabs: string[];
  cooldown: number;
  variables: VariableMapping[];
  plain_text: boolean;
  match_mode?: MatchMode | null;
  default_id?: string | null;
  user_modified?: boolean;
}

// ─── Overlord (docs/overlord.md) — Rust mirror in state/workspace.rs ─────────────────

export type OverlordCondition =
  | { event: 'context_pct'; at_or_above: number }
  | { event: 'turn_end' }
  | { event: 'commit' }
  | { event: 'tab_idle'; minutes: number }
  | { event: 'task_stale'; days: number }
  | { event: 'agent_unready' }
  | { event: 'no_todo_list' }
  | { event: 'permission_pending'; minutes: number }
  | { event: 'directive_unacked'; minutes: number };

export type OverlordAgentStateName = 'idle' | 'active' | 'permission';

/** The mechanical floor that makes envelope-free injection safe (docs/overlord.md §3).
 *  NOT proposable via Overlord's MCP surface — only the preferences UI writes these. */
export interface OverlordGuards {
  /** Agent states the target tab may be in; default ['idle']. */
  agent_state?: OverlordAgentStateName[];
  /** PTY output quiet for this long before injecting; default 3000. */
  min_quiet_ms?: number;
  /** Hard precondition: live agent REPL, else the directive lands in a bash shell. */
  require_live_repl: boolean;
  max_per_hour?: number;
  /** Serialize directives per tab (also keeps ack matching unambiguous). */
  only_if_no_outstanding: boolean;
}

export type OverlordGate =
  | { until: 'turn_end' }
  | { until: 'ack' }
  | { until: 'context_below'; pct: number }
  | { until: 'idle_ms'; ms: number };

export type OverlordStepTimeout = 'abort' | 'continue' | 'notify_human' | 'escalate_to_overlord';

export interface OverlordStep {
  kind: 'process' | 'slash';
  /** THE tunable field — the directive text. */
  text: string;
  /** Slash steps only: runtimes this step is valid for; omit = all. Engine skips + ledgers on mismatch. */
  runtimes?: ('claude' | 'codex' | 'gemini')[];
  /** Gate to await after injection; null/omitted = fire-and-forget. */
  await?: OverlordGate | null;
  timeout_seconds?: number;
  on_timeout?: OverlordStepTimeout;
}

export interface OverlordRule {
  id: string;
  name: string;
  description?: string | null;
  enabled: boolean;
  /** Workspace ids; [] = global. v1 scope is global + workspace only. */
  workspaces: string[];
  /** Seconds, per tab. */
  cooldown: number;
  default_id?: string | null;
  user_modified?: boolean;
  origin?: 'default' | 'user' | 'proposed';
  when: OverlordCondition;
  guards: OverlordGuards;
  /** 1+ steps; multi-step = ritual (gated sequence state machine). */
  sequence: OverlordStep[];
  /** Rule ids / default_ids this rule replaces in-scope (can't reuse the parent's default_id). */
  supersedes?: string[];
}

export type OverlordLedgerOutcome =
  | 'sent' | 'blocked_no_repl' | 'blocked_guard' | 'acked' | 'timed_out'
  | 'aborted' | 'skipped_runtime' | 'proposed';

/** Verbatim injection record (docs/overlord.md §3) — the only way to reconstruct
 *  "who told that tab to do what", since injections are indistinguishable from typing. */
export interface OverlordLedgerEntry {
  id: string;
  ts: string;
  tab_id: string;
  workspace_id: string;
  rule_id: string | null;
  origin: 'rule' | 'human' | 'overlord_judgment';
  step_index: number;
  /** VERBATIM injected bytes. */
  text: string;
  kind: 'process' | 'slash';
  outcome: OverlordLedgerOutcome;
}

export interface Preferences {
  ui_font_size: number;
  font_size: number;
  font_family: string;
  cursor_style: CursorStyle;
  cursor_blink: boolean;
  auto_save_interval: number;
  scrollback_limit: number;
  prompt_patterns: string[];
  clone_cwd: boolean;
  clone_scrollback: boolean;
  clone_ssh: boolean;
  clone_history: boolean;
  clone_notes: boolean;
  clone_auto_resume: boolean;
  clone_variables: boolean;
  number_duplicated_tabs: boolean;
  drag_to_split: boolean;
  theme: string;
  shell_title_integration: boolean;
  shell_integration: boolean;
  custom_themes: import('$lib/themes').Theme[];
  restore_session: boolean;
  session_restore_mode: string;
  notify_on_completion: boolean;
  notification_mode: string;
  notify_min_duration: number;
  notes_font_size: number;
  notes_font_family: string;
  notes_width: number;
  /** Width of the task side panel (docs/tasks.md §6). */
  tasks_width: number;
  notes_word_wrap: boolean;
  toast_font_size: number;
  toast_width: number;
  toast_duration: number;
  notification_sound: string;
  notification_volume: number;
  migrate_tab_notes: boolean;
  notes_scope: string | null;
  show_recent_workspaces: boolean;
  workspace_sort_order: string;
  show_workspace_tab_count: boolean;
  tab_button_style: string;
  terminal_renderer: string;
  triggers: Trigger[];
  hidden_default_triggers: string[];
  claude_triggers_prompted: boolean;
  /** Overlord master switch (per-window engine only ticks when enabled). */
  /** maiTerm task tracking (docs/tasks.md) — gates the MCP tools and the priming. */
  tasks_enabled: boolean;
  tasks_backlog_vocabulary_migrated: boolean;
  overlord_enabled: boolean;
  /** Rules land as proposed directives the human clicks to send (docs/overlord.md §3). */
  overlord_propose_mode: boolean;
  overlord_rules: OverlordRule[];
  hidden_default_overlord_rules: string[];
  claude_ide: boolean;
  claude_ide_ssh: boolean;
  claude_hooks: boolean;
  claude_auto_resume: boolean;
  codex_ide: boolean;
  codex_ide_ssh: boolean;
  codex_hooks: boolean;
  codex_auto_resume: boolean;
  codex_hooks_bypass_trust: boolean;
  composer_default_open: boolean;
  windows_shell: string;
  file_link_action: string;
  backup_directory: string | null;
  backup_interval: string;
  backup_exclude_scrollback: boolean;
  backup_trim_enabled: boolean;
  backup_trim_age: string;
  auto_suspend_minutes: number;
  group_active_tabs: boolean;
  auto_check_updates: boolean;
  quick_open_show_hidden: boolean;
  quick_open_show_ignored: boolean;
  /** Mesh Workspace soft per-topic turn cap (N) — delivery pauses here, awaiting resume. */
  mesh_soft_cap: number;
  /** Mesh Workspace hard per-topic turn ceiling (M ≫ N) — absolute backstop, complete-only. */
  mesh_hard_cap: number;
  /** Mesh Workspace per-topic TTL in minutes (0 = disabled) — time backstop. */
  mesh_topic_ttl_minutes: number;
  /** maiLink: master switch for the mobile-companion LAN bridge (off by default). */
  mailink_enabled?: boolean;
  /** maiLink: when true (default), every agent tab is available to paired phones minus
   *  per-tab opt-outs; when false, only tabs the user designates are available. */
  mailink_expose_all?: boolean;
  /** maiLink doorbell: OPTIONAL override for the shared push relay (self-hosters). Empty ⇒ built-in default. */
  mailink_relay_url?: string | null;
  /** Comms integration provider ("mattermost"; Slack may follow). */
  comms_provider?: string;
  /** Comms server base URL (e.g. https://chat.example.com). Empty/null = not configured. */
  comms_server_url?: string | null;
  /** Comms bot bearer token. Stored raw in state (no keychain layer); never exposed to MCP tools. */
  comms_bot_token?: string | null;
  /** Comms usernames whose thread @mentions carry full operator authority (others are scoped). */
  comms_authorized_users?: string[];
  /** Operator's free-text guidance for how the agent communicates on chat threads. */
  comms_instructions?: string | null;
  /** Comms usernames allowed to summon the bot from monitored channels (support-tier authority). */
  comms_pickup_users?: string[];
}

/** QR payload a phone scans to pair (docs/mailink-protocol.md §3.2). The phone dials
 *  `https://host:port` pinning `fp`, then POSTs `code` to `/mailink/v1/pair`. */
export interface MailinkPairingPayload {
  v: number;
  host: string;
  port: number;
  fp: string;
  code: string;
  name: string;
}

/** A paired maiLink device, sanitized for the Preferences list (no token hash / capability). */
export interface MailinkDevice {
  id: string;
  name: string;
  /** Push sender the relay uses: "apns" | "fcm" (absent until the phone registers for push). */
  push_platform?: string | null;
  /** APNs environment / FCM hint: "sandbox" | "production". */
  push_env?: string | null;
  /** True once the device registered both a push token and a relay capability (doorbell-ready). */
  has_push: boolean;
  created_at: number;
  last_seen_at: number;
}

export interface WindowData {
  id: string;
  label: string;
  /** Human-given window name (absent when unnamed — serde skip). */
  name?: string | null;
  workspaces: Workspace[];
  active_workspace_id: string | null;
  sidebar_width: number;
  sidebar_collapsed: boolean;
  /** Overlord board rows for this window (absent when empty — serde skip). */
}

export interface DuplicateWorkspaceResult {
  workspace: Workspace;
  tab_id_map: Record<string, string>;
}

export interface AppData {
  windows: WindowData[];
  preferences: Preferences;
}

export interface ShellInfo {
  id: string;
  name: string;
  path: string;
}

// Terminal backend types (alacritty_terminal)
export interface TerminalFrame {
  ansi: number[];
  cursor_x: number;
  cursor_y: number;
  cursor_visible: boolean;
  display_offset: number;
  total_lines: number;
  alternate_screen: boolean;
  has_selection: boolean;
}

/** What a viewport/selection command reports back. The pixels arrive on the
 *  term-frame event like every other frame; this is just the state the UI
 *  (scrollbar, selection affordances) needs from the response. */
export interface FrameMeta {
  display_offset: number;
  total_lines: number;
  alternate_screen: boolean;
  has_selection: boolean;
}

export interface ScrollInfo {
  display_offset: number;
  total_lines: number;
  viewport_rows: number;
  viewport_cols: number;
}

export interface SearchMatch {
  line: number;
  start_col: number;
  end_col: number;
  text: string;
}

export interface SearchResult {
  matches: SearchMatch[];
  total_count: number;
}

// OSC events from Rust
export interface OscCwdEvent { cwd: string; host: string | null; }
export interface OscShellEvent { cmd: string; exit_code: number | null; }

export interface ClaudeCodeToolRequest {
  request_id: string;
  tool: string;
  arguments: Record<string, unknown>;
}

// Live SCP upload progress, emitted as `scp-progress-{upload_id}`
export interface ScpProgress {
  upload_id: string;
  bytes_sent: number;
  total_bytes: number;
  percent: number;
  rate_bps: number;
  files_total: number;
  done: boolean;
  indeterminate: boolean;
}
