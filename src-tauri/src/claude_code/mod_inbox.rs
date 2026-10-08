//! Prompts handed to the maiterm-tab mod (claude_mod.rs) to submit, instead of typed into the
//! terminal.
//!
//! An interactive Claude session running the mod keeps one long-poll open on `GET
//! /hooks/inbox?tab_id=` for its tab. A prompt maiTerm sends that tab (a phone message, a
//! follow-up, an Overlord directive, a peer's message) is offered to that poll; the mod acks it
//! and submits it with `$.prompt.submit`, which starts a turn once the session is idle and
//! leaves whatever the human has in the input box where it is. So a message no longer waits for
//! a desktop draft to be sent or cleared, and nothing is pasted into a box someone is typing in.
//!
//! Delivery is take-or-retract, as for answers (mod_asks.rs), with one more step: the poll hands
//! the offer out, and the mod must ack it before it submits. The ack is refused for an offer that
//! was retracted, and `deliver` reports success only for an acked one. So the prompt is submitted
//! exactly when `deliver` said it was delivered, even if a poll's reply was lost on the way.
//!
//! A tab counts as served only while its mod polls: it re-polls within moments of each round,
//! so a tab unpolled for `STALE_AFTER` has no mod listening (it exited, or its module reloaded
//! and hasn't restarted the loop yet), and callers type as before.

use std::collections::{HashMap, VecDeque};
use std::sync::Arc;
use std::time::{Duration, Instant};

use parking_lot::Mutex;
use tokio::sync::Notify;

use crate::state::app_state::AgentSessionState;
use crate::state::AppState;

/// The mod re-polls between rounds within moments; this long without a poll means it is gone.
const STALE_AFTER: Duration = Duration::from_secs(5);
/// How long an offer may sit unacked before `deliver` retracts it.
pub const DELIVERY_TIMEOUT: Duration = Duration::from_secs(2);
/// The longest one poll parks (the mod's `$.http.fetch` dies at about 30 s).
pub const MAX_WAIT: Duration = super::mod_asks::MAX_WAIT;
/// A tab nobody has polled for this long is forgotten.
const FORGET_AFTER: Duration = Duration::from_secs(600);

#[derive(Clone, Copy, PartialEq, Debug)]
enum Stage {
    /// Waiting for a poll.
    Queued,
    /// A poll returned it; waiting for the ack.
    Handed,
    /// The mod acked it and submits it.
    Acked,
}

struct Offer {
    id: String,
    text: String,
    stage: Stage,
}

struct Inbox {
    /// Polls parked now (decremented by `PollGuard`); otherwise when the last poll ended.
    polling: usize,
    last_poll: Instant,
    offers: VecDeque<Offer>,
    wake: Arc<Notify>,
}

impl Inbox {
    fn new(now: Instant) -> Self {
        Inbox { polling: 0, last_poll: now, offers: VecDeque::new(), wake: Arc::new(Notify::new()) }
    }

    fn is_live(&self, now: Instant) -> bool {
        self.polling > 0 || now.duration_since(self.last_poll) < STALE_AFTER
    }
}

struct PollGuard<'a> {
    tabs: &'a Mutex<HashMap<String, Inbox>>,
    tab_id: &'a str,
}

impl Drop for PollGuard<'_> {
    fn drop(&mut self) {
        if let Some(inbox) = self.tabs.lock().get_mut(self.tab_id) {
            inbox.polling = inbox.polling.saturating_sub(1);
            inbox.last_poll = Instant::now();
        }
    }
}

/// What one poll came back with.
#[derive(Debug, PartialEq)]
pub enum Polled {
    /// A prompt to ack, then submit.
    Offer { id: String, text: String },
    /// Nothing yet: poll again.
    Empty,
}

#[derive(Default)]
pub struct ModInbox {
    tabs: Mutex<HashMap<String, Inbox>>,
    seq: std::sync::atomic::AtomicU64,
}

impl ModInbox {
    /// A mod is polling for this tab.
    pub fn is_live(&self, tab_id: &str) -> bool {
        let now = Instant::now();
        self.tabs.lock().get(tab_id).is_some_and(|i| i.is_live(now))
    }

