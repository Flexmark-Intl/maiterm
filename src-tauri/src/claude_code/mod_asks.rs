//! Answers handed to the maiterm-tab mod (claude_mod.rs) instead of typed into the terminal.
//!
//! When a Claude session runs the mod, a permission dialog and an AskUserQuestion selector each
//! have a mod hook waiting behind them. The hook registers an ask when it sends its event
//! (`/hooks?…&ask=<id>`), then long-polls `GET /hooks/ask/<id>` in short rounds, and whatever
//! maiTerm puts in the ask is the hook's answer. A permission hook's `{decision}` closes a dialog
//! that is already on screen, and a question's `{answers}` becomes the tool's result. A key the
//! human presses at the desktop still wins: the dialog closes, Claude abandons the hook, and the
//! ask stops being polled.
//!
//! An ask counts as live only while it is being polled. A hook re-polls within moments of each
//! round, so one unpolled for `STALE_AFTER` belongs to a hook that has gone (answered at the
//! desktop, the agent exited), and it is pruned.
//!
//! Delivery is take-or-retract: the hook takes the answer under the lock, and a responder that
//! finds it still untaken after `DELIVERY_TIMEOUT` removes it under the same lock and reports
//! failure. So an answer reports success only when the agent had it in hand, and never lands
//! after a failure was reported.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use parking_lot::Mutex;
use serde_json::Value;
use tokio::sync::Notify;

/// A hook re-polls between rounds within moments; this long without a poll means it is gone.
const STALE_AFTER: Duration = Duration::from_secs(5);
/// How long a delivered answer may sit untaken before the responder retracts it.
pub const DELIVERY_TIMEOUT: Duration = Duration::from_secs(2);
/// The longest one poll parks. The mod's `$.http.fetch` dies at about 30 s, so a round must be
/// well under that.
pub const MAX_WAIT: Duration = Duration::from_secs(10);

#[derive(Clone, Debug, PartialEq)]
pub enum AskKind {
    /// A permission dialog for a call to `tool`; `fingerprint` is its input as the permission
    /// ledger keys it (server.rs `tool_input_fingerprint`), so the call's own end settles it.
    Permission { tool: String, fingerprint: String },
    /// An AskUserQuestion selector, with the tool's `questions` as the model sent them.
    Question { questions: Value },
}

struct Ask {
    tab_id: String,
    session_id: String,
    /// `gate::agent_key`: "" for the main thread, else the subagent's id.
    agent: String,
    kind: AskKind,
    /// Polls parked on this ask now (decremented by `PollGuard`, so a poll whose client went
    /// away still counts down); otherwise when the last poll ended.
    polling: usize,
    last_poll: Instant,
    answer: Option<Value>,
    /// The hook took `answer`: `deliver` reports success and removes the ask.
    taken: bool,
    wake: Arc<Notify>,
}

impl Ask {
    fn is_live(&self, now: Instant) -> bool {
        !self.taken && (self.polling > 0 || now.duration_since(self.last_poll) < STALE_AFTER)
    }

    /// Kept in the map: live, or taken and still waiting for its `deliver` to report it — not
    /// forever, in case that request was dropped (`last_poll` is when it was taken).
    fn is_kept(&self, now: Instant) -> bool {
        self.is_live(now) || (self.taken && now.duration_since(self.last_poll) < Duration::from_secs(30))
    }
}

/// Ends one parked poll, whether it finished or its future was dropped (the hook's fetch was cut
/// when Claude abandoned the hook): without it an abandoned ask counted as polled forever.
struct PollGuard<'a> {
    asks: &'a Mutex<HashMap<String, Ask>>,
    id: &'a str,
}

impl Drop for PollGuard<'_> {
    fn drop(&mut self) {
        if let Some(ask) = self.asks.lock().get_mut(self.id) {
            ask.polling = ask.polling.saturating_sub(1);
            ask.last_poll = Instant::now();
        }
    }
}

