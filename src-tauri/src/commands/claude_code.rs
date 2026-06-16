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
}