    /// One poll round for a tab: the oldest queued offer (handing it out), else waits up to `wait`
    /// for one.
    pub async fn poll(&self, tab_id: &str, wait: Duration) -> Polled {
        let wake = {
            let now = Instant::now();
            let mut tabs = self.tabs.lock();
            tabs.retain(|_, i| i.polling > 0 || now.duration_since(i.last_poll) < FORGET_AFTER);
            let inbox = tabs.entry(tab_id.to_string()).or_insert_with(|| Inbox::new(now));
            inbox.last_poll = now;
            if let Some(o) = hand_out(inbox) {
                return o;
            }
            inbox.polling += 1;
            inbox.wake.clone()
        };
        let guard = PollGuard { tabs: &self.tabs, tab_id };
        // `notify_one` leaves a permit when nothing waits yet, so an offer queued between the
        // lock above and this wait is not missed.
        let _ = tokio::time::timeout(wait.min(MAX_WAIT), wake.notified()).await;
        drop(guard);
        let mut tabs = self.tabs.lock();
        tabs.get_mut(tab_id).and_then(hand_out).unwrap_or(Polled::Empty)
    }

    /// The mod has offer `id` in hand and is about to submit it. `false` when it was retracted
    /// (or never existed): the mod must then drop it.
    pub fn ack(&self, tab_id: &str, id: &str) -> bool {
        let mut tabs = self.tabs.lock();
        let Some(o) = tabs.get_mut(tab_id).and_then(|i| i.offers.iter_mut().find(|o| o.id == id)) else {
            return false;
        };
        if o.stage != Stage::Handed {
            return false;
        }
        o.stage = Stage::Acked;
        true
    }

