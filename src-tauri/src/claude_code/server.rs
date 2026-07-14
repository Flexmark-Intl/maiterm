use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use axum::{
    body::Body,
    extract::{
        ws::{Message as WsMessage, WebSocket, WebSocketUpgrade},
        Query, State,
    },
    http::{header, HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::{get, post},
    Router,
};
use futures_util::{SinkExt, StreamExt};
use rand::Rng;
use serde_json::Value;
use tauri::{AppHandle, Emitter};
use tokio::sync::{mpsc, oneshot};

use super::lockfile::cleanup_stale_lockfiles;
use super::protocol::{initialize_response, tool_list_response, JsonRpcRequest, JsonRpcResponse};
use super::registrar;
use crate::state::AppState;

const PING_INTERVAL: Duration = Duration::from_secs(30);
const RESPONSE_TIMEOUT: Duration = Duration::from_secs(120);
const SSE_KEEPALIVE_INTERVAL: Duration = Duration::from_secs(15);
/// How often to re-assert our `mcpServers.aiterm` entry in `~/.claude.json`.
/// The file is co-owned by the `claude` CLI, which can clobber our entry; this
/// heals any drift within one tick. Idempotent — only writes when it differs.
const MCP_REASSERT_INTERVAL: Duration = Duration::from_secs(30);
/// Dormancy reaper cadence. Codex/Gemini have no SessionEnd hook, so we poll each
/// non-Claude agent tab's PTY process tree this often and require this many
/// consecutive "agent gone" observations before synthesizing a SessionEnd
/// (debounces sysinfo refresh lag and the auto-resume exit→relaunch gap).
const DORMANCY_POLL_INTERVAL: Duration = Duration::from_millis(1500);
const DORMANCY_ABSENT_POLLS: u32 = 2;

/// Per-SSE-session sender: receives raw JSON strings, which the SSE stream wraps as data events.
type SseSessions = Arc<parking_lot::RwLock<HashMap<String, mpsc::UnboundedSender<String>>>>;

/// Per-connection tab affinity: maps transport connection ID → tab ID.
/// Set by `initSession` tool, used to auto-inject `tabId` into tool calls.
type ConnectionTabMap = Arc<parking_lot::RwLock<HashMap<String, String>>>;

/// Per-connection runtime: maps transport connection ID → detected agent runtime.
/// Set on the MCP `initialize` handshake (from `clientInfo.name`); used to
/// RUNTIME-GATE affinity recovery so a Codex connection can never bind to a
/// Claude tab (and vice-versa).
type ConnectionRuntimeMap = Arc<parking_lot::RwLock<HashMap<String, crate::state::AgentRuntime>>>;

#[derive(Clone)]
struct ServerState {
    app_handle: AppHandle,
    state: Arc<AppState>,
    expected_auth: String,
    sse_sessions: SseSessions,
    /// Maps connection IDs (SSE session, WS id, or "streamable-http") to tab IDs.
    connection_tabs: ConnectionTabMap,
    /// Maps connection IDs to the runtime detected on their `initialize` handshake.
    /// Gates affinity recovery to same-runtime sessions only.
    connection_runtimes: ConnectionRuntimeMap,
    /// Active transport connections (WS + SSE). Used to ref-count the connected
    /// flag so flapping sessions don't emit spurious disconnect events while
    /// another session is still alive. Also dampens log/event spam from the
    /// documented SSE-over-SSH reconnect flap.
    connection_count: Arc<AtomicUsize>,
}

/// Emit an event under both its runtime-neutral name and the legacy claude-* name,
/// so a frontend listener on either keeps working during the rename's soak. The
/// legacy emit is dropped in a later release once all listeners are migrated.
fn emit_dual(app: &AppHandle, agent_event: &str, legacy_event: &str, payload: Value) {
    let _ = app.emit(agent_event, payload.clone());
    let _ = app.emit(legacy_event, payload);
}
fn emit_dual_to(app: &AppHandle, label: &str, agent_event: &str, legacy_event: &str, payload: Value) {
    let _ = app.emit_to(label, agent_event, payload.clone());
    let _ = app.emit_to(label, legacy_event, payload);
}

/// One dormancy-reaper poll. For every NON-Claude agent session, check whether the
/// agent process is still alive in its tab's PTY descendant tree. After
/// `DORMANCY_ABSENT_POLLS` consecutive "gone" observations, remove the session and
/// emit the SAME `agent-hook-session-end` event Claude's SessionEnd hook emits, so
/// the sidebar dot clears through the identical frontend teardown path.
///
/// Claude sessions are filtered out (`uses_pty_dormancy()` is false), so Claude is
/// never polled and its lifecycle stays byte-identical. `absent` carries the
/// per-session consecutive-gone counters across polls (owned by the reaper task).
fn reap_dormant_sessions(app: &AppHandle, state: &Arc<AppState>, absent: &mut HashMap<String, u32>) {
    use crate::state::AgentRuntime;

    // Snapshot non-Claude candidates, then release the lock before the ps/sysinfo scan.
    let candidates: Vec<(String, AgentRuntime, String)> = {
        let sessions = state.agent_sessions.read();
        sessions
            .iter()
            .filter(|(_, s)| s.runtime.uses_pty_dormancy())
            .map(|(id, s)| (id.clone(), s.runtime, s.tab_id.clone()))
            .collect()
    };
    if candidates.is_empty() {
        absent.clear();
        return;
    }

    let mut tracked: std::collections::HashSet<String> = std::collections::HashSet::new();
    for (session_id, runtime, tab_id) in candidates {
        tracked.insert(session_id.clone());

        // Resolve the tab's shell PID: tab → pty_id → child_pid (no nested locks).
        let pty_id: Option<String> = state.tab_pty_map.read().get(&tab_id).cloned();
        let shell_pid: Option<u32> =
            pty_id.and_then(|pid| state.pty_registry.read().get(&pid).and_then(|h| h.child_pid));

        // Gone when the PTY is dead, or the agent binary is absent from its tree.
        let gone = match shell_pid {
            None => true,
            Some(pid) => !crate::pty::manager::agent_process_alive(pid, runtime.agent_process_names()),
        };

        if !gone {
            absent.remove(&session_id);
            continue;
        }

        let count = absent.entry(session_id.clone()).or_insert(0);
        *count += 1;
        if *count < DORMANCY_ABSENT_POLLS {
            continue;
        }
        absent.remove(&session_id);

        // Confirmed dormant: remove the backend session and synthesize SessionEnd.
        let removed = state.agent_sessions.write().remove(&session_id).is_some();
        if removed {
            log::info!(
                "Dormancy: {} session {} gone (tab {}) → synthesizing session-end",
                runtime.as_key(),
                &session_id[..session_id.len().min(8)],
                tab_id
            );
            emit_dual(
                app,
                "agent-hook-session-end",
                "claude-hook-session-end",
                serde_json::json!({
                    "runtime": runtime.as_key(),
                    "session_id": session_id,
                    "tab_id": tab_id,
                    "reason": "dormant",
                }),
            );
        }
    }

    // Drop counters for sessions that vanished from the map (reaped or ended via a
    // real hook) so the table can't grow unbounded.
    absent.retain(|id, _| tracked.contains(id));
}

/// Extract the bearer/IDE auth token from a request, trying each accepted header
/// in priority order. Returns None when no usable token is present — callers compare
/// the result against the expected token, so None can never authenticate. Never
/// returns an empty string or an unstripped "Bearer " prefix.
fn extract_auth(headers: &HeaderMap) -> Option<String> {
    // Claude Code IDE header (raw token) — checked first so it never shadows.
    if let Some(v) = headers.get("x-claude-code-ide-authorization").and_then(|v| v.to_str().ok()) {
        if !v.is_empty() { return Some(v.to_string()); }
    }
    // Neutral maiTerm header (raw token).
    if let Some(v) = headers.get("x-maiterm-authorization").and_then(|v| v.to_str().ok()) {
        if !v.is_empty() { return Some(v.to_string()); }
    }
    // Standard Authorization: Bearer <token> (Codex and others).
    if let Some(v) = headers.get("authorization").and_then(|v| v.to_str().ok()) {
        if let Some(tok) = v.strip_prefix("Bearer ").or_else(|| v.strip_prefix("bearer ")) {
            let tok = tok.trim();
            if !tok.is_empty() { return Some(tok.to_string()); }
        }
    }
    None
}

/// Result of the synchronous server preparation step. Holds the bound TCP
/// listener (std form — converted to tokio inside `serve_server`) along with
/// the port and auth token that were already written into `~/.claude.json`.
pub struct ServerSetup {
    pub std_listener: std::net::TcpListener,
    pub port: u16,
    pub auth: String,
}

/// Synchronous prep that must complete before the frontend is allowed to
/// spawn PTYs: bind the TCP port, pick an auth token, and write the lockfile
/// (which also updates `~/.claude.json` with our new port and installs
/// hooks/skill). Returning here means `~/.claude.json` is current, so when a
/// tab's auto-resume writes `claude --resume …` to its shell, the Claude Code
/// CLI will read the correct port on its first load.
///
/// Returns `None` when the IDE integration is disabled in preferences or when
/// we couldn't bind any port.
pub fn prepare_server(state: &Arc<AppState>) -> Option<ServerSetup> {
    cleanup_stale_lockfiles();

    if registrar::enabled_registrars(&state.app_data.read().preferences).is_empty() {
        log::info!("No agent IDE integration enabled in preferences");
        return None;
    }

    let mut rng = rand::thread_rng();
    let mut bound = None;
    for _ in 0..100 {
        let port = rng.gen_range(10000..65535u16);
        if let Ok(l) = std::net::TcpListener::bind(format!("127.0.0.1:{}", port)) {
            bound = Some(l);
            break;
        }
    }
    let std_listener = match bound {
        Some(l) => l,
        None => {
            log::error!("Failed to bind Claude Code server on any port");
            return None;
        }
    };
    let port = std_listener.local_addr().ok()?.port();

    let auth: String = rng
        .sample_iter(&rand::distributions::Alphanumeric)
        .take(32)
        .map(char::from)
        .collect();

    if auth.is_empty() {
        log::error!("Refusing to start MCP server with an empty auth token");
        return None;
    }

    *state.mcp_port.write() = Some(port);
    *state.mcp_auth.write() = Some(auth.clone());

    let workspace_folders = collect_workspace_folders(state);
    // Snapshot prefs (clone) so no app_data lock is held during the registrars'
    // filesystem I/O. workspace_folders is collected above to avoid a nested read.
    let prefs = state.app_data.read().preferences.clone();
    for r in registrar::enabled_registrars(&prefs) {
        r.install(port, &auth, &workspace_folders, &prefs);
    }

    log::info!("Claude Code IDE server bound on http://127.0.0.1:{}", port);

    Some(ServerSetup { std_listener, port, auth })
}

/// Start serving the axum router on the pre-bound listener. Runs forever
/// (until the graceful-shutdown channel fires on app exit). Must be called
/// from inside a tokio runtime context (i.e. via `tauri::async_runtime::spawn`).
pub async fn serve_server(app_handle: AppHandle, state: Arc<AppState>, setup: ServerSetup) {
    if let Err(e) = setup.std_listener.set_nonblocking(true) {
        log::error!("Failed to set listener nonblocking: {}", e);
        return;
    }
    let listener = match tokio::net::TcpListener::from_std(setup.std_listener) {
        Ok(l) => l,
        Err(e) => {
            log::error!("Failed to adopt std listener into tokio: {}", e);
            return;
        }
    };

    // Graceful shutdown signal — sender stored in AppState, triggered on app exit
    let (shutdown_tx, mut shutdown_rx) = tokio::sync::watch::channel(false);
    let mut reassert_shutdown = shutdown_tx.subscribe();
    let mut reaper_shutdown = shutdown_tx.subscribe();
    *state.mcp_shutdown.lock() = Some(shutdown_tx);

    // Periodically re-assert our `~/.claude.json` entry so a clobber by the
    // co-owning `claude` CLI self-heals instead of leaving Claude Code dialing a
    // dead port until the next app restart. Runs until graceful shutdown.
    let reassert_port = setup.port;
    let reassert_auth = setup.auth.clone();
    let reassert_state = state.clone();
    tauri::async_runtime::spawn(async move {
        let mut ticker = tokio::time::interval(MCP_REASSERT_INTERVAL);
        ticker.tick().await; // consume the immediate first tick
        loop {
            tokio::select! {
                _ = ticker.tick() => {
                    let prefs = reassert_state.app_data.read().preferences.clone();
                    for r in registrar::enabled_registrars(&prefs) {
                        r.reassert_if_drifted(reassert_port, &reassert_auth, &prefs);
                    }
                }
                res = reassert_shutdown.changed() => {
                    if res.is_err() || *reassert_shutdown.borrow() {
                        break;
                    }
                }
            }
        }
        log::debug!("MCP settings re-assert loop stopped");
    });

    // Dormancy reaper — Codex/Gemini have no SessionEnd hook, so a session going
    // dormant is inferred from the agent process leaving the tab's PTY tree; we then
    // synthesize the SAME SessionEnd teardown Claude gets from its hook. Claude
    // sessions are never inspected (uses_pty_dormancy() == false), so this is a
    // no-op for Claude and leaves its lifecycle byte-identical.
    let reaper_state = state.clone();
    let reaper_app = app_handle.clone();
    tauri::async_runtime::spawn(async move {
        let mut ticker = tokio::time::interval(DORMANCY_POLL_INTERVAL);
        ticker.tick().await; // consume the immediate first tick
        // session_id → consecutive polls the agent was observed gone.
        let mut absent: HashMap<String, u32> = HashMap::new();
        loop {
            tokio::select! {
                _ = ticker.tick() => {
                    reap_dormant_sessions(&reaper_app, &reaper_state, &mut absent);
                }
                res = reaper_shutdown.changed() => {
                    if res.is_err() || *reaper_shutdown.borrow() {
                        break;
                    }
                }
            }
        }
        log::debug!("Dormancy reaper stopped");
    });

    let sse_sessions: SseSessions = Arc::new(parking_lot::RwLock::new(HashMap::new()));
    let connection_tabs: ConnectionTabMap = Arc::new(parking_lot::RwLock::new(HashMap::new()));
    let connection_runtimes: ConnectionRuntimeMap = Arc::new(parking_lot::RwLock::new(HashMap::new()));

    let server_state = ServerState {
        app_handle,
        state,
        expected_auth: setup.auth,
        sse_sessions,
        connection_tabs,
        connection_runtimes,
        connection_count: Arc::new(AtomicUsize::new(0)),
    };

    let app = Router::new()
        // WebSocket — IDE integration (discovered via lock file)
        .route("/", get(ws_upgrade_handler))
        // Streamable HTTP — modern MCP transport (POST returns JSON or SSE)
        .route("/mcp", post(streamable_http_handler))
        // Legacy SSE — older MCP clients (GET /sse + POST /message)
        .route("/sse", get(sse_get_handler))
        .route("/message", post(sse_message_handler))
        // Claude Code hooks — lifecycle events from hook scripts
        .route("/hooks", post(hooks_handler))
        .with_state(server_state);

    log::info!("Claude Code IDE server listening on http://127.0.0.1:{}", setup.port);

    if let Err(e) = axum::serve(listener, app)
        .with_graceful_shutdown(async move {
            let _ = shutdown_rx.wait_for(|v| *v).await;
            log::info!("Claude Code server shutting down");
        })
        .await
    {
        log::error!("Claude Code server error: {}", e);
    }
}

/// Find the window label that owns a given tab ID.
fn find_window_for_tab(state: &Arc<AppState>, tab_id: &str) -> Option<String> {
    let app_data = state.app_data.read();
    for win in &app_data.windows {
        for ws in &win.workspaces {
            for pane in &ws.panes {
                if pane.tabs.iter().any(|t| t.id == tab_id) {
                    return Some(win.label.clone());
                }
            }
        }
    }
    None
}

/// Resolve the best window label to emit a tool event to.
/// Checks windowId, then tabId in the tool arguments, then falls back to the first window.
fn resolve_target_window(state: &Arc<AppState>, arguments: &Value) -> Option<String> {
    let app_data = state.app_data.read();

    // If a windowId (UUID) is provided, find the window by ID
    if let Some(window_id) = arguments.get("windowId").and_then(|v| v.as_str()) {
        if let Some(win) = app_data.windows.iter().find(|w| w.id == window_id) {
            return Some(win.label.clone());
        }
    }

    // If a tabId is provided, find the owning window
    if let Some(tab_id) = arguments.get("tabId").and_then(|v| v.as_str()) {
        drop(app_data); // release read lock for find_window_for_tab
        if let Some(label) = find_window_for_tab(state, tab_id) {
            return Some(label);
        }
        return state.app_data.read().windows.first().map(|w| w.label.clone());
    }

    // Fall back to the first window (main)
    app_data.windows.first().map(|w| w.label.clone())
}

/// Preference metadata: description, type, category, default value.
/// Read-only preferences cannot be set via the setPreference tool.
struct PrefMeta {
    description: &'static str,
    ptype: &'static str,
    category: &'static str,
    read_only: bool,
}

fn preference_meta() -> Vec<(&'static str, PrefMeta)> {
    vec![
        ("ui_font_size", PrefMeta { description: "UI font size in pixels (non-terminal elements)", ptype: "number", category: "Appearance", read_only: false }),
        ("font_size", PrefMeta { description: "Terminal font size in pixels", ptype: "number", category: "Terminal", read_only: false }),
        ("font_family", PrefMeta { description: "Terminal font family", ptype: "string", category: "Terminal", read_only: false }),
        ("cursor_style", PrefMeta { description: "Terminal cursor shape (block, underline, bar)", ptype: "string", category: "Terminal", read_only: false }),
        ("cursor_blink", PrefMeta { description: "Whether the cursor blinks", ptype: "boolean", category: "Terminal", read_only: false }),
        ("scrollback_limit", PrefMeta { description: "Maximum scrollback lines per terminal", ptype: "number", category: "Terminal", read_only: false }),
        ("shell_title_integration", PrefMeta { description: "Allow shell to set tab titles via OSC escape sequences", ptype: "boolean", category: "Terminal", read_only: false }),
        ("shell_integration", PrefMeta { description: "Enable OSC 133 shell integration for command detection", ptype: "boolean", category: "Terminal", read_only: false }),
        ("file_link_action", PrefMeta { description: "How file links in the terminal are activated", ptype: "string", category: "Terminal", read_only: false }),
        ("windows_shell", PrefMeta { description: "Default shell on Windows", ptype: "string", category: "Terminal", read_only: false }),
        ("theme", PrefMeta { description: "Color theme ID (built-in or custom)", ptype: "string", category: "Appearance", read_only: false }),
        ("auto_save_interval", PrefMeta { description: "Auto-save interval in seconds (0 to disable)", ptype: "number", category: "General", read_only: false }),
        ("restore_session", PrefMeta { description: "Restore tabs and workspaces on app restart", ptype: "boolean", category: "General", read_only: false }),
        ("number_duplicated_tabs", PrefMeta { description: "Prefix duplicated tab names with numbers", ptype: "boolean", category: "Tabs", read_only: false }),
        ("tab_button_style", PrefMeta { description: "Tab close button visibility (hover, always)", ptype: "string", category: "Tabs", read_only: false }),
        ("clone_cwd", PrefMeta { description: "Copy working directory when duplicating tabs", ptype: "boolean", category: "Tabs", read_only: false }),
        ("clone_scrollback", PrefMeta { description: "Copy scrollback buffer when duplicating tabs", ptype: "boolean", category: "Tabs", read_only: false }),
        ("clone_ssh", PrefMeta { description: "Copy SSH session when duplicating tabs", ptype: "boolean", category: "Tabs", read_only: false }),
        ("clone_history", PrefMeta { description: "Copy shell history when duplicating tabs", ptype: "boolean", category: "Tabs", read_only: false }),
        ("clone_notes", PrefMeta { description: "Copy notes when duplicating tabs", ptype: "boolean", category: "Tabs", read_only: false }),
        ("clone_auto_resume", PrefMeta { description: "Copy auto-resume config when duplicating tabs", ptype: "boolean", category: "Tabs", read_only: false }),
        ("clone_variables", PrefMeta { description: "Copy trigger variables when duplicating tabs", ptype: "boolean", category: "Tabs", read_only: false }),
        ("notification_mode", PrefMeta { description: "Notification delivery mode (auto, in_app, native, disabled)", ptype: "string", category: "Notifications", read_only: false }),
        ("notify_min_duration", PrefMeta { description: "Minimum command duration (seconds) before notifying on completion", ptype: "number", category: "Notifications", read_only: false }),
        ("notification_sound", PrefMeta { description: "Notification sound (default, system, none)", ptype: "string", category: "Notifications", read_only: false }),
        ("notification_volume", PrefMeta { description: "Notification volume percentage", ptype: "number", category: "Notifications", read_only: false }),
        ("toast_font_size", PrefMeta { description: "Toast notification font size", ptype: "number", category: "Notifications", read_only: false }),
        ("toast_width", PrefMeta { description: "Toast notification width in pixels", ptype: "number", category: "Notifications", read_only: false }),
        ("toast_duration", PrefMeta { description: "Toast auto-dismiss duration in seconds", ptype: "number", category: "Notifications", read_only: false }),
        ("notes_font_size", PrefMeta { description: "Notes panel font size", ptype: "number", category: "Notes", read_only: false }),
        ("notes_font_family", PrefMeta { description: "Notes panel font family", ptype: "string", category: "Notes", read_only: false }),
        ("notes_width", PrefMeta { description: "Notes panel width in pixels", ptype: "number", category: "Notes", read_only: false }),
        ("notes_word_wrap", PrefMeta { description: "Wrap long lines in notes panel", ptype: "boolean", category: "Notes", read_only: false }),
        ("notes_scope", PrefMeta { description: "Default notes panel view (tab, workspace)", ptype: "string", category: "Notes", read_only: false }),
        ("show_recent_workspaces", PrefMeta { description: "Show recently used workspaces section in sidebar", ptype: "boolean", category: "Workspace", read_only: false }),
        ("workspace_sort_order", PrefMeta { description: "Workspace list sort order (default, alphabetical, recent)", ptype: "string", category: "Workspace", read_only: false }),
        ("show_workspace_tab_count", PrefMeta { description: "Show tab count badges on workspace items", ptype: "boolean", category: "Workspace", read_only: false }),
        ("claude_ide", PrefMeta { description: "Enable Claude Code IDE integration (MCP server)", ptype: "boolean", category: "Integration", read_only: false }),
        ("claude_ide_ssh", PrefMeta { description: "Enable MCP bridge over SSH (reverse tunnel for remote Claude Code)", ptype: "boolean", category: "Integration", read_only: false }),
        ("claude_hooks", PrefMeta { description: "Enable hooks integration (session lifecycle events, tab indicators)", ptype: "boolean", category: "Integration", read_only: false }),
        ("claude_auto_resume", PrefMeta { description: "Enable hooks-based auto-resume (programmatic session ID capture)", ptype: "boolean", category: "Integration", read_only: false }),
        ("codex_ide", PrefMeta { description: "Enable Codex IDE integration (MCP server in ~/.codex/config.toml)", ptype: "boolean", category: "Integration", read_only: false }),
        ("codex_ide_ssh", PrefMeta { description: "Enable Codex MCP bridge over SSH", ptype: "boolean", category: "Integration", read_only: false }),
        ("codex_hooks", PrefMeta { description: "Enable Codex lifecycle hooks", ptype: "boolean", category: "Integration", read_only: false }),
        ("codex_auto_resume", PrefMeta { description: "Enable Codex hooks-based auto-resume", ptype: "boolean", category: "Integration", read_only: false }),
        ("codex_hooks_bypass_trust", PrefMeta { description: "Skip the one-time Codex hook-trust prompt (advanced)", ptype: "boolean", category: "Integration", read_only: false }),
        ("backup_directory", PrefMeta { description: "Backup directory path (null = scheduled backups disabled)", ptype: "string", category: "Backup", read_only: false }),
        ("backup_interval", PrefMeta { description: "Scheduled backup interval (off, hourly, daily, weekly, monthly)", ptype: "string", category: "Backup", read_only: false }),
        ("backup_exclude_scrollback", PrefMeta { description: "Exclude terminal scrollback from backups", ptype: "boolean", category: "Backup", read_only: false }),
        ("backup_trim_enabled", PrefMeta { description: "Auto-delete old backups", ptype: "boolean", category: "Backup", read_only: false }),
        ("backup_trim_age", PrefMeta { description: "Max age for auto-trim (1h, 1d, 1w, 1m, 1y)", ptype: "string", category: "Backup", read_only: false }),
        ("prompt_patterns", PrefMeta { description: "Regex patterns for remote prompt/CWD detection", ptype: "string[]", category: "Terminal", read_only: true }),
        ("custom_themes", PrefMeta { description: "User-created custom color themes", ptype: "object[]", category: "Appearance", read_only: true }),
        ("triggers", PrefMeta { description: "Trigger rules for terminal pattern matching", ptype: "object[]", category: "Triggers", read_only: true }),
        ("hidden_default_triggers", PrefMeta { description: "IDs of deleted default trigger templates", ptype: "string[]", category: "Triggers", read_only: true }),
    ]
}

/// Handle tools that can be resolved entirely on the backend without frontend involvement.
/// Returns Some(result) if handled, None if the tool should be forwarded to the frontend.
fn handle_backend_tool(tool_name: &str, arguments: &Value, state: &Arc<AppState>, app_handle: &AppHandle) -> Option<Value> {
    match tool_name {
        "listWindows" => {
            let app_data = state.app_data.read();
            let windows: Vec<Value> = app_data.windows.iter()
                .filter(|w| w.label != "preferences" && w.label != "help")
                .map(|w| {
                    let workspaces: Vec<Value> = w.workspaces.iter().map(|ws| {
                        let tab_count: usize = ws.panes.iter().map(|p| p.tabs.len()).sum();
                        serde_json::json!({
                            "id": ws.id,
                            "name": ws.name,
                            "paneCount": ws.panes.len(),
                            "tabCount": tab_count,
                            "isActive": Some(&ws.id) == w.active_workspace_id.as_ref(),
                        })
                    }).collect();
                    serde_json::json!({
                        "windowId": w.id,
                        "windowLabel": w.label,
                        "workspaceCount": workspaces.len(),
                        "workspaces": workspaces,
                    })
                })
                .collect();
            Some(serde_json::json!({ "windows": windows }))
        }
        "getPreferences" => {
            let query = arguments.get("query").and_then(|v| v.as_str()).unwrap_or("");
            let app_data = state.app_data.read();
            let prefs_json = serde_json::to_value(&app_data.preferences).unwrap_or(Value::Null);

            let entries: Vec<Value> = preference_meta().into_iter()
                .filter(|(key, meta)| {
                    if query.is_empty() { return true; }
                    let q = query.to_lowercase();
                    key.to_lowercase().contains(&q) || meta.description.to_lowercase().contains(&q)
                })
                .map(|(key, meta)| {
                    let value = prefs_json.get(key).cloned().unwrap_or(Value::Null);
                    let mut entry = serde_json::json!({
                        "key": key,
                        "value": value,
                        "description": meta.description,
                        "type": meta.ptype,
                        "category": meta.category,
                    });
                    if meta.read_only {
                        entry["readOnly"] = Value::Bool(true);
                    }
                    entry
                })
                .collect();

            Some(serde_json::json!({ "preferences": entries }))
        }
        "setPreference" => {
            let key = match arguments.get("key").and_then(|v| v.as_str()) {
                Some(k) => k,
                None => return Some(serde_json::json!({ "error": "Missing required parameter: key" })),
            };
            let value = match arguments.get("value") {
                Some(v) => v.clone(),
                None => return Some(serde_json::json!({ "error": "Missing required parameter: value" })),
            };

            // Verify key exists and is not read-only
            let meta_list = preference_meta();
            let meta = match meta_list.iter().find(|(k, _)| *k == key) {
                Some((_, m)) => m,
                None => return Some(serde_json::json!({ "error": format!("Unknown preference key: '{}'. Use getPreferences to discover available keys.", key) })),
            };
            if meta.read_only {
                return Some(serde_json::json!({ "error": format!("Preference '{}' is read-only and cannot be set via this tool.", key) }));
            }

            // Serialize current preferences to JSON, update the key, deserialize back
            let data_clone = {
                let mut app_data = state.app_data.write();
                let mut prefs_json = serde_json::to_value(&app_data.preferences).unwrap_or(Value::Null);
                if let Some(obj) = prefs_json.as_object_mut() {
                    obj.insert(key.to_string(), value.clone());
                }
                match serde_json::from_value::<crate::state::Preferences>(prefs_json) {
                    Ok(updated) => {
                        app_data.preferences = updated;
                        app_data.clone()
                    }
                    Err(e) => return Some(serde_json::json!({ "error": format!("Invalid value for '{}': {}", key, e) })),
                }
            };

            if let Err(e) = crate::state::save_state(&data_clone) {
                return Some(serde_json::json!({ "error": format!("Failed to save: {}", e) }));
            }

            // Broadcast change to all windows
            let _ = app_handle.emit("preferences-changed", &data_clone.preferences);

            Some(serde_json::json!({ "success": true, "key": key, "value": value }))
        }
        "createBackup" => {
            let app_data = state.app_data.read();
            let prefs = &app_data.preferences;

            // Determine backup directory — use argument override or configured default
            let dir = arguments.get("directory").and_then(|v| v.as_str())
                .or(prefs.backup_directory.as_deref());
            let dir = match dir {
                Some(d) => d.to_string(),
                None => return Some(serde_json::json!({ "error": "No backup directory configured. Pass 'directory' or set backup_directory in preferences." })),
            };

            let exclude_scrollback = arguments.get("excludeScrollback")
                .and_then(|v| v.as_bool())
                .unwrap_or(prefs.backup_exclude_scrollback);

            let dir_path = std::path::PathBuf::from(&dir);
            if !dir_path.exists() {
                if let Err(e) = std::fs::create_dir_all(&dir_path) {
                    return Some(serde_json::json!({ "error": format!("Failed to create backup directory: {}", e) }));
                }
            }

            let filtered = crate::commands::workspace::prepare_export(&app_data, exclude_scrollback, &state.scrollback_db);
            let json = match serde_json::to_string_pretty(&filtered) {
                Ok(j) => j,
                Err(e) => return Some(serde_json::json!({ "error": format!("Serialization failed: {}", e) })),
            };

            let now = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default();
            let secs = now.as_secs();
            // Simple UTC timestamp from epoch seconds
            let (s, m, h) = (secs % 60, (secs / 60) % 60, (secs / 3600) % 24);
            let days = secs / 86400;
            // Approximate date — good enough for filename uniqueness
            let y = 1970 + days / 365;
            let remainder = days % 365;
            let mo = remainder / 30 + 1;
            let da = remainder % 30 + 1;
            let timestamp = format!("{:04}{:02}{:02}_{:02}{:02}{:02}", y, mo, da, h, m, s);

            let filename = format!("aiterm_backup_{}.json.gz", timestamp);
            let file_path = dir_path.join(&filename);

            {
                use flate2::write::GzEncoder;
                use flate2::Compression;
                use std::io::Write;
                let file = match std::fs::File::create(&file_path) {
                    Ok(f) => f,
                    Err(e) => return Some(serde_json::json!({ "error": format!("Failed to create file: {}", e) })),
                };
                let mut encoder = GzEncoder::new(file, Compression::default());
                if let Err(e) = encoder.write_all(json.as_bytes()) {
                    return Some(serde_json::json!({ "error": format!("Compression failed: {}", e) }));
                }
                if let Err(e) = encoder.finish() {
                    return Some(serde_json::json!({ "error": format!("Compression finalize failed: {}", e) }));
                }
            }

            let path_str = file_path.to_string_lossy().to_string();
            log::debug!("MCP backup created: {}", path_str);
            Some(serde_json::json!({ "success": true, "path": path_str, "excludedScrollback": exclude_scrollback }))
        }
        "getClaudeSessions" => {
            let sessions = state.agent_sessions.read();
            let app_data = state.app_data.read();

            // Build a map of tab_id → (tab_name, workspace_name) for enrichment
            let mut tab_info: std::collections::HashMap<&str, (&str, &str)> = std::collections::HashMap::new();
            for window in &app_data.windows {
                for ws in &window.workspaces {
                    for pane in &ws.panes {
                        for tab in &pane.tabs {
                            tab_info.insert(&tab.id, (&tab.name, &ws.name));
                        }
                    }
                }
            }

            let entries: Vec<Value> = sessions.iter().map(|(sid, info)| {
                let (tab_name, ws_name) = tab_info.get(info.tab_id.as_str())
                    .copied()
                    .unwrap_or(("unknown", "unknown"));
                serde_json::json!({
                    "sessionId": sid,
                    "tabId": info.tab_id,
                    "tabName": tab_name,
                    "workspaceName": ws_name,
                    "state": info.state,
                    "cwd": info.cwd,
                    "toolName": info.tool_name,
                    "toolDetail": info.tool_detail,
                    "model": info.model,
                })
            }).collect();

            Some(serde_json::json!({ "sessions": entries, "count": entries.len() }))
        }
        _ => None,
    }
}

fn collect_workspace_folders(_state: &Arc<AppState>) -> Vec<String> {
    let mut folders = Vec::new();
    if let Some(home) = dirs::home_dir() {
        folders.push(home.to_string_lossy().to_string());
    }
    folders
}

/// Emit a connection-state change to every terminal window.
/// `app_handle.emit()` broadcasts globally and `listen()` catches both global +
/// window events in Tauri 2, so we target each window individually.
fn emit_connection_state(srv: &ServerState, connected: bool) {
    *srv.state.ide_connected.write() = connected;
    let payload = serde_json::json!({ "connected": connected });
    let app_data = srv.state.app_data.read();
    for win in &app_data.windows {
        emit_dual_to(&srv.app_handle, &win.label, "agent-ide-connection", "claude-code-connection", payload.clone());
    }
}

/// Increment the active-connection ref count. Emits `connected=true` only on
/// the 0→1 transition so overlapping WS/SSE sessions don't log-spam.
fn connection_inc(srv: &ServerState) {
    if srv.connection_count.fetch_add(1, Ordering::SeqCst) == 0 {
        emit_connection_state(srv, true);
    }
}

/// Decrement the active-connection ref count. Emits `connected=false` only on
/// the 1→0 transition. SSE-over-SSH flaps (documented) are absorbed as long as
/// at least one transport session remains live.
fn connection_dec(srv: &ServerState) {
    let prev = srv.connection_count.fetch_sub(1, Ordering::SeqCst);
    debug_assert!(prev > 0, "connection_dec called with zero count");
    if prev == 1 {
        emit_connection_state(srv, false);
    }
}

// ─── WebSocket handler (IDE integration via lock file) ─────────────────────

async fn ws_upgrade_handler(
    ws: WebSocketUpgrade,
    State(srv): State<ServerState>,
    headers: HeaderMap,
) -> Response {
    if extract_auth(&headers).as_deref() != Some(srv.expected_auth.as_str()) {
        log::warn!("Claude Code WS connection rejected: invalid auth");
        return StatusCode::UNAUTHORIZED.into_response();
    }

    ws.on_upgrade(move |socket| handle_ws_connection(socket, srv))
}

async fn handle_ws_connection(socket: WebSocket, srv: ServerState) {
    let ws_connection_id = format!("ws-{}", uuid::Uuid::new_v4());
    log::debug!("Claude Code WS client connected ({})", &ws_connection_id[..11]);
    connection_inc(&srv);

    // response_tx: handle_message sends raw JSON here; main loop writes to WS
    let (response_tx, mut response_rx) = mpsc::unbounded_channel::<String>();
    *srv.state.ide_notify_tx.lock() = Some(response_tx.clone());

    let (mut ws_write, mut ws_read) = socket.split();
    let mut ping_interval = tokio::time::interval(PING_INTERVAL);
    ping_interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

    loop {
        tokio::select! {
            msg = ws_read.next() => {
                match msg {
                    Some(Ok(WsMessage::Text(text))) => {
                        handle_message(&text, &srv.app_handle, &srv.state, &srv.connection_tabs, &srv.connection_runtimes, &ws_connection_id, &response_tx).await;
                    }
                    Some(Ok(WsMessage::Ping(data))) => {
                        let _ = ws_write.send(WsMessage::Pong(data)).await;
                    }
                    Some(Ok(WsMessage::Close(_))) | None => {
                        log::debug!("Claude Code WS client disconnected");
                        break;
                    }
                    Some(Err(e)) => {
                        log::warn!("Claude Code WS error: {}", e);
                        break;
                    }
                    _ => {}
                }
            }
            _ = ping_interval.tick() => {
                if ws_write.send(WsMessage::Ping(vec![].into())).await.is_err() {
                    break;
                }
            }
            response = response_rx.recv() => {
                if let Some(json) = response {
                    if ws_write.send(WsMessage::Text(json.into())).await.is_err() {
                        break;
                    }
                }
            }
        }
    }

    connection_dec(&srv);
    srv.connection_tabs.write().remove(&ws_connection_id);
    srv.connection_runtimes.write().remove(&ws_connection_id);
    // Only clear the global notify channel if we still own it — a newer
    // transport session may have overwritten it while we were running.
    {
        let mut guard = srv.state.ide_notify_tx.lock();
        if guard.as_ref().map(|tx| tx.same_channel(&response_tx)).unwrap_or(false) {
            *guard = None;
        }
    }
    log::debug!("Claude Code WS connection cleaned up ({})", &ws_connection_id[..11]);
}

// ─── Streamable HTTP handler (modern MCP transport) ────────────────────────

/// Handles POST /mcp — the Streamable HTTP MCP transport.
/// Each request is a JSON-RPC message. The response is returned as JSON
/// with an SSE wrapper (text/event-stream) so the client can handle
/// both synchronous and streaming responses uniformly.
async fn streamable_http_handler(
    State(srv): State<ServerState>,
    headers: HeaderMap,
    body: String,
) -> Response {
    if extract_auth(&headers).as_deref() != Some(srv.expected_auth.as_str()) {
        return StatusCode::UNAUTHORIZED.into_response();
    }

    // Connection affinity for streamable HTTP. The client echoes the Mcp-Session-Id we
    // assign on `initialize`; once it does, every request from that agent maps to the
    // same unique `mcp-<id>` key. See derive_streamable_connection_id for why the old
    // shared-constant fallback was the root of the Agent Bridge "bridge dropped" bug.
    let incoming_sid = headers.get("mcp-session-id").and_then(|v| v.to_str().ok());
    let is_initialize = serde_json::from_str::<serde_json::Value>(&body)
        .ok()
        .and_then(|v| v.get("method").and_then(|m| m.as_str()).map(|m| m == "initialize"))
        .unwrap_or(false);
    let (connection_id, assigned_sid) = derive_streamable_connection_id(incoming_sid, is_initialize);

    // Process the JSON-RPC message and get the response
    let response_json = process_message(&body, &srv.app_handle, &srv.state, &srv.connection_tabs, &srv.connection_runtimes, &connection_id).await;

    match response_json {
        Some(json) => {
            // Return as SSE event stream (single event then close)
            let sse_body = format!("event: message\ndata: {}\n\n", json);
            let mut builder = Response::builder()
                .status(200)
                .header(header::CONTENT_TYPE, "text/event-stream")
                .header(header::CACHE_CONTROL, "no-cache");
            // Hand the client the session id we minted on `initialize` so it scopes the
            // rest of the session to a unique connection key (no cross-agent clobber).
            if let Some(sid) = assigned_sid {
                builder = builder.header("mcp-session-id", sid);
            }
            builder.body(Body::from(sse_body)).unwrap()
        }
        None => {
            // Notification — no response needed
            StatusCode::ACCEPTED.into_response()
        }
    }
}

/// Derive a stable, UNIQUE connection id for a streamable-HTTP request.
///
/// Streamable HTTP has no persistent socket, so connection identity rides on the
/// `Mcp-Session-Id` header. The server assigns it on `initialize`; the client then
/// echoes it on every later request. The OLD code never assigned one and fell back to a
/// single shared constant `"streamable-http"` for ALL sessionless requests — so every
/// local agent collapsed onto one affinity key. Since `initSession` is the only writer
/// of that key, the last agent to init silently owned it and every other agent's tool
/// calls resolved to the WRONG tab — the Agent Bridge "bridge dropped" report, fixed
/// only by re-running `/maiterm init` (which re-claimed the shared slot).
///
/// Rules: reuse a provided id; mint+return one on `initialize`; for a sessionless
/// non-initialize request (client not echoing) mint a per-request id so distinct agents
/// are never merged onto one key (affinity recovery re-binds it). Returns
/// `(connection_id, session_id_to_return_in_response)`.
fn derive_streamable_connection_id(incoming_sid: Option<&str>, is_initialize: bool) -> (String, Option<String>) {
    match incoming_sid {
        Some(sid) if !sid.is_empty() => (format!("mcp-{}", sid), None),
        _ if is_initialize => {
            let sid = uuid::Uuid::new_v4().to_string();
            (format!("mcp-{}", sid), Some(sid))
        }
        _ => (format!("mcp-{}", uuid::Uuid::new_v4()), None),
    }
}

// ─── SSE handlers (MCP server via ~/.claude/settings.json) ─────────────────

async fn sse_get_handler(State(srv): State<ServerState>, headers: HeaderMap) -> Response {
    if extract_auth(&headers).as_deref() != Some(srv.expected_auth.as_str()) {
        log::warn!("Claude Code SSE connection rejected: invalid auth");
        return StatusCode::UNAUTHORIZED.into_response();
    }

    let session_id = uuid::Uuid::new_v4().to_string();
    // sse_tx: carries raw JSON response strings from handle_message
    let (sse_tx, sse_rx) = mpsc::unbounded_channel::<String>();
    srv.sse_sessions.write().insert(session_id.clone(), sse_tx.clone());

    // Wire notify_tx to a bridge that forwards raw JSON as SSE data events
    let (notify_tx, mut notify_rx) = mpsc::unbounded_channel::<String>();
    let sse_tx_for_notify = sse_tx.clone();
    tokio::spawn(async move {
        while let Some(json) = notify_rx.recv().await {
            let _ = sse_tx_for_notify.send(json);
        }
    });
    let notify_tx_owner = notify_tx.clone();
    *srv.state.ide_notify_tx.lock() = Some(notify_tx);

    connection_inc(&srv);
    log::debug!("Claude Code SSE client connected (session {}...)", &session_id[..8]);

    // First SSE event: tell Claude where to POST messages
    let endpoint_event =
        axum::body::Bytes::from(format!("event: endpoint\ndata: /message?sessionId={}\n\n", session_id));

    // SSE stream: start with endpoint event, then stream raw JSON wrapped as
    // SSE data events. When rx is idle for SSE_KEEPALIVE_INTERVAL, inject an
    // SSE comment line (": keepalive\n\n") so the TCP connection isn't left
    // silent — prevents SSH reverse tunnels / intermediate proxies / the
    // client itself from declaring the stream dead on idle.
    let stream = futures_util::stream::once(futures_util::future::ready(Ok::<_, std::convert::Infallible>(endpoint_event)))
        .chain(futures_util::stream::unfold(sse_rx, |mut rx| async move {
            match tokio::time::timeout(SSE_KEEPALIVE_INTERVAL, rx.recv()).await {
                Ok(Some(json)) => {
                    let event = axum::body::Bytes::from(format!("data: {}\n\n", json));
                    Some((Ok::<_, std::convert::Infallible>(event), rx))
                }
                Ok(None) => None, // channel closed — end the stream
                Err(_) => {
                    let event = axum::body::Bytes::from(": keepalive\n\n");
                    Some((Ok::<_, std::convert::Infallible>(event), rx))
                }
            }
        }));

    // Background task: detect client disconnect + clean up per-session state.
    // Keepalives are injected inline above; this task only polls for closure.
    let cleanup_srv = srv.clone();
    let cleanup_session_id = session_id.clone();
    tokio::spawn(async move {
        loop {
            tokio::time::sleep(SSE_KEEPALIVE_INTERVAL).await;
            let closed = cleanup_srv
                .sse_sessions
                .read()
                .get(&cleanup_session_id)
                .map(|tx| tx.is_closed())
                .unwrap_or(true);
            if closed {
                break;
            }
        }
        cleanup_srv.sse_sessions.write().remove(&cleanup_session_id);
        cleanup_srv.connection_tabs.write().remove(&format!("sse-{}", cleanup_session_id));
        cleanup_srv.connection_runtimes.write().remove(&format!("sse-{}", cleanup_session_id));
        connection_dec(&cleanup_srv);
        // Only clear the global notify channel if this session still owns it —
        // a newer session may have overwritten it while we were running.
        {
            let mut guard = cleanup_srv.state.ide_notify_tx.lock();
            if guard.as_ref().map(|tx| tx.same_channel(&notify_tx_owner)).unwrap_or(false) {
                *guard = None;
            }
        }
        log::debug!("Claude Code SSE client disconnected");
    });

    Response::builder()
        .status(200)
        .header(header::CONTENT_TYPE, "text/event-stream")
        .header(header::CACHE_CONTROL, "no-cache")
        .header(header::CONNECTION, "keep-alive")
        .body(Body::from_stream(stream))
        .unwrap()
}

#[derive(serde::Deserialize)]
struct SessionQuery {
    #[serde(rename = "sessionId")]
    session_id: String,
}

async fn sse_message_handler(
    State(srv): State<ServerState>,
    Query(params): Query<SessionQuery>,
    headers: HeaderMap,
    body: String,
) -> Response {
    if extract_auth(&headers).as_deref() != Some(srv.expected_auth.as_str()) {
        return StatusCode::UNAUTHORIZED.into_response();
    }

    let tx = srv.sse_sessions.read().get(&params.session_id).cloned();
    let Some(tx) = tx else {
        return StatusCode::NOT_FOUND.into_response();
    };

    let connection_id = format!("sse-{}", params.session_id);
    handle_message(&body, &srv.app_handle, &srv.state, &srv.connection_tabs, &srv.connection_runtimes, &connection_id, &tx).await;
    StatusCode::OK.into_response()
}

// ─── Shared JSON-RPC handler ────────────────────────────────────────────────

/// Decide which tab an unbound connection should recover affinity to, given the
/// set of active tabs (already filtered to the connection's runtime) and the set
/// of tabs that currently have a live connection.
///
/// Rule (unchanged from the inline logic it replaces):
/// - Exactly one active tab → that tab (any unbound call must be from it).
/// - Multiple active tabs → only the SOLE tab lacking a live connection (the one
///   reconnecting); ambiguous otherwise → `None` (caller must re-initSession).
///
/// The runtime FILTERING happens before this is called — `active_same_runtime_tabs`
/// already excludes other runtimes' sessions.
fn recover_affinity(
    active_same_runtime_tabs: &[String],
    bound_tabs: &std::collections::HashSet<&str>,
) -> Option<String> {
    if active_same_runtime_tabs.len() == 1 {
        return Some(active_same_runtime_tabs[0].clone());
    }
    let unbound: Vec<&String> = active_same_runtime_tabs
        .iter()
        .filter(|t| !bound_tabs.contains(t.as_str()))
        .collect();
    if unbound.len() == 1 {
        Some(unbound[0].clone())
    } else {
        None
    }
}

/// Process one JSON-RPC message and return the response as a raw JSON string.
/// Returns `None` for notifications (no id) that don't require a response.
/// `connection_id` identifies the transport connection (SSE session, WS, or streamable-http)
/// for tab affinity tracking.
async fn process_message(
    text: &str,
    app_handle: &AppHandle,
    state: &Arc<AppState>,
    connection_tabs: &ConnectionTabMap,
    connection_runtimes: &ConnectionRuntimeMap,
    connection_id: &str,
) -> Option<String> {
    let req: JsonRpcRequest = match serde_json::from_str(text) {
        Ok(r) => r,
        Err(e) => {
            log::warn!("Invalid JSON-RPC: {}", e);
            return None;
        }
    };

    let id = req.id.clone().unwrap_or(Value::Null);

    match req.method.as_str() {
        "initialize" => {
            let params = req.params.as_ref();
            let client_proto = params.and_then(|p| p.get("protocolVersion")).and_then(|v| v.as_str());
            let client_name = params
                .and_then(|p| p.get("clientInfo"))
                .and_then(|c| c.get("name"))
                .and_then(|v| v.as_str());
            let runtime = crate::state::AgentRuntime::detect(client_name);
            log::info!("MCP initialize: client='{}' detected_runtime={:?} protocol={}",
                client_name.unwrap_or("?"), runtime, client_proto.unwrap_or("(default)"));
            // Remember this connection's runtime so affinity recovery can gate on it
            // (a Codex connection must never recover onto a Claude tab, and vice-versa).
            connection_runtimes.write().insert(connection_id.to_string(), runtime);
            let resp = JsonRpcResponse::success(id, initialize_response(client_proto));
            Some(serde_json::to_string(&resp).unwrap())
        }
        "notifications/initialized" => None,
        "tools/list" => {
            let resp = JsonRpcResponse::success(id, tool_list_response());
            Some(serde_json::to_string(&resp).unwrap())
        }
        "tools/call" => {
            if let Some(params) = req.params {
                let tool_name = params
                    .get("name")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                let mut arguments = params
                    .get("arguments")
                    .cloned()
                    .unwrap_or(Value::Object(serde_json::Map::new()));

                // ── initSession: register connection → tab affinity ──
                if tool_name == "initSession" {
                    let tab_id = arguments.get("tabId").and_then(|v| v.as_str()).unwrap_or("").to_string();
                    let session_id = arguments.get("sessionId").and_then(|v| v.as_str()).unwrap_or("").to_string();

                    if tab_id.is_empty() {
                        let resp = JsonRpcResponse::success(
                            id,
                            serde_json::json!({
                                "content": [{ "type": "text", "text": "Error: tabId is required. Read your tab ID from the SessionStart hook context or $MAITERM_TAB_ID environment variable." }],
                                "isError": true
                            }),
                        );
                        return Some(serde_json::to_string(&resp).unwrap());
                    }

                    // Verify tab exists in this instance.
                    if find_window_for_tab(state, &tab_id).is_none() {
                        let this_server = crate::state::agent_runtime::mcp_server_name(crate::state::AgentRuntime::Claude);
                        let other_server = if cfg!(debug_assertions) { "maiterm" } else { "maiterm-dev" };
                        // A tabId unknown to this instance is usually a *stale*
                        // $MAITERM_TAB_ID (the shell outlived the tab it was spawned
                        // under), not a wrong-instance call. The old "use the other
                        // server" message caused a circular bounce when the id was
                        // stale in BOTH instances (each blamed the other). Give a
                        // deterministic recovery path instead: getActiveTab → retry.
                        let msg = format!(
                            "Tab '{}' was not found in this maiTerm instance ({}). This almost always means your \
                             $MAITERM_TAB_ID is stale (the shell outlived the tab it was created under, or was \
                             started under a different tab). To recover: call getActiveTab to get your real tab \
                             ID, then call initSession again with that tabId. \
                             Only if getActiveTab also fails should you assume you belong to the other instance \
                             and use '{}' tools.",
                            tab_id, this_server, other_server
                        );
                        let resp = JsonRpcResponse::success(
                            id,
                            serde_json::json!({
                                "content": [{ "type": "text", "text": msg }],
                                "isError": true
                            }),
                        );
                        return Some(serde_json::to_string(&resp).unwrap());
                    }

                    // Resolve this connection's runtime ONCE (set on `initialize`).
                    // Defaults to Claude when unknown so Claude behavior is unchanged
                    // and an unrecognized client never misroutes as Codex/Gemini.
                    let runtime = connection_runtimes
                        .read()
                        .get(connection_id)
                        .copied()
                        .unwrap_or(crate::state::AgentRuntime::Claude);

                    // Store connection → tab affinity
                    connection_tabs.write().insert(connection_id.to_string(), tab_id.clone());
                    log::debug!("initSession: connection {} → tab {} (claude session: {})",
                        &connection_id[..connection_id.len().min(8)], &tab_id[..tab_id.len().min(8)],
                        if session_id.is_empty() { "none" } else { &session_id[..session_id.len().min(8)] }
                    );

                    // The session id we surface to the frontend (for the
                    // <runtime>SessionId trigger var + auto-resume). Codex does NOT pass
                    // sessionId to initSession — nothing tells its agent the id (its hook
                    // shim injects no context, unlike Claude's SessionStart command hook)
                    // — so fall back to the SessionStart-hook session linked below.
                    let mut init_session_id = session_id.clone();

                    // Link agent session → tab mapping
                    {
                        use crate::state::app_state::{AgentSessionInfo, AgentSessionState};

                        // If the agent passed sessionId explicitly, use that
                        if !session_id.is_empty() {
                            let mut sessions = state.agent_sessions.write();
                            // Preserve existing fields (model, tool_name) if session already registered
                            let existing = sessions.remove(&session_id);
                            sessions.insert(
                                session_id.clone(),
                                AgentSessionInfo {
                                    runtime,
                                    tab_id: tab_id.clone(),
                                    cwd: existing.as_ref().and_then(|e| e.cwd.clone()),
                                    state: AgentSessionState::Active,
                                    tool_name: existing.as_ref().and_then(|e| e.tool_name.clone()),
                                    tool_detail: existing.as_ref().and_then(|e| e.tool_detail.clone()),
                                    pending_question: existing.as_ref().and_then(|e| e.pending_question.clone()),
                                    pending_question_at: existing.as_ref().and_then(|e| e.pending_question_at),
                                    model: existing.and_then(|e| e.model),
                                    connection_id: Some(connection_id.to_string()),
                                },
                            );
                        }

                        // Also pop the most recent pending SessionStart hook session and link it
                        let pending = {
                            let mut pending = state.pending_agent_sessions.write();
                            let cutoff = std::time::Instant::now() - std::time::Duration::from_secs(30);
                            pending.retain(|(_, _, ts)| *ts > cutoff);
                            pending.pop()
                        };
                        if let Some((pending_sid, pending_cwd, _)) = pending {
                            if session_id.is_empty() || pending_sid != session_id {
                                // Surface this hook session id when the agent passed none
                                // (Codex) so the frontend can wire codexSessionId + resume.
                                if init_session_id.is_empty() {
                                    init_session_id = pending_sid.clone();
                                }
                                let mut sessions = state.agent_sessions.write();
                                sessions.insert(
                                    pending_sid.clone(),
                                    AgentSessionInfo {
                                        runtime,
                                        tab_id: tab_id.clone(),
                                        cwd: pending_cwd,
                                        state: AgentSessionState::Active,
                                        tool_name: None,
                                        tool_detail: None,
                                        pending_question: None,
                                        pending_question_at: None,
                                        model: None,
                                        connection_id: Some(connection_id.to_string()),
                                    },
                                );
                                log::debug!("initSession: linked pending session {} → tab {}",
                                    &pending_sid[..pending_sid.len().min(8)], &tab_id[..tab_id.len().min(8)]);
                                // Re-emit session start now that we know the tab
                                emit_dual(app_handle, "agent-hook-session-start", "claude-hook-session-start", serde_json::json!({
                                    "session_id": pending_sid,
                                    "tab_id": &tab_id,
                                }));
                            }
                        }
                    }

                    // Persist the tab's runtime so it survives a reload and the
                    // frontend can resolve getTabRuntime. Scoped block so the write
                    // lock is dropped before any await/emit below.
                    {
                        let data_clone = {
                            let mut app_data = state.app_data.write();
                            let mut changed = false;
                            'outer: for win in app_data.windows.iter_mut() {
                                for ws in win.workspaces.iter_mut() {
                                    for pane in ws.panes.iter_mut() {
                                        for t in pane.tabs.iter_mut() {
                                            if t.id == tab_id {
                                                if t.runtime != Some(runtime) {
                                                    t.runtime = Some(runtime);
                                                    changed = true;
                                                }
                                                break 'outer;
                                            }
                                        }
                                    }
                                }
                            }
                            if changed { Some(app_data.clone()) } else { None }
                        };
                        if let Some(data) = data_clone {
                            if let Err(e) = crate::state::save_state(&data) {
                                log::warn!("initSession: failed to persist tab runtime: {}", e);
                            }
                        }
                    }

                    // Emit so the frontend sets the <runtime>SessionId trigger variable +
                    // configures auto-resume. Uses the explicit sessionId when the agent
                    // provided one (Claude), else the linked SessionStart-hook session
                    // (Codex, which doesn't pass sessionId).
                    if !init_session_id.is_empty() {
                        emit_dual(app_handle, "agent-init-session", "claude-init-session", serde_json::json!({
                            "runtime": runtime.as_key(),
                            "tab_id": &tab_id,
                            "session_id": &init_session_id,
                        }));
                    }

                    let resp = JsonRpcResponse::success(
                        id,
                        serde_json::json!({
                            "content": [{ "type": "text", "text": format!(
                                "Session initialized. All subsequent tool calls on this connection will target tab {}. You no longer need to pass tabId.",
                                tab_id
                            ) }]
                        }),
                    );
                    return Some(serde_json::to_string(&resp).unwrap());
                }

                // ── Auto-inject tabId from connection affinity ──
                // If the tool call doesn't include tabId but this connection has affinity, inject it.
                // Falls back to agent_sessions when SSE reconnects cleared the connection affinity.
                // Snapshot the affinity tab once so the has-affinity check and the injection
                // use the same value (no TOCTOU race with concurrent connection cleanup).
                let mut affinity_tab: Option<String> = connection_tabs.read().get(connection_id).cloned();

                // SSE reconnect recovery: if no connection affinity, check if there's
                // an active claude session whose tab we can restore affinity from.
                // Strategy: find sessions whose stored connection_id is no longer in
                // connection_tabs (orphaned by SSE disconnect). If exactly one orphaned
                // session exists, it's the one reconnecting. Falls back to the simpler
                // "exactly 1 active session" heuristic if connection_ids aren't set.
                if affinity_tab.is_none() {
                    let sessions = state.agent_sessions.read();
                    let ct = connection_tabs.read();

                    // RUNTIME GATE: recover affinity only among sessions of the SAME
                    // runtime as this connection. If the connection's runtime is known
                    // (set on `initialize`), a Codex connection can never recover onto a
                    // Claude tab (and vice-versa). If UNKNOWN (e.g. a connection that
                    // never did `initialize`), don't filter — preserving today's behavior
                    // exactly. For a Claude connection in a Claude-only setup all sessions
                    // are Claude, so this is identical to before.
                    let conn_runtime = connection_runtimes.read().get(connection_id).copied();

                    // Unique tabs that currently have a live (same-runtime) agent session.
                    let active_tabs: Vec<String> = sessions.values()
                        .filter(|info| conn_runtime.map_or(true, |rt| info.runtime == rt))
                        .map(|info| info.tab_id.clone())
                        .collect::<std::collections::HashSet<_>>()
                        .into_iter()
                        .collect();

                    // Recover affinity ONLY when we can name the caller unambiguously.
                    // With one active agent, any unbound call must be from it. With
                    // MULTIPLE active agents (e.g. an Agent Bridge — two bridged Claudes),
                    // guessing which tab an unbound connection belongs to can bind agent
                    // A's call to agent B's tab, corrupting cross-agent routing (a
                    // sendToBridgedAgent/getBridgedAgent from A would target B). So when
                    // 2+ agents are live, recover only if exactly ONE of them currently
                    // lacks a live connection affinity (the sole reconnecting one);
                    // otherwise refuse and make the caller re-initSession.
                    let bound: std::collections::HashSet<&str> =
                        ct.values().map(|s| s.as_str()).collect();
                    let recovered = recover_affinity(&active_tabs, &bound);

                    let active_count = active_tabs.len();
                    drop(ct);
                    drop(sessions);

                    if let Some(tab_id) = recovered {
                        connection_tabs.write().insert(connection_id.to_string(), tab_id.clone());
                        // Update the session's connection_id to the new connection
                        let mut sessions = state.agent_sessions.write();
                        for info in sessions.values_mut() {
                            if info.tab_id == tab_id {
                                info.connection_id = Some(connection_id.to_string());
                            }
                        }
                        log::debug!("Restored connection affinity for {} (sole unbound of {} active agent(s))",
                            &connection_id[..connection_id.len().min(11)], active_count);
                        affinity_tab = Some(tab_id);
                    } else if active_count > 1 {
                        log::debug!("Affinity recovery declined for {}: {} active agents, ambiguous — requiring initSession",
                            &connection_id[..connection_id.len().min(11)], active_count);
                    }
                }

                if let Some(ref tab) = affinity_tab {
                    if arguments.get("tabId").and_then(|v| v.as_str()).map_or(true, |s| s.is_empty()) {
                        if let Some(obj) = arguments.as_object_mut() {
                            obj.insert("tabId".to_string(), Value::String(tab.clone()));
                        }
                    }
                } else {
                    // No affinity yet — require initSession first for tab-specific tools.
                    // Allow global tools that don't need a tab to pass through.
                    let global_tools = [
                        "getOpenEditors", "getWorkspaceFolders", "getDiagnostics",
                        "sendNotification", "readLogs", "listWindows", "listWorkspaces",
                        "getPreferences", "setPreference", "createBackup",
                        "getClaudeSessions",
                    ];
                    if !global_tools.contains(&tool_name.as_str())
                        && arguments.get("tabId").and_then(|v| v.as_str()).map_or(true, |s| s.is_empty())
                    {
                        let resp = JsonRpcResponse::success(
                            id,
                            serde_json::json!({
                                "content": [{ "type": "text", "text":
                                    "Session not initialized. You must call initSession with your tabId first. \
                                     Read your tab ID from $MAITERM_TAB_ID environment variable."
                                }],
                                "isError": true
                            }),
                        );
                        return Some(serde_json::to_string(&resp).unwrap());
                    }
                }

                // Guard: if a tabId is provided, verify it exists in THIS instance.
                // Prevents cross-talk when both dev and prod are running.
                if let Some(tab_id) = arguments.get("tabId").and_then(|v| v.as_str()) {
                    if !tab_id.is_empty() && find_window_for_tab(state, tab_id).is_none() {
                        let other_server = if cfg!(debug_assertions) { "maiterm" } else { "maiterm-dev" };
                        let this_server = crate::state::agent_runtime::mcp_server_name(crate::state::AgentRuntime::Claude);
                        let err_msg = format!(
                            "Tab '{}' does not exist in this maiTerm instance ({}). \
                             You may be calling the wrong MCP server. Use '{}' tools instead.",
                            tab_id, this_server, other_server
                        );
                        log::warn!("MCP tool guard: {}", err_msg);
                        let resp = JsonRpcResponse::success(
                            id,
                            serde_json::json!({
                                "content": [{ "type": "text", "text": err_msg }],
                                "isError": true
                            }),
                        );
                        return Some(serde_json::to_string(&resp).unwrap());
                    }
                }

                // Backend-only tools: handle directly without emitting to frontend
                if let Some(result) = handle_backend_tool(&tool_name, &arguments, state, app_handle) {
                    let content_text = serde_json::to_string(&result).unwrap_or_default();
                    let resp = JsonRpcResponse::success(
                        id,
                        serde_json::json!({
                            "content": [{ "type": "text", "text": content_text }]
                        }),
                    );
                    Some(serde_json::to_string(&resp).unwrap())
                } else {
                    // Frontend-handled tools: emit to the correct window
                    let request_id = uuid::Uuid::new_v4().to_string();
                    let (tx, rx) = oneshot::channel::<Value>();
                    state
                        .ide_pending
                        .write()
                        .insert(request_id.clone(), tx);

                    let payload = serde_json::json!({
                        "request_id": request_id,
                        "tool": tool_name,
                        "arguments": arguments,
                    });

                    // Emit to the specific window that owns the tab (avoids race
                    // when preferences/help windows also listen for the event)
                    if let Some(label) = resolve_target_window(state, &arguments) {
                        emit_dual_to(app_handle, &label, "agent-ide-tool", "claude-code-tool", payload);
                    } else {
                        emit_dual(app_handle, "agent-ide-tool", "claude-code-tool", payload);
                    }

                    match tokio::time::timeout(RESPONSE_TIMEOUT, rx).await {
                        Ok(Ok(result)) => {
                            let content_text = serde_json::to_string(&result).unwrap_or_default();
                            let resp = JsonRpcResponse::success(
                                id,
                                serde_json::json!({
                                    "content": [{ "type": "text", "text": content_text }]
                                }),
                            );
                            Some(serde_json::to_string(&resp).unwrap())
                        }
                        Ok(Err(_)) => {
                            state.ide_pending.write().remove(&request_id);
                            let resp = JsonRpcResponse::error(
                                id,
                                -32603,
                                "Tool handler disconnected".to_string(),
                            );
                            Some(serde_json::to_string(&resp).unwrap())
                        }
                        Err(_) => {
                            state.ide_pending.write().remove(&request_id);
                            let resp = JsonRpcResponse::error(
                                id,
                                -32603,
                                "Tool response timeout".to_string(),
                            );
                            Some(serde_json::to_string(&resp).unwrap())
                        }
                    }
                }
            } else {
                let resp = JsonRpcResponse::error(id, -32602, "Missing params".to_string());
                Some(serde_json::to_string(&resp).unwrap())
            }
        }
        _ => {
            if req.id.is_some() {
                let resp = JsonRpcResponse::error(
                    id,
                    -32601,
                    format!("Method not found: {}", req.method),
                );
                Some(serde_json::to_string(&resp).unwrap())
            } else {
                None
            }
        }
    }
}

