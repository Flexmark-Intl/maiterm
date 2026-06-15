use parking_lot::RwLock;
use std::collections::HashMap;
use std::sync::atomic::AtomicU64;
use std::sync::mpsc::Sender;
use std::time::Instant;

use super::persistence::app_data_slug;
use super::scrollback_db::ScrollbackDb;
use super::workspace::AppData;
use crate::terminal::handle::TerminalHandle;

pub enum PtyCommand {
    Write(Vec<u8>),
    Resize { cols: u16, rows: u16 },
    Kill,
}

pub struct PtyHandle {
    pub sender: Sender<PtyCommand>,
    pub child_pid: Option<u32>,
}

pub struct FileWatcherHandle {
    pub _debouncer: notify_debouncer_mini::Debouncer<notify::RecommendedWatcher>,
}

/// Per-PTY byte counter
pub struct PtyStats {
    pub bytes_written: AtomicU64,
    pub bytes_read: AtomicU64,
    /// Millis since UNIX_EPOCH of the last PTY read. Used to detect an
    /// actively-drawing TUI so resizes can be coalesced (see resize_pty).
    pub last_read_ms: AtomicU64,
}

/// A resize waiting for the trailing debounce while the PTY is streaming.
/// Coalescing rapid resize requests into one SIGWINCH matters because TUIs
/// (Claude Code) re-render retained content on every width change — each one
/// mid-stream leaves a permanent duplicate in scrollback.
pub struct PendingResize {
    pub cols: u16,
    pub rows: u16,
    pub last_request: Instant,
}

/// Ring buffer cap for memory_samples. 720 samples × 60s cadence = 12h of history.
/// At ~40 bytes per sample serialized, the on-disk JSON stays under ~30KB.
pub const MEMORY_SAMPLE_CAP: usize = 720;

/// Memory sample emitted by the periodic memory_sampler task.
#[derive(Clone, serde::Serialize, serde::Deserialize)]
pub struct MemorySample {
    pub timestamp_secs: u64,
    pub rss_bytes: u64,
}

/// Remote file watch entry for SSH-based polling.
pub struct RemoteFileWatch {
    pub user_host: String,
    pub remote_path: String,
    pub last_mtime: Option<u64>,
}

/// Active SSH MCP tunnel info (reverse port forward to expose local MCP on remote).
pub struct SshTunnel {
    pub pid: u32,
    pub remote_port: u16,
    pub host_key: String,
    pub tab_ids: std::collections::HashSet<String>,
}

/// Tracked Claude Code session (registered via hooks).
pub struct AgentSessionInfo {
    /// Which agent runtime owns this session; detected at initSession (Stage 3 sets Claude everywhere as a placeholder).
    #[allow(dead_code)]
    pub runtime: crate::state::AgentRuntime,
    pub tab_id: String,
    pub cwd: Option<String>,
    pub state: AgentSessionState,
    /// Current tool being executed (set by PreToolUse, cleared by PostToolUse/Stop)
    pub tool_name: Option<String>,
    /// Model used in this session (set by SessionStart)
    pub model: Option<String>,
    /// MCP connection ID that called initSession for this session.
    /// Used to recover affinity after SSE reconnects: if a session's
    /// connection_id is no longer in connection_tabs, it's orphaned.
    pub connection_id: Option<String>,
}

#[derive(Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentSessionState {
    Active,
    WaitingInput,
    WaitingPermission,
    Stopped,
}