    /// Offers `text` to the tab's mod and waits for the ack. `true` once the mod has it and is
    /// submitting it; `false` when no mod is polling or none acked in time, and then the offer
    /// is retracted, so it can never be submitted after this returned.
    pub async fn deliver(&self, tab_id: &str, text: &str) -> bool {
        let id = format!("i{}", self.seq.fetch_add(1, std::sync::atomic::Ordering::Relaxed));
        {
            let now = Instant::now();
            let mut tabs = self.tabs.lock();
            let Some(inbox) = tabs.get_mut(tab_id).filter(|i| i.is_live(now)) else { return false };
            inbox.offers.push_back(Offer { id: id.clone(), text: text.to_string(), stage: Stage::Queued });
            inbox.wake.notify_one();
        }
        let deadline = Instant::now() + DELIVERY_TIMEOUT;
        loop {
            {
                let mut tabs = self.tabs.lock();
                let Some(inbox) = tabs.get_mut(tab_id) else { return false };
                let Some(pos) = inbox.offers.iter().position(|o| o.id == id) else { return false };
                if inbox.offers[pos].stage == Stage::Acked {
                    inbox.offers.remove(pos);
                    return true;
                }
                if Instant::now() >= deadline {
                    inbox.offers.remove(pos);
                    return false;
                }
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }
}

/// The oldest offer still waiting for a poll, handed out. One handed out but never acked (its
/// reply lost) stays where it is until `deliver` retracts it.
fn hand_out(inbox: &mut Inbox) -> Option<Polled> {
    let o = inbox.offers.iter_mut().find(|o| o.stage == Stage::Queued)?;
    o.stage = Stage::Handed;
    Some(Polled::Offer { id: o.id.clone(), text: o.text.clone() })
}

/// Whether a prompt for this tab should go to its mod now rather than be typed: a mod is polling,
/// and either the agent is between turns (a typed prompt and a submitted one then do the same, and
/// only the typed one can land in a box the human is typing in), or the human has a draft in the
/// box (typing would send it; the mod leaves it alone, the prompt waiting for the turn to end).
///
/// A busy agent with an empty box keeps the keystrokes: Claude folds a prompt typed mid-turn into
/// the running turn, which a phone message steering the agent relies on, while a submitted one
/// waits for the turn to end.
///
/// Only for text that may be offered at all (`offerable`).
pub fn takes_now(app: &AppState, tab_id: &str) -> bool {
    if !app.mod_inbox.is_live(tab_id) {
        return false;
    }
    agent_between_turns(app, tab_id) || crate::mailink::draft_hold::draft_in_box(app, tab_id)
}

/// Whether `text` can go to a mod at all. Not a slash command: a submitted prompt is the model's
/// to read, so `/compact` or `/model` would reach it as words instead of running; those are
/// typed. Not empty either: typed, an empty prompt is a bare Enter.
pub fn offerable(text: &str) -> bool {
    let t = text.trim_start();
    !t.is_empty() && !t.starts_with('/')
}

fn agent_between_turns(app: &AppState, tab_id: &str) -> bool {
    let sessions = app.agent_sessions.read();
    let mut tab_sessions = sessions.values().filter(|s| s.tab_id == tab_id).peekable();
    tab_sessions.peek().is_some()
        && tab_sessions.all(|s| {
            s.pending_question.is_none()
                && matches!(s.state, AgentSessionState::WaitingInput | AgentSessionState::Stopped)
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn parked_poll(inbox: &Arc<ModInbox>, tab: &'static str) -> tokio::task::JoinHandle<Polled> {
        let inbox = inbox.clone();
        let h = tokio::spawn(async move { inbox.poll(tab, Duration::from_secs(5)).await });
        tokio::time::sleep(Duration::from_millis(30)).await;
        h
    }

    #[tokio::test]
    async fn an_acked_offer_is_delivered() {
        let inbox = Arc::new(ModInbox::default());
        let poll = parked_poll(&inbox, "tab").await;
        let delivering = {
            let inbox = inbox.clone();
            tokio::spawn(async move { inbox.deliver("tab", "hello").await })
        };
        let Polled::Offer { id, text } = poll.await.unwrap() else { panic!("the poll should get the offer") };
        assert_eq!(text, "hello");
        assert!(inbox.ack("tab", &id));
        assert!(delivering.await.unwrap());
        // Gone once reported: a second ack is refused.
        assert!(!inbox.ack("tab", &id));
    }

    #[tokio::test]
    async fn nothing_is_offered_to_a_tab_no_mod_polls() {
        let inbox = ModInbox::default();
        assert!(!inbox.is_live("tab"));
        assert!(!inbox.deliver("tab", "hello").await);
    }

    #[tokio::test]
    async fn an_offer_handed_out_but_never_acked_is_retracted_and_its_late_ack_refused() {
        let inbox = Arc::new(ModInbox::default());
        let poll = parked_poll(&inbox, "tab").await;
        let started = Instant::now();
        let delivering = {
            let inbox = inbox.clone();
            tokio::spawn(async move { inbox.deliver("tab", "hello").await })
        };
        let Polled::Offer { id, .. } = poll.await.unwrap() else { panic!("the poll should get the offer") };
        // The reply was lost on its way to the mod: no ack comes.
        assert!(!delivering.await.unwrap());
        assert!(started.elapsed() >= DELIVERY_TIMEOUT);
        assert!(!inbox.ack("tab", &id), "a retracted offer must never be submitted");
    }

    #[tokio::test]
    async fn offers_are_handed_out_oldest_first_one_per_poll() {
        let inbox = Arc::new(ModInbox::default());
        let _ = inbox.poll("tab", Duration::from_millis(1)).await; // the mod is listening
        let a = {
            let inbox = inbox.clone();
            tokio::spawn(async move { inbox.deliver("tab", "first").await })
        };
        tokio::time::sleep(Duration::from_millis(10)).await;
        let b = {
            let inbox = inbox.clone();
            tokio::spawn(async move { inbox.deliver("tab", "second").await })
        };
        tokio::time::sleep(Duration::from_millis(10)).await;
        for want in ["first", "second"] {
            let Polled::Offer { id, text } = inbox.poll("tab", Duration::from_millis(1)).await else { panic!("expected an offer") };
            assert_eq!(text, want);
            assert!(inbox.ack("tab", &id));
        }
        assert!(a.await.unwrap() && b.await.unwrap());
        assert_eq!(inbox.poll("tab", Duration::from_millis(1)).await, Polled::Empty);
    }

    #[tokio::test]
    async fn a_poll_whose_client_went_away_stops_counting_after_a_while() {
        let inbox = Arc::new(ModInbox::default());
        let poll = parked_poll(&inbox, "tab").await;
        assert_eq!(inbox.tabs.lock().get("tab").map(|i| i.polling), Some(1));
        poll.abort();
        let _ = poll.await;
        assert_eq!(inbox.tabs.lock().get("tab").map(|i| i.polling), Some(0));
        // Still live for STALE_AFTER: the mod re-polls between rounds.
        assert!(inbox.is_live("tab"));
        inbox.tabs.lock().get_mut("tab").unwrap().last_poll = Instant::now() - STALE_AFTER;
        assert!(!inbox.is_live("tab"));
    }
}