// ─── Agent Hooks ────────────────────────────────────────────────────────────

/// Canonical, runtime-neutral meaning of a raw hook event. Each runtime's wire
/// event names normalize into one of these (see `normalize_hook_event`) so the
/// handler logic is written once. Claude expresses "waiting for the human" as a
/// `Notification` with a `notification_type` subfield; a non-Claude runtime that
/// signals the same thing via a distinct top-level event (e.g. Codex's
/// `PermissionRequest`) normalizes to the SAME `Notification` variant with the
/// subtype synthesized, so it flows through the identical state/emit path.
#[derive(Debug, PartialEq)]
enum HookPhase {
    SessionStart,
    SessionEnd,
    Stop,
    Prompt,
    ToolPre,
    ToolPost,
    Notification { notification_type: String },
    Compact,
    Other,
}

/// Map a raw hook event name (+ body, for the Notification subtype) to a canonical
/// `HookPhase`. `runtime` is accepted for future runtimes whose names diverge; the
/// names handled here are shared by Claude and Codex (Codex-only events like
/// PermissionRequest/PostCompact are added in a later stage). Unrecognized names
/// fall through to `Other` (logged, no state change) — matching the prior behavior.
fn normalize_hook_event(_runtime: crate::state::AgentRuntime, name: &str, event: &Value) -> HookPhase {
    match name {
        "SessionStart" => HookPhase::SessionStart,
        "SessionEnd" => HookPhase::SessionEnd,
        "Stop" => HookPhase::Stop,
        "UserPromptSubmit" => HookPhase::Prompt,
        "PreToolUse" => HookPhase::ToolPre,
        "PostToolUse" => HookPhase::ToolPost,
        "PreCompact" => HookPhase::Compact,
        "Notification" => HookPhase::Notification {
            notification_type: event
                .get("notification_type")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string(),
        },
        // Codex expresses "the human is at an approval prompt" as a top-level
        // PermissionRequest event (not a Notification subtype). Synthesize the
        // permission_prompt subtype so it flows through the SAME Notification arm that
        // sets WaitingPermission — the bridge then holds delivery identically.
        "PermissionRequest" => HookPhase::Notification {
            notification_type: "permission_prompt".to_string(),
        },
        // Codex emits PostCompact alongside PreCompact; both are compaction signals.
        "PostCompact" => HookPhase::Compact,
        // NOTE: Codex has no SessionEnd hook — a Codex session going away is derived from
        // dormancy (PTY exit / shell-prompt return), not a hook event.
        _ => HookPhase::Other,
    }
}

