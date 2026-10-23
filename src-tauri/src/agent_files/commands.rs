//! The agent-activity commands (docs/file-tree.md §5.2, §9). Async, with the work under
//! `spawn_blocking`: matching paths to trees may touch the disk.

use std::collections::HashSet;
use std::sync::Arc;

use tauri::State;

use super::{AgentMarkAbs, LedgerEntry};
use crate::filetree::FileTreeState;
use crate::state::AppState;

async fn blocking<T: Send + 'static>(f: impl FnOnce() -> Result<T, String> + Send + 'static) -> Result<T, String> {
    tauri::async_runtime::spawn_blocking(f).await.map_err(|e| e.to_string())?
}

/// A tab's ledger: `scope` "turn" (its current or last turn) or "session". Reads and searches
/// only with `include_reads`. Empty for a tab no agent has worked in.
#[tauri::command]
pub async fn agent_files_activity(ft: State<'_, Arc<FileTreeState>>, tab_id: String, scope: String, include_reads: bool) -> Result<Vec<LedgerEntry>, String> {
    let ft = ft.inner().clone();
    blocking(move || Ok(ft.agents.activity(&tab_id, &scope, include_reads).map(|a| a.entries).unwrap_or_default())).await
}

/// The marks still standing for this window's tabs, with absolute paths: what the column draws
/// when it mounts, before any `agent` event arrives.
#[tauri::command]
pub async fn agent_files_marks(window: tauri::Window, state: State<'_, Arc<AppState>>, ft: State<'_, Arc<FileTreeState>>) -> Result<Vec<AgentMarkAbs>, String> {
    let (st, ft) = (state.inner().clone(), ft.inner().clone());
    let label = window.label().to_string();
    blocking(move || {
        let tabs: HashSet<String> = {
            let data = st.app_data.read();
            data.windows
                .iter()
                .filter(|w| w.label == label)
                .flat_map(|w| w.workspaces.iter())
                .flat_map(|ws| ws.panes.iter())
                .flat_map(|p| p.tabs.iter())
                .map(|t| t.id.clone())
                .collect()
        };
        Ok(ft.agents.standing_marks(Some(&tabs)))
    })
    .await
}

/// A reload replaced tab `old_tab_id` with `new_tab_id` (a new id for the same agent session):
/// its ledger moves with it.
#[tauri::command]
pub async fn agent_files_remap_tab(ft: State<'_, Arc<FileTreeState>>, old_tab_id: String, new_tab_id: String) -> Result<(), String> {
    let ft = ft.inner().clone();
    blocking(move || {
        ft.agents.remap_tab(&old_tab_id, &new_tab_id);
        Ok(())
    })
    .await
}
