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

/// Batched per-tab last activity (unix ms) for the Loom's Focus list — the phone's own rule
/// (`mailink::last_activity_ts`: last real transcript turn, else scrollback time, else
/// `suspended_at`, else now), so a resume does not date a chat. Independent of the Overlord
/// facts poll, which runs only while Overlord is on and skips exempt tabs.
#[tauri::command]
pub async fn get_tabs_last_activity(
    state: State<'_, Arc<AppState>>,
    tab_ids: Vec<String>,
) -> Result<HashMap<String, u64>, String> {
    let app_state = state.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        tab_ids
            .into_iter()
            .map(|id| {
                let ts = crate::mailink::tab_last_activity_ms(&app_state, &id);
                (id, ts)
            })
            .collect()
    })
    .await
    .map_err(|e| format!("last-activity probe failed to run: {}", e))
}

/// What the tab's agent has said since `since_ms` — how Overlord harvests the answer to a
/// directive it typed, rather than depending on the agent volunteering `replyToOverlord`.
/// Transcript I/O, so it runs on the blocking pool like the facts poll.
#[tauri::command]
pub async fn get_agent_reply_since(
    state: State<'_, Arc<AppState>>,
    tab_id: String,
    since_ms: i64,
) -> Result<Option<String>, String> {
    let app_state = state.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        crate::mailink::agent_reply_since(&app_state, &tab_id, since_ms)
    })
    .await
    .map_err(|e| format!("agent reply read failed to run: {}", e))
}

/// A tab's chat as the phone sees it (`mailink::tab_transcript`), for the Workstream Loom's
/// Focus view. Up to an 8 MiB transcript tail read, so it runs on the blocking pool.
#[tauri::command]
pub async fn get_tab_transcript(
    state: State<'_, Arc<AppState>>,
    tab_id: String,
) -> Result<Vec<Value>, String> {
    let app_state = state.inner().clone();
    tauri::async_runtime::spawn_blocking(move || crate::mailink::tab_transcript(&app_state, &tab_id))
        .await
        .map_err(|e| format!("transcript read failed to run: {}", e))
}

/// A tab's model, effort and context, plus its runtime (the phone's chat `meta`). `None` before
/// the tab has an agent session.
#[tauri::command]
pub async fn get_tab_meta(
    state: State<'_, Arc<AppState>>,
    tab_id: String,
) -> Result<Option<Value>, String> {
    let app_state = state.inner().clone();
    tauri::async_runtime::spawn_blocking(move || crate::mailink::tab_meta_view(&app_state, &tab_id))
        .await
        .map_err(|e| format!("meta read failed to run: {}", e))
}

/// The models this tab can be switched to with `/model` (the phone's `GET /models?tab=`).
#[tauri::command]
pub async fn list_tab_models(
    state: State<'_, Arc<AppState>>,
    tab_id: String,
) -> Result<Value, String> {
    let app_state = state.inner().clone();
    tauri::async_runtime::spawn_blocking(move || serde_json::json!(crate::mailink::models_for_tab(&app_state, &tab_id)))
        .await
        .map_err(|e| format!("model list failed to run: {}", e))
}

/// What is currently blocking a tab: a tool permission gate, or an AskUserQuestion the agent
/// raised. `None` when nothing is open. Overlord needs the distinction to decide whether it
/// may answer or must put the decision to the human.
#[tauri::command]
pub fn get_tab_prompt(
    state: State<'_, Arc<AppState>>,
    tab_id: String,
) -> Result<Option<Value>, String> {
    Ok(crate::mailink::tab_prompt_view(state.inner(), &tab_id))
}

/// Whether Claude's workspace-trust dialog is open on this tab (mailink/trust.rs). Every
/// automated path that types into a tab asks first: an Enter there confirms "No, exit".
#[tauri::command]
pub fn trust_dialog_open(state: State<'_, Arc<AppState>>, tab_id: String) -> bool {
    crate::mailink::trust_dialog_open(state.inner(), &tab_id)
}