/// Handle POST /hooks — receives agent hook events (Claude today; other runtimes
/// post here with a ?runtime= tag). SessionStart registers a session→tab mapping;
/// other events use it to route Tauri events to the correct frontend tab.
async fn hooks_handler(
    State(srv): State<ServerState>,
    headers: HeaderMap,
    Query(params): Query<HashMap<String, String>>,
    body: String,
) -> Response {
    if extract_auth(&headers).as_deref() != Some(srv.expected_auth.as_str()) {
        return StatusCode::UNAUTHORIZED.into_response();
    }

    let event: Value = match serde_json::from_str(&body) {
        Ok(v) => v,
        Err(_) => return StatusCode::BAD_REQUEST.into_response(),
    };

    let hook_event_name = event
        .get("hook_event_name")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let session_id = event
        .get("session_id")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();

    log::debug!("Claude hook: received '{}' session={}", hook_event_name, &session_id[..session_id.len().min(8)]);

    // Which agent runtime this hook came from: explicit ?runtime= (set by non-Claude
    // shims), else the runtime already recorded for this session, else Claude.
    let runtime = params.get("runtime")
        .and_then(|s| crate::state::AgentRuntime::from_key(s))
        .or_else(|| srv.state.agent_sessions.read().get(&session_id).map(|s| s.runtime))
        .unwrap_or(crate::state::AgentRuntime::Claude);
    let runtime_key = runtime.as_key();

    // tab_id comes from query param (set by the command hook script from $MAITERM_TAB_ID)
    // Validate it actually exists — it may be stale after HMR reload or tab recreation.
    let tab_id_from_param = params.get("tab_id").and_then(|raw_id| {
        if raw_id.is_empty() {
            return None;
        }
        // Check if this tab ID exists in any workspace
        let exists = {
            let app_data = srv.state.app_data.read();
            app_data.windows.iter().any(|w| {
                w.workspaces.iter().any(|ws| {
                    ws.panes.iter().any(|p| p.tabs.iter().any(|t| t.id == *raw_id))
                })
            })
        };
        if exists {
            Some(raw_id.clone())
        } else {
            // Tab ID doesn't exist — don't fall back to active tab as that
            // causes cross-talk between dev/prod instances.
            log::warn!(
                "Claude hook: tab_id '{}' not found (stale env var or wrong instance), ignoring",
                raw_id
            );
            None
        }
    });

    match normalize_hook_event(runtime, hook_event_name, &event) {
        HookPhase::SessionStart => {
            let tab_id = tab_id_from_param.clone().unwrap_or_default();
            let cwd = event.get("cwd").and_then(|v| v.as_str()).map(String::from);
            let model = event.get("model").and_then(|v| v.as_str()).map(String::from);

            if !session_id.is_empty() && !tab_id.is_empty() {
                use crate::state::app_state::{AgentSessionInfo, AgentSessionState};
                let mut sessions = srv.state.agent_sessions.write();
                sessions.insert(
                    session_id.clone(),
                    AgentSessionInfo {
                        runtime,
                        tab_id: tab_id.clone(),
                        cwd: cwd.clone(),
                        state: AgentSessionState::Active,
                        tool_name: None,
                        tool_detail: None,
                        pending_question: None,
                        pending_question_at: None,
                        model: model.clone(),
                        connection_id: None,
                    },
                );
                log::info!("Claude hook: session {} started for tab {}", session_id, tab_id);
            } else if !session_id.is_empty() {
                // No tab_id yet — buffer for initSession to pick up
                let mut pending = srv.state.pending_agent_sessions.write();
                // Clean entries older than 30s
                let cutoff = std::time::Instant::now() - std::time::Duration::from_secs(30);
                pending.retain(|(_, _, ts)| *ts > cutoff);
                pending.push((session_id.clone(), cwd.clone(), std::time::Instant::now()));
                log::info!("Claude hook: session {} started (pending tab assignment)", session_id);
            }

            // Persist the tab's runtime for NON-Claude runtimes from the hook path.
            // Codex doesn't always call initSession (where Tab.runtime is otherwise set),
            // so tag it here too; Claude tabs are left untouched (None → defaults claude).
            if runtime != crate::state::AgentRuntime::Claude && !tab_id.is_empty() {
                let mut app_data = srv.state.app_data.write();
                let mut changed = false;
                'find_tab: for win in &mut app_data.windows {
                    for ws in &mut win.workspaces {
                        for pane in &mut ws.panes {
                            for t in &mut pane.tabs {
                                if t.id == tab_id {
                                    if t.runtime != Some(runtime) {
                                        t.runtime = Some(runtime);
                                        changed = true;
                                    }
                                    break 'find_tab;
                                }
                            }
                        }
                    }
                }
                if changed {
                    let data = app_data.clone();
                    drop(app_data);
                    if let Err(e) = crate::state::save_state(&data) {
                        log::warn!("Failed to persist tab runtime for {}: {}", tab_id, e);
                    }
                }
            }

            let source = event.get("source").and_then(|v| v.as_str()).unwrap_or("");
            emit_dual(&srv.app_handle, "agent-hook-session-start", "claude-hook-session-start", serde_json::json!({
                "runtime": runtime_key,
                "session_id": session_id,
                "tab_id": if tab_id.is_empty() { None } else { Some(&tab_id) },
                "cwd": event.get("cwd"),
                "source": source,
            }));

            // Non-Claude runtimes (Codex) don't pass sessionId to initSession — nothing
            // tells their agent the id — so the SessionStart hook (which carries both the
            // resumable session id and the tab) is where we surface the init-session
            // event that wires <runtime>SessionId + auto-resume on the frontend. Claude
            // still gets its init-session from the initSession tool (explicit sessionId).
            if runtime != crate::state::AgentRuntime::Claude
                && !session_id.is_empty()
                && !tab_id.is_empty()
            {
                emit_dual(&srv.app_handle, "agent-init-session", "claude-init-session", serde_json::json!({
                    "runtime": runtime_key,
                    "tab_id": &tab_id,
                    "session_id": &session_id,
                }));
            }
        }

        HookPhase::SessionEnd => {
            let tab_id = {
                let mut sessions = srv.state.agent_sessions.write();
                sessions.remove(&session_id).map(|s| s.tab_id)
            }
            .or(tab_id_from_param);

            log::info!("Claude hook: session {} ended (tab {:?})", session_id, tab_id);

            emit_dual(&srv.app_handle, "agent-hook-session-end", "claude-hook-session-end", serde_json::json!({
                "runtime": runtime_key,
                "session_id": session_id,
                "tab_id": tab_id,
                "reason": event.get("reason"),
            }));
        }

        HookPhase::Notification { notification_type } => {
            let tab_id = {
                let sessions = srv.state.agent_sessions.read();
                sessions.get(&session_id).map(|s| s.tab_id.clone())
            }
            .or(tab_id_from_param);

            // notification_type comes from normalize_hook_event: the raw field for
            // Claude, or a synthesized subtype for a runtime that signals the same
            // human-waiting state via a distinct top-level event.
            // Update session state based on notification type. The `_` arm PRESERVES
            // the prior state for an unrecognized type (and the event still emits).
            if !session_id.is_empty() {
                use crate::state::app_state::AgentSessionState;
                let mut sessions = srv.state.agent_sessions.write();
                if let Some(session) = sessions.get_mut(&session_id) {
                    session.state = match notification_type.as_str() {
                        "idle_prompt" => AgentSessionState::WaitingInput,
                        "permission_prompt" => AgentSessionState::WaitingPermission,
                        _ => session.state,
                    };
                    // Codex's PermissionRequest carries the gated tool's name + input on the
                    // event itself (Claude's permission Notification does not — its tool context
                    // came from the preceding PreToolUse). Refresh so the maiLink permission
                    // card names the tool/command being approved even if hook ordering diverges.
                    if notification_type == "permission_prompt" {
                        if let Some(tn) = event
                            .get("tool_name")
                            .and_then(|v| v.as_str())
                            .filter(|s| !s.is_empty())
                        {
                            session.tool_name = Some(tn.to_string());
                            session.tool_detail = event
                                .get("tool_input")
                                .and_then(crate::mailink::transcript::compact_tool_arg);
                        }
                    }
                }
            }

            log::debug!("Claude hook: Notification type='{}' session={} (tab {:?})",
                notification_type, &session_id[..session_id.len().min(8)], tab_id);
            emit_dual(&srv.app_handle, "agent-hook-notification", "claude-hook-notification", serde_json::json!({
                "runtime": runtime_key,
                "session_id": session_id,
                "tab_id": tab_id,
                "notification_type": notification_type,
                "title": event.get("title"),
                "body": event.get("body"),
            }));
        }

        HookPhase::Stop => {
            let tab_id = {
                let sessions = srv.state.agent_sessions.read();
                sessions.get(&session_id).map(|s| s.tab_id.clone())
            }
            .or(tab_id_from_param);

            // Update session state to stopped + clear tool
            if !session_id.is_empty() {
                use crate::state::app_state::AgentSessionState;
                let mut sessions = srv.state.agent_sessions.write();
                if let Some(session) = sessions.get_mut(&session_id) {
                    session.state = AgentSessionState::Stopped;
                    session.tool_name = None;
                    session.tool_detail = None;
                    session.pending_question = None;
                    session.pending_question_at = None;
                }
            }

            log::debug!("Claude hook: Stop for session {} (tab {:?})", &session_id[..session_id.len().min(8)], tab_id);
            emit_dual(&srv.app_handle, "agent-hook-stop", "claude-hook-stop", serde_json::json!({
                "runtime": runtime_key,
                "session_id": session_id,
                "tab_id": tab_id,
            }));
        }

        HookPhase::Prompt => {
            let tab_id = {
                let sessions = srv.state.agent_sessions.read();
                sessions.get(&session_id).map(|s| s.tab_id.clone())
            }
            .or(tab_id_from_param);

            // Update session state to processing
            if !session_id.is_empty() {
                use crate::state::app_state::AgentSessionState;
                let mut sessions = srv.state.agent_sessions.write();
                if let Some(session) = sessions.get_mut(&session_id) {
                    session.state = AgentSessionState::Active;
                }
            }

            log::debug!("Claude hook: UserPromptSubmit session={} (tab {:?})", &session_id[..session_id.len().min(8)], tab_id);
            emit_dual(&srv.app_handle, "agent-hook-user-prompt", "claude-hook-user-prompt", serde_json::json!({
                "runtime": runtime_key,
                "session_id": session_id,
                "tab_id": tab_id,
            }));
        }

        HookPhase::ToolPre => {
            let tab_id = {
                let sessions = srv.state.agent_sessions.read();
                sessions.get(&session_id).map(|s| s.tab_id.clone())
            }
            .or(tab_id_from_param);

            let tool_name = event
                .get("tool_name")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();

            // Update session state back to active + track current tool
            if !session_id.is_empty() {
                use crate::state::app_state::AgentSessionState;
                let mut sessions = srv.state.agent_sessions.write();
                if let Some(session) = sessions.get_mut(&session_id) {
                    session.state = AgentSessionState::Active;
                    session.tool_name = if tool_name.is_empty() { None } else { Some(tool_name.clone()) };
                    // Compact primary-arg label (e.g. the Bash command) so a permission prompt
                    // for this tool can show WHAT is being approved (maiLink card).
                    session.tool_detail = event
                        .get("tool_input")
                        .and_then(crate::mailink::transcript::compact_tool_arg);
                    // Capture the structured AskUserQuestion prompt (its tool_input.questions feed
                    // the maiLink PendingPrompt); any other tool starting means no open question.
                    if tool_name == "AskUserQuestion" {
                        session.pending_question = event.get("tool_input").cloned();
                        session.pending_question_at = Some(
                            std::time::SystemTime::now()
                                .duration_since(std::time::UNIX_EPOCH)
                                .map(|d| d.as_millis() as i64)
                                .unwrap_or(0),
                        );
                    } else {
                        session.pending_question = None;
                        session.pending_question_at = None;
                    }
                }
            }

            log::debug!("Claude hook: PreToolUse tool='{}' session={} (tab {:?})",
                tool_name, &session_id[..session_id.len().min(8)], tab_id);
            emit_dual(&srv.app_handle, "agent-hook-pre-tool-use", "claude-hook-pre-tool-use", serde_json::json!({
                "runtime": runtime_key,
                "session_id": session_id,
                "tab_id": tab_id,
                "tool_name": tool_name,
                "tool_input": event.get("tool_input"),
            }));
        }

        HookPhase::ToolPost => {
            let tab_id = {
                let sessions = srv.state.agent_sessions.read();
                sessions.get(&session_id).map(|s| s.tab_id.clone())
            }
            .or(tab_id_from_param);

            let tool_name = event
                .get("tool_name")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();

            // Clear current tool (back to thinking)
            if !session_id.is_empty() {
                use crate::state::app_state::AgentSessionState;
                let mut sessions = srv.state.agent_sessions.write();
                if let Some(session) = sessions.get_mut(&session_id) {
                    session.tool_name = None;
                    session.tool_detail = None;
                    // AskUserQuestion completing means the human answered → no open question.
                    if tool_name == "AskUserQuestion" {
                        session.pending_question = None;
                        session.pending_question_at = None;
                        // The WaitingPermission coinciding with an open ask (Claude fires a
                        // permission_prompt Notification while AskUserQuestion waits) is spent
                        // the moment the ask completes. Without this reset it lingers until the
                        // NEXT hook event (seconds of post-answer thinking), and maiLink
                        // synthesizes a ghost "permission" card from the stale state. Scoped to
                        // AskUserQuestion so a real gate held for another (parallel) tool is
                        // never masked.
                        if matches!(session.state, AgentSessionState::WaitingPermission) {
                            session.state = AgentSessionState::Active;
                        }
                    }
                }
            }

            log::debug!("Claude hook: PostToolUse tool='{}' session={} (tab {:?})",
                tool_name, &session_id[..session_id.len().min(8)], tab_id);
            emit_dual(&srv.app_handle, "agent-hook-post-tool-use", "claude-hook-post-tool-use", serde_json::json!({
                "runtime": runtime_key,
                "session_id": session_id,
                "tab_id": tab_id,
                "tool_name": tool_name,
                "tool_input": event.get("tool_input"),
            }));
        }

        HookPhase::Compact => {
            let tab_id = {
                let sessions = srv.state.agent_sessions.read();
                sessions.get(&session_id).map(|s| s.tab_id.clone())
            }
            .or(tab_id_from_param);

            let trigger = event
                .get("trigger")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();

            log::debug!("Claude hook: PreCompact trigger='{}' session={} (tab {:?})",
                trigger, &session_id[..session_id.len().min(8)], tab_id);
            emit_dual(&srv.app_handle, "agent-hook-pre-compact", "claude-hook-pre-compact", serde_json::json!({
                "runtime": runtime_key,
                "session_id": session_id,
                "tab_id": tab_id,
                "trigger": trigger,
            }));
        }

        HookPhase::Other => {
            log::debug!("Claude hook: unhandled event type '{}'", hook_event_name);
        }
    }

    StatusCode::OK.into_response()
}