#[derive(Default)]
pub struct ModAsks {
    asks: Mutex<HashMap<String, Ask>>,
}

/// What a poll came back with.
pub enum Polled {
    Answer(Value),
    /// Nothing yet: poll again.
    Pending,
    /// No such ask (never registered, already answered, or pruned): stop polling.
    Unknown,
}

/// A live ask of one tab, for the responder to choose a delivery by.
#[derive(Clone, Debug)]
pub struct LiveAsk {
    pub id: String,
    pub kind: AskKind,
}

impl ModAsks {
    /// Records an ask a mod hook is about to wait on. Registering an id again replaces it.
    pub fn register(&self, id: &str, tab_id: &str, session_id: &str, agent: &str, kind: AskKind) {
        let now = Instant::now();
        let mut asks = self.asks.lock();
        asks.retain(|_, a| a.is_kept(now));
        asks.insert(id.to_string(), Ask {
            tab_id: tab_id.to_string(),
            session_id: session_id.to_string(),
            agent: agent.to_string(),
            kind,
            polling: 0,
            // Counts as a poll: the hook polls straight after registering.
            last_poll: now,
            answer: None,
            taken: false,
            wake: Arc::new(Notify::new()),
        });
    }

    /// One poll round: the answer if there is one (taking it, which ends the ask), else waits
    /// up to `wait` for one to arrive.
    pub async fn poll(&self, id: &str, wait: Duration) -> Polled {
        let wake = {
            let mut asks = self.asks.lock();
            let Some(ask) = asks.get_mut(id).filter(|a| !a.taken) else { return Polled::Unknown };
            if let Some(answer) = ask.answer.take() {
                ask.taken = true;
                ask.last_poll = Instant::now();
                return Polled::Answer(answer);
            }
            ask.polling += 1;
            ask.wake.clone()
        };
        let guard = PollGuard { asks: &self.asks, id };
        // `notify_one` leaves a permit when nothing waits yet, so an answer delivered between
        // the lock above and this wait is not missed.
        let _ = tokio::time::timeout(wait.min(MAX_WAIT), wake.notified()).await;
        drop(guard);
        let mut asks = self.asks.lock();
        let Some(ask) = asks.get_mut(id).filter(|a| !a.taken) else { return Polled::Unknown };
        match ask.answer.take() {
            Some(answer) => {
                ask.taken = true;
                ask.last_poll = Instant::now();
                Polled::Answer(answer)
            }
            None => Polled::Pending,
        }
    }

    /// Drops an ask whose prompt was answered some other way (a key at the desktop), so no
    /// answer can be delivered to it and reported as taken.
    pub fn cancel(&self, id: &str) {
        if let Some(ask) = self.asks.lock().remove(id) {
            ask.wake.notify_one();
        }
    }

    /// Drops the asks the end of one call settles: its permission dialog (matched by agent, tool
    /// and input, as the permission ledger matches it), or, for AskUserQuestion, the agent's
    /// question. The prompt can't be open once its call has ended.
    pub fn settle_call(&self, session_id: &str, agent: &str, tool: &str, fingerprint: &str) {
        self.drop_where(|a| {
            a.session_id == session_id && a.agent == agent && match &a.kind {
                AskKind::Permission { tool: t, fingerprint: f } => t == tool && f == fingerprint,
                AskKind::Question { .. } => tool == "AskUserQuestion",
            }
        });
    }

    /// Drops every ask of one agent in a session (`agent` "" = the main thread): its turn ended
    /// (Stop, an idle prompt) or the subagent did (SubagentStop). A subagent's ask outlives the
    /// parent's turn, as its dialog does, so this never sweeps across agents.
    pub fn settle_agent(&self, session_id: &str, agent: &str) {
        self.drop_where(|a| a.session_id == session_id && a.agent == agent);
    }

    /// Drops every ask of a session that ended.
    pub fn settle_session(&self, session_id: &str) {
        self.drop_where(|a| a.session_id == session_id);
    }

