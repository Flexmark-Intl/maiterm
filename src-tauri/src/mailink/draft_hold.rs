//! Messages held until an agent's tab can take them, and the one gate that decides it.
//!
//! **A busy agent is not a reason to hold.** An agent mid-turn takes a typed message the way it
//! takes the human's own: Claude queues it (or folds it into the running turn), and nothing is
//! interrupted. What a message must never do is land in something that is not the agent's input:
//!   * a dialog — a permission, a question, Claude's trust dialog. Typed there, the text answers
//!     it (a digit picks a row, the CR confirms the highlighted one).
//!   * the human's draft. Everything maiTerm types ends in a CR, and Claude's box submits whatever
//!     is in it, so a message typed on top of a half-written draft sends the draft with it.
//!   * the human's typing: a key in the last 2 s may not have echoed into the box yet, and for
//!     a runtime whose box can't be read (Codex, Gemini) it is the only evidence of a draft.
//!
//! Anything held waits here, per tab and in order, and is typed once those clear. A message
//! for a tab that already has one waiting queues behind it, never jumps it.
//!
//! Two kinds of sender share the queue: the phone's `POST /message` (v0.19, `ChatDetail.held`)
//! and the desktop's own (`deliver_or_hold`: board answers, Overlord directives, notices), whose
//! outcome goes back to the webview as an `agent-inbox-outcome` event rather than to the phone.
//!
//! The hold is in memory: a restart drops what was waiting (logged), as it drops the PTY the
//! message was for — and forgets the outcomes, so after a restart a held msg_id is in neither
//! `ChatDetail.held` nor `heldOutcomes` (the phone reads that absence as dropped).

use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, LazyLock, OnceLock};

use parking_lot::Mutex;
use tauri::{AppHandle, Emitter};

use crate::state::{AgentRuntime, AppState};

/// How long a held message waits before it is dropped. A draft left for half an hour is
/// abandoned, and a message typed then would land in a conversation that has moved on; the drop
/// is logged (and reported, for a desktop sender).
const HOLD_MAX_MS: u64 = 30 * 60 * 1000;
const POLL_MS: u64 = 1000;

/// A human key this recent holds anything maiTerm would type: it may not have reached the box yet.
const TYPING_GUARD_MS: u64 = 2000;

/// How long a held message's outcome stays in `ChatDetail.heldOutcomes`.
const OUTCOME_KEEP_MS: u64 = 60 * 60 * 1000;

/// For the outcome events of desktop sends, set by the first `deliver_or_hold`.
static APP: OnceLock<AppHandle> = OnceLock::new();

pub(crate) struct Held {
    /// The id the sender was answered with — the phone's key for `held` / `heldOutcomes`, the
    /// desktop's for `agent-inbox-outcome`.
    pub msg_id: String,
    /// Already-staged file paths typed ahead of the text (the phone's images).
    pub paths: Vec<String>,
    pub text: String,
    pub what: &'static str,
    /// Sent from the desktop: its outcome is an `agent-inbox-outcome` event, and the phone never
    /// lists it (it has no bubble for a message it didn't send).
    pub desktop: bool,
}