/// Channel-based wrapper: process a message and send the response to a channel.
/// Used by WebSocket and legacy SSE handlers.
async fn handle_message(
    text: &str,
    app_handle: &AppHandle,
    state: &Arc<AppState>,
    connection_tabs: &ConnectionTabMap,
    connection_runtimes: &ConnectionRuntimeMap,
    connection_id: &str,
    response_tx: &mpsc::UnboundedSender<String>,
) {
    if let Some(json) = process_message(text, app_handle, state, connection_tabs, connection_runtimes, connection_id).await {
        let _ = response_tx.send(json);
    }
}

#[cfg(test)]
mod tests {
    use super::derive_streamable_connection_id;
    use super::recover_affinity;
    use super::{normalize_hook_event, HookPhase};
    use crate::state::AgentRuntime;
    use std::collections::HashSet;

    fn norm(name: &str, ev: serde_json::Value) -> HookPhase {
        normalize_hook_event(AgentRuntime::Claude, name, &ev)
    }

    #[test]
    fn claude_event_names_map_to_canonical_phases() {
        let nil = serde_json::json!({});
        assert_eq!(norm("SessionStart", nil.clone()), HookPhase::SessionStart);
        assert_eq!(norm("SessionEnd", nil.clone()), HookPhase::SessionEnd);
        assert_eq!(norm("Stop", nil.clone()), HookPhase::Stop);
        assert_eq!(norm("UserPromptSubmit", nil.clone()), HookPhase::Prompt);
        assert_eq!(norm("PreToolUse", nil.clone()), HookPhase::ToolPre);
        assert_eq!(norm("PostToolUse", nil.clone()), HookPhase::ToolPost);
        assert_eq!(norm("PreCompact", nil.clone()), HookPhase::Compact);
        // An unknown event falls through to Other (logged, no state change).
        assert_eq!(norm("Frobnicate", nil), HookPhase::Other);
    }

