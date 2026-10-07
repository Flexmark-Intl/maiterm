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
//! message was for — and forgets the outcomes, so after a restart a held msg_id is in neither
//! `ChatDetail.held` nor `heldOutcomes` (the phone reads that absence as dropped).

use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, LazyLock};

use parking_lot::Mutex;

use crate::state::AppState;

/// How long a held message waits for the box to empty before it is dropped. A draft left for
/// half an hour is abandoned, and a message typed then would land in a conversation that has
/// moved on; the drop is logged.
const HOLD_MAX_MS: u64 = 30 * 60 * 1000;
const POLL_MS: u64 = 1000;

/// How long a held message's outcome stays in `ChatDetail.heldOutcomes`.
const OUTCOME_KEEP_MS: u64 = 60 * 60 * 1000;

pub(crate) struct Held {
    /// The id `POST /message` answered with — the phone's key for `held` / `heldOutcomes`.
    pub msg_id: String,
    /// Already-staged file paths typed ahead of the text (the phone's images).
    pub paths: Vec<String>,
    pub text: String,
    pub what: &'static str,
}

struct Queue {
    items: VecDeque<(Held, u64)>,
}

static QUEUES: LazyLock<Mutex<HashMap<String, Queue>>> = LazyLock::new(|| Mutex::new(HashMap::new()));

/// What became of held messages, per tab: (msg_id, "typed" | "dropped", at, why). The phone
/// can't tell typed from dropped by watching for an echo (merged turns, captions, a stale
/// transcript after sleep all read as "never came"), and this side knows.
type Outcome = (String, &'static str, u64, Option<&'static str>);
static OUTCOMES: LazyLock<Mutex<HashMap<String, Vec<Outcome>>>> = LazyLock::new(|| Mutex::new(HashMap::new()));

fn record(tab_id: &str, msg_id: &str, outcome: &'static str, why: Option<&'static str>) {
    let now = super::now_ms();
    let mut all = OUTCOMES.lock();
    all.retain(|_, v| {
        v.retain(|o| now.saturating_sub(o.2) < OUTCOME_KEEP_MS);
        !v.is_empty()
    });
    all.entry(tab_id.to_string()).or_default().push((msg_id.to_string(), outcome, now, why));
}

/// A fresh id for a held send, unique even for two inside one millisecond.
pub(crate) fn new_msg_id() -> String {
    static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let n = SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    format!("m_{}_h{n}", super::now_ms())
}

/// `ChatDetail.held` (v0.19): what is still waiting for this tab, oldest first.
pub(crate) fn held_for_tab(tab_id: &str) -> Vec<serde_json::Value> {
    QUEUES
        .lock()
        .get(tab_id)
        .map(|q| {
            q.items
                .iter()
                .map(|(h, at)| serde_json::json!({ "msg_id": h.msg_id, "heldAt": at, "reason": "draft" }))
                .collect()
        })
        .unwrap_or_default()
}

/// `ChatDetail.heldOutcomes` (v0.19): what became of this tab's held sends in the last hour.
pub(crate) fn outcomes_for_tab(tab_id: &str) -> Vec<serde_json::Value> {
    let now = super::now_ms();
    OUTCOMES
        .lock()
        .get(tab_id)
        .map(|v| {
            v.iter()
                .filter(|o| now.saturating_sub(o.2) < OUTCOME_KEEP_MS)
                .map(|(id, outcome, at, why)| {
                    let mut o = serde_json::json!({ "msg_id": id, "outcome": outcome, "at": at });
                    if let Some(w) = why {
                        o["why"] = serde_json::json!(w);
                    }
                    o
                })
                .collect()
        })
        .unwrap_or_default()
}

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
            drop_all(&tab_id, "its terminal is gone", "terminal_gone");
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
                record(&tab_id, &h.msg_id, "dropped", Some("expired"));
            }
            if q.items.is_empty() {
                queues.remove(&tab_id);
                return;
            }
        }
        // An agent still owns the terminal (the comms rule, `agent_owns_terminal`): an exited
        // agent's last frame — an empty box — can stay on screen above the shell prompt. Over
        // ssh a live connection stands in for the remote agent, the known residual gap. First,
        // because it awaits: every check after it is synchronous up to the typing.
        if !crate::comms::agent_owns_terminal(&app, &pty).await {
            continue;
        }
        // A box that reads EMPTY, not merely "no draft seen": a shell prompt reads `Unknown`,
        // and so does a screen covering the box (a draft taller than the screen, a full-screen
        // view) — typing then would run the message as a command, or send the draft after all.
        if !matches!(super::agent_input_box(&app, &tab_id), super::input_box::InputBox::Empty) {
            continue;
        }
        // The same last looks every typed message gets: no dialog of any kind.
        if super::open_prompt(&app, &tab_id).is_some() {
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
            Ok(()) => {
                log::info!("[maiLink] held {} for tab {tab_id} delivered", held.what);
                record(&tab_id, &held.msg_id, "typed", None);
            }
            Err(e) => {
                log::warn!("[maiLink] held {} for tab {tab_id} failed to type: {e}", held.what);
                record(&tab_id, &held.msg_id, "dropped", Some("type_failed"));
            }
        }
        // The next one waits for Claude to take this one: until then the box holds its text and
        // reads as a draft, which is the ordering this wants.
    }
}

fn drop_all(tab_id: &str, why: &str, code: &'static str) {
    let Some(q) = QUEUES.lock().remove(tab_id) else { return };
    for (h, _) in q.items {
        log::warn!("[maiLink] held {} for tab {tab_id} dropped: {why}", h.what);
        record(tab_id, &h.msg_id, "dropped", Some(code));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn outcomes_are_reported_per_tab_with_their_reason() {
        record("tab-outcomes-a", "m_1_h0", "typed", None);
        record("tab-outcomes-a", "m_2_h1", "dropped", Some("expired"));
        record("tab-outcomes-b", "m_3_h2", "dropped", Some("terminal_gone"));
        let a = outcomes_for_tab("tab-outcomes-a");
        assert_eq!(a.len(), 2);
        assert_eq!(a[0]["msg_id"], "m_1_h0");
        assert_eq!(a[0]["outcome"], "typed");
        assert!(a[0].get("why").is_none());
        assert_eq!(a[1]["why"], "expired");
        assert_eq!(outcomes_for_tab("tab-outcomes-b").len(), 1);
        assert!(outcomes_for_tab("tab-outcomes-none").is_empty());
    }

    #[test]
    fn held_lists_what_is_waiting_oldest_first() {
        {
            let mut queues = QUEUES.lock();
            let q = queues.entry("tab-held-a".into()).or_insert_with(|| Queue { items: VecDeque::new() });
            for (id, at) in [("m_1_h0", 10), ("m_2_h1", 20)] {
                q.items.push_back((Held { msg_id: id.into(), paths: vec![], text: "x".into(), what: "phone message" }, at));
            }
        }
        let held = held_for_tab("tab-held-a");
        assert_eq!(held.len(), 2);
        assert_eq!(held[0]["msg_id"], "m_1_h0");
        assert_eq!(held[0]["heldAt"], 10);
        assert_eq!(held[0]["reason"], "draft");
        assert!(has_held("tab-held-a"));
        assert!(held_for_tab("tab-held-none").is_empty());
    }

    #[test]
    fn msg_ids_are_unique_within_a_millisecond() {
        let a = new_msg_id();
        let b = new_msg_id();
        assert_ne!(a, b);
    }
}
