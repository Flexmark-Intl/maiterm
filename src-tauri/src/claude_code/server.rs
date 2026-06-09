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

use super::lockfile::{cleanup_stale_lockfiles, ensure_mcp_settings, write_lockfile};
use super::protocol::{initialize_response, tool_list_response, JsonRpcRequest, JsonRpcResponse};
use crate::state::AppState;

const PING_INTERVAL: Duration = Duration::from_secs(30);
const RESPONSE_TIMEOUT: Duration = Duration::from_secs(120);
const SSE_KEEPALIVE_INTERVAL: Duration = Duration::from_secs(15);
/// How often to re-assert our `mcpServers.aiterm` entry in `~/.claude.json`.
/// The file is co-owned by the `claude` CLI, which can clobber our entry; this
/// heals any drift within one tick. Idempotent — only writes when it differs.
const MCP_REASSERT_INTERVAL: Duration = Duration::from_secs(30);

/// Per-SSE-session sender: receives raw JSON strings, which the SSE stream wraps as data events.
type SseSessions = Arc<parking_lot::RwLock<HashMap<String, mpsc::UnboundedSender<String>>>>;

/// Per-connection tab affinity: maps transport connection ID → tab ID.
/// Set by `initSession` tool, used to auto-inject `tabId` into tool calls.
type ConnectionTabMap = Arc<parking_lot::RwLock<HashMap<String, String>>>;

#[derive(Clone)]
struct ServerState {
    app_handle: AppHandle,
    state: Arc<AppState>,
    expected_auth: String,
    sse_sessions: SseSessions,
    /// Maps connection IDs (SSE session, WS id, or "streamable-http") to tab IDs.
    connection_tabs: ConnectionTabMap,
    /// Active transport connections (WS + SSE). Used to ref-count the connected
    /// flag so flapping sessions don't emit spurious disconnect events while
    /// another session is still alive. Also dampens log/event spam from the
    /// documented SSE-over-SSH reconnect flap.
    connection_count: Arc<AtomicUsize>,
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