/// What the agent's input box on this tab holds: "empty", "has_text", or "unknown" when the
/// screen isn't a layout maiTerm recognises (docs/follow-ups.md §6.1, `mailink/input_box.rs`).
/// Follow-ups ask before typing, so a human's half-written draft is never submitted.
#[tauri::command]
pub fn agent_input_box(state: State<'_, Arc<AppState>>, tab_id: String) -> &'static str {
    match crate::mailink::agent_input_box(state.inner(), &tab_id) {
        crate::mailink::input_box::InputBox::Empty => "empty",
        crate::mailink::input_box::InputBox::HasText => "has_text",
        crate::mailink::input_box::InputBox::Unknown => "unknown",
    }
}

/// Is this agent session's transcript on THIS machine — i.e. could a resume typed into a local
/// shell find it? False for a session recorded over ssh (docs/follow-ups.md §6.2).
#[tauri::command]
pub async fn agent_session_is_local(runtime: String, session_id: String) -> bool {
    tauri::async_runtime::spawn_blocking(move || crate::mailink::transcript::session_is_local(&runtime, &session_id))
        .await
        .unwrap_or(false)
}

/// Answer a tab's open prompt, through the SAME hardened path the phone uses — the
/// runtime-specific permission keymap, the one-shot selector guard, and the
/// did-it-actually-submit check. `prompt_id` is the stale-guard: pass the one from
/// `get_tab_prompt` so a slow decision can never answer a prompt that opened since.
#[tauri::command]
pub async fn answer_tab_prompt(
    state: State<'_, Arc<AppState>>,
    tab_id: String,
    prompt_id: Option<String>,
    choice: Option<String>,
    answers: Option<Vec<crate::mailink::Answer>>,
) -> Result<Value, String> {
    let app = state.inner().clone();
    // Trusting a folder lets an agent read, edit and run everything in it, including whatever
    // the folder's own settings pre-approve. The phone may answer it: that is the human. The
    // Overlord agent may not, whatever its doctrine says about unblocking the fleet.
    if crate::mailink::trust_dialog_open(&app, &tab_id) {
        return Ok(serde_json::json!({ "ok": false, "reason": "human_decision",
            "detail": "That tab is at Claude's workspace-trust dialog. Trusting a folder is the human's decision: escalate it (needs_human) rather than answer it." }));
    }
    Ok(crate::mailink::respond_to_prompt(
        &app,
        &tab_id,
        prompt_id.as_deref(),
        choice.as_deref(),
        answers.as_deref(),
    )
    .await)
}

/// The human answering a tab's prompt from the Loom (docs/loom.md): the same responder as
/// `answer_tab_prompt`, without its trust-dialog refusal, which exists to keep the Overlord
/// AGENT from trusting folders. A separate command, never a flag on that one, so no path the
/// agent's tool call reaches can carry the human's exemption.
#[tauri::command]
pub async fn answer_tab_prompt_as_human(
    state: State<'_, Arc<AppState>>,
    tab_id: String,
    prompt_id: Option<String>,
    choice: Option<String>,
    answers: Option<Vec<crate::mailink::Answer>>,
) -> Result<Value, String> {
    let app = state.inner().clone();
    Ok(crate::mailink::respond_to_prompt(
        &app,
        &tab_id,
        prompt_id.as_deref(),
        choice.as_deref(),
        answers.as_deref(),
    )
    .await)
}

/// Type a message into a tab's agent from the Loom's composer: the phone's `POST /message`
/// rules (wake an unregistered tab, never type at the trust dialog) plus a refusal while a
/// prompt is open. See `mailink::send_tab_message`. `files` are local paths (pasted
/// screenshots are already temp files); an SSH tab gets copies staged on its host.
#[tauri::command]
pub async fn send_tab_message(
    app_handle: tauri::AppHandle,
    state: State<'_, Arc<AppState>>,
    tab_id: String,
    text: String,
    files: Option<Vec<String>>,
) -> Result<Value, String> {
    let app = state.inner().clone();
    let files = files.unwrap_or_default();
    Ok(crate::mailink::send_tab_message(&app, Some(&app_handle), &tab_id, &text, &files).await)
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

/// Publish this window's Overlord engine snapshot for maiLink (mailink/overlord.rs,
/// docs/mailink-protocol.md §13). Frontend-owned shape, passed through; Rust gates it on tab
/// designation, stamps `windowLabel`/`version`/`receivedAt`, and queues a doorbell for each
/// escalation that is new since the last publish.
#[tauri::command]
pub fn publish_overlord_snapshot(
    window: tauri::Window,
    state: State<'_, Arc<AppState>>,
    snapshot: Value,
) -> Result<(), String> {
    crate::mailink::overlord::publish(state.inner(), window.label(), snapshot);
    Ok(())
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
/// the existing Overlord workspace if one is already flagged, with its Board tab put back
/// first if it was closed: the board holds the deck and the Loom, and nothing else creates
/// one, so a closed board left no way in.
#[tauri::command]
pub fn create_overlord_workspace(
    window: tauri::Window,
    state: State<'_, Arc<AppState>>,
) -> Result<crate::state::Workspace, String> {
    use crate::state::workspace::{Tab, TabType, Workspace};
    let label = window.label().to_string();
    let board_tab = || {
        let mut board = Tab::new("Board".to_string());
        board.tab_type = TabType::Board;
        board.custom_name = true;
        board
    };
    let (ws, data_clone) = {
        let mut app_data = state.app_data.write();
        let win = app_data.window_mut(&label).ok_or("Window not found")?;
        if let Some(existing) = win.workspaces.iter_mut().find(|w| w.overlord) {
            let has_board = existing.panes.iter().any(|p| p.tabs.iter().any(|t| t.tab_type == TabType::Board));
            if has_board {
                return Ok(existing.clone());
            }
            let Some(pane) = existing.panes.get_mut(0) else {
                return Ok(existing.clone());
            };
            let board = board_tab();
            pane.active_tab_id = Some(board.id.clone());
            pane.tabs.insert(0, board);
            let ws = existing.clone();
            (ws, app_data.clone())
        } else {
            let mut ws = Workspace::new("Overlord".to_string());
            ws.overlord = true;
            if let Some(pane) = ws.panes.get_mut(0) {
                if let Some(term) = pane.tabs.get_mut(0) {
                    term.name = "Overlord Agent".to_string();
                    term.custom_name = true;
                }
                let board = board_tab();
                pane.active_tab_id = Some(board.id.clone());
                pane.tabs.insert(0, board);
            }
            win.workspaces.push(ws.clone());
            (ws, app_data.clone())
        }
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

/// Exempt every tab in a workspace from (or return them to) Overlord supervision
/// (docs/overlord.md §11).
#[tauri::command]
pub fn set_workspace_overlord_exempt(
    window: tauri::Window,
    state: State<'_, Arc<AppState>>,
    workspace_id: String,
    exempt: bool,
) -> Result<(), String> {
    let label = window.label().to_string();
    let data_clone = {
        let mut app_data = state.app_data.write();
        let win = app_data.window_mut(&label).ok_or("Window not found")?;
        let workspace = win
            .workspaces
            .iter_mut()
            .find(|w| w.id == workspace_id)
            .ok_or("Workspace not found")?;
        workspace.overlord_exempt = exempt;
        app_data.clone()
    };
    save_state(&data_clone)
}
