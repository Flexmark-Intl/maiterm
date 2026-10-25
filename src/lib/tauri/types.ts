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

/** When a follow-up comes due (docs/follow-ups.md §3). A string `kind` in Rust too — never an
 *  enum, which an older build would fail to parse and wipe state over. */
export interface FollowUpDue {
  kind: 'at' | 'service_ready' | 'service_stopped' | 'task_done' | 'script' | (string & {});
  /** kind "at": RFC 3339, wall clock. */
  at?: string | null;
  /** The workspace the condition was set in — the tab may have moved since. */
  workspace_id?: string | null;
  service_id?: string | null;
  task_id?: string | null;
  /** The service's name or the task's title, as it was when the condition was set. */
  label?: string | null;
  /** An event condition that has happened (RFC 3339) — due from then on. Set once, in Rust. */
  met_at?: string | null;
  /** What happened, for the agent: "it came up", "it was dropped", "the service was removed". */
  outcome?: string | null;
  /** kind "script" (§5.1): the watch script, inline; the folder it runs in; its schedule. */
  script?: string | null;
  cwd?: string | null;
  /** kind "remote_script" only (an ssh tab's): the tunnel key of the host it runs on, `cwd` being
   *  a folder there. Its own kind so an older build, which ignores this field, never runs it here. */
  host?: string | null;
  every_secs?: number | null;
  timeout_secs?: number | null;
  /** kind "script": approved to run. Set only in Rust — whatever is sent here is ignored. */
  approved?: boolean;
  /** kind "script": what the met run printed. */
  report?: string | null;
}

/** What the watch-script runner knows about one script's runs since this launch. */
export interface WatchStatus {
  last_run_at: string | null;
  /** "met" | "not_yet" | "broken" | "unreachable" (a remote script's host couldn't be reached) */
  last_result: string | null;
  detail: string | null;
  broken_runs: number;
  running: boolean;
}