struct Queue {
    /// (message, held at, why it is waiting right now)
    items: VecDeque<(Held, u64, &'static str)>,
}

static QUEUES: LazyLock<Mutex<HashMap<String, Queue>>> = LazyLock::new(|| Mutex::new(HashMap::new()));

/// What became of held messages, per tab: (msg_id, "typed" | "dropped", at, why). The phone
/// can't tell typed from dropped by watching for an echo (merged turns, captions, a stale
/// transcript after sleep all read as "never came"), and this side knows.
type Outcome = (String, &'static str, u64, Option<&'static str>);
static OUTCOMES: LazyLock<Mutex<HashMap<String, Vec<Outcome>>>> = LazyLock::new(|| Mutex::new(HashMap::new()));

fn record(tab_id: &str, held: &Held, outcome: &'static str, why: Option<&'static str>) {
    let now = super::now_ms();
    if held.desktop {
        if let Some(app) = APP.get() {
            let _ = app.emit(
                "agent-inbox-outcome",
                serde_json::json!({ "tab_id": tab_id, "msg_id": held.msg_id, "outcome": outcome, "why": why }),
            );
        }
        return;
    }
    let mut all = OUTCOMES.lock();
    all.retain(|_, v| {
        v.retain(|o| now.saturating_sub(o.2) < OUTCOME_KEEP_MS);
        !v.is_empty()
    });
    all.entry(tab_id.to_string()).or_default().push((held.msg_id.clone(), outcome, now, why));
}

/// A fresh id for a held send, unique even for two inside one millisecond.
pub(crate) fn new_msg_id() -> String {
    static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let n = SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    format!("m_{}_h{n}", super::now_ms())
}

/// `ChatDetail.held` (v0.19): the phone's messages still waiting for this tab, oldest first.
/// `reason` stays `"draft"` on the wire whatever holds it — the only value v0.19 defines.
pub(crate) fn held_for_tab(tab_id: &str) -> Vec<serde_json::Value> {
    QUEUES
        .lock()
        .get(tab_id)
        .map(|q| {
            q.items
                .iter()
                .filter(|(h, _, _)| !h.desktop)
                .map(|(h, at, _)| serde_json::json!({ "msg_id": h.msg_id, "heldAt": at, "reason": "draft" }))
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
    super::runtime_for_tab(app, tab_id) == Some(AgentRuntime::Claude)
        && matches!(super::agent_input_box(app, tab_id), super::input_box::InputBox::HasText)
}

/// The human typed into this tab's terminal in the last 2 s (`write_terminal`'s stamp; keys that
/// answered a permission prompt don't count).
fn human_typing(app: &AppState, tab_id: &str) -> bool {
    let Some(pty) = super::pty_for_tab(app, tab_id) else { return false };
    let at = app
        .pty_stats
        .read()
        .get(&pty)
        .map_or(0, |s| s.last_human_key_ms.load(std::sync::atomic::Ordering::Relaxed));
    at != 0 && super::now_ms().saturating_sub(at) < TYPING_GUARD_MS
}

/// Something is already waiting for this tab: a new message must queue behind it, never jump it.
pub(crate) fn has_held(tab_id: &str) -> bool {
    QUEUES.lock().get(tab_id).is_some_and(|q| !q.items.is_empty())
}

/// Should a message with images (typed paths) be held rather than typed now? Also while a prompt
/// the tab's mod submitted waits for its turn: typed now, this would overtake it.
pub(crate) fn must_hold(app: &AppState, tab_id: &str) -> bool {
    has_held(tab_id) || draft_in_box(app, tab_id) || app.mod_inbox.has_unstarted(tab_id)
}

/// A dialog of any kind is up in this tab: a hooked prompt, one only the screen shows, or
/// Claude's trust dialog (where a CR is "No, exit"). Read off the SCREEN as well as the hooks:
/// Claude's permission Notification lands ~6 s after its dialog opens, and a message typed
/// mid-turn in that window would have its CR approve the highlighted "Yes".
pub(crate) fn dialog_open(app: &AppState, tab_id: &str) -> bool {
    super::open_prompt(app, tab_id).is_some()
        || super::live_screen_text(app, tab_id).is_some_and(|s| super::permission::any_dialog_open(&s))
        || super::trust_dialog_open(app, tab_id)
}

/// Why a TYPED message can't go into this tab right now, or `None` when it can. Never "busy".
///
/// Claude's box must read EMPTY, not merely "no draft seen": a shell prompt reads `Unknown`, and
/// so does a screen covering the box (a draft taller than the screen, a full-screen view) —
/// typing then would run the message as a command, or send the draft after all. Another
/// runtime's box can't be read at all, so the human's recent keys are the only evidence there.
fn typing_hold(app: &AppState, tab_id: &str) -> Option<&'static str> {
    if human_typing(app, tab_id) {
        return Some("typing");
    }
    if super::runtime_for_tab(app, tab_id) == Some(AgentRuntime::Claude) {
        return match super::agent_input_box(app, tab_id) {
            super::input_box::InputBox::Empty => None,
            super::input_box::InputBox::HasText => Some("draft"),
            super::input_box::InputBox::Unknown => Some("box_unreadable"),
        };
    }
    None
}

/// Why a text message can't be delivered to this tab right now (`None`: it can). The tab's mod
/// submits around a draft and the human's typing, so when it takes the text only a dialog holds.
fn hold_reason(app: &AppState, tab_id: &str, text: &str) -> Option<&'static str> {
    if dialog_open(app, tab_id) {
        return Some("dialog");
    }
    if crate::claude_code::mod_inbox::offerable(text) && crate::claude_code::mod_inbox::takes_now(app, tab_id) {
        return None;
    }
    // Typed, it would overtake a prompt the tab's mod submitted that still waits for its turn.
    if app.mod_inbox.has_unstarted(tab_id) {
        return Some("ordering");
    }
    typing_hold(app, tab_id)
}

/// Queue a message for this tab; starts the tab's worker if none is running.
pub(crate) fn hold(app: Arc<AppState>, tab_id: &str, held: Held, why: &'static str) {
    let start = {
        let mut queues = QUEUES.lock();
        let fresh = !queues.contains_key(tab_id);
        let q = queues.entry(tab_id.to_string()).or_insert_with(|| Queue { items: VecDeque::new() });
        log::info!("[inbox] {} for tab {tab_id} held: {}", held.what, why_text(why));
        q.items.push_back((held, super::now_ms(), why));
        fresh
    };
    if start {
        let tab = tab_id.to_string();
        tauri::async_runtime::spawn(async move { worker(app, tab).await });
    }
}

fn why_text(why: &str) -> &'static str {
    match why {
        "dialog" => "a dialog is open in the tab",
        "draft" => "the human has a draft in the agent's input box",
        "typing" => "the human is typing in the tab",
        "box_unreadable" => "the agent's input box can't be read off the screen",
        "ordering" => "an earlier prompt is still waiting for its turn",
        "queued_behind" => "an earlier message is still waiting",
        _ => "waiting",
    }
}

