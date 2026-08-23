//! Overlord engine commands (docs/overlord.md).
//!
//! The Overlord engine is a per-window frontend store; Rust's role is exposing facts it
//! already caches. `get_overlord_tab_facts` is the engine's polling surface: per-tab context
//! gauge + last-real-turn ts from the transcript tail-facts cache (mailink/transcript.rs).

use crate::state::persistence::save_state;
use crate::state::AppState;
use serde_json::Value;
use std::collections::HashMap;
use std::sync::Arc;
use tauri::State;

/// Ring cap for the per-window Overlord ledger. Old entries fall off at append time.
const LEDGER_MAX: usize = 500;

/// Batched per-tab agent facts for the Overlord engine ticker. Tabs with no resolvable
/// agent session are simply absent from the result map. File I/O (tail stats + occasional
/// re-parse on a changed transcript) runs on the blocking pool — the frontend polls this
/// for every agent tab in the window, and the main event loop must stay free (same
/// pinwheel lesson as get_agent_liveness).
#[tauri::command]
pub async fn get_overlord_tab_facts(
    state: State<'_, Arc<AppState>>,
    tab_ids: Vec<String>,
) -> Result<HashMap<String, Value>, String> {
    let app_state = state.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        let mut out = HashMap::new();
        for tab_id in tab_ids {
            if let Some(facts) = crate::mailink::overlord_tab_facts(&app_state, &tab_id) {
                out.insert(tab_id, facts);
            }
        }
        out
    })
    .await
    .map_err(|e| format!("overlord facts probe failed to run: {}", e))
}

/// Append entries to this window's Overlord ledger (verbatim injection record —
/// docs/overlord.md §3). Frontend-owned entry format; ring-buffered at LEDGER_MAX.
#[tauri::command]
pub fn append_overlord_ledger(
    window: tauri::Window,
    state: State<'_, Arc<AppState>>,
    entries: Vec<Value>,
) -> Result<(), String> {
    let label = window.label().to_string();
    let data_clone = {
        let mut app_data = state.app_data.write();
        let win = app_data.window_mut(&label).ok_or("Window not found")?;
        win.overlord_ledger.extend(entries);
        let len = win.overlord_ledger.len();
        if len > LEDGER_MAX {
            win.overlord_ledger.drain(0..len - LEDGER_MAX);
        }
        app_data.clone()
    };
    save_state(&data_clone)
}

/// This window's Overlord ledger, oldest first.
#[tauri::command]
pub fn get_overlord_ledger(
    window: tauri::Window,
    state: State<'_, Arc<AppState>>,
) -> Result<Vec<Value>, String> {
    let label = window.label().to_string();
    let app_data = state.app_data.read();
    let win = app_data.window(&label).ok_or("Window not found")?;
    Ok(win.overlord_ledger.clone())
}

/// Create this window's Overlord workspace (docs/overlord.md §11): overlord flag set,
/// first pane holding a Board tab (active) + a terminal tab for the agent. Appended to
/// the end of the workspace list (it's excluded from ordinary ordering anyway). Returns
/// the existing Overlord workspace unchanged if one is already flagged.
#[tauri::command]
pub fn create_overlord_workspace(
    window: tauri::Window,
    state: State<'_, Arc<AppState>>,
) -> Result<crate::state::Workspace, String> {
    use crate::state::workspace::{Tab, TabType, Workspace};
    let label = window.label().to_string();
    let (ws, data_clone) = {
        let mut app_data = state.app_data.write();
        let win = app_data.window_mut(&label).ok_or("Window not found")?;
        if let Some(existing) = win.workspaces.iter().find(|w| w.overlord) {
            return Ok(existing.clone());
        }
        let mut ws = Workspace::new("Overlord".to_string());
        ws.overlord = true;
        if let Some(pane) = ws.panes.get_mut(0) {
            if let Some(term) = pane.tabs.get_mut(0) {
                term.name = "Overlord Agent".to_string();
                term.custom_name = true;
            }
            let mut board = Tab::new("Board".to_string());
            board.tab_type = TabType::Board;
            board.custom_name = true;
            let board_id = board.id.clone();
            pane.tabs.insert(0, board);
            pane.active_tab_id = Some(board_id);
        }
        win.workspaces.push(ws.clone());
        (ws, app_data.clone())
    };
    save_state(&data_clone)?;
    Ok(ws)
}

/// Flag/unflag a workspace as this window's Overlord workspace (docs/overlord.md §11).
#[tauri::command]
pub fn set_workspace_overlord(
    window: tauri::Window,
    state: State<'_, Arc<AppState>>,
    workspace_id: String,
    enabled: bool,
) -> Result<(), String> {
    let label = window.label().to_string();
    let data_clone = {
        let mut app_data = state.app_data.write();
        let win = app_data.window_mut(&label).ok_or("Window not found")?;
        // At most one Overlord workspace per window: flagging one clears any other.
        if enabled {
            for ws in win.workspaces.iter_mut() {
                ws.overlord = false;
            }
        }
        let workspace = win
            .workspaces
            .iter_mut()
            .find(|w| w.id == workspace_id)
            .ok_or("Workspace not found")?;
        workspace.overlord = enabled;
        app_data.clone()
    };
    save_state(&data_clone)
}