    #[test]
    fn notification_carries_its_subtype_for_the_permission_path() {
        // permission_prompt / idle_prompt are what drive WaitingPermission/WaitingInput.
        assert_eq!(
            norm("Notification", serde_json::json!({ "notification_type": "permission_prompt" })),
            HookPhase::Notification { notification_type: "permission_prompt".to_string() }
        );
        assert_eq!(
            norm("Notification", serde_json::json!({ "notification_type": "idle_prompt" })),
            HookPhase::Notification { notification_type: "idle_prompt".to_string() }
        );
        // A Notification with NO/unknown subtype still normalizes (the arm preserves
        // state and still emits) — never silently dropped.
        assert_eq!(
            norm("Notification", serde_json::json!({})),
            HookPhase::Notification { notification_type: String::new() }
        );
    }

    #[test]
    fn codex_events_map_to_canonical_phases() {
        let nil = serde_json::json!({});
        // Codex's top-level PermissionRequest converges with Claude's permission_prompt:
        // both become the Notification phase carrying "permission_prompt" (the only path
        // that sets WaitingPermission), so the bridge holds delivery identically.
        assert_eq!(
            norm("PermissionRequest", nil.clone()),
            HookPhase::Notification { notification_type: "permission_prompt".to_string() }
        );
        // Codex's PostCompact joins PreCompact as a compaction signal.
        assert_eq!(norm("PostCompact", nil.clone()), HookPhase::Compact);
        // Codex shares these names with Claude verbatim.
        assert_eq!(norm("Stop", nil.clone()), HookPhase::Stop);
        assert_eq!(norm("PreToolUse", nil), HookPhase::ToolPre);
    }