/// What `deliver_or_hold` did with a message.
pub(crate) enum Delivery {
    /// Handed to the agent (its mod) or typed, now. A busy agent takes it when it is ready.
    Delivered(super::Delivered),
    /// Waiting for `reason` to clear; typed by the tab's worker, outcome as an event.
    Held { msg_id: String, reason: &'static str },
    /// Nothing there can take it: no terminal, or no agent owns it (it exited — typed, the
    /// message would run as a shell command).
    Refused(&'static str),
}

/// Deliver a desktop message to a tab's agent now, or hold it until it can be: the one gate for
/// everything the desktop types for someone else (board answers, Overlord directives and
/// notices). Busy is not a hold reason — see the module doc.
pub(crate) async fn deliver_or_hold(
    app: &Arc<AppState>,
    handle: &AppHandle,
    tab_id: &str,
    text: &str,
    what: &'static str,
) -> Delivery {
    let _ = APP.set(handle.clone());
    if text.trim().is_empty() {
        return Delivery::Refused("empty");
    }
    let Some(pty) = super::pty_for_tab(app, tab_id) else {
        return Delivery::Refused("no_terminal");
    };
    if !crate::comms::agent_owns_terminal(app, &pty).await {
        return Delivery::Refused("no_agent");
    }
    let held = |reason: &'static str| {
        let msg_id = new_msg_id();
        let h = Held { msg_id: msg_id.clone(), paths: Vec::new(), text: text.to_string(), what, desktop: true };
        hold(app.clone(), tab_id, h, reason);
        Delivery::Held { msg_id, reason }
    };
    let reason = if has_held(tab_id) { Some("queued_behind") } else { hold_reason(app, tab_id, text) };
    if let Some(reason) = reason {
        return held(reason);
    }
    match super::submit_prompt(app, tab_id, &pty, text).await {
        Ok(d) => {
            log::info!("[inbox] {what} for tab {tab_id} delivered ({d:?})");
            Delivery::Delivered(d)
        }
        // The mod was asked and didn't take it after all, and there is a draft: wait for it.
        Err(_) if draft_in_box(app, tab_id) => held("draft"),
        Err(e) => {
            log::warn!("[inbox] {what} for tab {tab_id} failed to type: {e}");
            Delivery::Refused("type_failed")
        }
    }
}

/// Types the tab's held messages in order, each once nothing holds it (`hold_reason`). Ends
/// (dropping what's left, logged) when the tab's terminal is gone, and removes its queue when
/// emptied — under the lock, so a `hold` racing the exit starts a fresh worker.
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
            while q.items.front().is_some_and(|(_, at, _)| now.saturating_sub(*at) > HOLD_MAX_MS) {
                let (h, _, why) = q.items.pop_front().unwrap();
                log::warn!("[inbox] held {} for tab {tab_id} dropped after 30 min ({})", h.what, why_text(why));
                record(&tab_id, &h, "dropped", Some("expired"));
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
        // Typed from a COPY, and taken off the queue only when its outcome is recorded: a phone
        // reading ChatDetail mid-typing (up to ~1 s, more with images) must still find it in
        // `held`, not in neither list — which reads as "never delivered". Only this worker pops,
        // and `hold` appends at the back, so the front is still this item afterwards.
        let Some((paths, text)) = QUEUES
            .lock()
            .get(&tab_id)
            .and_then(|q| q.items.front())
            .map(|(h, _, _)| (h.paths.clone(), h.text.clone()))
        else {
            continue;
        };
        let set_why = |why: &'static str| {
            if let Some(front) = QUEUES.lock().get_mut(&tab_id).and_then(|q| q.items.front_mut()) {
                front.2 = why;
            }
        };
        // Typed paths can't go to the mod, and wait out an unstarted mod prompt like any typing.
        let why = if paths.is_empty() {
            hold_reason(&app, &tab_id, &text)
        } else if dialog_open(&app, &tab_id) {
            Some("dialog")
        } else if app.mod_inbox.has_unstarted(&tab_id) {
            Some("ordering")
        } else {
            typing_hold(&app, &tab_id)
        };
        if let Some(why) = why {
            set_why(why);
            continue;
        }
        let typed = if paths.is_empty() {
            super::submit_prompt(&app, &tab_id, &pty, &text).await.map(|_| ())
        } else {
            super::inject_paths_then_text(&app, &pty, &paths, &text, true).await
        };
        // The mod was asked and didn't take it after all, and there is a draft: keep waiting.
        if typed.is_err() && draft_in_box(&app, &tab_id) {
            set_why("draft");
            continue;
        }
        let mut queues = QUEUES.lock();
        let popped = queues.get_mut(&tab_id).and_then(|q| q.items.pop_front());
        drop(queues);
        let Some((h, _, _)) = popped else { continue };
        match typed {
            Ok(()) => {
                log::info!("[inbox] held {} for tab {tab_id} delivered", h.what);
                record(&tab_id, &h, "typed", None);
            }
            Err(e) => {
                log::warn!("[inbox] held {} for tab {tab_id} failed to type: {e}", h.what);
                record(&tab_id, &h, "dropped", Some("type_failed"));
            }
        }
        // The next one waits for Claude to take this one: until then the box holds its text and
        // reads as a draft, which is the ordering this wants.
    }
}

