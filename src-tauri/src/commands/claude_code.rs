use std::sync::Arc;
use serde_json::Value;
use tauri::State;

use crate::state::AppState;

/// Called by the frontend to send a tool response back to Claude CLI.
#[tauri::command]
pub fn claude_code_respond(
    state: State<'_, Arc<AppState>>,
    request_id: String,
    result: Value,
) -> Result<(), String> {
    let mut pending = state.ide_pending.write();
    if let Some(tx) = pending.remove(&request_id) {
        let _ = tx.send(result);
        Ok(())
    } else {
        Err(format!("No pending request with id: {}", request_id))
    }
}

/// One tab's live agent session, as a window's page needs it to rebuild its agent state.
#[derive(serde::Serialize)]
pub struct LiveAgentSession {
    pub tab_id: String,
    pub session_id: String,
    pub runtime: &'static str,
    /// The page's states: "active" | "idle" | "permission".
    pub state: &'static str,
    pub tool_name: Option<String>,
    pub tool_detail: Option<String>,
    pub cwd: Option<String>,
}

/// The agent sessions Rust is tracking for these tabs, one per tab (its current session).
///
/// A window's page learns about agents only from hook events, so a page that reloads (a
/// WebKit crash, a window reload) starts with none: every idle agent read as offline and a
/// mesh opened its readiness dialog over agents that were all still running. The page asks
/// this once on load; a hook that has already arrived wins over it.
#[tauri::command]
pub fn get_live_agent_sessions(
    state: State<'_, Arc<AppState>>,
    tab_ids: Vec<String>,
) -> Vec<LiveAgentSession> {
    use crate::state::app_state::AgentSessionState as S;
    let mut out = Vec::new();
    for tab_id in tab_ids {
        let Some(sid) = crate::mailink::current_session_id(&state, &tab_id) else { continue };
        let sessions = state.agent_sessions.read();
        let Some(s) = sessions.get(&sid) else { continue };
        let page_state = match s.state {
            S::Active => "active",
            S::WaitingPermission => "permission",
            S::WaitingInput | S::Stopped => "idle",
        };
        out.push(LiveAgentSession {
            tab_id,
            session_id: sid.clone(),
            runtime: s.runtime.as_key(),
            state: page_state,
            tool_name: s.tool_name.clone(),
            tool_detail: s.tool_detail.clone(),
            cwd: s.cwd.clone(),
        });
    }
    out
}

/// Called by the frontend to forward a notification (e.g. selection change) to Claude CLI.
#[tauri::command]
pub fn claude_code_notify_selection(
    state: State<'_, Arc<AppState>>,
    payload: Value,
) -> Result<(), String> {
    let guard = state.ide_notify_tx.lock();
    if let Some(tx) = guard.as_ref() {
        let json = serde_json::to_string(&payload).map_err(|e| e.to_string())?;
        tx.send(json).map_err(|e| e.to_string())
    } else {
        // No client connected, silently ignore
        Ok(())
    }
}

/// Re-apply on-disk integration for non-Claude runtimes after a preference toggle.
/// Claude is managed at startup + by the re-assert timer, so it's skipped here. Each
/// enabled runtime is (idempotently) installed and each disabled one unregistered, using
/// the live MCP port/auth. No-op when the MCP server isn't up yet. Lets a user enable
/// Codex from Preferences and have ~/.codex configured immediately, without a restart.
#[tauri::command]
pub fn refresh_agent_integrations(state: State<'_, Arc<AppState>>) {
    if let Some(why) = crate::instance::global_write_refusal() {
        log::info!("Agent integrations not refreshed: {why}");
        return;
    }
    let port = match *state.mcp_port.read() {
        Some(p) => p,
        None => return,
    };
    let auth = state.mcp_auth.read().clone().unwrap_or_default();
    if auth.is_empty() {
        return;
    }
    let prefs = state.app_data.read().preferences.clone();
    for r in crate::claude_code::registrar::all_registrars() {
        if r.runtime() == crate::state::AgentRuntime::Claude {
            continue;
        }
        if r.enabled(&prefs) {
            r.install(port, &auth, &[], &prefs);
        } else {
            r.unregister(port, &auth);
        }
    }
    // Proxied servers follow each runtime's integration switch.
    crate::mcp_proxy::entries::sync(port, &auth, &prefs);
}