    // Regression for the Agent Bridge "bridge dropped" bug: sessionless streamable-HTTP
    // requests used to collapse onto one shared "streamable-http" key, so two local
    // agents clobbered each other's tab affinity. Each initialize must mint its OWN id.
    #[test]
    fn initialize_mints_unique_session_ids() {
        let (c1, a1) = derive_streamable_connection_id(None, true);
        let (c2, a2) = derive_streamable_connection_id(None, true);
        assert!(a1.is_some() && a2.is_some(), "initialize must assign a session id");
        assert_ne!(c1, c2, "two agents must NOT share one connection key");
        assert_ne!(c1, "streamable-http", "must not fall back to the shared constant");
        assert_eq!(c1, format!("mcp-{}", a1.unwrap()), "returned id must match the connection key");
    }

    #[test]
    fn provided_session_id_is_reused_verbatim() {
        let (c, a) = derive_streamable_connection_id(Some("abc-123"), false);
        assert_eq!(c, "mcp-abc-123");
        assert!(a.is_none(), "no new id assigned when the client already has one");
    }

    #[test]
    fn extract_auth_accepts_known_headers_and_rejects_junk() {
        use axum::http::HeaderMap;
        use super::extract_auth;
        let mut h = HeaderMap::new();
        assert_eq!(extract_auth(&h), None, "no header -> None (never authenticates)");
        h.insert("x-claude-code-ide-authorization", "tok-claude".parse().unwrap());
        assert_eq!(extract_auth(&h).as_deref(), Some("tok-claude"));
        let mut h2 = HeaderMap::new();
        h2.insert("authorization", "Bearer tok-codex".parse().unwrap());
        assert_eq!(extract_auth(&h2).as_deref(), Some("tok-codex"), "Bearer prefix stripped");
        let mut h3 = HeaderMap::new();
        h3.insert("authorization", "Bearer ".parse().unwrap());
        assert_eq!(extract_auth(&h3), None, "empty bearer token -> None, not Some(\"\")");
        let mut h4 = HeaderMap::new();
        h4.insert("x-claude-code-ide-authorization", "".parse().unwrap());
        assert_eq!(extract_auth(&h4), None, "empty header value -> None");
    }