fn drop_all(tab_id: &str, why: &str, code: &'static str) {
    let Some(q) = QUEUES.lock().remove(tab_id) else { return };
    for (h, _, _) in q.items {
        log::warn!("[inbox] held {} for tab {tab_id} dropped: {why}", h.what);
        record(tab_id, &h, "dropped", Some(code));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn held(id: &str, desktop: bool) -> Held {
        Held { msg_id: id.into(), paths: vec![], text: "x".into(), what: "phone message", desktop }
    }

    #[test]
    fn outcomes_are_reported_per_tab_with_their_reason() {
        record("tab-outcomes-a", &held("m_1_h0", false), "typed", None);
        record("tab-outcomes-a", &held("m_2_h1", false), "dropped", Some("expired"));
        record("tab-outcomes-b", &held("m_3_h2", false), "dropped", Some("terminal_gone"));
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
    fn a_desktop_outcome_never_reaches_the_phone() {
        record("tab-outcomes-desk", &held("m_4_h3", true), "typed", None);
        assert!(outcomes_for_tab("tab-outcomes-desk").is_empty());
    }

    #[test]
    fn held_lists_the_phones_messages_oldest_first() {
        {
            let mut queues = QUEUES.lock();
            let q = queues.entry("tab-held-a".into()).or_insert_with(|| Queue { items: VecDeque::new() });
            q.items.push_back((held("m_1_h0", false), 10, "draft"));
            q.items.push_back((held("m_9_h9", true), 15, "typing"));
            q.items.push_back((held("m_2_h1", false), 20, "dialog"));
        }
        let held = held_for_tab("tab-held-a");
        assert_eq!(held.len(), 2);
        assert_eq!(held[0]["msg_id"], "m_1_h0");
        assert_eq!(held[0]["heldAt"], 10);
        assert_eq!(held[0]["reason"], "draft");
        assert_eq!(held[1]["msg_id"], "m_2_h1");
        assert_eq!(held[1]["reason"], "draft");
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
