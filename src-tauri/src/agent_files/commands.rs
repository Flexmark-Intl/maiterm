//! The agent-activity commands (docs/file-tree.md §5.2, §9). Async, with the work under
//! `spawn_blocking`: matching paths to trees may touch the disk.

use std::collections::HashSet;
use std::sync::Arc;

use tauri::State;

use super::baseline::{Baseline, BaselineStat};
use super::{AgentMarkAbs, LedgerEntry, TabTurn};
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
    blocking(move || Ok(ft.agents.standing_marks(Some(&window_tabs(&st, &label))))).await
}

/// The current turn of each of this window's tabs, for a Files column that was closed while
/// prompts and new sessions went by (§9.4): its turn tracker starts from these.
#[tauri::command]
pub async fn agent_files_turns(window: tauri::Window, state: State<'_, Arc<AppState>>, ft: State<'_, Arc<FileTreeState>>) -> Result<Vec<TabTurn>, String> {
    let (st, ft) = (state.inner().clone(), ft.inner().clone());
    let label = window.label().to_string();
    blocking(move || Ok(ft.agents.turns(Some(&window_tabs(&st, &label))))).await
}

fn window_tabs(st: &AppState, label: &str) -> HashSet<String> {
    let data = st.app_data.read();
    data.windows
        .iter()
        .filter(|w| w.label == label)
        .flat_map(|w| w.workspaces.iter())
        .flat_map(|ws| ws.panes.iter())
        .flat_map(|p| p.tabs.iter())
        .map(|t| t.id.clone())
        .collect()
}

/// What `path_abs` held before `tab_id`'s agent first edited it: `scope` "turn" (its current or
/// last turn) or "session" (§14). Only the tab's own baselines exist here, so a caller acting for
/// an agent must already have checked the agent IS that tab (`showDiff` `@turn` does, in
/// claude_code/server.rs).
#[tauri::command]
pub async fn agent_files_baseline(ft: State<'_, Arc<FileTreeState>>, tab_id: String, path_abs: String, scope: String) -> Result<Baseline, String> {
    let ft = ft.inner().clone();
    blocking(move || Ok(baseline_of(&ft, &tab_id, &path_abs, &scope))).await
}

/// Lines added and removed in `path_abs` since that baseline: the Turn lens tooltip's `+N −N`.
#[tauri::command]
pub async fn agent_files_baseline_stat(ft: State<'_, Arc<FileTreeState>>, tab_id: String, path_abs: String, scope: String) -> Result<BaselineStat, String> {
    let ft = ft.inner().clone();
    blocking(move || {
        let (turn, session) = ft.agents.turn_and_session(&tab_id).unwrap_or((0, String::new()));
        Ok(ft.agents.baselines.stat(&tab_id, &session, &path_abs, scope_of(&scope), turn))
    })
    .await
}

fn scope_of(scope: &str) -> &'static str {
    if scope == "session" {
        "session"
    } else {
        "turn"
    }
}

/// A tab's baseline as the ledger's current turn and session name it. A tab the ledger has no
/// turn for has none.
pub fn baseline_of(ft: &FileTreeState, tab_id: &str, path_abs: &str, scope: &str) -> Baseline {
    let (turn, session) = ft.agents.turn_and_session(tab_id).unwrap_or((0, String::new()));
    if turn == 0 {
        return Baseline { content: None, unavailable: Some("not_captured".into()), turn: 0, source: "maiterm".into(), runtime: String::new() };
    }
    ft.agents.baselines.lookup(tab_id, &session, path_abs, scope_of(scope), turn)
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