    if !state.app_data.read().preferences.claude_code_ide {
        log::info!("Claude Code IDE integration disabled in preferences");
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

    *state.claude_code_port.write() = Some(port);
    *state.claude_code_auth.write() = Some(auth.clone());

    let workspace_folders = collect_workspace_folders(state);
    let hooks_enabled = state.app_data.read().preferences.claude_code_hooks;
    if let Err(e) = write_lockfile(port, &auth, workspace_folders, hooks_enabled) {
        log::warn!("Failed to write Claude Code lock file: {}", e);
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
    *state.claude_code_shutdown.lock() = Some(shutdown_tx);

    // Periodically re-assert our `~/.claude.json` entry so a clobber by the
    // co-owning `claude` CLI self-heals instead of leaving Claude Code dialing a
    // dead port until the next app restart. Runs until graceful shutdown.
    let reassert_port = setup.port;
    let reassert_auth = setup.auth.clone();
    tauri::async_runtime::spawn(async move {
        let mut ticker = tokio::time::interval(MCP_REASSERT_INTERVAL);
        ticker.tick().await; // consume the immediate first tick
        loop {
            tokio::select! {
                _ = ticker.tick() => {
                    if let Err(e) = ensure_mcp_settings(reassert_port, &reassert_auth) {
                        log::warn!("MCP settings re-assert failed: {}", e);
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

    let sse_sessions: SseSessions = Arc::new(parking_lot::RwLock::new(HashMap::new()));
    let connection_tabs: ConnectionTabMap = Arc::new(parking_lot::RwLock::new(HashMap::new()));

    let server_state = ServerState {
        app_handle,
        state,
        expected_auth: setup.auth,
        sse_sessions,
        connection_tabs,
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
        ("claude_code_ide", PrefMeta { description: "Enable Claude Code IDE integration (MCP server)", ptype: "boolean", category: "Integration", read_only: false }),
        ("claude_code_ide_ssh", PrefMeta { description: "Enable MCP bridge over SSH (reverse tunnel for remote Claude Code)", ptype: "boolean", category: "Integration", read_only: false }),
        ("claude_code_hooks", PrefMeta { description: "Enable hooks integration (session lifecycle events, tab indicators)", ptype: "boolean", category: "Integration", read_only: false }),
        ("claude_code_auto_resume", PrefMeta { description: "Enable hooks-based auto-resume (programmatic session ID capture)", ptype: "boolean", category: "Integration", read_only: false }),
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
            let sessions = state.claude_sessions.read();
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
    *srv.state.claude_code_connected.write() = connected;
    let payload = serde_json::json!({ "connected": connected });
    let app_data = srv.state.app_data.read();
    for win in &app_data.windows {
        let _ = srv.app_handle.emit_to(&win.label, "claude-code-connection", payload.clone());
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
    let auth = headers
        .get("x-claude-code-ide-authorization")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_string();

    if auth != srv.expected_auth {
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
    *srv.state.claude_code_notify_tx.lock() = Some(response_tx.clone());

    let (mut ws_write, mut ws_read) = socket.split();
    let mut ping_interval = tokio::time::interval(PING_INTERVAL);
    ping_interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

    loop {
        tokio::select! {
            msg = ws_read.next() => {
                match msg {
                    Some(Ok(WsMessage::Text(text))) => {
                        handle_message(&text, &srv.app_handle, &srv.state, &srv.connection_tabs, &ws_connection_id, &response_tx).await;
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
    // Only clear the global notify channel if we still own it — a newer
    // transport session may have overwritten it while we were running.
    {
        let mut guard = srv.state.claude_code_notify_tx.lock();
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
    let auth = headers
        .get("x-claude-code-ide-authorization")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");

    if auth != srv.expected_auth {
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
    let response_json = process_message(&body, &srv.app_handle, &srv.state, &srv.connection_tabs, &connection_id).await;

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
    let auth = headers
        .get("x-claude-code-ide-authorization")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");

    if auth != srv.expected_auth {
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
    *srv.state.claude_code_notify_tx.lock() = Some(notify_tx);

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
        connection_dec(&cleanup_srv);
        // Only clear the global notify channel if this session still owns it —
        // a newer session may have overwritten it while we were running.
        {
            let mut guard = cleanup_srv.state.claude_code_notify_tx.lock();
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
    let auth = headers
        .get("x-claude-code-ide-authorization")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");

    if auth != srv.expected_auth {
        return StatusCode::UNAUTHORIZED.into_response();
    }

    let tx = srv.sse_sessions.read().get(&params.session_id).cloned();
    let Some(tx) = tx else {
        return StatusCode::NOT_FOUND.into_response();
    };

    let connection_id = format!("sse-{}", params.session_id);
    handle_message(&body, &srv.app_handle, &srv.state, &srv.connection_tabs, &connection_id, &tx).await;
    StatusCode::OK.into_response()
}

// ─── Shared JSON-RPC handler ────────────────────────────────────────────────

/// Process one JSON-RPC message and return the response as a raw JSON string.
/// Returns `None` for notifications (no id) that don't require a response.
/// `connection_id` identifies the transport connection (SSE session, WS, or streamable-http)
/// for tab affinity tracking.
async fn process_message(
    text: &str,
    app_handle: &AppHandle,
    state: &Arc<AppState>,
    connection_tabs: &ConnectionTabMap,
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
            let resp = JsonRpcResponse::success(id, initialize_response());
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
                                "content": [{ "type": "text", "text": "Error: tabId is required. Read your tab ID from the SessionStart hook context or $AITERM_TAB_ID environment variable." }],
                                "isError": true
                            }),
                        );
                        return Some(serde_json::to_string(&resp).unwrap());
                    }

                    // Verify tab exists in this instance.
                    if find_window_for_tab(state, &tab_id).is_none() {
                        let this_server = if cfg!(debug_assertions) { "maiterm-dev" } else { "maiterm" };
                        let other_server = if cfg!(debug_assertions) { "maiterm" } else { "maiterm-dev" };
                        // A tabId unknown to this instance is usually a *stale*
                        // $AITERM_TAB_ID (the shell outlived the tab it was spawned
                        // under), not a wrong-instance call. The old "use the other
                        // server" message caused a circular bounce when the id was
                        // stale in BOTH instances (each blamed the other). Give a
                        // deterministic recovery path instead: getActiveTab → retry.
                        let msg = format!(
                            "Tab '{}' was not found in this maiTerm instance ({}). This almost always means your \
                             $AITERM_TAB_ID is stale (the shell outlived the tab it was created under, or was \
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

                    // Store connection → tab affinity
                    connection_tabs.write().insert(connection_id.to_string(), tab_id.clone());
                    log::debug!("initSession: connection {} → tab {} (claude session: {})",
                        &connection_id[..connection_id.len().min(8)], &tab_id[..tab_id.len().min(8)],
                        if session_id.is_empty() { "none" } else { &session_id[..session_id.len().min(8)] }
                    );

                    // Link claude session → tab mapping
                    {
                        use crate::state::app_state::{ClaudeSessionInfo, ClaudeSessionState};

                        // If Claude passed sessionId explicitly, use that
                        if !session_id.is_empty() {
                            let mut sessions = state.claude_sessions.write();
                            // Preserve existing fields (model, tool_name) if session already registered
                            let existing = sessions.remove(&session_id);
                            sessions.insert(
                                session_id.clone(),
                                ClaudeSessionInfo {
                                    tab_id: tab_id.clone(),
                                    cwd: existing.as_ref().and_then(|e| e.cwd.clone()),
                                    state: ClaudeSessionState::Active,
                                    tool_name: existing.as_ref().and_then(|e| e.tool_name.clone()),
                                    model: existing.and_then(|e| e.model),
                                    connection_id: Some(connection_id.to_string()),
                                },
                            );
                        }

                        // Also pop the most recent pending SessionStart hook session and link it
                        let pending = {
                            let mut pending = state.pending_hook_sessions.write();
                            let cutoff = std::time::Instant::now() - std::time::Duration::from_secs(30);
                            pending.retain(|(_, _, ts)| *ts > cutoff);
                            pending.pop()
                        };
                        if let Some((pending_sid, pending_cwd, _)) = pending {
                            if session_id.is_empty() || pending_sid != session_id {
                                let mut sessions = state.claude_sessions.write();
                                sessions.insert(
                                    pending_sid.clone(),
                                    ClaudeSessionInfo {
                                        tab_id: tab_id.clone(),
                                        cwd: pending_cwd,
                                        state: ClaudeSessionState::Active,
                                        tool_name: None,
                                        model: None,
                                        connection_id: Some(connection_id.to_string()),
                                    },
                                );
                                log::debug!("initSession: linked pending session {} → tab {}",
                                    &pending_sid[..pending_sid.len().min(8)], &tab_id[..tab_id.len().min(8)]);
                                // Re-emit session start now that we know the tab
                                let _ = app_handle.emit("claude-hook-session-start", serde_json::json!({
                                    "session_id": pending_sid,
                                    "tab_id": &tab_id,
                                }));
                            }
                        }
                    }

                    // Emit event so frontend can set claudeSessionId trigger variable
                    // (unconditional — session ID is useful for triggers beyond auto-resume)
                    if !session_id.is_empty() {
                        let _ = app_handle.emit("claude-init-session", serde_json::json!({
                            "tab_id": &tab_id,
                            "session_id": &session_id,
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
                // Falls back to claude_sessions when SSE reconnects cleared the connection affinity.
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
                    let sessions = state.claude_sessions.read();
                    let ct = connection_tabs.read();

                    // Unique tabs that currently have a live Claude session.
                    let active_tabs: Vec<String> = sessions.values()
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
                    let recovered = if active_tabs.len() == 1 {
                        Some(active_tabs[0].clone())
                    } else {
                        let bound: std::collections::HashSet<&str> =
                            ct.values().map(|s| s.as_str()).collect();
                        let unbound: Vec<&String> = active_tabs.iter()
                            .filter(|t| !bound.contains(t.as_str()))
                            .collect();
                        if unbound.len() == 1 { Some(unbound[0].clone()) } else { None }
                    };

                    let active_count = active_tabs.len();
                    drop(ct);
                    drop(sessions);

                    if let Some(tab_id) = recovered {
                        connection_tabs.write().insert(connection_id.to_string(), tab_id.clone());
                        // Update the session's connection_id to the new connection
                        let mut sessions = state.claude_sessions.write();
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
                                     Read your tab ID from $AITERM_TAB_ID environment variable."
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
                        let this_server = if cfg!(debug_assertions) { "maiterm-dev" } else { "maiterm" };
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
                        .claude_code_pending
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
                        let _ = app_handle.emit_to(&label, "claude-code-tool", payload);
                    } else {
                        let _ = app_handle.emit("claude-code-tool", payload);
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
                            state.claude_code_pending.write().remove(&request_id);
                            let resp = JsonRpcResponse::error(
                                id,
                                -32603,
                                "Tool handler disconnected".to_string(),
                            );
                            Some(serde_json::to_string(&resp).unwrap())
                        }
                        Err(_) => {
                            state.claude_code_pending.write().remove(&request_id);
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

// ─── Claude Code Hooks ──────────────────────────────────────────────────────

/// Handle POST /hooks — receives Claude Code hook events.
/// SessionStart registers a session→tab mapping. Other events use
/// that mapping to route Tauri events to the correct frontend tab.
async fn hooks_handler(
    State(srv): State<ServerState>,
    headers: HeaderMap,
    Query(params): Query<HashMap<String, String>>,
    body: String,
) -> Response {
    let auth = headers
        .get("x-claude-code-ide-authorization")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    if auth != srv.expected_auth {
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

    // tab_id comes from query param (set by the command hook script from $AITERM_TAB_ID)
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

    match hook_event_name {
        "SessionStart" => {
            let tab_id = tab_id_from_param.clone().unwrap_or_default();
            let cwd = event.get("cwd").and_then(|v| v.as_str()).map(String::from);
            let model = event.get("model").and_then(|v| v.as_str()).map(String::from);

            if !session_id.is_empty() && !tab_id.is_empty() {
                use crate::state::app_state::{ClaudeSessionInfo, ClaudeSessionState};
                let mut sessions = srv.state.claude_sessions.write();
                sessions.insert(
                    session_id.clone(),
                    ClaudeSessionInfo {
                        tab_id: tab_id.clone(),
                        cwd: cwd.clone(),
                        state: ClaudeSessionState::Active,
                        tool_name: None,
                        model: model.clone(),
                        connection_id: None,
                    },
                );
                log::info!("Claude hook: session {} started for tab {}", session_id, tab_id);
            } else if !session_id.is_empty() {
                // No tab_id yet — buffer for initSession to pick up
                let mut pending = srv.state.pending_hook_sessions.write();
                // Clean entries older than 30s
                let cutoff = std::time::Instant::now() - std::time::Duration::from_secs(30);
                pending.retain(|(_, _, ts)| *ts > cutoff);
                pending.push((session_id.clone(), cwd.clone(), std::time::Instant::now()));
                log::info!("Claude hook: session {} started (pending tab assignment)", session_id);
            }

            let source = event.get("source").and_then(|v| v.as_str()).unwrap_or("");
            let _ = srv.app_handle.emit("claude-hook-session-start", serde_json::json!({
                "session_id": session_id,
                "tab_id": if tab_id.is_empty() { None } else { Some(&tab_id) },
                "cwd": event.get("cwd"),
                "source": source,
            }));
        }

        "SessionEnd" => {
            let tab_id = {
                let mut sessions = srv.state.claude_sessions.write();
                sessions.remove(&session_id).map(|s| s.tab_id)
            }
            .or(tab_id_from_param);

            log::info!("Claude hook: session {} ended (tab {:?})", session_id, tab_id);

            let _ = srv.app_handle.emit("claude-hook-session-end", serde_json::json!({
                "session_id": session_id,
                "tab_id": tab_id,
                "reason": event.get("reason"),
            }));
        }

        "Notification" => {
            let tab_id = {
                let sessions = srv.state.claude_sessions.read();
                sessions.get(&session_id).map(|s| s.tab_id.clone())
            }
            .or(tab_id_from_param);

            let notification_type = event
                .get("notification_type")
                .and_then(|v| v.as_str())
                .unwrap_or("");

            // Update session state based on notification type
            if !session_id.is_empty() {
                use crate::state::app_state::ClaudeSessionState;
                let mut sessions = srv.state.claude_sessions.write();
                if let Some(session) = sessions.get_mut(&session_id) {
                    session.state = match notification_type {
                        "idle_prompt" => ClaudeSessionState::WaitingInput,
                        "permission_prompt" => ClaudeSessionState::WaitingPermission,
                        _ => session.state,
                    };
                }
            }

            log::debug!("Claude hook: Notification type='{}' session={} (tab {:?})",
                notification_type, &session_id[..session_id.len().min(8)], tab_id);
            let _ = srv.app_handle.emit("claude-hook-notification", serde_json::json!({
                "session_id": session_id,
                "tab_id": tab_id,
                "notification_type": notification_type,
                "title": event.get("title"),
                "body": event.get("body"),
            }));
        }

        "Stop" => {
            let tab_id = {
                let sessions = srv.state.claude_sessions.read();
                sessions.get(&session_id).map(|s| s.tab_id.clone())
            }
            .or(tab_id_from_param);

            // Update session state to stopped + clear tool
            if !session_id.is_empty() {
                use crate::state::app_state::ClaudeSessionState;
                let mut sessions = srv.state.claude_sessions.write();
                if let Some(session) = sessions.get_mut(&session_id) {
                    session.state = ClaudeSessionState::Stopped;
                    session.tool_name = None;
                }
            }

            log::debug!("Claude hook: Stop for session {} (tab {:?})", &session_id[..session_id.len().min(8)], tab_id);
            let _ = srv.app_handle.emit("claude-hook-stop", serde_json::json!({
                "session_id": session_id,
                "tab_id": tab_id,
            }));
        }

        "UserPromptSubmit" => {
            let tab_id = {
                let sessions = srv.state.claude_sessions.read();
                sessions.get(&session_id).map(|s| s.tab_id.clone())
            }
            .or(tab_id_from_param);

            // Update session state to processing
            if !session_id.is_empty() {
                use crate::state::app_state::ClaudeSessionState;
                let mut sessions = srv.state.claude_sessions.write();
                if let Some(session) = sessions.get_mut(&session_id) {
                    session.state = ClaudeSessionState::Active;
                }
            }

            log::debug!("Claude hook: UserPromptSubmit session={} (tab {:?})", &session_id[..session_id.len().min(8)], tab_id);
            let _ = srv.app_handle.emit("claude-hook-user-prompt", serde_json::json!({
                "session_id": session_id,
                "tab_id": tab_id,
            }));
        }

        "PreToolUse" => {
            let tab_id = {
                let sessions = srv.state.claude_sessions.read();
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
                use crate::state::app_state::ClaudeSessionState;
                let mut sessions = srv.state.claude_sessions.write();
                if let Some(session) = sessions.get_mut(&session_id) {
                    session.state = ClaudeSessionState::Active;
                    session.tool_name = if tool_name.is_empty() { None } else { Some(tool_name.clone()) };
                }
            }

            log::debug!("Claude hook: PreToolUse tool='{}' session={} (tab {:?})",
                tool_name, &session_id[..session_id.len().min(8)], tab_id);
            let _ = srv.app_handle.emit("claude-hook-pre-tool-use", serde_json::json!({
                "session_id": session_id,
                "tab_id": tab_id,
                "tool_name": tool_name,
                "tool_input": event.get("tool_input"),
            }));
        }

        "PostToolUse" => {
            let tab_id = {
                let sessions = srv.state.claude_sessions.read();
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
                let mut sessions = srv.state.claude_sessions.write();
                if let Some(session) = sessions.get_mut(&session_id) {
                    session.tool_name = None;
                }
            }

            log::debug!("Claude hook: PostToolUse tool='{}' session={} (tab {:?})",
                tool_name, &session_id[..session_id.len().min(8)], tab_id);
            let _ = srv.app_handle.emit("claude-hook-post-tool-use", serde_json::json!({
                "session_id": session_id,
                "tab_id": tab_id,
                "tool_name": tool_name,
                "tool_input": event.get("tool_input"),
            }));
        }

        "PreCompact" => {
            let tab_id = {
                let sessions = srv.state.claude_sessions.read();
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
            let _ = srv.app_handle.emit("claude-hook-pre-compact", serde_json::json!({
                "session_id": session_id,
                "tab_id": tab_id,
                "trigger": trigger,
            }));
        }

        other => {
            log::debug!("Claude hook: unhandled event type '{}'", other);
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
    connection_id: &str,
    response_tx: &mpsc::UnboundedSender<String>,
) {
    if let Some(json) = process_message(text, app_handle, state, connection_tabs, connection_id).await {
        let _ = response_tx.send(json);
    }
}

#[cfg(test)]
mod tests {
    use super::derive_streamable_connection_id;

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
}
