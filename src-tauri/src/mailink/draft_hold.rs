//! Messages held while the human has a draft in the agent's input box.
//!
//! Everything maiTerm types into an agent ends in a CR, and Claude's box submits whatever is in
//! it — so a message typed on top of a half-written desktop draft sends the draft with it, as
//! one prompt the human never meant to send. A phone message (and a Mattermost pickup) is held
//! instead, per tab and in order, and typed once the box reads EMPTY again: the human sent the
//! draft or cleared it. Only for Claude, whose box can be read off the screen
//! (`input_box.rs`); another runtime's screen doesn't parse, so nothing is held for it.
//!
//! The hold is in memory: a restart drops what was waiting (logged), as it drops the PTY the
//! message was for.

use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, LazyLock};

use parking_lot::Mutex;

use crate::state::AppState;

/// How long a held message waits for the box to empty before it is dropped. A draft left for
/// half an hour is abandoned, and a message typed then would land in a conversation that has
/// moved on; the drop is logged.
const HOLD_MAX_MS: u64 = 30 * 60 * 1000;
const POLL_MS: u64 = 1000;

pub(crate) struct Held {
    /// Already-staged file paths typed ahead of the text (the phone's images).
    pub paths: Vec<String>,
    pub text: String,
    pub what: &'static str,
}

struct Queue {
    items: VecDeque<(Held, u64)>,
}

static QUEUES: LazyLock<Mutex<HashMap<String, Queue>>> = LazyLock::new(|| Mutex::new(HashMap::new()));

/// The human has text in this tab's agent input box (Claude only — the one box that parses).
pub(crate) fn draft_in_box(app: &AppState, tab_id: &str) -> bool {
    super::runtime_for_tab(app, tab_id) == Some(crate::state::AgentRuntime::Claude)
        && matches!(super::agent_input_box(app, tab_id), super::input_box::InputBox::HasText)
}

/// Something is already waiting for this tab: a new message must queue behind it, never jump it.
pub(crate) fn has_held(tab_id: &str) -> bool {
    QUEUES.lock().get(tab_id).is_some_and(|q| !q.items.is_empty())
}

/// Should a message to this tab be held rather than typed now?
pub(crate) fn must_hold(app: &AppState, tab_id: &str) -> bool {
    has_held(tab_id) || draft_in_box(app, tab_id)
}

/// Queue a message for this tab; starts the tab's worker if none is running.
pub(crate) fn hold(app: Arc<AppState>, tab_id: &str, held: Held) {
    let start = {
        let mut queues = QUEUES.lock();
        let fresh = !queues.contains_key(tab_id);
        let q = queues.entry(tab_id.to_string()).or_insert_with(|| Queue { items: VecDeque::new() });
        log::info!("[maiLink] {} for tab {tab_id} held: the human has a draft in the agent's input box", held.what);
        q.items.push_back((held, super::now_ms()));
        fresh
    };
    if start {
        let tab = tab_id.to_string();
        tauri::async_runtime::spawn(async move { worker(app, tab).await });
    }
}

/// Types the tab's held messages in order, each once the box reads empty and no dialog is up.
/// Ends (dropping what's left, logged) when the tab's terminal is gone, and removes its queue
/// when emptied — under the lock, so a `hold` racing the exit starts a fresh worker.
async fn worker(app: Arc<AppState>, tab_id: String) {
    loop {
        tokio::time::sleep(std::time::Duration::from_millis(POLL_MS)).await;
        let Some(pty) = super::pty_for_tab(&app, &tab_id) else {
            drop_all(&tab_id, "its terminal is gone");
            return;
        };
        // Expire from the front: everything behind waited no longer than it.
        {
            let mut queues = QUEUES.lock();
            let Some(q) = queues.get_mut(&tab_id) else { return };
            let now = super::now_ms();
            while q.items.front().is_some_and(|(_, at)| now.saturating_sub(*at) > HOLD_MAX_MS) {
                let (h, _) = q.items.pop_front().unwrap();
                log::warn!("[maiLink] held {} for tab {tab_id} dropped: the draft stayed for 30 min", h.what);
            }
            if q.items.is_empty() {
                queues.remove(&tab_id);
                return;
            }
        }
        // The same last looks every typed message gets: no draft, no dialog of any kind.
        if draft_in_box(&app, &tab_id) || super::open_prompt(&app, &tab_id).is_some() {
            continue;
        }
        if super::live_screen_text(&app, &tab_id).is_some_and(|s| super::permission::any_dialog_open(&s)) {
            continue;
        }
        if super::trust_dialog_open(&app, &tab_id) {
            continue;
        }
        let Some((held, _)) = QUEUES.lock().get_mut(&tab_id).and_then(|q| q.items.pop_front()) else {
            continue;
        };
        let typed = if held.paths.is_empty() {
            super::inject_text(&app, &pty, &held.text, true).await
        } else {
            super::inject_paths_then_text(&app, &pty, &held.paths, &held.text, true).await
        };
        match typed {
            Ok(()) => log::info!("[maiLink] held {} for tab {tab_id} delivered", held.what),
            Err(e) => log::warn!("[maiLink] held {} for tab {tab_id} failed to type: {e}", held.what),
        }
        // The next one waits for Claude to take this one: until then the box holds its text and
        // reads as a draft, which is the ordering this wants.
    }
}

fn drop_all(tab_id: &str, why: &str) {
    if let Some(q) = QUEUES.lock().remove(tab_id) {
        for (h, _) in q.items {
            log::warn!("[maiLink] held {} for tab {tab_id} dropped: {why}", h.what);
        }
    }
}