    /// Only untaken asks: a taken one is `deliver`'s to report and remove.
    fn drop_where(&self, mut settled: impl FnMut(&Ask) -> bool) {
        let mut asks = self.asks.lock();
        asks.retain(|_, a| {
            let drop = !a.taken && settled(a);
            if drop {
                a.wake.notify_one();
            }
            !drop
        });
    }

    /// The live asks of a tab's session. Callers decide by count and kind.
    pub fn live_for(&self, tab_id: &str, session_id: &str) -> Vec<LiveAsk> {
        let now = Instant::now();
        let mut asks = self.asks.lock();
        asks.retain(|_, a| a.is_kept(now));
        asks.iter()
            .filter(|(_, a)| a.is_live(now) && a.tab_id == tab_id && a.session_id == session_id && a.answer.is_none())
            .map(|(id, a)| LiveAsk { id: id.clone(), kind: a.kind.clone() })
            .collect()
    }

    /// Hands `answer` to the hook waiting on `id` and waits for it to be taken. `true` once the
    /// hook has it; `false` when the ask is gone or nobody took it in time, and then the answer
    /// is retracted, so it can never be taken after this returned.
    pub async fn deliver(&self, id: &str, answer: Value) -> bool {
        {
            let mut asks = self.asks.lock();
            let Some(ask) = asks.get_mut(id) else { return false };
            if ask.answer.is_some() || ask.taken {
                return false;
            }
            ask.answer = Some(answer);
            ask.wake.notify_one();
        }
        let deadline = Instant::now() + DELIVERY_TIMEOUT;
        loop {
            {
                let mut asks = self.asks.lock();
                match asks.get(id) {
                    Some(a) if a.taken => {
                        asks.remove(id);
                        return true;
                    }
                    // Cancelled or settled while the answer waited: it was never taken.
                    None => return false,
                    Some(_) if Instant::now() >= deadline => {
                        asks.remove(id);
                        return false;
                    }
                    Some(_) => {}
                }
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    }
}

/// The decision a permission dialog's row stands for, when the mod can give it: "Yes" allows,
/// and any "No" row denies — as the dialog's No does, it stops the turn so the human can say
/// what to do instead. `None` for every other row, which then goes in as its keystroke.
///
/// That includes "Yes, and don't ask again for: python3 *". The rule that row saves is worked out
/// by the dialog itself: the event's `permission_suggestions` are a different, narrower rule (the
/// exact command — seen on 2.1.295), so allowing with them would quietly save less than the human
/// chose. The keystroke presses the row they read.
pub fn permission_decision(label: &str) -> Option<Value> {
    let l = label.trim().to_lowercase();
    if l == "yes" {
        return Some(serde_json::json!({ "behavior": "allow" }));
    }
    if l == "no" || l.starts_with("no,") {
        return Some(serde_json::json!({
            "behavior": "deny",
            "message": "The user declined this from maiTerm.",
            "interrupt": true,
        }));
    }
    None
}

/// The `answers` an AskUserQuestion result carries — question text → answer — from one
/// `(selected labels, other text)` per question, in order. Claude joins a multi-select's labels
/// with ", ", and the free-text row's words stand in for a label. `None` when the answers don't
/// fit the questions: a count mismatch, a label no option has, or a single-select question
/// without exactly one answer.
pub fn question_answers(questions: &Value, answers: &[(Vec<String>, Option<String>)]) -> Option<Value> {
    let qs = questions.as_array()?;
    if qs.len() != answers.len() {
        return None;
    }
    let mut out = serde_json::Map::new();
    for (q, (selected, other)) in qs.iter().zip(answers) {
        let text = q.get("question")?.as_str()?;
        let labels: Vec<&str> = q
            .get("options")
            .and_then(Value::as_array)
            .map(|o| o.iter().filter_map(|o| o.get("label")?.as_str()).collect())
            .unwrap_or_default();
        if selected.iter().any(|s| !labels.contains(&s.as_str())) {
            return None;
        }
        let other = other.as_deref().map(str::trim).filter(|t| !t.is_empty());
        let multi = q.get("multiSelect").and_then(Value::as_bool).unwrap_or(false);
        let mut parts: Vec<&str> = selected.iter().map(String::as_str).collect();
        parts.extend(other);
        if !multi && parts.len() != 1 {
            return None;
        }
        out.insert(text.to_string(), Value::String(parts.join(", ")));
    }
    Some(Value::Object(out))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn permission() -> AskKind {
        AskKind::Permission { tool: "Bash".into(), fingerprint: "fp".into() }
    }

    fn polling(asks: &ModAsks, id: &str) -> Option<usize> {
        asks.asks.lock().get(id).map(|a| a.polling)
    }

    #[tokio::test]
    async fn a_poll_whose_client_went_away_stops_counting_as_polled() {
        let asks = Arc::new(ModAsks::default());
        asks.register("a1", "tab", "sess", "", permission());
        let poller = {
            let asks = asks.clone();
            tokio::spawn(async move { asks.poll("a1", Duration::from_secs(5)).await })
        };
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert_eq!(polling(&asks, "a1"), Some(1));
        // What hyper does when the mod's fetch is cut: the handler future is dropped mid-wait.
        poller.abort();
        let _ = poller.await;
        assert_eq!(polling(&asks, "a1"), Some(0));
    }

    #[tokio::test]
    async fn an_answer_delivered_before_the_poll_parks_is_not_missed() {
        let asks = Arc::new(ModAsks::default());
        asks.register("a1", "tab", "sess", "", permission());
        let delivering = {
            let asks = asks.clone();
            tokio::spawn(async move { asks.deliver("a1", json!({"behavior": "allow"})).await })
        };
        tokio::time::sleep(Duration::from_millis(50)).await;
        // The answer is already there: the poll takes it at once rather than parking 5 s.
        let started = Instant::now();
        assert!(matches!(asks.poll("a1", Duration::from_secs(5)).await, Polled::Answer(_)));
        assert!(started.elapsed() < Duration::from_secs(1));
        assert!(delivering.await.unwrap());
    }

    #[tokio::test]
    async fn a_permit_left_by_an_early_notify_wakes_the_next_wait() {
        let notify = Notify::new();
        notify.notify_one();
        tokio::time::timeout(Duration::from_millis(100), notify.notified()).await
            .expect("notify_one stores a permit for a wait that starts later");
    }

    #[tokio::test]
    async fn an_ask_settled_while_its_answer_waits_reports_not_taken() {
        let asks = Arc::new(ModAsks::default());
        asks.register("a1", "tab", "sess", "", permission());
        let delivering = {
            let asks = asks.clone();
            tokio::spawn(async move { asks.deliver("a1", json!({"behavior": "allow"})).await })
        };
        tokio::time::sleep(Duration::from_millis(50)).await;
        asks.settle_call("sess", "", "Bash", "fp");
        assert!(!delivering.await.unwrap());
    }

    #[test]
    fn settling_matches_the_call_or_the_agent_and_never_crosses_agents() {
        let asks = ModAsks::default();
        asks.register("main", "tab", "sess", "", permission());
        asks.register("sub", "tab", "sess", "agent-1", permission());
        asks.register("other", "tab", "sess", "", AskKind::Permission { tool: "Bash".into(), fingerprint: "fp2".into() });
        asks.register("q", "tab", "sess", "", AskKind::Question { questions: json!([]) });
        asks.settle_call("sess", "", "Bash", "fp");
        let ids = |asks: &ModAsks| {
            let mut v: Vec<String> = asks.live_for("tab", "sess").into_iter().map(|a| a.id).collect();
            v.sort();
            v
        };
        assert_eq!(ids(&asks), vec!["other", "q", "sub"]);
        asks.settle_call("sess", "", "AskUserQuestion", "whatever");
        assert_eq!(ids(&asks), vec!["other", "sub"]);
        // The main thread's turn ending leaves the subagent's dialog alone.
        asks.settle_agent("sess", "");
        assert_eq!(ids(&asks), vec!["sub"]);
        asks.settle_session("sess");
        assert!(ids(&asks).is_empty());
    }

    #[tokio::test]
    async fn an_answer_reaches_a_parked_poll() {
        let asks = Arc::new(ModAsks::default());
        asks.register("a1", "tab", "sess", "", permission());
        let poller = {
            let asks = asks.clone();
            tokio::spawn(async move { asks.poll("a1", Duration::from_secs(5)).await })
        };
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert!(asks.deliver("a1", json!({"behavior": "allow"})).await);
        match poller.await.unwrap() {
            Polled::Answer(v) => assert_eq!(v, json!({"behavior": "allow"})),
            _ => panic!("the poll should have taken the answer"),
        }
        assert!(matches!(asks.poll("a1", Duration::from_millis(1)).await, Polled::Unknown));
    }

    #[tokio::test]
    async fn an_untaken_answer_is_retracted_and_never_taken_later() {
        let asks = ModAsks::default();
        asks.register("a1", "tab", "sess", "", permission());
        assert!(!asks.deliver("a1", json!({"behavior": "allow"})).await);
        assert!(matches!(asks.poll("a1", Duration::from_millis(1)).await, Polled::Unknown));
    }

    #[tokio::test]
    async fn live_for_names_only_this_sessions_unanswered_asks() {
        let asks = ModAsks::default();
        asks.register("a1", "tab", "sess", "", permission());
        asks.register("a2", "tab", "old", "", permission());
        asks.register("a3", "other", "sess", "", permission());
        let live = asks.live_for("tab", "sess");
        assert_eq!(live.len(), 1);
        assert_eq!(live[0].id, "a1");
        assert!(!asks.deliver("nope", json!({})).await);
    }

    #[test]
    fn permission_rows_map_to_decisions_only_where_the_mod_can_give_them() {
        assert_eq!(permission_decision("Yes"), Some(json!({ "behavior": "allow" })));
        assert_eq!(permission_decision("No").unwrap()["behavior"], "deny");
        assert_eq!(permission_decision("No, and tell Claude what to do differently").unwrap()["behavior"], "deny");
        // Rows whose effect the dialog works out itself go in as their keystroke.
        assert_eq!(permission_decision("Yes, and don\u{2019}t ask again for: python3 *"), None);
        assert_eq!(permission_decision("Yes, and always allow access to /tmp/x from this project"), None);
        assert_eq!(permission_decision("Yes, and switch to auto mode"), None);
        assert_eq!(permission_decision("Yes, auto-accept edits"), None);
        assert_eq!(permission_decision("Nope"), None);
    }

    #[test]
    fn question_answers_follow_the_selector_rules() {
        let qs = json!([
            { "question": "Which color?", "options": [{ "label": "Red" }, { "label": "Blue" }], "multiSelect": false },
            { "question": "Which fruits?", "options": [{ "label": "Apple" }, { "label": "Pear" }], "multiSelect": true }
        ]);
        let got = question_answers(&qs, &[
            (vec!["Blue".into()], None),
            (vec!["Apple".into(), "Pear".into()], Some(" Plum ".into())),
        ]);
        assert_eq!(got, Some(json!({ "Which color?": "Blue", "Which fruits?": "Apple, Pear, Plum" })));
        // Other text alone answers a single-select question.
        assert_eq!(
            question_answers(&qs, &[(vec![], Some("Green".into())), (vec![], None)]),
            Some(json!({ "Which color?": "Green", "Which fruits?": "" }))
        );
        // A label the question doesn't offer, two answers to a single-select, a short list.
        assert_eq!(question_answers(&qs, &[(vec!["Teal".into()], None), (vec![], None)]), None);
        assert_eq!(question_answers(&qs, &[(vec!["Red".into(), "Blue".into()], None), (vec![], None)]), None);
        assert_eq!(question_answers(&qs, &[(vec!["Red".into()], None)]), None);
    }
}
