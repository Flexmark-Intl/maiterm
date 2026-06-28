import type { AgentRuntime } from '$lib/agents/types';

export type TabType = 'terminal' | 'editor' | 'diff';

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
  tab_type: TabType;
  editor_file: EditorFileInfo | null;
  diff_context: DiffContext | null;
  import_highlight?: boolean;
  agent_bridge?: AgentBridge | null;
  /** Which AI agent runtime this tab is running; detected at initSession. */
  runtime?: AgentRuntime | null;
  /** maiLink: when true, this tab is exposed to the maiLink mobile companion as a chat. */
  mailink_native?: boolean;
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
  /** maiLink doorbell: push-relay endpoint (e.g. the Cloudflare worker /push). Empty ⇒ off. */
  mailink_relay_url?: string | null;
  /** maiLink doorbell: shared secret sent as x-mailink-relay-key to the relay. */
  mailink_relay_key?: string | null;
}

export interface WindowData {
  id: string;
  label: string;
  workspaces: Workspace[];
  active_workspace_id: string | null;
  sidebar_width: number;
  sidebar_collapsed: boolean;
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
