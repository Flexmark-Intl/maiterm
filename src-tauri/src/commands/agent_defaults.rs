//! Tauri commands for Agent Defaults (docs/agent-defaults.md). The work is in
//! `crate::agent_defaults`; these only find the human's files and keep file IO off the IPC thread.

use crate::agent_defaults::{self, Paths, Runtime, Status};

fn paths() -> Result<Paths, String> {
    Paths::real().ok_or_else(|| "Could not determine home directory".to_string())
}

/// What each agent's settings file holds now.
#[tauri::command]
pub async fn agent_defaults_status() -> Result<Status, String> {
    let p = paths()?;
    tauri::async_runtime::spawn_blocking(move || agent_defaults::status(&p))
        .await
        .map_err(|e| format!("Status task failed: {e}"))
}

/// Set one default (`value` null: the agent's own default), then report what is on disk, so
/// the UI shows the file rather than what it hoped happened.
#[tauri::command]
pub async fn agent_defaults_set(runtime: Runtime, key: String, value: Option<String>) -> Result<Status, String> {
    // These are the human's real agent settings, shared by every maiTerm build (crate::instance).
    if let Some(why) = crate::instance::global_write_refusal() {
        return Err(why);
    }
    let p = paths()?;
    tauri::async_runtime::spawn_blocking(move || {
        agent_defaults::set(&p, runtime, &key, value.as_deref())?;
        Ok(agent_defaults::status(&p))
    })
    .await
    .map_err(|e| format!("Write task failed: {e}"))?
}