    #[test]
    fn sessionless_requests_never_merge() {
        // Even if the client never echoes our id, distinct calls must not share a key.
        let (c1, _) = derive_streamable_connection_id(None, false);
        let (c2, _) = derive_streamable_connection_id(None, false);
        assert_ne!(c1, c2);
        assert_ne!(c1, "streamable-http");
        // An empty header is treated as absent, not as the literal key "mcp-".
        let (c3, _) = derive_streamable_connection_id(Some(""), false);
        assert_ne!(c3, "mcp-");
    }

    // ── Affinity recovery decision (runtime filtering happens BEFORE this) ──
    // These mirror the pre-refactor inline behavior exactly.

    #[test]
    fn recover_affinity_single_active_tab_is_recovered() {
        let active = vec!["tab-a".to_string()];
        let bound: HashSet<&str> = HashSet::new();
        assert_eq!(recover_affinity(&active, &bound), Some("tab-a".to_string()));
        // Even if that single tab is already bound, one active agent means any
        // unbound call must be it (matches the original `len()==1` short-circuit).
        let bound2: HashSet<&str> = ["tab-a"].into_iter().collect();
        assert_eq!(recover_affinity(&active, &bound2), Some("tab-a".to_string()));
    }

    #[test]
    fn recover_affinity_two_active_one_unbound_recovers_the_unbound() {
        let active = vec!["tab-a".to_string(), "tab-b".to_string()];
        // tab-a is bound (has a live connection); tab-b is the sole reconnecting one.
        let bound: HashSet<&str> = ["tab-a"].into_iter().collect();
        assert_eq!(recover_affinity(&active, &bound), Some("tab-b".to_string()));
    }

    #[test]
    fn recover_affinity_two_active_both_bound_or_both_unbound_is_none() {
        let active = vec!["tab-a".to_string(), "tab-b".to_string()];
        // Both bound → ambiguous → None.
        let both_bound: HashSet<&str> = ["tab-a", "tab-b"].into_iter().collect();
        assert_eq!(recover_affinity(&active, &both_bound), None);
        // Both unbound → ambiguous → None.
        let none_bound: HashSet<&str> = HashSet::new();
        assert_eq!(recover_affinity(&active, &none_bound), None);
    }

    #[test]
    fn recover_affinity_empty_active_set_is_none() {
        let active: Vec<String> = vec![];
        let bound: HashSet<&str> = HashSet::new();
        assert_eq!(recover_affinity(&active, &bound), None);
    }
}
