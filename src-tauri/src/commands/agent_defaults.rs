//! Tauri commands for Agent Defaults (docs/agent-defaults.md). The work is in
//! `crate::agent_defaults`; these only find the human's files and keep file IO off the IPC thread.

use std::sync::Arc;

use tauri::State;

use crate::agent_defaults::{self, Paths, Runtime, Status};
use crate::state::AppState;

/// The human's files: which Claude settings their tabs read depends on their accounts.
fn paths(state: &AppState) -> Result<Paths, String> {
    Paths::real(&state.app_data.read().preferences).ok_or_else(|| "Could not determine home directory".to_string())
}

/// What each agent's settings file holds now.
#[tauri::command]
pub async fn agent_defaults_status(state: State<'_, Arc<AppState>>) -> Result<Status, String> {
    let p = paths(&state)?;
    tauri::async_runtime::spawn_blocking(move || agent_defaults::status(&p))
        .await
        .map_err(|e| format!("Status task failed: {e}"))
}

/// Set one default (`value` null: the agent's own default), then report what is on disk, so
/// the UI shows the file rather than what it hoped happened.
#[tauri::command]
pub async fn agent_defaults_set(
    state: State<'_, Arc<AppState>>,
    runtime: Runtime,
    key: String,
    value: Option<String>,
) -> Result<Status, String> {
    // These are the human's real agent settings, shared by every maiTerm build (crate::instance).
    if let Some(why) = crate::instance::global_write_refusal() {
        return Err(why);
    }
    let p = paths(&state)?;
    tauri::async_runtime::spawn_blocking(move || {
        agent_defaults::set(&p, runtime, &key, value.as_deref())?;
        Ok(agent_defaults::status(&p))
    })
    .await
    .map_err(|e| format!("Write task failed: {e}"))?
}