pub struct AppState {
    pub scrollback_db: ScrollbackDb,
    pub pty_registry: RwLock<HashMap<String, PtyHandle>>,
    /// alacritty_terminal instances keyed by pty_id
    pub terminal_registry: RwLock<HashMap<String, TerminalHandle>>,
    /// Maps tab_id → pty_id so we can auto-kill a previous PTY when a new one
    /// is spawned for the same tab (e.g. HMR remount, frontend crash recovery).
    pub tab_pty_map: RwLock<HashMap<String, String>>,
    pub app_data: RwLock<AppData>,
    // File watchers keyed by tab ID
    pub file_watchers: RwLock<HashMap<String, FileWatcherHandle>>,
    // In-flight SCP uploads: upload_id → cooperative cancel flag
    pub scp_uploads: RwLock<HashMap<String, std::sync::Arc<std::sync::atomic::AtomicBool>>>,
    // Claude Code IDE integration
    pub claude_code_port: RwLock<Option<u16>>,
    pub claude_code_auth: RwLock<Option<String>>,
    pub claude_code_pending: RwLock<HashMap<String, tokio::sync::oneshot::Sender<serde_json::Value>>>,
    pub claude_code_connected: RwLock<bool>,
    pub claude_code_notify_tx: parking_lot::Mutex<Option<tokio::sync::mpsc::UnboundedSender<String>>>,
    pub claude_code_shutdown: parking_lot::Mutex<Option<tokio::sync::watch::Sender<bool>>>,
    // SSH MCP tunnels: keyed by host_key (user@host)
    pub ssh_tunnels: RwLock<HashMap<String, SshTunnel>>,
    // Remote file watchers (SSH stat polling): keyed by tab_id
    pub remote_file_watchers: RwLock<HashMap<String, RemoteFileWatch>>,
    pub remote_watcher_running: std::sync::atomic::AtomicBool,
    // Resizes deferred while the PTY is actively streaming (keyed by pty_id)
    pub pending_resizes: RwLock<HashMap<String, PendingResize>>,
    // Diagnostics
    pub pty_stats: RwLock<HashMap<String, PtyStats>>,
    pub memory_samples: RwLock<Vec<MemorySample>>,
    // Claude Code hook sessions: session_id → session info
    pub agent_sessions: RwLock<HashMap<String, AgentSessionInfo>>,
    // Pending session IDs from SessionStart HTTP hooks awaiting initSession to assign a tab
    pub pending_agent_sessions: RwLock<Vec<(String, Option<String>, Instant)>>, // (session_id, cwd, timestamp)
}

impl AppState {
    pub fn new() -> Self {
        let db_path = dirs::data_dir()
            .expect("No data directory found")
            .join(app_data_slug())
            .join("aiterm-scrollback.db");
        let scrollback_db = ScrollbackDb::open(db_path)
            .expect("Failed to open scrollback database");

        Self {
            scrollback_db,
            pty_registry: RwLock::new(HashMap::new()),
            terminal_registry: RwLock::new(HashMap::new()),
            tab_pty_map: RwLock::new(HashMap::new()),
            app_data: RwLock::new(AppData::default()),
            file_watchers: RwLock::new(HashMap::new()),
            scp_uploads: RwLock::new(HashMap::new()),
            claude_code_port: RwLock::new(None),
            claude_code_auth: RwLock::new(None),
            claude_code_pending: RwLock::new(HashMap::new()),
            claude_code_connected: RwLock::new(false),
            claude_code_notify_tx: parking_lot::Mutex::new(None),
            claude_code_shutdown: parking_lot::Mutex::new(None),
            ssh_tunnels: RwLock::new(HashMap::new()),
            remote_file_watchers: RwLock::new(HashMap::new()),
            remote_watcher_running: std::sync::atomic::AtomicBool::new(false),
            pending_resizes: RwLock::new(HashMap::new()),
            pty_stats: RwLock::new(HashMap::new()),
            memory_samples: RwLock::new(Vec::new()),
            agent_sessions: RwLock::new(HashMap::new()),
            pending_agent_sessions: RwLock::new(Vec::new()),
        }
    }

    /// Current alacritty grid size for a live PTY, if one exists.
    pub fn live_grid_size(&self, pty_id: &str) -> Option<(u16, u16)> {
        use alacritty_terminal::grid::Dimensions;
        let registry = self.terminal_registry.read();
        let handle = registry.get(pty_id)?;
        Some((handle.term.columns() as u16, handle.term.screen_lines() as u16))
    }
}