/** A prompt to deliver back into its tab's agent later (docs/follow-ups.md). */
export interface FollowUp {
  id: string;
  text: string;
  due: FollowUpDue;
  /** "agent" | "human" | "maiterm" */
  author: string;
  created_at: string;
  expires_at?: string | null;
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
  /** The stack service this tab runs (docs/stack.md §3–§5); absent for an ordinary tab.
   *  The binding lives here and nowhere else. */
  service_id?: string | null;
  /** Which checkout this tab runs its service on (docs/hive.md §6.9): absent for the service's
   *  own folder, else `"checkout:<abs>"` | `"branch:<name>"`. A string in Rust; one tab per
   *  `(service_id, service_target)`. Rides with `service_id`: carried on reload, never on a
   *  duplicate, cleared by a move. */
  service_target?: string | null;
  /** This tab's part in its workspace's Hive (docs/hive.md §5): "coordinator" | "worker",
   *  absent for an ordinary tab. A string in Rust, never an enum. Out of the strip like a
   *  service, but an agent: ask `isStripHidden`/`isServiceTab`, never this field directly.
   *  A reload carries it; a duplicate never does. */
  hive_role?: string | null;
  /** Prompts scheduled back into this tab's agent (docs/follow-ups.md). Omitted by Rust when
   *  empty, so read it as `tab.follow_ups ?? []`. A reload moves them; a duplicate starts
   *  without them. */
  follow_ups?: FollowUp[];
  /** Exempt from Overlord (docs/overlord.md §11): no rules, probes, proposals, cards or
   *  agent tools touch this tab. `Workspace.overlord_exempt` covers a whole workspace. */
  overlord_exempt?: boolean;
  /** Muted (docs/mute.md): nothing from this tab notifies — toast, OS, sound, phone push.
   *  `Workspace.muted` covers a whole workspace; read `isTabMuted`, not this alone. */
  muted?: boolean;
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

/** Restart policy for a stack service. `on_change` needs a file watcher — v3. */
export type ServiceRestart = 'never' | 'on_crash';

/** Who declared a service. */
export type ServiceOrigin = 'human' | 'agent' | 'suggested';

/** Agent runtimes maiTerm can hold accounts for. Only `claude` is implemented — the Rust
 *  registry refuses the rest rather than half-wiring them (docs/login.md §5.4). */
export type AccountRuntime = 'claude' | 'codex' | 'gemini' | 'grok';

/** One managed agent account (docs/login.md).
 *
 *  **Carries no credential material.** Tokens live in the OS keychain under maiTerm's own
 *  service name; a `/login` credential is never ours to hold — it sits in the account's config
 *  root, which the runtime owns. This is metadata, and it is persisted to `aiterm-state.json`,
 *  which is plaintext.
 *
 *  Optional fields are `skip_serializing_if` in Rust, so they arrive as `undefined` rather than
 *  `null` — compare field-by-field with `?? null`, never `JSON.stringify`. */
export interface ManagedAccount {
  id: string;
  /** Widened to `string` because persisted state may name a runtime this build doesn't know;
   *  the Rust registry decides what is actually supported. */
  runtime: AccountRuntime | string;
  /** User-facing name. Defaults to the email or org read back after login. */
  label: string;
  /** `email` + `org_id` are the duplicate-detection key (§5.1): the browser reuses its
   *  claude.ai session, so "add a second account" can silently return the first. */
  email?: string;
  org_id?: string;
  org_name?: string;
  /** Plan as the runtime reports it ("max", "pro"). Display only. */
  plan?: string;
  /** Unix seconds. */
  created_at: number;
  /** Last time this root was confirmed to still resolve to THIS identity (§6.1) — a missing
   *  token falls through to whatever login exists on the host, so `loggedIn` is never
   *  treated as confirmation. */
  last_verified_at?: number;
  /** Hosts the remote token is enabled for (§6). Explicit, never inferred. */
  remote_hosts?: string[];
  /** Use this account on every SSH host no other account claims by name (§6).
   *
   *  At most one account per runtime holds this — two accounts each claiming every host is a
   *  question with no answer, and per §6.1 the wrong answer is invisible. Named hosts win over
   *  it, which is how one box is split off from the catch-all. */
  remote_all_hosts?: boolean;
  /** Unix seconds. Expiry is DERIVED from this + 1 year, never parsed from a credential. */
  token_minted_at?: number;
}

/** One service in a workspace's stack (docs/stack.md §3). Mirrors the Rust `Service`.
 *
 *  No `tab_id`: which tab runs it is derived from `Tab.service_id`. No status: that is
 *  runtime state in the stack store, never persisted. */
export interface Service {
  id: string;
  /** The handle agents use — "web", "api". */
  name: string;
  /** Case/whitespace-normalized name — the dedup key within a workspace. Recomputed by
   *  Rust on persist. */
  normalized_name: string;
  /** Typed into the tab's shell verbatim. */
  command: string;
  /** Absolute path. */
  cwd: string;
  /** Exported before the command. */
  env?: [string, string][];
  /** Mirrors `Tab.auto_resume_ssh_command`; always null in v1. */
  ssh_command?: string | null;
  auto_start?: boolean;
  /** Set by a start, cleared by a stop; a relaunch/resume starts the services that have it. */
  was_running?: boolean;
  restart?: ServiceRestart;
  /** Regex over stripped output; first match → ready. Optional named group `port`. */
  ready_pattern?: string | null;
  /** Last known endpoint — observed by the ready trigger or reported by an agent. Stale
   *  until the service is next ready (docs/stack.md §9). */
  port?: number | null;
  url?: string | null;
  origin: ServiceOrigin;
  /** An agent wrote this definition. Human-defined = `origin !== 'agent' && !agent_edited`:
   *  agents may report its endpoint, never edit it (`stack/model.ts` `refusedAgentEdits`). */
  agent_edited?: boolean;
  /** Running intent per copy on another checkout, by target (docs/hive.md §6.9); the own
   *  folder's copy keeps `was_running`. Cleared on duplicate, never shared. */
  running_targets?: string[];
  /** Another workspace's service, used here: never started, stopped or retargeted from here. */
  shared_from?: ServiceRef | null;
  /** "service" (absent) | "job": a run-queue job's ephemeral row (docs/hive.md §6.8), Rust's
   *  while its job runs. Never restarted, never `was_running`, never in the rollup. */
  kind?: string;
  created_at: string;
  updated_at: string;
}

/** Where a `shared_from` service really lives. */
export interface ServiceRef {
  workspace_id: string;
  service_id: string;
}

/** Which job, if any, leases a stack copy (docs/hive.md §6.8): a stop or retarget waits for it. */
export interface StackLeaseState {
  copy_key: string;
  job: { id: string; entry: string; worker_slug: string; state: string } | null;
  holders: number;
}

/** A stop or retarget waiting in Rust for a copy's lease (Run when free); `stack-lease-freed`
 *  carries it back to the window when the copy is free. */
export interface StackHeldAction {
  id: string;
  copy_key: string;
  workspace_id: string;
  window: string;
  /** "stop" | "restart" | "retarget". */
  action: string;
  to?: string | null;
  /** "human" | "agent". */
  by: string;
  /** The job it waited behind. */
  job: string;
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
  /** What a Blocked task is waiting for when it isn't another task. Lives only while
   *  `status` is 'blocked' (`settleBlocker`); dependencies are derived from `blocked_by`. */
  blocker?: TaskBlocker | null;
}

/** 'decision': the human chooses. 'action': only the human can do it (a sudo, a login).
 *  'external': waiting on something outside maiTerm (a review, CI, a person). */
export type BlockerKind = 'decision' | 'action' | 'external';

export interface BlockerOption {
  label: string;
  detail?: string | null;
  recommended?: boolean;
}

export interface TaskBlocker {
  kind: BlockerKind;
  /** One line: the question, the action, or what is being waited on. */
  question: string;
  context?: string | null;
  /** Choices for a decision; empty means the answer is free text. */
  options?: BlockerOption[];
  /** For an action: the exact command the human has to run. */
  command?: string | null;
  asked_at: string;
  asked_by: TaskNote['by'];
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
  /** Topic threads (empty for normal workspaces). On a linked mesh each topic lives on its
   *  owner's workspace. */
  mesh_topics?: MeshTopic[];
  /** Linked meshes: mesh workspaces in one window sharing this id act as one mesh. */
  mesh_group?: string | null;
  /** This workspace's task list (docs/tasks.md). Order is the array order. */
  tasks?: Task[];
  /** Named task groups — one per distinct job in this workspace. */
  workstreams?: Workstream[];
  /** The services this project runs (docs/stack.md). Definitions only. */
  stack?: Service[];
  /** Overlord workspace flag — hosts the board + agent tab; at most one per window. */
  overlord?: boolean;
  /** Every tab in this workspace is exempt from Overlord (docs/overlord.md §11). */
  overlord_exempt?: boolean;
  /** Every tab in this workspace is muted (docs/mute.md). */
  muted?: boolean;
  /** The Files root frozen for this workspace (docs/file-tree.md §4.2), in `watch::place` form:
   *  "/abs" locally, "user@host:/abs" on an ssh host. Absent/null = follow the active tab.
   *  Changed only through `setWorkspaceFileTreePin`. */
  file_tree_pin?: string | null;
  /** This workspace is a Hive (docs/hive.md §3.2); absent for an ordinary one. Never copied:
   *  duplicate and move-to-window are refused, a window duplicate skips it, share leaves it out. */
  hive?: Hive | null;
  archived_tabs: Tab[];
  import_highlight?: boolean;
  suspended?: boolean;
}

/** `Workspace.hive` (docs/hive.md §3.2). Every kind field is a string in Rust, never an enum.
 *  Fields a later build adds round-trip through Rust untouched. */
export interface Hive {
  /** Its own uuid, never the workspace id: names `<app data>/hive/<id>/`. */
  id: string;
  goal: string;
  instructions: string;
  folders: HiveFolder[];
  /** Workers at once, enforced in Rust. */
  cap: number;
  /** "base" | "integration"; fixed at creation. */
  landing_mode: string;
  models: HiveModels;
  tools: string[];
  /** checkpoint → "coordinator" | "ask". */
  checkpoints: Record<string, string>;
  budget_cents: number;
  push_after_land: boolean;
  settings: HiveSettings;
  criteria: string[];
  /** "active" | "paused" | "done". */
  status: string;
  done_at?: number | null;
  /** Names the `hive/<slug>` branches; stable across workspace renames. */
  slug: string;
  created_at: string;
}

/** Hive settings: the sections built so far, the rest round-tripped as they are. */
export interface HiveSettings {
  run_queue?: RunQueueSettings;
  land?: LandSettings;
  autonomy?: AutonomySettings;
  gallery?: GallerySettings;
  [section: string]: unknown;
}

/** hive/record.rs `GallerySettings` (docs/hive.md §10.3). */
export interface GallerySettings {
  /** Canonical folders a tool writes only to its own place: `galleryAdd` takes a path in one. */
  output_folders: string[];
  /** Hosts a worker's sandbox may reach for paid generation. */
  generator_hosts: string[];
}

/** hive/gallery/model.rs `MadeBy`: from the filer's identity, never a tool's arguments. */
export interface GalleryMadeBy {
  /** "worker" | "coordinator" | "human", or a kind a newer build writes. */
  kind: string;
  tab_id: string;
  role: string;
  worker_title: string;
  /** The worker's board number; 0 for anyone else. */
  badge: number;
}

/** hive/gallery/model.rs `Recipe` (docs/hive.md §10.2). */
export interface GalleryRecipe {
  tool: string;
  model?: string;
  prompt?: string;
  negative?: string;
  seed?: number;
  params?: unknown;
  source_url?: string;
  cost_usd?: number;
}

/** hive/gallery/model.rs `AssetRow`: the light row the gallery view lists. */
export interface GalleryAssetRow {
  id: string;
  /** "image" | "video" | "audio" | "document" | "model3d" | "other", or a newer one. */
  kind: string;
  file: string;
  mime: string;
  bytes: number;
  width?: number | null;
  height?: number | null;
  duration_ms?: number | null;
  title: string;
  made_by: GalleryMadeBy;
  task_id?: string | null;
  prompt_head?: string | null;
  model?: string | null;
  cost_micros: number;
  parents: string[];
  /** Children out of the trash: the "⧉ n variants" count. */
  children: number;
  /** "" | "picked" | "final" | "rejected". */
  status: string;
  starred: boolean;
  tags: string[];
  collections: string[];
  /** "pending" | "ready" | "none" | "failed". */
  thumb: string;
  /** `poster`, `strip`, `wave`: which pictures the thumbnailer made. */
  extras: Record<string, unknown>;
  created_at: string;
  created_ms: number;
  deleted_at?: string | null;
}

/** hive/gallery/view.rs `AssetDetail`: one asset whole, its stored path and its lineage. */
export interface GalleryAssetDetail extends Omit<GalleryAssetRow, 'prompt_head' | 'model' | 'cost_micros'> {
  sha256: string;
  caption?: string | null;
  source_path: string;
  recipe?: GalleryRecipe | null;
  purged: boolean;
  path: string;
  parent_chips: GalleryChip[];
  child_chips: GalleryChip[];
}

/** hive/gallery/view.rs `AssetChip`: an asset drawn elsewhere (a card, a chat). */
export interface GalleryChip {
  id: string;
  kind: string;
  title: string;
  /** The `gallery://` part to draw ("thumb" or "wave"); none means the kind's icon. */
  picture?: 'thumb' | 'wave' | null;
}

export interface GalleryCollection {
  id: string;
  name: string;
  asset_ids: string[];
  cover?: string | null;
  created_at: string;
}

/** hive/gallery/model.rs `ListFilter`: every field narrows; absent narrows nothing. */
export interface GalleryListFilter {
  kind?: string | null;
  /** "worker" | "coordinator" | "human", or a tab id. */
  made_by?: string | null;
  task_id?: string | null;
  collection?: string | null;
  status?: string | null;
  tag?: string | null;
  starred?: boolean | null;
  query?: string | null;
  include_trash?: boolean;
  limit?: number | null;
}

/** hive/gallery/view.rs `AddResult`: the human's drop. */
export interface GalleryAddResult {
  assets: GalleryAssetDetail[];
  refused: { path: string; error: string }[];
}

/** `hive-gallery-changed`, sent to the Hive's window only. */
export interface GalleryChangedEvent {
  workspace_id: string;
  hive_id: string;
  ids: string[];
}

/** hive/record.rs `AutonomySettings` (docs/hive.md §4.7, §7.6). */
export interface AutonomySettings {
  /** A coordinator-owned prompt unanswered this long notifies the human (60–3600, default 300). */
  prompt_timeout_secs: number;
  /** A daily summary of what the coordinator decided alone, in its chat. */
  daily_digest: boolean;
}

/** hive/prompts.rs `RouteView`: who holds one of a worker's open prompts (docs/hive.md §7.4). */
export interface HivePromptRoute {
  tab_id: string;
  task_id: string;
  /** The prompt id (`p_…`, `q_…`) or the blocker's key (`b_<task>_<asked_at>`). */
  key: string;
  /** "permission" | "question" | "trust" | "blocker". */
  kind: string;
  checkpoint: string;
  /** "coordinator" | "human". */
  owner: string;
  /** Why the human holds it: "ask", "timeout", "take_over", or the allowlist's reason. */
  why: string;
  opened_ms: number;
  /** When the coordinator's time runs out; 0 when the human holds it. */
  due_ms: number;
  taken_over: boolean;
  timed_out: boolean;
}

/** hive/prompts.rs `HumanNotice`, sent as `hive-prompt-notify`: a worker's prompt needs the
 *  human after all (outside the allowlist, the coordinator's time ran out, a checkpoint now Ask me). */
export interface HivePromptNotice {
  workspace_id: string;
  tab_id: string;
  /** "permission" | "question" | "trust" | "blocker". */
  kind: string;
  /** `Hive · #2 Session store`. */
  title: string;
  /** "timeout", "ask", "take_over", or the reason it is the human's. */
  why: string;
}

/** hive/decisions.rs `BlockerAnswerArg`: what the human chose on a blocker card. */
export interface HiveBlockerAnswerArg {
  /** The option's label, or null for words only. */
  option: string | null;
  text: string;
}

/** hive/record.rs `LandSettings` (docs/hive.md §8.1, §6.8). */
export interface LandSettings {
  /** Paths the human adds to the config rule: a land touching one goes to the human. */
  config_paths: string[];
  /** Lands passed to `hiveLand` in one repo share one land check. */
  batch: boolean;
}

/** Which commands a worker runs only through the queue, and the Hive's resource limits (§6.8). */
export interface RunQueueSettings {
  commands: QueuedCommand[];
  /** resource → capacity: lowers this Hive's share of a machine-wide pool, or sets its own. */
  resources: Record<string, number>;
}

export interface QueuedCommand {
  /** The Hive folder's name. */
  folder: string;
  /** The leading words `hiveRun`'s command must start with (`cargo test`). */
  prefix: string;
  /** Resources it holds; `{worker}` expands (`migrate:{worker}`). */
  needs: string[];
  /** Stack services it leases while it runs, by name. */
  services: string[];
  env: [string, string][];
  /** Paths beyond the worker's own its sandbox may write; placeholders and `~` expand. */
  extra_write: string[];
}

export interface HiveFolder {
  /** One path component: how briefs and worktree paths name it. */
  name: string;
  /** Canonical, symlinks resolved. */
  path: string;
  /** "git" | "plain". */
  kind: string;
  base?: string | null;
  setup?: string | null;
  land_check?: string | null;
  primary: boolean;
  /** Local-path remotes, read when added; on every agent's denyWrite (§7.3). */
  local_remotes: string[];
}

export interface HiveModels {
  coordinator: HiveModel;
  worker: HiveModel;
}

export interface HiveModel {
  runtime: string;
  model: string;
  effort: string;
}

/** What the Create sheet sends to `hive_create` (docs/hive.md §3). Rust re-detects every
 *  folder, so `local_remotes` and the canonical path are never taken from here. */
export interface HiveSpec {
  name: string;
  goal: string;
  instructions: string;
  folders: HiveFolder[];
  cap: number;
  /** The human confirmed a cap above the machine's ceiling. */
  cap_confirmed: boolean;
  landing_mode: string;
  models: HiveModels;
  tools: string[];
  budget_cents: number;
  push_after_land: boolean;
  /** The Stack step (docs/hive.md §3 step 3): definitions to import, and `shared_from`
   *  references to infrastructure another workspace runs. Rust re-checks every row. */
  stack: Service[];
}

export interface HiveReplaceable {
  pane_id: string;
  tab_id: string;
}

/** `hive_preflight`: what the Create sheet needs before it opens. */
export interface HivePreflight {
  eligible: boolean;
  reason: string | null;
  /** The fresh shell the Hive replaces, closed after Create. */
  replaceable_tab: HiveReplaceable | null;
  /** The folder of the tab the human was last looking at, when it's a folder on this disk. */
  default_primary: string | null;
  /** The machine's worker ceiling: min((RAM − 32 GB) / 16 GB, ncpu / 4), at least 1. */
  ceiling: number;
  default_cap: number;
  platform_ok: boolean;
  claude_ok: boolean;
  claude_detail: string | null;
}

/** `hive_detect_folder`: what the sheet shows for a picked folder (docs/hive.md §6.1, §6.6). */
export interface HiveFolderInfo {
  /** Canonical, symlinks resolved. */
  path: string;
  /** "git" (a repo's top folder) | "plain". */
  kind: string;
  /** Inside a repo but not its top: the repo the sheet offers instead. */
  repo_top: string | null;
  /** The checked-out branch, the default Base. */
  base: string | null;
  /** Claude would skip the trust dialog here. False is a warning, never a refusal. */
  trusted: boolean;
  local_remotes: string[];
}

/** A tool the Hive can make things with (docs/hive.md §3 Tools). `id` is what `Hive.tools` holds. */
export interface HiveDetectedTool {
  id: string;
  /** "mcp" | "cli" | "skill" | "comfyui". */
  kind: string;
  label: string;
  host: string | null;
}

export interface HiveCreated {
  workspace: Workspace;
  coordinator_tab_id: string;
  replace_tab: HiveReplaceable | null;
}

/** A worker's last verified `hiveReport` (hive/worker.rs `WorkerReport`). */
export interface HiveWorkerReport {
  /** "done" | "blocked" | "failed". */
  status: string;
  summary: string;
  receipts: { commits: string[]; files: string[]; assets: string[] };
  notes?: string | null;
  at: string;
}

export interface HiveCardFolder {
  name: string;
  /** "git" | "plain". */
  kind: string;
  branch?: string | null;
  target?: string | null;
  /** Commits its landing target doesn't have; null on the board, which runs no git. */
  ahead?: number | null;
}

/** One worker on the Hive board (hive/tools.rs `WorkerCard`, docs/hive.md §4.3). The task's lane,
 *  blocker and notes and the agent's state are read live from their stores, not from here. */
export interface HiveWorkerCard {
  badge: number;
  /** Its slug. */
  worker: string;
  title: string;
  task_id: string;
  tab_id?: string | null;
  role: string;
  /** "waiting" | "launching" | "running" | "blocked" | "reported" | "failed" | "stopped" |
   *  "landing" | "landed" | "retired", or a phase a newer build wrote. */
  phase: string;
  wait_reason?: string | null;
  /** Task ids it waits on. */
  depends_on: string[];
  /** The same, as `#badge slug`. */
  depends_on_labels: string[];
  folders: HiveCardFolder[];
  model: string;
  effort: string;
  last_report?: HiveWorkerReport | null;
  created_at: string;
  launched_at?: string | null;
  /** How it left, once it has (docs/hive.md §8.4): its column reads its recorded sessions. */
  retired?: HiveRetired | null;
  /** A land of its was undone (§8.6). */
  undone?: boolean;
  /** A land of its is suspect: its target failed its land check alone after it (§8.6). */
  suspect?: boolean;
  /** A reviewer's builder, as `#badge slug` (docs/hive.md §7.7). */
  reviews?: string | null;
  /** A builder's reviews so far. */
  review?: HiveReviewView | null;
  /** Its land while one runs or holds (docs/hive.md §8.1), and once landed. */
  land?: HiveLandView | null;
  /** Its task's newest assets out of the trash: shown across the card's top in place of the diff (§4.3). */
  assets?: GalleryChip[];
}

/** hive/land/mod.rs `LandView`: a land as its card shows it. */
export interface HiveLandView {
  id: string;
  /** "locking" | "checking" | "advancing" | "held" | "awaiting_human" | "queued_batch", or a newer one. */
  state: string;
  /** "Landed web; holding api: …" while held. */
  held_reason?: string | null;
  /** Whose land lock it waits on. */
  waiting_for?: string | null;
  /** The agent request it waits on while "awaiting_human": the worker's column shows that card. */
  request?: string | null;
  /** The batch (`Q-…`) it lands in (docs/hive.md §6.8). */
  batch?: string | null;
  /** Per repo: "pending" | "checking" | "tested" | "advancing" | "held" | "landed", and once it
   *  landed its push: "ok" | "held: <why>" | "off". */
  repos: { folder: string; status: string; pushed?: string | null }[];
}

/** hive/review.rs `ReviewView`: a builder's reviews as its card shows them. */
export interface HiveReviewView {
  /** Failed verdicts since its brief was last revised. */
  attempts: number;
  max_attempts: number;
  verdicts: number;
  /** "pass" | "fail": the latest verdict. */
  last?: string | null;
  escalated: boolean;
  /** "reassign" | "decompose" | "revise" | "accept" | "defer". */
  resolution?: string | null;
}

/** hive/worker.rs `Retired`. */
export interface HiveRetired {
  reason: string;
  at: string;
  /** What stayed on disk, one line each, with why. */
  kept: string[];
  /** The commit its land left the target at. */
  landed?: string | null;
}

/** hive/delete.rs `HiveDeletePreview`: what the delete sheet lists before anything is touched. */
export interface HiveDeletePreview {
  unlanded: { worker: string; folder: string; branch: string; target?: string | null; ahead: number }[];
  /** Worktrees that would stay, each with why. */
  dirty: string[];
  scratch: { worker: string; path: string; files: number; reported: number }[];
  gallery_files: number;
  gallery_bytes: number;
  live_agents: string[];
  /** While any worker is landing, delete is refused. */
  lands_in_flight: string[];
  /** Every branch the Hive made: all kept. */
  branches: string[];
}

/** hive/delete.rs `HiveDeleteResult`. */
export interface HiveDeleteResult {
  worktrees_removed: number;
  kept: string[];
  branches: string[];
  gallery_kept: boolean;
  trust_removed: number;
  requests_failed: number;
  folder_left?: string | null;
  dropped_tabs: string[];
  active_tabs: { pane_id: string; tab_id?: string | null }[];
}

export interface HiveHeadroom {
  cap: number;
  live: number;
  starting: number;
  spawns_left_this_hour: number;
  spawns_left_today: number;
  /** Claude's reset minute ("HH:MM") while the usage-limit breaker holds. */
  usage_limit_until?: string | null;
  usage_limit_reason?: string | null;
}

export interface HiveBoard {
  workers: HiveWorkerCard[];
  headroom: HiveHeadroom;
  hive_status: string;
  starts_workers: boolean;
  /** Epoch ms when the headroom changes with no event (the breaker ends, a spawn ages out of
   *  the hour or day): the board refetches then. */
  refresh_at?: number | null;
  /** Folders whose target fails its land check alone: lands there need fix_forward (§8.6). */
  suspect_repos?: string[];
}

/** hive/land/log.rs `LandLogEntry` folded (`FoldedEntry`): one line of the land log (§8.6). */
export interface HiveLandLogEntry {
  /** `L-…/<folder>`, `U-…/<folder>` for an undo, `B-…/<folder>` for a post-land check. */
  id: string;
  land: string;
  /** "land" | "batch_member" | "integration" | "undo" | "base_check", or a newer one. */
  kind: string;
  task_id: string;
  worker: string;
  title: string;
  folder: string;
  target: string;
  reviewed_sha: string;
  tested_sha: string;
  merge_commit?: string | null;
  base_before: string;
  base_after: string;
  summary: string;
  /** "coordinator" | "human". */
  by: string;
  request?: string | null;
  /** The coordinator's decision (`D-…`) that landed it: what the human's Undo overrides. */
  decision?: string | null;
  /** "pass" | "accepted" | "none". */
  review: string;
  undoes?: string | null;
  /** "ok" | "held: <why>" | "off". */
  pushed: string;
  /** "passed" | "failed" (a post-land check), "passed" on a land whose check passed, else "". */
  check: string;
  at: string;
  /** The undo line that reverted it. */
  undone_by?: string | null;
  suspect: boolean;
}

/** hive/land/log.rs `RepoStanding`. */
export interface HiveRepoStanding {
  folder: string;
  target: string;
  suspect: boolean;
  failed_check?: string | null;
  failed_at?: string | null;
  suspects: string[];
}

/** hive/land/undo.rs `LandLogView`: what the land log view shows. */
export interface HiveLandLog {
  /** Oldest first. */
  entries: HiveLandLogEntry[];
  repos: HiveRepoStanding[];
  /** Undos that run, wait on the human or hold. */
  undos: { id: string; undoes: string; state: string; by: string; held_reason?: string | null; request?: string | null }[];
  /** The integration mode: Undo is the human's alone. */
  integration: boolean;
}

/** hive/memory `MemoryFileView`: one file in the Hive's memory folder (docs/hive.md §9). */
export interface HiveMemoryFile {
  /** Relative to `memory/`, `/`-separated in a subfolder. */
  name: string;
  title: string;
  description: string;
  type: string;
  modified_ms: number;
  size: number;
  /** Rust's provenance: `coordinator`, `#2 Session store`, `human`, `unknown`, `maiTerm`. */
  by: string;
  /** `coordinator` | `worker` | `human` | `unknown` | `maiterm`. */
  by_kind: string;
  created_by: string;
  indexed: boolean;
  /** Folded into an `archive-*.md` line to keep MEMORY.md inside what Claude reads. */
  folded: boolean;
  /** Linked from the kept block, so not listed again in the index. */
  pinned: boolean;
  /** Listed, never indexed (plan D6). */
  in_subfolder: boolean;
  /** maiTerm's own archive fold: read-only. */
  archive: boolean;
}

export interface HiveMemoryBudget {
  lines: number;
  bytes: number;
  kept_lines: number;
  kept_bytes: number;
  max_lines: number;
  max_bytes: number;
  kept_max_lines: number;
  kept_max_bytes: number;
}

/** hive/memory `MemoryView`: what the Memory view shows. */
export interface HiveMemoryView {
  files: HiveMemoryFile[];
  /** The kept block as it is on disk. */
  kept: string[];
  /** `unfiled.md`'s lines not yet kept there by the human. */
  unfiled: string[];
  status: string | null;
  kept_changed: boolean;
  budget: HiveMemoryBudget;
}

export interface HiveMemoryFileText {
  text: string;
  sha256: string;
}

/** hive/decisions `DecisionOption`. */
export interface HiveDecisionOption {
  label: string;
  recommended: boolean;
}

/** hive/decisions `PrecedentRef`: the precedent a decision cited; `rule` null once it's deleted. */
export interface HivePrecedentRef {
  id: string;
  rule: string | null;
  status: string | null;
}

/** hive/decisions `DecisionView`: one decision in Memory → Decisions (docs/hive.md §7.6). */
export interface HiveDecisionView {
  id: string;
  at: string;
  /** Rust's: a checkpoint, `never`, `config`, `integration`, `other`, `offer`, `digest`. */
  category: string;
  /** `prompt` | `question` | `blocker` | `land` | `resolve` | `plan` | `offer` | `digest`. */
  source: string;
  task_id: string | null;
  worker: string | null;
  question: string;
  context: string | null;
  options: HiveDecisionOption[];
  /** The coordinator's recommendation, when it gave one. */
  recommended: string | null;
  answer: string | null;
  reason: string | null;
  /** `open` | `recommended` | `answered` | `lapsed`, or `digest` for a daily digest (no decision). */
  status: string;
  /** `coordinator` | `human` | `maiterm`, once answered. */
  by: string;
  by_label: string;
  after_timeout: boolean;
  precedent: HivePrecedentRef | null;
  outcome: string | null;
  overridden: boolean;
  override_note: string | null;
  can_override: boolean;
}

/** hive/precedents `Precedent` (docs/hive.md §7.5). */
export interface HivePrecedent {
  v: number;
  id: string;
  hive_id: string;
  hive_name: string;
  decision_id: string;
  blocker_key: string | null;
  asked_at: string;
  answered_at: string;
  category: string;
  label: string | null;
  question: string;
  context: string | null;
  options: HiveDecisionOption[];
  recommended: string | null;
  answer: string;
  reason: string | null;
  rule: string;
  /** `coordinator` | `human`: whose wording the rule is. */
  rule_by: string;
  /** `hive` | `global`. */
  scope: string;
  /** `active` | `retired`. */
  status: string;
  cited: number;
  overridden: number;
  promoted_to: string | null;
  created_at: string;
}

/** hive/precedents `PrecedentsView`: this Hive's and the global ones, retired included. */
export interface HivePrecedentsView {
  hive: HivePrecedent[];
  global: HivePrecedent[];
}

/** hive/land/undo.rs `UndoPreview`: what the human's Undo would do, before anything is written. */
export interface HiveUndoPreview {
  id: string;
  land: string;
  folder: string;
  target: string;
  title: string;
  worker: string;
  /** "fast-forward": one commit holding the inverse; "merge": a revert of its merge commit. */
  method: string;
  commits: { sha: string; subject: string }[];
  commits_total: number;
  tip: string;
  config_files: string[];
  config_fp?: string | null;
  has_check: boolean;
  /** Why it can't be undone now, in words. */
  refusal?: string | null;
}

/** `git diff --shortstat`, parsed. */
export interface HiveShortstat {
  files: number;
  insertions: number;
  deletions: number;
}

/** A worker's changes against its landing target, summed over its worktrees (docs/hive.md §4.4).
 *  `hive_diff_stats` at mount, then `hive-diff-stat` ({@link HiveDiffStatEvent}) when it moves. */
export interface HiveDiffStat {
  task_id: string;
  /** Tracked files, committed and not, against the merge base. */
  total: HiveShortstat;
  /** Untracked, unignored files: `total` can't see them. */
  untracked: number;
  /** Commits only: what a land would take. */
  committed: HiveShortstat;
  /** Epoch ms. */
  computed_at: number;
  /** A worktree that couldn't be measured, by folder name; the sums leave it out. */
  error?: string | null;
}

export interface HiveDiffStatEvent {
  workspace_id: string;
  stat: HiveDiffStat;
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
  stack_console_height: number;
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
  /** Workspace stack (docs/stack.md) — gates the stack MCP tools and the priming line. */
  stack_enabled: boolean;
  tasks_backlog_vocabulary_migrated: boolean;
  /** Managed agent accounts (docs/login.md). Four states, not two (§10): setup is separate
   *  from the toggle. `accounts_setup_complete` false → the pane shows "Set up…";
   *  true with `accounts_enabled` false → configured but injecting nothing, one click to
   *  re-enable; clearing setup revokes and returns to false. */
  accounts_setup_complete: boolean;
  accounts_enabled: boolean;
  /** Metadata only — never credential material (§9.1). Omitted by Rust when empty. */
  managed_accounts?: ManagedAccount[];
  /** Runtime slug → account id. One active account per runtime, so Claude and Codex can be
   *  live at the same time. Omitted by Rust when empty. */
  active_account_ids?: Record<string, string>;
  overlord_enabled: boolean;
  /** Rules land as proposed directives the human clicks to send (docs/overlord.md §3). */
  overlord_propose_mode: boolean;
  /** The Overlord AGENT answers tabs' escalations (docs/overlord.md §9.1.2). Off by default;
   *  never settable over MCP. Read `preferencesStore.escalationsToOverlord`, not this alone. */
  overlord_answers_escalations: boolean;
  /** The human's standing answers for the Overlord agent, one entry each. Omitted by Rust
   *  when empty. Written only by the human or an approved proposal. */
  overlord_playbook?: string[];
  /** Follow-ups (docs/follow-ups.md §4) — a sub-feature of the Overlord. Never read alone:
   *  `preferencesStore.followUpsLive` is the combined answer. */
  follow_ups_enabled: boolean;
  /** A due follow-up on a tab whose agent has exited relaunches it, then delivers (§6.2). */
  follow_ups_resume_agent: boolean;
  /** Run agents' watch scripts without asking (§5.1). Off by default; never settable over MCP. */
  follow_ups_scripts_unattended: boolean;
  /** The Chrome extension link (docs/browser-link.md): host manifests + the browser MCP tools. */
  browser_link_enabled: boolean;
  // Files (docs/file-tree.md §16). Required: `set_preferences` REPLACES the whole object, so
  // the store must round-trip every one of these or a save resets it to its default.
  /** Master switch: the column, Cmd/Ctrl+Shift+B and (later) the tree's MCP tools. Default true. */
  file_tree_enabled: boolean;
  /** Follow the active tab's folder (§4.2). Default true. */
  file_tree_follow: boolean;
  /** Reveal the active editor's file when it is under the root. Default true. */
  file_tree_auto_reveal: boolean;
  /** Single click opens a preview tab (§10.5). Default true. */
  file_tree_preview_tabs: boolean;
  /** Show dotfiles (client filter on `Entry.f & 2`). Default true. */
  file_tree_show_hidden: boolean;
  /** Show gitignored entries, dimmed (client filter on `Entry.f & 1`). Default true. */
  file_tree_show_ignored: boolean;
  /** Extra names hidden by Rust's listing, as gitignore lines. Default []. */
  file_tree_exclude: string[];
  /** Git decorations, the branch row and the Changed lens (§7). Default true. */
  file_tree_git: boolean;
  /** Agent activity in the tree (§9): marks reach the trees. Off, the ledger still records
   *  (getAgentFileActivity, the agent-in-flight guard). Default true. */
  file_tree_agent_activity: boolean;
  /** Turn baselines (§14): an agent's first edit of a file in a turn reads it first, for
   *  Compare with Turn/Session Start and showDiff '@turn'. Default true. */
  file_tree_turn_baselines: boolean;
  /** The Files MCP tools, listed only while this and `file_tree_enabled` are on. Default true. */
  file_tree_agent_tools: boolean;
  /** Sticky ancestor rows; 0 = off. Default 5. */
  file_tree_sticky_max: number;
  /** 'compact' | 'normal' | 'comfortable' (row height 20/22/24). Default 'normal'. */
  file_tree_density: string;
  /** 'color' | 'mono'. Default 'color'. */
  file_tree_icons: string;
  /** Vim keys in the tree (§11.2). Default false. */
  file_tree_vim_keys: boolean;
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
  claude_setup_prompted?: boolean;
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

/** A window a workspace or tab can be moved to (`list_move_targets`). */
export interface MoveTargetWindow {
  /** The persistent window id (what `listWindows` gives agents). */
  id: string;
  label: string;
  name: string | null;
  /** What an unnamed window's titlebar shows. */
  active_workspace_name: string | null;
  is_current: boolean;
  /** Never the Overlord workspace. */
  workspaces: { id: string; name: string; bridge_all: boolean; mesh_group: string | null }[];
}

/** An agent's request only the human can approve (src-tauri/src/agent_requests.rs). */
export interface AgentRequest {
  id: string;
  /** The tab whose agent asked. */
  tab_id: string;
  /** The window that filed it and carries it out. */
  window_label: string;
  kind: 'mesh_enable' | 'mesh_disable' | 'mesh_link' | 'mesh_unlink' | 'tab_move' | 'workspace_move' | 'tab_create' | 'restart'
    | 'tab_reload' | 'window_reload' | 'bridge_connect' | 'bridge_fork' | 'bridge_disconnect' | 'mcp_add'
    // A Hive's land or undo (docs/hive.md §8.1, §8.3): filed and carried out by Rust, never a window.
    | 'hive_land' | 'hive_undo';
  /** Exactly what runs on approval, built by maiTerm. */
  args: Record<string, unknown>;
  /** What the card says will happen, written by maiTerm. */
  summary: string;
  effects: string[];
  /** The agent's own words. */
  reason: string | null;
  /** The asking tab's name and workspace when it asked. */
  asker_title: string;
  asker_workspace: string;
  requested_at: number;
  expires_at: number;
  status: 'pending' | 'approved' | 'done' | 'failed' | 'denied' | 'expired';
  decided_at: number | null;
  decided_by: 'desktop' | 'phone' | null;
  outcome: string | null;
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
  /** The Files column is open in this window (docs/file-tree.md §3.1). Only the human's toggle
   *  changes it (`setFileTreeOpen`). */
  file_tree_open: boolean;
  /** Files column width in px, 200–560, default 280 (`setFileTreeWidth`, once per gesture). */
  file_tree_width: number;
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

/** What the Windows Claude setup wizard found (src-tauri/src/windows_setup.rs). Off Windows
 *  `windows` is false and everything else is empty. */
export interface ClaudeSetupReport {
  windows: boolean;
  /** Git for Windows' bin\bash.exe; null = Git for Windows isn't installed. */
  git_bash: string | null;
  claude: string | null;
  claude_version: string | null;
  /** `claude` works in a new terminal outside maiTerm; maiTerm's tabs find it either way. */
  claude_on_path: boolean;
  /** An older Claude Desktop's Claude.exe that comes first on PATH. */
  claude_shadowed_by: string | null;
  /** Default WSL distro's version (1 or 2); null when WSL has no distro. */
  wsl_version: number | null;
  winget: boolean;
  shell_pref: string;
  /** The shell new tabs actually get. */
  shell: string;
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
  /** Raw argv of an ssh that died with 255 and was not followed by another command. */
  dropped_ssh: string | null;
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

// Move project (docs/relocate.md) — mirrors src-tauri/src/relocate + commands/relocate.rs
export interface MissingRoot {
  root: string;
  subpaths: string[];
  tab_ids: string[];
  service_ids: string[];
  candidates: string[];
}

export interface AgentPlan {
  claude_sessions: number;
  claude_memory: boolean;
  claude_trust: boolean;
  codex_sessions: number;
  codex_trust: boolean;
  gemini_projects: number;
}

export interface RelocatePreview {
  old: string;
  new: string;
  tabs: number;
  services: number;
  agents: AgentPlan;
  /** The volume folds case, so paths match case-insensitively. */
  fold: boolean;
}

export interface RelocateOutcome {
  moved: boolean;
  state: { tabs: [string, string][]; services: number; pins: [string, string][]; approvals_carried: number; backup_directory: boolean };
  agents: { done: string[]; warnings: string[] };
  unconfirmed_windows: string[];
}

// ── Files (docs/file-tree.md §4–§5) ────────────────────────────────────────────────────────
// Every path between Rust and the tree is ROOT-RELATIVE POSIX (`src/lib/x.ts`, '' for the
// root), keyed by the subscription. Native paths appear only in `TreePlace.root`,
// `OpenAck.root_native` and the absolute paths handed to `ftLocate`.

/** What terminalsStore knows about a tab that Rust doesn't keep. */
export interface PlaceHints {
  /** terminalsStore `osc.cwd` (OSC 7). */
  osc_cwd?: string | null;
  /** terminalsStore `osc.cwdHost`. */
  osc_host?: string | null;
  /** terminalsStore `osc.promptCwd`. Accepted, but only used once remote roots exist (phase 6). */
  prompt_cwd?: string | null;
  /** Bypass the 800 ms process-table cache: pass true on ssh start/exit edges. */
  fresh?: boolean;
  /** Only looking (the root switcher): an ssh tab's host is asked only when its Files helper
   *  is already up, so nothing starts a helper or waits on a host that is down. */
  passive?: boolean;
}

/** Which folder a tab's tree shows (`ftResolvePlace`). */
export interface TreePlace {
  /** The hub key in `watch::place` form: canonical "/abs" locally, "user@host:/abs" remote.
   *  Pass it to `ftOpen` and use it as the view-state LRU key. */
  place: string;
  /** 'local' | 'remote'. Phase 1 opens local places only: `ftOpen` refuses a remote one. */
  kind: string;
  /** Remote: the ssh destination (connect args only, never a full command line). */
  host_key: string | null;
  /** Remote: the host for a human, for the header chip (`ews@nova`). */
  host_label: string | null;
  /** Native absolute path locally; POSIX absolute (or `~/…`) remotely. */
  root: string;
  /** The tab's folder relative to the root: the you-are-here tick. Null when it IS the root,
   *  or isn't under it (a pinned root elsewhere). Reveal it; never select it. */
  cwd_rel: string | null;
  /** 'pty' | 'osc7' | 'agent' | 'last_cwd' | 'auto_resume' | 'restore' | 'prompt' | 'pin' |
   *  'hive' (a Hive agent's worktree or read copy, docs/hive.md §4.1). */
  source: string;
  /** 'local' | 'network' | 'unknown'. */
  fs_class: string;
  /** Home, '/', a drive or volume root: only opened folders are watched (§6.5). */
  shallow: boolean;
  /** The root is the top level of the repository the tab's folder is in, not just that
   *  folder: following re-roots to it even from a root containing it, such as home (§4.2).
   *  False for a pin, a missing folder and an ssh folder the host didn't check. */
  project: boolean;
  /** A saved folder that no longer exists. Show "Folder missing: …" with a Move project link;
   *  never fall back to home. `ftOpen` on it fails. */
  missing: string | null;
}

/** A folder to open, with the generation the client already holds (null = none). */
export interface DirReq {
  rel: string;
  known_gen?: number | null;
}

export interface RepoInfo {
  /** The repo's top level relative to the root: '' when the root is the top level, '..' steps
   *  when the root is a folder inside a larger repo. */
  toplevel_rel: string;
  /** Absolute native paths. */
  git_dir: string;
  common_dir: string;
}

export interface Caps {
  /** 'live' | 'partial' (some folders polled: watch limit) | 'poll' (network mount) | 'none'. */
  watch: string;
  /** Git decorations: the root is in a repository and `file_tree_git` is on. */
  git: boolean;
  /** Move to Trash is offered. A trash that fails anyway answers `needs.code === 'no_trash'`. */
  trash: boolean;
  remote: boolean;
  /** File operations (`ftOp`) are allowed. */
  writable: boolean;
}

/** One file operation (`ftOp`, docs/file-tree.md §10.2). Every path is a rel checked in Rust,
 *  except an `import`'s sources: native absolute paths from an OS drop. */
export interface FileOp {
  kind: 'create_file' | 'create_dir' | 'rename' | 'move' | 'copy' | 'duplicate' | 'trash' | 'delete' | 'import' | 'restore';
  src?: string[];
  /** The destination folder (create, move, copy, import); '' or null = the root. */
  dest?: string | null;
  /** create: the new name ('a/b/c.ts' makes the folders on the way); rename: one component. */
  name?: string | null;
  /** move: an exact destination rel per `src` (what `OpResult.undo` uses to put each item back
   *  under the name it had). When set, `dest` is ignored. */
  to?: string[];
  /** 'fail' (default) | 'keep_both' (a platform-style "a copy.ts") | 'skip'. */
  conflict?: 'fail' | 'keep_both' | 'skip';
  /** delete: the ticket from `ftPrepareDelete`. */
  token?: string | null;
  /** The human confirmed a second time over an agent's in-flight work. */
  ack_agents?: boolean;
  /** restore: when the items were trashed (unix seconds); `undo` fills it in. */
  trashed_at?: number | null;
}

/** One item done. create: `from` = the new rel, `to` null. move/rename/copy/duplicate/import:
 *  `from` = the source (native for import), `to` = the new rel. trash/delete/restore: `from`
 *  = the rel, `to` null. */
export interface OpItem {
  from: string;
  to: string | null;
}

export interface OpFail {
  rel: string;
  code: 'exists' | 'gone' | 'inside_itself' | 'no_trash' | 'not_in_trash' | 'unsupported' | 'io';
  msg: string;
}

/** Something the human decides before the op can run (the ConfirmStrip). */
export interface OpNeeds {
  /** 'no_trash': offer Delete Permanently (`ftPrepareDelete`, then `delete`). 'agent_active':
   *  ask again, then resend with `ack_agents: true` (never answered until phase 4's ledger).
   *  'live_cwd': refused; show `detail` with a Move project… action. */
  code: 'no_trash' | 'agent_active' | 'live_cwd';
  detail: string;
  /** The tabs concerned. */
  tabs: string[];
}

/** `ftOp`'s answer. A rejected promise means nothing changed (a path outside the root, the
 *  root itself, a bad ticket). */
export interface OpResult {
  done: OpItem[];
  failed: OpFail[];
  /** Left alone by `conflict: 'skip'`, or already where asked. */
  skipped: string[];
  /** The op that reverses this one, for the undo stack; null when nothing can (a permanent
   *  delete, a trash on macOS: "Show in Trash" instead). */
  undo: FileOp | null;
  needs: OpNeeds | null;
}

/** `ftPrepareDelete`'s answer: a single-use ticket for exactly those rels, good for 60 s. */
export interface DeleteTicket {
  token: string;
  files: number;
  dirs: number;
}

/** `ftOpen`'s answer. Events may reach the Channel BEFORE this resolves: queue them. */
export interface OpenAck {
  sub_id: number;
  /** The canonical root on this computer: `relToAbs(ack, rel)` joins onto this with `sep`. */
  root_native: string;
  /** '/' or '\\'. */
  sep: string;
  /** The volume folds case: compare names case-insensitively (validation, dedupe). */
  case_insensitive: boolean;
  /** NFC and NFD spell one name here (macOS): git keys are NFC, a row's name is as stored,
   *  so a row is looked up in the git map by its NFC spelling. */
  nfc_names: boolean;
  repo: RepoInfo | null;
  caps: Caps;
  /** A remote root: the host's own name as its Files helper reported it, for the wrong-host
   *  warning when the tab's OSC 7 names another machine. null locally and until the helper
   *  has said. On a remote root `root_native` is the POSIX path there and `sep` is '/'. */
  remote_hostname: string | null;
}

/** Hover-only details (`ftStat`). */
export interface StatInfo {
  size: number;
  mtime_ms: number;
  exec: boolean;
  /** A symlink's target as written in the link. */
  link_target: string | null;
  /** `git diff --numstat HEAD` for a changed file: [added, deleted] lines. Null for a clean,
   *  untracked or binary file, or outside a repository. */
  numstat: [number, number] | null;
}

/** The branch row, from the `# branch.*` headers of a status run. */
export interface BranchInfo {
  /** The branch name, or '(detached)'. */
  head: string;
  /** The commit, or '(initial)' before the first one. */
  oid: string;
  upstream: string | null;
  ahead: number;
  behind: number;
}

/** A git letter: M modified, A added, D deleted, R renamed, C conflict, ? untracked, T type
 *  change. Folders carry rolled-up severity (C > D > M > A > ?). */
export type GitLetter = 'M' | 'A' | 'D' | 'R' | 'C' | '?' | 'T';

/** `git` on the Channel (§7.3). `full`: replace everything held. Otherwise drop `del` from both
 *  maps, then set `files` and `dirs`. An untracked folder is in `files` with '?' (everything in
 *  it is untracked too). `truncated`: past 20k changed files only `dirs` and `count` arrive. */
export interface GitUpdate {
  gen: number;
  full: boolean;
  files: [string, GitLetter][];
  dirs: [string, GitLetter][];
  del: string[];
  branch: BranchInfo | null;
  truncated: boolean;
  /** Changed entries in all (the branch row's "7 changed"). */
  count: number;
}

/** `ftSearch`'s answer (Rust's `filetree::types::SearchResult`; named apart from the terminal
 *  search's). Drop it when `seq` is older than the newest query sent. */
export interface TreeSearchResult {
  seq: number;
  /** The index is being built: `search_ready` arrives on the Channel when it is done. */
  indexing: boolean;
  items: TreeSearchHit[];
  /** No index: a home or volume folder, indexed only when the human asks (`indexShallow`). */
  unavailable: string | null;
}

export interface TreeSearchHit {
  rel: string;
  score: number;
  /** Matched CHARACTER positions in `rel` (not UTF-16 units), ascending. */
  idx: number[];
}

/** One row of a listing. Sorted by Rust (folders first, natural, case-insensitive): never
 *  re-sort in JS. */
export interface TreeEntry {
  /** The name as the disk spells it: the row key is `parentRel + '/' + n`. */
  n: string;
  /** 'f' file | 'd' folder | 'l' link to a file (or broken) | 'L' link to a folder | 'o' other. */
  k: 'f' | 'd' | 'l' | 'L' | 'o';
  /** Bit flags: TREE_FLAG_*. */
  f: number;
}

/** Gitignored: hidden unless `file_tree_show_ignored`, dimmed when shown. */
export const TREE_FLAG_IGNORED = 1;
/** Dotfile (or Windows hidden attribute): hidden unless `file_tree_show_hidden`. */
export const TREE_FLAG_HIDDEN = 2;
/** A link back to itself or an ancestor: show it, never expand it. */
export const TREE_FLAG_LOOP = 4;
/** Couldn't be read. */
export const TREE_FLAG_UNREADABLE = 8;

/**
 * Events on a subscription's Channel, in order, generation-checked (§5.3):
 * - `dir` arrives in 512-entry chunks; chunk 0 REPLACES whatever is held for `rel`; apply the
 *   folder when `last` arrives. A chunk > 0 whose `gen` doesn't match the one being built, or
 *   that arrives without chunk 0: drop it and `ftRefresh(rel)`. `truncated`: show
 *   "50,000 of {total} shown · search to find more".
 * - `not_modified`: the `known_gen` sent is current. If you don't hold that gen, `ftRefresh`.
 * - `delta`: apply only when holding `gen == from` (then hold `gen`); otherwise `ftRefresh(rel)`.
 *   `at[i]` is where entry i of `add` then `chg` sits AFTER the delta: drop `del` and the `chg`
 *   names, then insert in ascending `at` (`chg` may change an entry's kind, and so its place).
 * - `dir_error`: 'denied' | 'gone' (drop the folder and `ftCollapse` it) | 'timeout' (a network
 *   folder not answering: show "not responding…"; its listing still arrives when it does).
 * - `health`: show `note` in the footer when set; absent note = healthy, no footer.
 * - `root_gone`: "This folder no longer exists · Follow the tab / pick another". Never re-root
 *   to home by yourself.
 * - `git`: decorations (see `GitUpdate`). `health.git` is 'off' | 'live' | 'slow' | 'error'.
 * - `search_ready`: the find index finished building; re-ask a query answered `indexing`.
 * - `agent`: agents touched rows (§9), at most one event per 100 ms; draw each row's latest
 *   mark per tab and agent. Also sent on subscribe and on resume with the marks that still
 *   stand under the root.
 */
export type TreeEvent =
  | { t: 'dir'; rel: string; gen: number; chunk: number; last: boolean; total: number; truncated: boolean; entries: TreeEntry[] }
  | { t: 'not_modified'; rel: string; gen: number }
  | { t: 'delta'; rel: string; from: number; gen: number; add: TreeEntry[]; del: string[]; chg: TreeEntry[]; at: number[] }
  | { t: 'dir_error'; rel: string; code: 'denied' | 'gone' | 'timeout'; msg: string }
  | { t: 'health'; watch: string; git: string; note: string | null }
  | ({ t: 'git' } & GitUpdate)
  | { t: 'search_ready' }
  | { t: 'agent'; marks: AgentMark[] }
  | { t: 'root_gone' }
  /** The root's repository changed (`git init` there, its `.git` removed): open the same
   *  place again, keeping the view. The hub was built for the old repository. */
  | { t: 'reopen' };

/** What an agent did to one row (docs/file-tree.md §9.1). Evidence is always named: say
 *  "changed while claude's Bash ran" for `fs_during_tool`, never "claude edited". */
export interface AgentMark {
  rel: string;
  /** 'read' | 'search' | 'edit' | 'write' | 'create' | 'delete' | 'changed' | 'commit' */
  op: string;
  /** 'pending' (the call is running: editing now) | 'ok' | 'failed' */
  phase: string;
  tab_id: string;
  /** '' for the main agent, otherwise the subagent's id. */
  agent: string;
  turn: number;
  /** 'tool' | 'fs_during_tool' | 'commit' */
  evidence: string;
  at_ms: number;
}

/** `AgentMark` with an absolute path, before a tree placed it (`agentFilesMarks`). */
export interface AgentMarkAbs extends Omit<AgentMark, 'rel'> {
  path: string;
}

/** A tab's current turn as the agent ledger counts it (`agentFilesTurns`). */
export interface TabTurn {
  tab_id: string;
  turn: number;
  open: boolean;
  /** When it began (its prompt, or its first call when the prompt was not seen), ms epoch. */
  start_ms: number;
  /** When it ended; null while open, or when its end was never seen. */
  end_ms: number | null;
}

/** What a file held before a tab's agent first edited it this turn or session (§14,
 *  `agentFilesBaseline`). `content` null with no `unavailable` is a new file (diff against
 *  empty). */
export interface Baseline {
  content: string | null;
  /** 'timeout' | 'binary' | 'too_large' | 'unreadable' | 'evicted' (memory budget) |
   *  'too_many_sessions' (dropped with its session's, other sessions ran) | 'not_captured' | 'pending' */
  unavailable: string | null;
  /** The turn it was captured in. */
  turn: number;
  /** 'maiterm' */
  source: string;
  /** The tab's agent runtime ('claude', 'codex', …); '' when unknown. */
  runtime: string;
}

/** Lines added and removed since a baseline (`agentFilesBaselineStat`). */
export interface BaselineStat {
  added: number;
  removed: number;
  /** As `Baseline.unavailable`, or 'current_unreadable' (the file on disk isn't text now). */
  unavailable: string | null;
  turn: number;
}

/** One entry of a tab's agent ledger (`agentFilesActivity`). `path` is absolute, or
 *  `user@host:/abs` for an ssh tab's. */
export interface LedgerEntry {
  path: string;
  op: string;
  tool: string;
  tool_use_id: string | null;
  agent: string;
  /** 'pending' | 'ok' | 'failed' */
  status: string;
  /** 'tool' | 'fs_during_tool' | 'commit' */
  evidence: string;
  turn: number;
  t_pre_ms: number;
  t_post_ms: number | null;
}

/** Every Channel message: `seq` increases by one per message. Ack from inside `onmessage`:
 *  `ftAck(sub, seq)` on every 32nd message, or when ≥100 ms passed since the last ack (checked
 *  on arrival, never on a timer). Past 256 unacked messages or 8 MB Rust stops sending until
 *  the next ack, then resends each folder that changed meanwhile as a whole `dir`. */
export type TreeEnvelope = { seq: number } & TreeEvent;
