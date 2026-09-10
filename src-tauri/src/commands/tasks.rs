//! maiTerm task commands (docs/tasks.md).
//!
//! Tasks live on the workspace, not the window — a workspace is a project. The frontend
//! `tasks` store owns its copy and persists whole lists per workspace, the same shape as
//! `set_workspace_mesh_topics`; Rust's job is integrity (canonical dedup keys, stamped
//! `updated_at`) and durability.

use crate::state::persistence::save_state;
use crate::state::{AppState, Task, Workstream};
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
    mut workstreams: Vec<Workstream>,
) -> Result<(), String> {
    let label = window.label().to_string();
    for t in tasks.iter_mut() {
        t.normalized_title = Task::normalize_title(&t.title);
        // Last gate before disk. A status outside the lanes is invisible on the board
        // (lanes match by equality) and permanently unfinished to the dependency check, so
        // one bad write wedges every task blocked on it — persisted. The frontend coerces
        // too; this is the same defense-in-depth as recomputing normalized_title.
        if !matches!(
            t.status.as_str(),
            "backlog" | "todo" | "active" | "blocked" | "review" | "done" | "dropped"
        ) {
            // Unknown lands in `todo`, not `backlog`: backlog is the deliberate parking
            // lot, and silently filing live work there would hide it (docs/tasks.md §3).
            log::warn!("task {}: unknown status {:?} coerced to todo", t.id, t.status);
            t.status = "todo".to_string();
        }
    }
    for w in workstreams.iter_mut() {
        w.normalized_name = Workstream::normalize_name(&w.name);
    }
    // Drop workstreams nothing points at any more. They exist only to group tasks, so an
    // empty one is a label with no referent — and leaving them would let an agent's
    // throwaway names accumulate on the board forever.
    let data_clone = {
        let mut app_data = state.app_data.write();
        let win = app_data.window_mut(&label).ok_or("Window not found")?;
        let workspace = win
            .workspaces
            .iter_mut()
            .find(|w| w.id == workspace_id)
            .ok_or("Workspace not found")?;
        // A workstream is kept while ANY task still points at it — including the rows parked
        // on archived tabs, which are out of `tasks` but not gone. Without that, archiving
        // the tab that owned a workstream's only rows left the label with no referent, and
        // the very next task write in that workspace deleted it for good; restoring the tab
        // then brought its rows back into "Ungrouped", with the board's rename silently
        // refusing to fix it because the workstream id no longer existed.
        workstreams.retain(|w| {
            let id = w.id.as_str();
            tasks.iter().any(|t| t.workstream_id.as_deref() == Some(id))
                || workspace.archived_tabs.iter().any(|tab| {
                    tab.archived_tasks
                        .iter()
                        .any(|t| t.workstream_id.as_deref() == Some(id))
                })
        });
        workspace.tasks = tasks;
        workspace.workstreams = workstreams;
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
) -> Result<Vec<(String, Vec<Task>, Vec<Workstream>)>, String> {
    let label = window.label().to_string();
    let app_data = state.app_data.read();
    let win = app_data.window(&label).ok_or("Window not found")?;
    Ok(win
        .workspaces
        .iter()
        .map(|w| (w.id.clone(), w.tasks.clone(), w.workstreams.clone()))
        .collect())
}
