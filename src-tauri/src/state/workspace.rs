use serde::{Deserialize, Serialize};
use std::collections::HashMap;

// Kept for migration from old state files
#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum Layout {
    #[default]
    Horizontal,
    Vertical,
    Grid,
}

// Kept for migration from old state files
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct PaneSizes {
    #[serde(default)]
    pub horizontal: HashMap<String, f64>,
    #[serde(default)]
    pub vertical: HashMap<String, f64>,
    #[serde(default)]
    pub grid: HashMap<String, f64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum SplitDirection {
    Horizontal,
    Vertical,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum SplitNode {
    #[serde(rename = "leaf")]
    Leaf { pane_id: String },
    #[serde(rename = "split")]
    Split {
        id: String,
        direction: SplitDirection,
        ratio: f64,
        children: Box<(SplitNode, SplitNode)>,
    },
}

impl SplitNode {
    #[allow(dead_code)]
    pub fn contains_pane(&self, pane_id: &str) -> bool {
        match self {
            SplitNode::Leaf { pane_id: id } => id == pane_id,
            SplitNode::Split { children, .. } => {
                children.0.contains_pane(pane_id) || children.1.contains_pane(pane_id)
            }
        }
    }

    /// `before` places the new pane on the left/top side of the target
    /// instead of the right/bottom.
    pub fn split_pane(
        &self,
        target_pane_id: &str,
        new_pane_id: &str,
        direction: SplitDirection,
        before: bool,
    ) -> SplitNode {
        match self {
            SplitNode::Leaf { pane_id } if pane_id == target_pane_id => {
                let target = SplitNode::Leaf {
                    pane_id: target_pane_id.to_string(),
                };
                let new = SplitNode::Leaf {
                    pane_id: new_pane_id.to_string(),
                };
                SplitNode::Split {
                    id: uuid::Uuid::new_v4().to_string(),
                    direction,
                    ratio: 0.5,
                    children: Box::new(if before { (new, target) } else { (target, new) }),
                }
            }
            SplitNode::Leaf { .. } => self.clone(),
            SplitNode::Split {
                id,
                direction: dir,
                ratio,
                children,
            } => SplitNode::Split {
                id: id.clone(),
                direction: dir.clone(),
                ratio: *ratio,
                children: Box::new((
                    children.0.split_pane(target_pane_id, new_pane_id, direction.clone(), before),
                    children.1.split_pane(target_pane_id, new_pane_id, direction, before),
                )),
            },
        }
    }

    pub fn remove_pane(&self, pane_id: &str) -> Option<SplitNode> {
        match self {
            SplitNode::Leaf { pane_id: id } if id == pane_id => None,
            SplitNode::Leaf { .. } => Some(self.clone()),
            SplitNode::Split {
                id,
                direction,
                ratio,
                children,
            } => {
                let left = children.0.remove_pane(pane_id);
                let right = children.1.remove_pane(pane_id);
                match (left, right) {
                    (None, None) => None,
                    (Some(node), None) | (None, Some(node)) => Some(node),
                    (Some(l), Some(r)) => Some(SplitNode::Split {
                        id: id.clone(),
                        direction: direction.clone(),
                        ratio: *ratio,
                        children: Box::new((l, r)),
                    }),
                }
            }
        }
    }

    pub fn set_ratio(&self, split_id: &str, new_ratio: f64) -> SplitNode {
        match self {
            SplitNode::Leaf { .. } => self.clone(),
            SplitNode::Split {
                id,
                direction,
                ratio,
                children,
            } => {
                let r = if id == split_id { new_ratio } else { *ratio };
                SplitNode::Split {
                    id: id.clone(),
                    direction: direction.clone(),
                    ratio: r,
                    children: Box::new((
                        children.0.set_ratio(split_id, new_ratio),
                        children.1.set_ratio(split_id, new_ratio),
                    )),
                }
            }
        }
    }

    #[allow(dead_code)]
    pub fn all_pane_ids(&self) -> Vec<String> {
        match self {
            SplitNode::Leaf { pane_id } => vec![pane_id.clone()],
            SplitNode::Split { children, .. } => {
                let mut ids = children.0.all_pane_ids();
                ids.extend(children.1.all_pane_ids());
                ids
            }
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum TabType {
    #[default]
    Terminal,
    Editor,
    Diff,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DiffContext {
    pub request_id: String,
    pub file_path: String,
    pub old_content: String,
    pub new_content: String,
    pub tab_name: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EditorFileInfo {
    pub file_path: String,
    #[serde(default)]
    pub is_remote: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub remote_ssh_command: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub remote_path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub language: Option<String>,
}

/// Agent Bridge: a durable pairing between two Claude tabs that can message each
/// other. Persisted on both tabs (symmetric) so the bridge survives app restart and
/// is rebuilt by the frontend once both tabs + their resumed sessions are back.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentBridge {
    /// The tab this one is bridged to.
    pub partner_tab_id: String,
    /// Human-readable label of the partner (for the agent's own awareness).
    pub partner_label: String,
    /// Partner's last-known Claude session id — refreshed when the partner
    /// re-initializes after a resume; used to detect/re-bind a drifted session.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub partner_session_id: Option<String>,
    /// "caller" (initiated the bridge), "fork" (the forked peer), or "peer"
    /// (an existing tab connected without forking).
    #[serde(default)]
    pub role: String,
    /// Conversation turn counter (messages this tab has sent).
    #[serde(default)]
    pub turn: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Tab {
    pub id: String,
    pub name: String,
    pub pty_id: Option<String>,
    #[serde(default)]
    pub scrollback: Option<String>,
    /// True when the user has explicitly renamed this tab (disables OSC title).
    #[serde(default)]
    pub custom_name: bool,
    /// Transient flag set after merge import — cleared on tab activation.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub import_highlight: bool,
    /// Persisted restore context: local cwd from last session.
    #[serde(default)]
    pub restore_cwd: Option<String>,
    /// Persisted restore context: SSH command from last session.
    #[serde(default)]
    pub restore_ssh_command: Option<String>,
    /// Persisted restore context: remote cwd from last session.
    #[serde(default)]
    pub restore_remote_cwd: Option<String>,
    /// Auto-resume: local cwd to restore to on startup.
    #[serde(default)]
    pub auto_resume_cwd: Option<String>,
    /// Auto-resume: SSH command to replay on startup.
    #[serde(default, alias = "pinned_ssh_command")]
    pub auto_resume_ssh_command: Option<String>,
    /// Auto-resume: remote cwd — used with auto_resume_ssh_command.
    #[serde(default, alias = "pinned_remote_cwd")]
    pub auto_resume_remote_cwd: Option<String>,
    /// Auto-resume: command to run after connect (e.g. "claude").
    #[serde(default, alias = "pinned_command")]
    pub auto_resume_command: Option<String>,
    /// Auto-resume: last command entered by the user (for pre-fill memory).
    /// Only updated when user submits a command, never cleared on disable.
    #[serde(default)]
    pub auto_resume_remembered_command: Option<String>,
    /// Auto-resume: when true, prompt pre-fills from stored values, not live PTY.
    #[serde(default)]
    pub auto_resume_pinned: bool,
    /// Auto-resume: when false, stored settings are preserved but won't fire on restart.
    #[serde(default = "default_true")]
    pub auto_resume_enabled: bool,
    /// User-editable notes scratchpad for this tab.
    #[serde(default)]
    pub notes: Option<String>,
    /// Persisted source/render mode for the notes panel.
    #[serde(default)]
    pub notes_mode: Option<String>,
    /// Whether the notes panel is open for this tab.
    #[serde(default)]
    pub notes_open: bool,
    /// Whether the composer dock is open for this tab.
    /// `None` = inherit `composer_default_open` preference; `Some(x)` = user explicitly toggled.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub composer_open: Option<bool>,
    /// Persisted in-progress composer draft text for this tab.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub composer_draft: Option<String>,
    /// Trigger-extracted variables (persisted across restarts).
    #[serde(default)]
    pub trigger_variables: HashMap<String, String>,
    /// Last known working directory (absolute path, updated live from OSC 7 / prompt patterns).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_cwd: Option<String>,
    /// Resolved display name shown in the archive list (original name/custom_name preserved).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub archived_name: Option<String>,
    /// Timestamp when the tab was archived (ISO 8601).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub archived_at: Option<String>,
    /// Timestamp when the tab was last suspended (ISO 8601). Set when the PTY is
    /// killed via suspend, cleared when it goes live again. Surfaced as the
    /// "age" of a suspended tab in the hidden-tabs menu.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub suspended_at: Option<String>,
    #[serde(default)]
    pub tab_type: TabType,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub editor_file: Option<EditorFileInfo>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub diff_context: Option<DiffContext>,
    /// Agent Bridge pairing (persisted both sides) — see AgentBridge. The `agent_link`
    /// alias migrates state written before the link→bridge rename.
    #[serde(default, alias = "agent_link", skip_serializing_if = "Option::is_none")]
    pub agent_bridge: Option<AgentBridge>,
    /// Which AI agent runtime this tab is running (Claude/Codex/…), detected at
    /// initSession from the MCP client. `None` until detected; populated by a later
    /// stage. Drives per-runtime resume command, fork capability, and bridge adapter.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub runtime: Option<crate::state::AgentRuntime>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Pane {
    pub id: String,
    pub name: String,
    pub tabs: Vec<Tab>,
    pub active_tab_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkspaceNote {
    pub id: String,
    pub content: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mode: Option<String>,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Workspace {
    pub id: String,
    pub name: String,
    #[serde(alias = "windows")]
    pub panes: Vec<Pane>,
    #[serde(alias = "active_window_id")]
    pub active_pane_id: Option<String>,
    #[serde(default)]
    pub split_root: Option<SplitNode>,
    #[serde(default)]
    pub workspace_notes: Vec<WorkspaceNote>,
    #[serde(default)]
    pub archived_tabs: Vec<Tab>,
    /// Transient flag set after merge import — cleared on workspace activation.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub import_highlight: bool,
    /// Whether this workspace is suspended (PTYs killed, resources freed).
    #[serde(default)]
    pub suspended: bool,
    // Old field kept for migration deserialization only
    #[serde(default, alias = "window_sizes", skip_serializing)]
    #[allow(dead_code)]
    pub pane_sizes: Option<PaneSizes>,
}

/// Saved window geometry (logical pixels) for a specific monitor configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WindowGeometry {
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WindowData {
    pub id: String,
    pub label: String,
    pub workspaces: Vec<Workspace>,
    pub active_workspace_id: Option<String>,
    #[serde(default = "default_sidebar_width")]
    pub sidebar_width: u32,
    #[serde(default)]
    pub sidebar_collapsed: bool,
    /// Window geometry per monitor count (e.g. "1" for single monitor, "2" for dual).
    /// When monitors change, the window repositions to the saved geometry for that count.
    #[serde(default, skip_serializing_if = "std::collections::HashMap::is_empty")]
    pub window_geometry: std::collections::HashMap<String, WindowGeometry>,
    // Legacy flat fields — migrated to window_geometry on first save
    #[serde(default, skip_serializing)]
    window_x: Option<f64>,
    #[serde(default, skip_serializing)]
    window_y: Option<f64>,
    #[serde(default, skip_serializing)]
    window_width: Option<f64>,
    #[serde(default, skip_serializing)]
    window_height: Option<f64>,
}

impl WindowData {
    pub fn new(label: String) -> Self {
        Self {
            id: uuid::Uuid::new_v4().to_string(),
            label,
            workspaces: Vec::new(),
            active_workspace_id: None,
            sidebar_width: default_sidebar_width(),
            sidebar_collapsed: false,
            window_geometry: std::collections::HashMap::new(),
            window_x: None,
            window_y: None,
            window_width: None,
            window_height: None,
        }
    }

    /// Get geometry for a given monitor count, falling back to legacy fields.
    pub fn geometry_for(&self, monitor_count: usize) -> Option<&WindowGeometry> {
        self.window_geometry.get(&monitor_count.to_string())
    }

    /// Migrate legacy flat fields into the geometry map (called on first save).
    pub fn migrate_legacy_geometry(&mut self, monitor_count: usize) {
        if let (Some(x), Some(y), Some(w), Some(h)) = (self.window_x, self.window_y, self.window_width, self.window_height) {
            self.window_geometry.entry(monitor_count.to_string()).or_insert(WindowGeometry {
                x, y, width: w, height: h,
            });
            self.window_x = None;
            self.window_y = None;
            self.window_width = None;
            self.window_height = None;
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct AppData {
    #[serde(default)]
    pub windows: Vec<WindowData>,
    // Old fields kept for migration deserialization only
    #[serde(default, skip_serializing)]
    pub workspaces: Option<Vec<Workspace>>,
    #[serde(default, skip_serializing)]
    pub active_workspace_id: Option<String>,
    #[serde(default, skip_serializing)]
    pub layout: Option<Layout>,
    #[serde(default, skip_serializing)]
    pub sidebar_width: Option<u32>,
    #[serde(default, skip_serializing)]
    pub sidebar_collapsed: Option<bool>,
    #[serde(default)]
    pub preferences: Preferences,
}

impl AppData {
    pub fn window(&self, label: &str) -> Option<&WindowData> {
        self.windows.iter().find(|w| w.label == label)
    }

    pub fn window_mut(&mut self, label: &str) -> Option<&mut WindowData> {
        self.windows.iter_mut().find(|w| w.label == label)
    }

    /// Collect every tab ID across all windows (live + archived). Used to
    /// identify orphan rows in side tables (e.g. scrollback DB).
    pub fn all_tab_ids(&self) -> std::collections::HashSet<String> {
        let mut ids = std::collections::HashSet::new();
        for win in &self.windows {
            for ws in &win.workspaces {
                for pane in &ws.panes {
                    for tab in &pane.tabs {
                        ids.insert(tab.id.clone());
                    }
                }
                for tab in &ws.archived_tabs {
                    ids.insert(tab.id.clone());
                }
            }
        }
        ids
    }
}

fn default_sidebar_width() -> u32 {
    // Keep in sync with SIDEBAR_DEFAULT_WIDTH in workspaces.svelte.ts.
    215
}

fn default_font_size() -> u32 {
    13
}

fn default_notes_font_size() -> u32 {
    13
}

fn default_font_family() -> String {
    "Menlo".to_string()
}

fn default_cursor_style() -> CursorStyle {
    CursorStyle::Block
}

fn default_cursor_blink() -> bool {
    true
}

fn default_auto_save_interval() -> u32 {
    10
}

fn default_scrollback_limit() -> u32 {
    10000
}

fn default_prompt_patterns() -> Vec<String> {
    vec![
        "\\u@\\h:\\d\\p".to_string(),
        "\\h \\u[\\d]\\p".to_string(),
        "[\\u@\\h \\d]\\p".to_string(),
        "PS \\d>".to_string(),
        "\\d>".to_string(),
    ]
}

fn default_theme() -> String {
    "tokyo-night".to_string()
}

fn default_notify_min_duration() -> u32 {
    5
}

fn default_notification_mode() -> String {
    "auto".to_string()
}

fn default_windows_shell() -> String {
    "powershell".to_string()
}

fn default_file_link_action() -> String {
    "modifier_click".to_string()
}

fn default_backup_exclude_scrollback() -> bool {
    true
}

fn default_backup_trim_age() -> String {
    "1m".to_string()
}

fn default_tab_button_style() -> String {
    "hover".to_string()
}

/// Terminal renderer: "dom" (default — xterm.js DOM renderer, no GPU/canvas
/// compositing so it can't ghost) or "canvas" (legacy @xterm/addon-canvas).
/// DOM is correct here because Rust owns scrollback and we render only one
/// bounded viewport, so the canvas/webgl throughput advantage never applied.
fn default_terminal_renderer() -> String {
    "dom".to_string()
}

fn default_true() -> bool {
    true
}

fn default_notes_width() -> u32 {
    320
}

fn default_toast_font_size() -> u32 {
    14
}

fn default_toast_width() -> u32 {
    400
}

fn default_toast_duration() -> u32 {
    8
}

fn default_notification_sound() -> String {
    "default".to_string()
}

fn default_notification_volume() -> u32 {
    50
}

/// Deserialize notification_sound: accepts string or bool (migration from old format).
fn deserialize_notification_sound<'de, D>(deserializer: D) -> Result<String, D::Error>
where
    D: serde::Deserializer<'de>,
{
    use serde::Deserialize;
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum StringOrBool {
        Str(String),
        Bool(bool),
    }
    match StringOrBool::deserialize(deserializer)? {
        StringOrBool::Str(s) => Ok(s),
        StringOrBool::Bool(true) => Ok("default".to_string()),
        StringOrBool::Bool(false) => Ok("none".to_string()),
    }
}

fn default_trigger_cooldown() -> f64 {
    5.0
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum TriggerActionType {
    #[default]
    Notify,
    #[serde(rename = "send_command")]
    SendCommand,
    #[serde(rename = "set_tab_state")]
    SetTabState,
    #[serde(rename = "enable_auto_resume")]
    EnableAutoResume,
    #[serde(rename = "replay_auto_resume")]
    ReplayAutoResume,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TriggerActionEntry {
    pub action_type: TriggerActionType,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub command: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tab_state: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VariableMapping {
    pub name: String,
    pub group: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub template: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Trigger {
    pub id: String,
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    pub pattern: String,
    /// New: array of actions to execute on match
    #[serde(default)]
    pub actions: Vec<TriggerActionEntry>,
    pub enabled: bool,
    #[serde(default)]
    pub workspaces: Vec<String>,
    #[serde(default)]
    pub tabs: Vec<String>,
    #[serde(default = "default_trigger_cooldown")]
    pub cooldown: f64,
    /// Variable extraction from capture groups (ordered)
    #[serde(default)]
    pub variables: Vec<VariableMapping>,
    /// When true, the pattern is plain text matched against TUI-normalized output
    /// (spaces in the pattern match any gap caused by cursor positioning).
    #[serde(default)]
    pub plain_text: bool,
    /// Match mode: "regex" | "plain_text" | "variable". When present, takes precedence over plain_text.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub match_mode: Option<String>,
    /// Links this trigger to an app-provided default template (e.g. "claude-resume").
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_id: Option<String>,
    /// True when the user has manually edited this default trigger.
    /// Prevents auto-updating from the app-provided template.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub user_modified: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum CursorStyle {
    #[default]
    Block,
    Underline,
    Bar,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Preferences {
    /// UI font size (non-terminal elements)
    #[serde(default = "default_font_size")]
    pub ui_font_size: u32,
    #[serde(default = "default_font_size")]
    pub font_size: u32,
    #[serde(default = "default_font_family")]
    pub font_family: String,
    #[serde(default = "default_cursor_style")]
    pub cursor_style: CursorStyle,
    #[serde(default = "default_cursor_blink")]
    pub cursor_blink: bool,
    #[serde(default = "default_auto_save_interval")]
    pub auto_save_interval: u32,
    #[serde(default = "default_scrollback_limit")]
    pub scrollback_limit: u32,
    #[serde(default = "default_prompt_patterns")]
    pub prompt_patterns: Vec<String>,
    #[serde(default = "default_true")]
    pub clone_cwd: bool,
    #[serde(default = "default_true")]
    pub clone_scrollback: bool,
    #[serde(default = "default_true")]
    pub clone_ssh: bool,
    #[serde(default = "default_true")]
    pub clone_history: bool,
    #[serde(default = "default_true")]
    pub clone_notes: bool,
    #[serde(default = "default_true")]
    pub clone_auto_resume: bool,
    #[serde(default = "default_true")]
    pub clone_variables: bool,
    #[serde(default = "default_true")]
    pub number_duplicated_tabs: bool,
    #[serde(default = "default_theme")]
    pub theme: String,
    #[serde(default)]
    pub shell_title_integration: bool,
    #[serde(default = "default_true")]
    pub shell_integration: bool,
    /// One-time marker for the "shell_integration default-on" migration. Once
    /// set, the migration never re-flips it, so a user who deliberately turns
    /// Command Completion off stays off across launches.
    #[serde(default)]
    pub shell_integration_default_migrated: bool,
    #[serde(default)]
    pub custom_themes: Vec<serde_json::Value>,
    #[serde(default = "default_true")]
    pub restore_session: bool,
    /// One-time marker for the "restore_session default-on" migration. Once set,
    /// the migration never re-flips it, so a user who deliberately turns Restore
    /// on Relaunch off stays off across launches.
    #[serde(default)]
    pub restore_session_default_migrated: bool,
    /// Legacy field kept for migration deserialization only.
    #[serde(default, skip_serializing)]
    #[allow(dead_code)]
    pub notify_on_completion: bool,
    #[serde(default = "default_notification_mode")]
    pub notification_mode: String,
    #[serde(default = "default_notify_min_duration")]
    pub notify_min_duration: u32,
    #[serde(default = "default_notes_font_size")]
    pub notes_font_size: u32,
    #[serde(default = "default_font_family")]
    pub notes_font_family: String,
    #[serde(default = "default_notes_width")]
    pub notes_width: u32,
    #[serde(default = "default_true")]
    pub notes_word_wrap: bool,
    #[serde(default = "default_toast_font_size")]
    pub toast_font_size: u32,
    #[serde(default = "default_toast_width")]
    pub toast_width: u32,
    #[serde(default = "default_toast_duration")]
    pub toast_duration: u32,
    #[serde(default = "default_notification_sound")]
    #[serde(deserialize_with = "deserialize_notification_sound")]
    pub notification_sound: String,
    #[serde(default = "default_notification_volume")]
    pub notification_volume: u32,
    #[serde(default = "default_true")]
    pub migrate_tab_notes: bool,
    #[serde(default)]
    pub notes_scope: Option<String>,
    #[serde(default = "default_true")]
    pub show_recent_workspaces: bool,
    #[serde(default)]
    pub workspace_sort_order: String,
    #[serde(default)]
    pub show_workspace_tab_count: bool,
    #[serde(default = "default_tab_button_style")]
    pub tab_button_style: String,
    /// Terminal renderer: "dom" (default) or "canvas".
    #[serde(default = "default_terminal_renderer")]
    pub terminal_renderer: String,
    #[serde(default)]
    pub triggers: Vec<Trigger>,
    /// Default trigger IDs the user has intentionally deleted (prevents re-seeding).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub hidden_default_triggers: Vec<String>,
    /// Whether the user has been prompted to enable Claude Code integrations.
    #[serde(default)]
    pub claude_triggers_prompted: bool,
    /// Enable the Claude Code IDE/MCP integration server.
    /// `claude_code_ide` alias migrates state from before the per-runtime key rename.
    #[serde(default = "default_true", alias = "claude_code_ide")]
    pub claude_ide: bool,
    /// Enable MCP bridge over SSH (reverse tunnel to expose local MCP tools to remote Claude Code).
    #[serde(default = "default_true", alias = "claude_code_ide_ssh")]
    pub claude_ide_ssh: bool,
    /// Enable Claude Code hooks integration (registers lifecycle hooks in ~/.claude/settings.json).
    #[serde(default = "default_true", alias = "claude_code_hooks")]
    pub claude_hooks: bool,
    /// Enable hooks-based auto-resume (initSession sets session ID, auto-configures resume).
    #[serde(default = "default_true", alias = "claude_code_auto_resume")]
    pub claude_auto_resume: bool,
    /// Enable the Codex IDE/MCP integration (opt-in; writes ~/.codex/config.toml).
    #[serde(default)]
    pub codex_ide: bool,
    /// Enable the Codex MCP bridge over SSH.
    #[serde(default)]
    pub codex_ide_ssh: bool,
    /// Enable Codex lifecycle hooks (command hooks in ~/.codex/hooks.json).
    #[serde(default)]
    pub codex_hooks: bool,
    /// Enable Codex hooks-based auto-resume.
    #[serde(default)]
    pub codex_auto_resume: bool,
    /// Pass --dangerously-bypass-hook-trust style handling / suppress the one-time
    /// Codex hook-trust prompt friction (advanced; default off).
    #[serde(default)]
    pub codex_hooks_bypass_trust: bool,
    /// Default open state for the composer dock on tabs that haven't been explicitly toggled.
    #[serde(default = "default_true")]
    pub composer_default_open: bool,
    /// Windows shell preference: "powershell", "pwsh", "cmd", "gitbash", "wsl"
    #[serde(default = "default_windows_shell")]
    pub windows_shell: String,
    /// File link click behavior: "click", "modifier_click", "alt_click", "disabled"
    #[serde(default = "default_file_link_action")]
    pub file_link_action: String,
    /// Backup directory path (None = scheduled backups disabled)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub backup_directory: Option<String>,
    /// Scheduled backup interval: "off", "hourly", "daily", "weekly", "monthly"
    #[serde(default)]
    pub backup_interval: String,
    /// Deprecated: exports are always compressed now. Kept for deserialization compat.
    #[serde(default = "default_true", skip_serializing)]
    #[allow(dead_code)]
    pub backup_compress: bool,
    /// Exclude terminal scrollback from backups
    #[serde(default = "default_backup_exclude_scrollback")]
    pub backup_exclude_scrollback: bool,
    /// Auto-delete old backups
    #[serde(default)]
    pub backup_trim_enabled: bool,
    /// Max age for auto-trim: "1h", "1d", "1w", "1m", "1y"
    #[serde(default = "default_backup_trim_age")]
    pub backup_trim_age: String,
    /// Auto-suspend inactive workspaces after N minutes (0 = disabled)
    #[serde(default)]
    pub auto_suspend_minutes: u32,
    /// Group active (non-suspended) tabs before suspended ones
    #[serde(default)]
    pub group_active_tabs: bool,
    /// Automatically check for updates on app launch
    #[serde(default = "default_true")]
    pub auto_check_updates: bool,

    /// Quick Open: show hidden/dotfiles by default
    #[serde(default)]
    pub quick_open_show_hidden: bool,

    /// Quick Open: show gitignored files by default
    #[serde(default)]
    pub quick_open_show_ignored: bool,
}

impl Default for Preferences {
    fn default() -> Self {
        Self {
            ui_font_size: default_font_size(),
            font_size: default_font_size(),
            font_family: default_font_family(),
            cursor_style: default_cursor_style(),
            cursor_blink: default_cursor_blink(),
            auto_save_interval: default_auto_save_interval(),
            scrollback_limit: default_scrollback_limit(),
            prompt_patterns: default_prompt_patterns(),
            clone_cwd: true,
            clone_scrollback: true,
            clone_ssh: true,
            clone_history: true,
            clone_notes: true,
            clone_auto_resume: true,
            clone_variables: true,
            number_duplicated_tabs: true,
            theme: default_theme(),
            shell_title_integration: false,
            shell_integration: true,
            shell_integration_default_migrated: true,
            custom_themes: Vec::new(),
            restore_session: true,
            restore_session_default_migrated: true,
            notify_on_completion: false,
            notification_mode: default_notification_mode(),
            notify_min_duration: default_notify_min_duration(),
            notes_font_size: default_notes_font_size(),
            notes_font_family: default_font_family(),
            notes_width: default_notes_width(),
            notes_word_wrap: true,
            toast_font_size: default_toast_font_size(),
            toast_width: default_toast_width(),
            toast_duration: default_toast_duration(),
            notification_sound: default_notification_sound(),
            notification_volume: default_notification_volume(),
            migrate_tab_notes: true,
            notes_scope: None,
            show_recent_workspaces: true,
            workspace_sort_order: String::new(),
            show_workspace_tab_count: false,
            tab_button_style: default_tab_button_style(),
            terminal_renderer: default_terminal_renderer(),
            triggers: Vec::new(),
            hidden_default_triggers: Vec::new(),
            claude_triggers_prompted: false,
            claude_ide: true,
            claude_ide_ssh: true,
            claude_hooks: true,
            claude_auto_resume: true,
            codex_ide: false,
            codex_ide_ssh: false,
            codex_hooks: false,
            codex_auto_resume: false,
            codex_hooks_bypass_trust: false,
            composer_default_open: true,
            windows_shell: default_windows_shell(),
            file_link_action: default_file_link_action(),
            backup_directory: None,
            backup_interval: String::new(),
            backup_compress: true,
            backup_exclude_scrollback: true,
            backup_trim_enabled: false,
            backup_trim_age: default_backup_trim_age(),
            auto_suspend_minutes: 0,
            group_active_tabs: false,
            auto_check_updates: true,
            quick_open_show_hidden: false,
            quick_open_show_ignored: false,
        }
    }
}

impl Tab {
    pub fn new(name: String) -> Self {
        Self {
            id: uuid::Uuid::new_v4().to_string(),
            name,
            pty_id: None,
            scrollback: None,
            custom_name: false,
            restore_cwd: None,
            restore_ssh_command: None,
            restore_remote_cwd: None,
            auto_resume_cwd: None,
            auto_resume_ssh_command: None,
            auto_resume_remote_cwd: None,
            auto_resume_command: None,
            auto_resume_remembered_command: None,
            auto_resume_pinned: false,
            auto_resume_enabled: true,
            notes: None,
            notes_mode: None,
            notes_open: false,
            composer_open: None,
            composer_draft: None,
            trigger_variables: HashMap::new(),
            archived_name: None,
            archived_at: None,
            suspended_at: None,
            tab_type: TabType::default(),
            editor_file: None,
            last_cwd: None,
            diff_context: None,
            import_highlight: false,
            agent_bridge: None,
            runtime: None,
        }
    }

    pub fn new_editor(name: String, file_info: EditorFileInfo) -> Self {
        Self {
            id: uuid::Uuid::new_v4().to_string(),
            name,
            pty_id: None,
            scrollback: None,
            custom_name: true,
            restore_cwd: None,
            restore_ssh_command: None,
            restore_remote_cwd: None,
            auto_resume_cwd: None,
            auto_resume_ssh_command: None,
            auto_resume_remote_cwd: None,
            auto_resume_command: None,
            auto_resume_remembered_command: None,
            auto_resume_pinned: false,
            auto_resume_enabled: true,
            notes: None,
            notes_mode: None,
            notes_open: false,
            composer_open: None,
            composer_draft: None,
            trigger_variables: HashMap::new(),
            archived_name: None,
            archived_at: None,
            suspended_at: None,
            tab_type: TabType::Editor,
            editor_file: Some(file_info),
            last_cwd: None,
            diff_context: None,
            import_highlight: false,
            agent_bridge: None,
            runtime: None,
        }
    }

    pub fn new_diff(name: String, diff_context: DiffContext) -> Self {
        Self {
            id: uuid::Uuid::new_v4().to_string(),
            name,
            pty_id: None,
            scrollback: None,
            custom_name: true,
            restore_cwd: None,
            restore_ssh_command: None,
            restore_remote_cwd: None,
            auto_resume_cwd: None,
            auto_resume_ssh_command: None,
            auto_resume_remote_cwd: None,
            auto_resume_command: None,
            auto_resume_remembered_command: None,
            auto_resume_pinned: false,
            auto_resume_enabled: true,
            notes: None,
            notes_mode: None,
            notes_open: false,
            composer_open: None,
            composer_draft: None,
            trigger_variables: HashMap::new(),
            archived_name: None,
            archived_at: None,
            suspended_at: None,
            tab_type: TabType::Diff,
            editor_file: None,
            last_cwd: None,
            diff_context: Some(diff_context),
            import_highlight: false,
            agent_bridge: None,
            runtime: None,
        }
    }
}

impl Pane {
    pub fn new(name: String) -> Self {
        let tab = Tab::new("Terminal".to_string());
        let tab_id = tab.id.clone();
        Self {
            id: uuid::Uuid::new_v4().to_string(),
            name,
            tabs: vec![tab],
            active_tab_id: Some(tab_id),
        }
    }
}

impl Workspace {
    pub fn new(name: String) -> Self {
        let pane = Pane::new("Terminal".to_string());
        let pane_id = pane.id.clone();
        Self {
            id: uuid::Uuid::new_v4().to_string(),
            name,
            panes: vec![pane],
            active_pane_id: Some(pane_id.clone()),
            split_root: Some(SplitNode::Leaf { pane_id }),
            workspace_notes: Vec::new(),
            archived_tabs: Vec::new(),
            import_highlight: false,
            suspended: false,
            pane_sizes: None,
        }
    }
}

#[cfg(test)]
mod pref_migration_tests {
    use super::Preferences;

    /// The per-runtime key rename (claude_code_ide -> claude_ide, etc.) must migrate
    /// an existing state file via #[serde(alias)] WITHOUT resetting a user's explicit
    /// choice to the default. This is the cross-language regression the rename guards.
    #[test]
    fn old_claude_code_keys_migrate_and_preserve_disabled() {
        // A user who DISABLED the IDE + hooks under the old key names.
        let old = serde_json::json!({ "claude_code_ide": false, "claude_code_hooks": false });
        let prefs: Preferences = serde_json::from_value(old).unwrap();
        assert!(!prefs.claude_ide, "claude_code_ide:false must map to claude_ide:false, not default-true");
        assert!(!prefs.claude_hooks, "claude_code_hooks:false must map to claude_hooks:false");
        // Keys absent from the old file still take their defaults.
        assert!(prefs.claude_ide_ssh, "absent claude_ide_ssh defaults to true");

        // Re-serialize: emits the NEW key, never the legacy one (alias is read-only).
        let out = serde_json::to_value(&prefs).unwrap();
        assert!(out.get("claude_ide").is_some(), "serializes under the new key");
        assert!(out.get("claude_code_ide").is_none(), "does not emit the legacy key");
    }

    /// Mirrors the setPreference MCP path (to_value -> insert(meta_key) -> from_value):
    /// because the meta key now matches the field's serialize name, the insert OVERWRITES
    /// rather than adding a second aliased key, so from_value sees no duplicate field.
    #[test]
    fn set_preference_roundtrip_has_no_duplicate_field() {
        let prefs = Preferences::default();
        let mut json = serde_json::to_value(&prefs).unwrap();
        json.as_object_mut().unwrap().insert("claude_ide".to_string(), serde_json::json!(false));
        let updated: Preferences = serde_json::from_value(json).expect("no duplicate-field error");
        assert!(!updated.claude_ide);
    }

    /// Codex integration is opt-in: every codex_* key must default OFF so upgrading
    /// users never get a surprise ~/.codex write. Defaults and absent-key deserialize
    /// (empty `{}`) must both yield all-false.
    #[test]
    fn codex_keys_default_off() {
        let p = Preferences::default();
        assert!(!p.codex_ide && !p.codex_hooks && !p.codex_ide_ssh && !p.codex_auto_resume && !p.codex_hooks_bypass_trust);

        // Deserializing an empty object (no codex_* keys present) also yields all-false.
        let empty: Preferences = serde_json::from_value(serde_json::json!({})).unwrap();
        assert!(!empty.codex_ide && !empty.codex_hooks && !empty.codex_ide_ssh && !empty.codex_auto_resume && !empty.codex_hooks_bypass_trust);
    }
}
