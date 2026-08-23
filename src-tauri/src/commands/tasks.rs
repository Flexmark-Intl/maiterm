//! maiTerm task commands (docs/tasks.md).
//!
//! Tasks live on the workspace, not the window — a workspace is a project. The frontend
//! `tasks` store owns its copy and persists whole lists per workspace, the same shape as
//! `set_workspace_mesh_topics`; Rust's job is integrity (canonical dedup keys, stamped
//! `updated_at`) and durability.

use crate::state::persistence::save_state;
use crate::state::{AppState, Task};
use std::sync::Arc;
use tauri::State;

/// Replace one workspace's task list. Whole-list persistence: the frontend store is the
/// editing surface and sends the full vector, so ordering is simply the Vec order.
///
/// `normalized_title` is recomputed here rather than trusted from the caller, so the dedup
/// key can never drift from the title no matter which writer produced the row (human
/// panel, MCP `createTasks`, importer). Same defense-in-depth contract as
/// `MeshTopic::normalized_label`.
#[tauri::command]
pub fn set_workspace_tasks(
    window: tauri::Window,
    state: State<'_, Arc<AppState>>,
    workspace_id: String,
    mut tasks: Vec<Task>,
) -> Result<(), String> {
    let label = window.label().to_string();
    for t in tasks.iter_mut() {
        t.normalized_title = Task::normalize_title(&t.title);
    }
    let data_clone = {
        let mut app_data = state.app_data.write();
        let win = app_data.window_mut(&label).ok_or("Window not found")?;
        let workspace = win
            .workspaces
            .iter_mut()
            .find(|w| w.id == workspace_id)
            .ok_or("Workspace not found")?;
        workspace.tasks = tasks;
        app_data.clone()
    };
    save_state(&data_clone)
}

/// Every task in this window, as `(workspace_id, tasks)` pairs in workspace order. The
/// store hydrates from this on mount; it is also what the Overlord board groups by.
#[tauri::command]
pub fn get_window_tasks(
    window: tauri::Window,
    state: State<'_, Arc<AppState>>,
) -> Result<Vec<(String, Vec<Task>)>, String> {
    let label = window.label().to_string();
    let app_data = state.app_data.read();
    let win = app_data.window(&label).ok_or("Window not found")?;
    Ok(win
        .workspaces
        .iter()
        .map(|w| (w.id.clone(), w.tasks.clone()))
        .collect())
}
