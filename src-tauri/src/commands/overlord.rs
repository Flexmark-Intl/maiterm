//! Overlord engine commands (docs/overlord.md).
//!
//! The Overlord engine is a per-window frontend store; Rust's role is exposing facts it
//! already caches. `get_overlord_tab_facts` is the engine's polling surface: per-tab context
//! gauge + last-real-turn ts from the transcript tail-facts cache (mailink/transcript.rs).

use crate::state::AppState;
use serde_json::Value;
use std::collections::HashMap;
use std::sync::Arc;
use tauri::State;

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
