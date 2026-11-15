//! Prompts handed to the maiterm-tab mod (claude_mod.rs) to submit, instead of typed into the
//! terminal.
//!
//! An interactive Claude session running the mod keeps one long-poll open on `GET
//! /hooks/inbox?tab_id=` for its tab. A prompt maiTerm sends that tab between turns (a phone
//! message, a follow-up, an Overlord directive, a peer's message) is offered to that poll; the
//! mod acks it and submits it with `$.prompt.submit`, which starts a turn and leaves whatever the
//! human has in the input box where it is. So a message no longer waits for a desktop draft to be
//! sent or cleared, and nothing is pasted into a box someone is typing in. When it may be offered
//! is `takes_now`.
//!
//! Delivery is take-or-retract, as for answers (mod_asks.rs), with one more step: the poll hands
//! the offer out, and the mod must ack it before it submits. The ack is refused for an offer that
//! was retracted, and `deliver` reports success only for an acked one. So the prompt is submitted
//! exactly when `deliver` said the mod has it, even if a poll's reply was lost on the way.
//!
//! Whether it then RAN is decided by identity, never by a clock or a coincidence: Claude drops
//! submits after an Esc interrupt (`deliver`). An acked prompt's outcome is its mod's `started`
//! report for that offer's id, read against the runs the mod relayed since the ack, each matched
//! to an offer by its text (`prompt_submitted`): its own run → `Ran`; no run at all → `Dropped`;
//! only runs that aren't its own → `Unconfirmed`, as either it was dropped or Claude changed its
//! text. Until the report it is `Pending`, settled here when the report comes, however late. What
//! the inbox settles alone goes to its sink (`ModInbox::set_settle`): a drop to be typed, an
//! unconfirmed one to the human. So none is lost silently, and none is ever typed twice.
//!
//! A tab counts as served only while its mod polls: it re-polls within moments of each round,
//! so a tab unpolled for `STALE_AFTER` has no mod listening (it exited, or its module reloaded
//! and hasn't restarted the loop yet), and callers type as before.

use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, OnceLock};
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
/// How long an acked prompt counts as waiting in Claude without a `started` report: longer than
/// any turn it can be queued behind. Only a report lost on a live connection runs this out; a
/// mod that stops polling takes its prompts with it (`poll`).
const UNSTARTED_FOR: Duration = Duration::from_secs(6 * 3600);
/// How long `deliver` waits, after the ack, for the mod's `started` report on a prompt offered
/// between turns, so its caller can learn the outcome in the same call: there it comes within
/// moments, as `$.prompt.submit` resolves at once. This decides only WHEN the caller hears, never
/// WHAT: run out, the answer is `Handover::Pending` (unknown), never delivered, and the report is
/// still settled when it comes (`ModInbox::started`).
const START_WAIT: Duration = Duration::from_secs(3);
/// Outcomes kept for `deliver` to collect; more only if callers stopped collecting them.
const MAX_RESULTS: usize = 32;

/// What the inbox settles alone (`ModInbox::set_settle`).
#[derive(Debug, PartialEq, Clone, Copy)]
pub enum Settled {
    /// Claude dropped it after its `deliver` stopped waiting: nothing ran, so it is to be typed.
    Dropped,
    /// Its report came with prompts run since the ack, none of them identifiably this one: it
    /// may have run or not. Never typed (it could run twice); the human is told.
    Unconfirmed,
}

type Settle = Box<dyn Fn(&str, Settled, String) + Send + Sync>;

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
    /// Its `deliver` waits for the `started` report (offered between turns).
    awaited: bool,
}

/// An acked offer whose turn hasn't started (the mod reports `started`).
struct Unstarted {
    id: String,
    text: String,
    at: Instant,
    /// `Inbox::prompts` as it stood at the ack.
    seen: u64,
    /// A relayed run since the ack was this prompt, by its text (`ModInbox::prompt_submitted`).
    ran: bool,
    /// A `deliver` call still waits for its report and takes the outcome. Otherwise the inbox
    /// settles it alone (`ModInbox::set_settle`).
    awaited: bool,
}

struct Inbox {
    /// Polls parked now (decremented by `PollGuard`); otherwise when the last poll ended.
    polling: usize,
    last_poll: Instant,
    offers: VecDeque<Offer>,
    /// Acked offers whose turn hasn't started: Claude holds them until it is idle, so a prompt
    /// typed now would overtake them.
    unstarted: Vec<Unstarted>,
    /// `UserPromptSubmit` events the tab's mod has relayed: the proof that a prompt ran.
    prompts: u64,
    /// Started offers a `deliver` awaits and their outcome, for it to collect.
    results: VecDeque<(String, Handover)>,
    /// Claude dropped a submitted prompt, and drops every one until a prompt is typed (the next
    /// `UserPromptSubmit`): until then prompts are typed, or held for a draft, not offered.
    dropping: bool,
    wake: Arc<Notify>,
    /// Woken on every ack and `started` report, for the `deliver` calls waiting on them.
    reports: Arc<Notify>,
}

impl Inbox {
    fn new(now: Instant) -> Self {
        Inbox {
            polling: 0,
            last_poll: now,
            offers: VecDeque::new(),
            unstarted: Vec::new(),
            prompts: 0,
            results: VecDeque::new(),
            dropping: false,
            wake: Arc::new(Notify::new()),
            reports: Arc::new(Notify::new()),
        }
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

/// What became of a prompt offered to a tab's mod (`ModInbox::deliver`).
#[derive(Debug, PartialEq, Clone, Copy)]
pub enum Handover {
    /// No mod acked it, and it was retracted: nothing was submitted. The caller types it.
    NotTaken,
    /// The mod reported its submit, and Claude ran it.
    Ran,
    /// The mod reported its submit, and Claude dropped it without running it (after an Esc
    /// interrupt): nothing ran. The caller types it.
    Dropped,
    /// The mod has it and is submitting it, and hasn't reported yet: it may run, or be dropped.
    /// Unknown, not delivered — but the caller must NOT type it, or it could run twice. The
    /// inbox settles it when the report comes: a drop is handed back to be typed.
    Pending,
    /// The mod reported its submit, and prompts ran since the ack, but none was identifiably
    /// this one (`Settled::Unconfirmed`): it may have run or been dropped. Not delivered, and
    /// not to be typed; the inbox has told the human.
    Unconfirmed,
}

#[derive(Default)]
pub struct ModInbox {
    tabs: Mutex<HashMap<String, Inbox>>,
    seq: std::sync::atomic::AtomicU64,
    settle: OnceLock<Settle>,
}

/// A `deliver` call's claim on its offer. Dropped with the call — returned, or cancelled (an
/// axum request whose client went away): a claim still armed then gives the offer up, retracting
/// it if unacked, and leaving its `started` report to the inbox if acked.
struct Claim<'a> {
    inbox: &'a ModInbox,
    tab_id: &'a str,
    id: &'a str,
    text: &'a str,
    armed: bool,
}

impl Drop for Claim<'_> {
    fn drop(&mut self) {
        // Its report came in just as the call went away: dropped, and nobody to type it. (An
        // unconfirmed one was surfaced by `started` already.)
        if self.armed && self.inbox.abandon(self.tab_id, self.id) == Some(Handover::Dropped) {
            self.inbox.settle_alone(self.tab_id, self.id, Settled::Dropped, self.text.to_string());
        }
    }
}

/// Waits until `check` answers or `reports` fires `wait` out. `check` runs first, and again on
/// every wakeup; the waiter is registered before each check, so a report between the two isn't
/// missed. `None` when `wait` ran out first: the caller then decides under its own lock.
async fn wait_for<T>(reports: &Notify, wait: Duration, mut check: impl FnMut() -> Option<T>) -> Option<T> {
    let deadline = tokio::time::Instant::now() + wait;
    loop {
        let notified = reports.notified();
        tokio::pin!(notified);
        notified.as_mut().enable();
        if let Some(v) = check() {
            return Some(v);
        }
        // Checked here rather than left to the timer, which may fire a deadline already past
        // only on its next tick.
        if tokio::time::Instant::now() >= deadline || tokio::time::timeout_at(deadline, notified).await.is_err() {
            return None;
        }
    }
}

impl ModInbox {
    /// Where a prompt goes that the inbox settles alone (`Settled`): a drop reported after its
    /// `deliver` stopped waiting, to be typed like any held message; an unconfirmed one, to the
    /// human. Set once, at startup.
    pub fn set_settle(&self, f: impl Fn(&str, Settled, String) + Send + Sync + 'static) {
        let _ = self.settle.set(Box::new(f));
    }

    fn settle_alone(&self, tab_id: &str, id: &str, how: Settled, text: String) {
        let Some(f) = self.settle.get() else {
            log::error!("[inbox] prompt {id} the mod submitted for tab {tab_id} settled {how:?} with nothing to take it: lost");
            return;
        };
        match how {
            Settled::Dropped => log::warn!("[inbox] Claude dropped prompt {id} the mod submitted for tab {tab_id}, after its sender stopped waiting — handed back to be typed"),
            Settled::Unconfirmed => log::warn!("[inbox] prompt {id} the mod submitted for tab {tab_id} is unconfirmed: prompts ran since its ack, none identifiably it (dropped, or its text changed on the way) — not retyped, the human is told"),
        }
        f(tab_id, how, text);
    }

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
            // A mod polling again after a lapse is a new one (the agent restarted, the module
            // reloaded): prompts the old one submitted went with its process, or are no longer
            // its to report.
            // Not handed back: whether they ran is unknown, and typed again they could run twice.
            if !inbox.is_live(now) {
                for u in inbox.unstarted.drain(..) {
                    log::warn!("[inbox] prompt {} the mod submitted for tab {tab_id} was never reported started before its mod went away: it may not have run", u.id);
                }
                inbox.dropping = false;
            }
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
        let Some(inbox) = tabs.get_mut(tab_id) else { return false };
        let Some(o) = inbox.offers.iter_mut().find(|o| o.id == id) else { return false };
        if o.stage != Stage::Handed {
            return false;
        }
        o.stage = Stage::Acked;
        let (text, awaited) = (o.text.clone(), o.awaited);
        let seen = inbox.prompts;
        inbox.unstarted.push(Unstarted { id: id.to_string(), text, at: Instant::now(), seen, ran: false, awaited });
        inbox.reports.notify_waiters();
        true
    }

    /// The mod's `$.prompt.submit` of acked offer `id` resolved (or failed): it no longer waits
    /// in Claude ahead of anything. Whether its prompt RAN is whether one of the runs the mod
    /// relayed since the ack was this prompt (`Unstarted::ran`, `prompt_submitted`): the mod
    /// relays each `UserPromptSubmit`, and waits for maiTerm to take it, before Claude lets the
    /// prompt start, so a prompt's own run always arrives before its report.
    ///   * its own run → `Ran`;
    ///   * no run at all since the ack → `Dropped`: certain, whatever the texts;
    ///   * runs, none its own → `Unconfirmed`. Either Claude dropped it while another prompt ran
    ///     (a human's, typed meanwhile), or it ran with a text that doesn't match the one sent (a
    ///     plugin rewrote it). Neither is guessed: retyped, the second would run twice; counted
    ///     delivered, the first would be lost without a word. So the human is told.
    ///
    /// Matched by id, whenever it comes: to the `deliver` still waiting for it, else settled
    /// here (`set_settle`). A report for an id not waiting (unknown, already reported, or its mod
    /// gone since) is ignored.
    pub fn started(&self, tab_id: &str, id: &str) {
        let alone = {
            let mut tabs = self.tabs.lock();
            let Some(inbox) = tabs.get_mut(tab_id) else { return };
            let Some(pos) = inbox.unstarted.iter().position(|u| u.id == id) else { return };
            let u = inbox.unstarted.remove(pos);
            let outcome = if u.ran {
                Handover::Ran
            } else if inbox.prompts == u.seen {
                // Not offered again until a prompt runs: the prompt is typed instead, or held for
                // a draft in the box, which then holds it as for any tab. (Not for an unconfirmed
                // one: a prompt has run since, which re-arms Claude's submits.)
                inbox.dropping = true;
                log::warn!("[inbox] Claude dropped the prompt the mod submitted for tab {tab_id} (no UserPromptSubmit; after an interrupt it drops them until a prompt is typed) — prompts are typed until one runs");
                Handover::Dropped
            } else {
                Handover::Unconfirmed
            };
            if u.awaited {
                inbox.results.push_back((u.id.clone(), outcome));
                while inbox.results.len() > MAX_RESULTS {
                    inbox.results.pop_front();
                }
                inbox.reports.notify_waiters();
            }
            match outcome {
                Handover::Ran => {
                    log::info!("[inbox] prompt {id} the mod submitted for tab {tab_id} ran");
                    None
                }
                // Its waiting caller types it.
                Handover::Dropped if u.awaited => None,
                Handover::Dropped => Some((u, Settled::Dropped)),
                // Told whether or not a caller waits: the caller can only not type it.
                _ => Some((u, Settled::Unconfirmed)),
            }
        };
        if let Some((u, how)) = alone {
            self.settle_alone(tab_id, &u.id, how, u.text);
        }
    }

    /// `deliver` stops waiting on offer `id`: retracted if not acked yet; its outcome if its
    /// report is already in; else left for `started` to settle alone (`None`).
    fn abandon(&self, tab_id: &str, id: &str) -> Option<Handover> {
        let mut tabs = self.tabs.lock();
        let inbox = tabs.get_mut(tab_id)?;
        inbox.offers.retain(|o| o.id != id);
        if let Some(pos) = inbox.results.iter().position(|(i, _)| i == id) {
            return inbox.results.remove(pos).map(|(_, outcome)| outcome);
        }
        if let Some(u) = inbox.unstarted.iter_mut().find(|u| u.id == id) {
            u.awaited = false;
        }
        None
    }

    /// The tab's mod relayed a `UserPromptSubmit`: a prompt ran, typed or submitted, with this
    /// text (the event's `prompt`). It is the run of the oldest acked prompt not yet run whose
    /// text is the same (`same_text`): the mod's submit can't carry an id, and this is the one
    /// thing the event says of which prompt it was. Two acked with one text are told apart by
    /// order, which counts right whichever ran: each run is one prompt's. (A human typing the
    /// very same words meanwhile counts as it, which is the same message arriving.)
    pub fn prompt_submitted(&self, tab_id: &str, prompt: Option<&str>) {
        if let Some(inbox) = self.tabs.lock().get_mut(tab_id) {
            inbox.prompts += 1;
            inbox.dropping = false;
            let Some(prompt) = prompt else { return };
            let ran = same_text(prompt);
            if let Some(u) = inbox.unstarted.iter_mut().find(|u| !u.ran && same_text(&u.text) == ran) {
                u.ran = true;
            }
        }
    }

    /// Claude is dropping this tab's submitted prompts (`Inbox::dropping`).
    pub fn dropping(&self, tab_id: &str) -> bool {
        self.tabs.lock().get(tab_id).is_some_and(|i| i.dropping)
    }

    /// A prompt the mod submitted for this tab is still waiting in Claude for its turn.
    pub fn has_unstarted(&self, tab_id: &str) -> bool {
        let now = Instant::now();
        let mut tabs = self.tabs.lock();
        let Some(inbox) = tabs.get_mut(tab_id) else { return false };
        // A report lost on the way only keeps prompts on the mod a while longer.
        // Not handed back, as for a mod gone (`poll`).
        inbox.unstarted.retain(|u| {
            let keep = now.duration_since(u.at) < UNSTARTED_FOR;
            if !keep {
                log::warn!("[inbox] prompt {} the mod submitted for tab {tab_id} was never reported started: it may not have run", u.id);
            }
            keep
        });
        inbox.is_live(now) && !inbox.unstarted.is_empty()
    }

    /// Offers `text` to the tab's mod and waits for its ack: `NotTaken` when no mod is polling or
    /// none acked in time, and then the offer is retracted, so it can never be submitted after
    /// this returned. Acked, the mod submits it, and the outcome is its `started` report.
    ///
    /// `between_turns` (the agent was idle when offered): also wait for that report, and answer
    /// `Dropped` when Claude dropped the prompt, so the caller types it instead. Claude Code
    /// (2.1.316, reproduced 2026-10-31) drops every `$.prompt.submit` after a turn is interrupted
    /// with Esc, resolving it as if it ran, until someone TYPES a prompt: so after the phone's
    /// Stop every message went to the mod and vanished. Typing it re-arms Claude. Between turns
    /// the report comes within moments; past `START_WAIT` (a turn began meanwhile and the prompt
    /// waits behind it) the answer is `Pending`, and the inbox settles it when the report comes.
    ///
    /// Mid-turn a submit waits for the turn to end, so the answer is `Pending` at the ack.
    pub async fn deliver(&self, tab_id: &str, text: &str, between_turns: bool) -> Handover {
        self.deliver_within(tab_id, text, between_turns, DELIVERY_TIMEOUT, START_WAIT).await
    }

    /// `deliver` with its two deadlines. Each only ends a wait that is taking too long: the ack's
    /// in `NotTaken` (retracted, so certainly not submitted), the report's in `Pending` (unknown).
    /// Neither is ever read as delivered.
    async fn deliver_within(
        &self,
        tab_id: &str,
        text: &str,
        between_turns: bool,
        ack_wait: Duration,
        start_wait: Duration,
    ) -> Handover {
        let id = format!("i{}", self.seq.fetch_add(1, std::sync::atomic::Ordering::Relaxed));
        let reports = {
            let now = Instant::now();
            let mut tabs = self.tabs.lock();
            let Some(inbox) = tabs.get_mut(tab_id).filter(|i| i.is_live(now)) else { return Handover::NotTaken };
            inbox.offers.push_back(Offer {
                id: id.clone(),
                text: text.to_string(),
                stage: Stage::Queued,
                awaited: between_turns,
            });
            inbox.wake.notify_one();
            inbox.reports.clone()
        };
        let mut claim = Claim { inbox: self, tab_id, id: &id, text, armed: true };

        // The ack, and the offer off the queue: `Some(true)` acked, `Some(false)` gone (retracted
        // with `retract`), `None` not yet.
        let acked = |retract: bool| -> Option<bool> {
            let mut tabs = self.tabs.lock();
            let Some(inbox) = tabs.get_mut(tab_id) else { return Some(false) };
            let Some(pos) = inbox.offers.iter().position(|o| o.id == id) else { return Some(false) };
            if inbox.offers[pos].stage == Stage::Acked {
                inbox.offers.remove(pos);
                return Some(true);
            }
            if retract {
                inbox.offers.remove(pos);
                return Some(false);
            }
            None
        };
        // Out of time, the ack still counts if it is in by the time the lock is held.
        let acked = match wait_for(&reports, ack_wait, || acked(false)).await {
            Some(a) => a,
            None => acked(true).unwrap_or(false),
        };
        if !acked {
            claim.armed = false;
            return Handover::NotTaken;
        }
        if !between_turns {
            claim.armed = false;
            return Handover::Pending;
        }

        let collect = || -> Option<Handover> {
            let mut tabs = self.tabs.lock();
            let inbox = tabs.get_mut(tab_id)?;
            let pos = inbox.results.iter().position(|(i, _)| *i == id)?;
            inbox.results.remove(pos).map(|(_, outcome)| outcome)
        };
        // Out of time, a report that came in meanwhile is still this call's; otherwise the inbox
        // takes the prompt over, all under one lock, so a report is settled exactly once.
        let outcome = match wait_for(&reports, start_wait, collect).await {
            Some(outcome) => Some(outcome),
            None => self.abandon(tab_id, &id),
        };
        claim.armed = false;
        outcome.unwrap_or_else(|| {
            log::info!("[inbox] prompt {id} the mod submitted for tab {tab_id} has no start report yet (a turn began first?) — pending; settled when it comes");
            Handover::Pending
        })
    }
}

/// A prompt's text as compared with what a relayed run says it was: line endings and runs of
/// whitespace don't count (Claude may trim the ends, or turn a CRLF into an LF), words do.
fn same_text(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// The oldest offer still waiting for a poll, handed out. One handed out but never acked (its
/// reply lost) stays where it is until `deliver` retracts it.
fn hand_out(inbox: &mut Inbox) -> Option<Polled> {
    let o = inbox.offers.iter_mut().find(|o| o.stage == Stage::Queued)?;
    o.stage = Stage::Handed;
    Some(Polled::Offer { id: o.id.clone(), text: o.text.clone() })
}

/// Whether a prompt for this tab should go to its mod now rather than be typed: a mod is polling,
/// and the agent is between turns. There a typed prompt and a submitted one do the same, except
/// that only the typed one can land in a box the human is typing in, and the submitted one leaves
/// their draft alone — so a draft no longer holds anything at an idle agent.
///
/// Not mid-turn. Claude folds a prompt typed mid-turn into the running turn (phone steering relies
/// on that), while a submitted one waits inside Claude for the turn to end: out of maiTerm's
/// sight, already reported delivered, and run at once if the human presses Esc to stop the agent
/// (tested), right after they stopped it. So mid-turn the callers' rules stand — typed into an
/// empty box, held over a draft until the turn ends, when the mod takes it.
///
/// Except while a prompt the mod submitted is still waiting for its turn (`has_unstarted`, the
/// race of a turn starting just as one was offered): then everything follows it through the mod,
/// or a typed prompt would overtake it.
///
/// Only for text that may be offered at all (`offerable`).
///
/// Never while Claude is dropping the tab's submitted prompts (`ModInbox::dropping`): an offer
/// would be acked and lost, and a message held for a draft re-offered every second to the same
/// end. Then the tab is served as one without a mod.
pub fn takes_now(app: &AppState, tab_id: &str) -> bool {
    app.mod_inbox.is_live(tab_id)
        && !app.mod_inbox.dropping(tab_id)
        && (agent_between_turns(app, tab_id) || app.mod_inbox.has_unstarted(tab_id))
}

/// Whether `text` can go to a mod at all. Not a slash command: a submitted prompt is the model's
/// to read, so `/compact` or `/model` would reach it as words instead of running; those are
/// typed. Nor a `!` command: bash mode exists only for a `!` typed into the box, and submitted,
/// the command reaches the model as words and never runs. Not empty either: typed, an empty
/// prompt is a bare Enter.
pub fn offerable(text: &str) -> bool {
    let t = text.trim_start();
    !t.is_empty() && !t.starts_with('/') && !t.starts_with('!')
}

/// The tab's agent is idle: not mid-turn and not at a question.
pub(crate) fn agent_between_turns(app: &AppState, tab_id: &str) -> bool {
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
            tokio::spawn(async move { inbox.deliver("tab", "hello", false).await })
        };
        let Polled::Offer { id, text } = poll.await.unwrap() else { panic!("the poll should get the offer") };
        assert_eq!(text, "hello");
        assert!(inbox.ack("tab", &id));
        // Mid-turn the submit waits for the turn to end: the mod has it, the outcome is to come.
        assert_eq!(delivering.await.unwrap(), Handover::Pending);
        // Gone once reported: a second ack is refused.
        assert!(!inbox.ack("tab", &id));
    }

    #[tokio::test]
    async fn an_acked_prompt_counts_as_waiting_in_claude_until_its_turn_starts() {
        let inbox = Arc::new(ModInbox::default());
        let poll = parked_poll(&inbox, "tab").await;
        let delivering = {
            let inbox = inbox.clone();
            tokio::spawn(async move { inbox.deliver("tab", "hello", false).await })
        };
        let Polled::Offer { id, .. } = poll.await.unwrap() else { panic!("the poll should get the offer") };
        assert!(!inbox.has_unstarted("tab"));
        assert!(inbox.ack("tab", &id));
        assert_eq!(delivering.await.unwrap(), Handover::Pending);
        assert!(inbox.has_unstarted("tab"), "a later prompt must follow it through the mod");
        inbox.started("tab", &id);
        assert!(!inbox.has_unstarted("tab"));
    }

    /// Between turns the delivery waits for the submit: a `UserPromptSubmit` relayed since the
    /// ack is the proof the prompt ran.
    #[tokio::test]
    async fn a_prompt_claude_ran_is_delivered() {
        let inbox = Arc::new(ModInbox::default());
        let poll = parked_poll(&inbox, "tab").await;
        let delivering = {
            let inbox = inbox.clone();
            tokio::spawn(async move { inbox.deliver("tab", "hello", true).await })
        };
        let Polled::Offer { id, .. } = poll.await.unwrap() else { panic!("the poll should get the offer") };
        assert!(inbox.ack("tab", &id));
        inbox.prompt_submitted("tab", Some("hello"));
        inbox.started("tab", &id);
        assert_eq!(delivering.await.unwrap(), Handover::Ran);
    }

    /// After an Esc interrupt Claude resolves the submit without running it: no
    /// `UserPromptSubmit`. The delivery fails, so the caller types the prompt.
    #[tokio::test]
    async fn a_prompt_claude_dropped_is_not_delivered() {
        let inbox = Arc::new(ModInbox::default());
        // A prompt that ran before the ack doesn't count for this one.
        let _ = inbox.poll("tab", Duration::ZERO).await;
        inbox.prompt_submitted("tab", Some("hello"));
        let poll = parked_poll(&inbox, "tab").await;
        let delivering = {
            let inbox = inbox.clone();
            tokio::spawn(async move { inbox.deliver("tab", "hello", true).await })
        };
        let Polled::Offer { id, .. } = poll.await.unwrap() else { panic!("the poll should get the offer") };
        assert!(inbox.ack("tab", &id));
        inbox.started("tab", &id);
        assert_eq!(delivering.await.unwrap(), Handover::Dropped);
        assert!(!inbox.has_unstarted("tab"), "a dropped prompt must not pull later ones onto the mod");
        assert!(inbox.dropping("tab"), "nothing is offered until a prompt runs");
        // The prompt typed in its place runs, and re-arms Claude's submits.
        inbox.prompt_submitted("tab", Some("hello"));
        assert!(!inbox.dropping("tab"));
    }

    #[tokio::test]
    async fn a_mod_polling_again_after_a_lapse_starts_with_nothing_waiting() {
        let inbox = Arc::new(ModInbox::default());
        let poll = parked_poll(&inbox, "tab").await;
        let delivering = {
            let inbox = inbox.clone();
            tokio::spawn(async move { inbox.deliver("tab", "hello", false).await })
        };
        let Polled::Offer { id, .. } = poll.await.unwrap() else { panic!("the poll should get the offer") };
        assert!(inbox.ack("tab", &id));
        assert_eq!(delivering.await.unwrap(), Handover::Pending);
        // The agent exits with the prompt still queued; no `started` ever comes.
        inbox.tabs.lock().get_mut("tab").unwrap().last_poll = Instant::now() - STALE_AFTER;
        assert!(!inbox.has_unstarted("tab"));
        // Its replacement polls: mid-turn prompts must not be routed to it on the old one's account.
        let _ = inbox.poll("tab", Duration::from_millis(1)).await;
        assert!(!inbox.has_unstarted("tab"));
    }

    #[test]
    fn slash_and_bang_commands_and_empty_text_are_typed() {
        for typed in ["/compact", "  /model opus", "!ls", " ! cd ~/x && npm run deploy", "", "   "] {
            assert!(!offerable(typed), "{typed:?} must be typed, never submitted");
        }
        for offered in ["train", "what does a/b mean", "great!"] {
            assert!(offerable(offered), "{offered:?} may go to the mod");
        }
    }

    #[tokio::test]
    async fn nothing_is_offered_to_a_tab_no_mod_polls() {
        let inbox = ModInbox::default();
        assert!(!inbox.is_live("tab"));
        assert_eq!(inbox.deliver("tab", "hello", false).await, Handover::NotTaken);
    }

    #[tokio::test]
    async fn an_offer_handed_out_but_never_acked_is_retracted_and_its_late_ack_refused() {
        let inbox = Arc::new(ModInbox::default());
        let poll = parked_poll(&inbox, "tab").await;
        let started = Instant::now();
        let delivering = {
            let inbox = inbox.clone();
            tokio::spawn(async move { inbox.deliver("tab", "hello", false).await })
        };
        let Polled::Offer { id, .. } = poll.await.unwrap() else { panic!("the poll should get the offer") };
        // The reply was lost on its way to the mod: no ack comes.
        assert_eq!(delivering.await.unwrap(), Handover::NotTaken);
        assert!(started.elapsed() >= DELIVERY_TIMEOUT);
        assert!(!inbox.ack("tab", &id), "a retracted offer must never be submitted");
    }

    #[tokio::test]
    async fn offers_are_handed_out_oldest_first_one_per_poll() {
        let inbox = Arc::new(ModInbox::default());
        let _ = inbox.poll("tab", Duration::from_millis(1)).await; // the mod is listening
        let a = {
            let inbox = inbox.clone();
            tokio::spawn(async move { inbox.deliver("tab", "first", false).await })
        };
        tokio::time::sleep(Duration::from_millis(10)).await;
        let b = {
            let inbox = inbox.clone();
            tokio::spawn(async move { inbox.deliver("tab", "second", false).await })
        };
        tokio::time::sleep(Duration::from_millis(10)).await;
        for want in ["first", "second"] {
            let Polled::Offer { id, text } = inbox.poll("tab", Duration::from_millis(1)).await else { panic!("expected an offer") };
            assert_eq!(text, want);
            assert!(inbox.ack("tab", &id));
        }
        assert_eq!(a.await.unwrap(), Handover::Pending);
        assert_eq!(b.await.unwrap(), Handover::Pending);
        assert_eq!(inbox.poll("tab", Duration::from_millis(1)).await, Polled::Empty);
    }

    // The tests below step a delivery by hand (`now_or_never`) and send the mod's reports
    // themselves, so each order is forced rather than raced, and none of them waits on a clock.
    // `NEVER` only bounds a wait the test always ends with a report first.
    const NEVER: Duration = Duration::from_secs(3600);

    type Delivering<'a> = std::pin::Pin<Box<dyn std::future::Future<Output = Handover> + 'a>>;

    type Settles = Arc<Mutex<Vec<(String, String)>>>;

    /// A live tab whose settled prompts are recorded rather than typed or shown: (dropped, to be
    /// typed; unconfirmed, put to the human).
    async fn listening_inbox() -> (Arc<ModInbox>, Settles, Settles) {
        let inbox = Arc::new(ModInbox::default());
        let (handed_back, told) = (Settles::default(), Settles::default());
        let (dropped, unconfirmed) = (handed_back.clone(), told.clone());
        inbox.set_settle(move |tab, how, text| {
            let sink = if how == Settled::Dropped { &dropped } else { &unconfirmed };
            sink.lock().push((tab.to_string(), text));
        });
        let _ = inbox.poll("tab", Duration::ZERO).await; // the mod is listening
        (inbox, handed_back, told)
    }

    /// Offers `text` between turns, and the mod takes and acks it. `start_wait` ZERO: the
    /// delivery stops waiting for the report at once, as the old 3 s did once run out.
    async fn acked<'a>(inbox: &'a ModInbox, text: &'a str, start_wait: Duration) -> (Delivering<'a>, String) {
        use futures_util::FutureExt;
        let mut delivering: Delivering<'a> = Box::pin(inbox.deliver_within("tab", text, true, NEVER, start_wait));
        assert!(delivering.as_mut().now_or_never().is_none(), "offered, waiting for the ack");
        let Polled::Offer { id, .. } = inbox.poll("tab", Duration::ZERO).await else { panic!("the poll should get the offer") };
        assert!(inbox.ack("tab", &id));
        (delivering, id)
    }

    /// The bug: past the 3 s start wait `deliver` answered true, so a drop reported later was
    /// lost. Now the delivery is `Pending`, and the late drop is handed back to be typed, once.
    #[tokio::test]
    async fn a_drop_reported_after_the_start_wait_is_handed_back_not_counted_delivered() {
        use futures_util::FutureExt;
        let (inbox, handed_back, told) = listening_inbox().await;
        let (mut delivering, id) = acked(&inbox, "hello", Duration::ZERO).await;
        assert_eq!(delivering.as_mut().now_or_never(), Some(Handover::Pending), "no report is not delivered");
        assert!(inbox.has_unstarted("tab"), "until its report, later prompts follow it through the mod");
        assert!(handed_back.lock().is_empty());

        inbox.started("tab", &id); // no UserPromptSubmit since the ack: Claude dropped it
        assert_eq!(*handed_back.lock(), vec![("tab".to_string(), "hello".to_string())]);
        assert!(inbox.dropping("tab"), "nothing is offered until a prompt runs");
        assert!(!inbox.has_unstarted("tab"));

        // A repeated report settles nothing twice: the prompt is typed once.
        inbox.started("tab", &id);
        assert_eq!(handed_back.lock().len(), 1);
        assert!(told.lock().is_empty());
    }

    #[tokio::test]
    async fn a_run_reported_after_the_start_wait_is_settled_with_nothing_typed() {
        use futures_util::FutureExt;
        let (inbox, handed_back, told) = listening_inbox().await;
        let (mut delivering, id) = acked(&inbox, "hello", Duration::ZERO).await;
        assert_eq!(delivering.as_mut().now_or_never(), Some(Handover::Pending));
        inbox.prompt_submitted("tab", Some("hello"));
        inbox.started("tab", &id);
        assert!(handed_back.lock().is_empty(), "a prompt that ran is never typed again");
        assert!(told.lock().is_empty());
        assert!(!inbox.dropping("tab"));
        assert!(!inbox.has_unstarted("tab"));
    }

    /// The report can land before the delivery even sees its ack: still that delivery's outcome.
    #[tokio::test]
    async fn a_report_before_the_delivery_looks_is_still_its_outcome() {
        use futures_util::FutureExt;
        let (inbox, handed_back, told) = listening_inbox().await;
        let (mut delivering, id) = acked(&inbox, "hello", NEVER).await;
        inbox.started("tab", &id);
        assert_eq!(delivering.as_mut().now_or_never(), Some(Handover::Dropped), "the caller types it");
        assert!(handed_back.lock().is_empty(), "the caller types it, so it isn't handed back too");

        let (mut delivering, id) = acked(&inbox, "again", NEVER).await;
        inbox.prompt_submitted("tab", Some("again"));
        inbox.started("tab", &id);
        assert_eq!(delivering.as_mut().now_or_never(), Some(Handover::Ran));
        assert!(told.lock().is_empty());
    }

    /// Two prompts the mod holds, reported in the other order, each matched by its own id.
    #[tokio::test]
    async fn reports_out_of_order_are_matched_by_id() {
        use futures_util::FutureExt;
        let (inbox, handed_back, told) = listening_inbox().await;
        let (mut first, first_id) = acked(&inbox, "first", Duration::ZERO).await;
        assert_eq!(first.as_mut().now_or_never(), Some(Handover::Pending));
        let (mut second, second_id) = acked(&inbox, "second", NEVER).await;
        assert!(second.as_mut().now_or_never().is_none(), "waiting for its own report");

        inbox.started("tab", &second_id);
        assert_eq!(second.as_mut().now_or_never(), Some(Handover::Dropped));
        assert!(handed_back.lock().is_empty(), "the first one's report isn't in yet");
        inbox.started("tab", &first_id);
        assert_eq!(*handed_back.lock(), vec![("tab".to_string(), "first".to_string())]);
        assert!(told.lock().is_empty());
    }

    /// A caller that goes away mid-wait (an HTTP request cancelled) leaves its prompt to the
    /// inbox: unacked, retracted; acked, settled by its report whenever that comes.
    #[tokio::test]
    async fn a_delivery_cancelled_mid_wait_leaves_its_prompt_settled() {
        use futures_util::FutureExt;
        let (inbox, handed_back, told) = listening_inbox().await;

        let mut delivering = Box::pin(inbox.deliver_within("tab", "unacked", true, NEVER, NEVER));
        assert!(delivering.as_mut().now_or_never().is_none());
        let Polled::Offer { id, .. } = inbox.poll("tab", Duration::ZERO).await else { panic!("the poll should get the offer") };
        drop(delivering);
        assert!(!inbox.ack("tab", &id), "a retracted offer must never be submitted");

        let (delivering, id) = acked(&inbox, "report later", NEVER).await;
        drop(delivering);
        inbox.started("tab", &id);
        assert_eq!(*handed_back.lock(), vec![("tab".to_string(), "report later".to_string())]);

        // Reported dropped while the caller is still there, then it goes before collecting it.
        let (mut delivering, id) = acked(&inbox, "report first", NEVER).await;
        assert!(delivering.as_mut().now_or_never().is_none());
        inbox.started("tab", &id);
        drop(delivering);
        assert_eq!(handed_back.lock().len(), 2);
        assert_eq!(handed_back.lock()[1].1, "report first");
        assert!(told.lock().is_empty());
    }

    /// A mod gone before reporting takes the prompt's fate with it: maybe run, so never retyped.
    #[tokio::test]
    async fn a_pending_prompt_whose_mod_went_away_is_not_typed_again() {
        use futures_util::FutureExt;
        let (inbox, handed_back, told) = listening_inbox().await;
        let (mut delivering, id) = acked(&inbox, "hello", Duration::ZERO).await;
        assert_eq!(delivering.as_mut().now_or_never(), Some(Handover::Pending));
        inbox.tabs.lock().get_mut("tab").unwrap().last_poll = Instant::now() - STALE_AFTER;
        let _ = inbox.poll("tab", Duration::ZERO).await; // its replacement
        inbox.started("tab", &id); // a stray report from the old one
        assert!(handed_back.lock().is_empty());
        assert!(!inbox.has_unstarted("tab"));
        assert!(told.lock().is_empty());
    }

    /// The bug the counter had: a prompt the human typed meanwhile made a drop read as a run,
    /// and the prompt was lost. Not identifiably run, it is unconfirmed: never typed again (it
    /// may have run), never counted delivered, and put to the human.
    #[tokio::test]
    async fn a_drop_reported_after_another_prompt_ran_is_unconfirmed_not_ran() {
        use futures_util::FutureExt;
        let (inbox, handed_back, told) = listening_inbox().await;
        let (mut delivering, id) = acked(&inbox, "hello", NEVER).await;
        inbox.prompt_submitted("tab", Some("the human's own")); // interleaved
        inbox.started("tab", &id);
        assert_eq!(delivering.as_mut().now_or_never(), Some(Handover::Unconfirmed));
        assert_eq!(*told.lock(), vec![("tab".to_string(), "hello".to_string())]);
        assert!(handed_back.lock().is_empty(), "it may have run: never typed again");
        assert!(!inbox.dropping("tab"), "the human's prompt ran, which re-arms Claude's submits");

        // The same, reported after its delivery stopped waiting.
        let (mut delivering, id) = acked(&inbox, "later", Duration::ZERO).await;
        assert_eq!(delivering.as_mut().now_or_never(), Some(Handover::Pending));
        inbox.prompt_submitted("tab", Some("the human's next"));
        inbox.started("tab", &id);
        assert_eq!(told.lock().len(), 2);
        assert_eq!(told.lock()[1].1, "later");
        assert!(handed_back.lock().is_empty());
    }

    /// Another prompt running beside it doesn't hide its own run.
    #[tokio::test]
    async fn an_interleaved_prompt_beside_its_own_run_still_reads_ran() {
        use futures_util::FutureExt;
        let (inbox, handed_back, told) = listening_inbox().await;
        let (mut delivering, id) = acked(&inbox, "hello\r\nworld", NEVER).await;
        inbox.prompt_submitted("tab", Some("the human's own"));
        // Claude's copy of the text: line endings and the ends trimmed don't make it another.
        inbox.prompt_submitted("tab", Some("  hello\nworld \n"));
        inbox.started("tab", &id);
        assert_eq!(delivering.as_mut().now_or_never(), Some(Handover::Ran));
        assert!(handed_back.lock().is_empty() && told.lock().is_empty());
    }

    /// Two prompts with one text: each run is one prompt's, oldest first, so the count is right
    /// whichever of them ran — and one run for two is never read as both.
    #[tokio::test]
    async fn the_same_text_sent_twice_counts_each_run_once() {
        use futures_util::FutureExt;
        let (inbox, handed_back, told) = listening_inbox().await;
        let (mut first, first_id) = acked(&inbox, "same", Duration::ZERO).await;
        assert_eq!(first.as_mut().now_or_never(), Some(Handover::Pending));
        let (mut second, second_id) = acked(&inbox, "same", NEVER).await;
        inbox.prompt_submitted("tab", Some("same"));
        inbox.prompt_submitted("tab", Some("same"));
        inbox.started("tab", &second_id);
        assert_eq!(second.as_mut().now_or_never(), Some(Handover::Ran));
        inbox.started("tab", &first_id);
        assert!(handed_back.lock().is_empty() && told.lock().is_empty(), "both ran");

        // Sent twice, run once: one is Ran, and the other can't be told from a drop beside it.
        let (mut first, first_id) = acked(&inbox, "twice", Duration::ZERO).await;
        assert_eq!(first.as_mut().now_or_never(), Some(Handover::Pending));
        let (mut second, second_id) = acked(&inbox, "twice", NEVER).await;
        inbox.prompt_submitted("tab", Some("twice"));
        inbox.started("tab", &second_id);
        assert_eq!(second.as_mut().now_or_never(), Some(Handover::Unconfirmed));
        inbox.started("tab", &first_id);
        assert_eq!(*told.lock(), vec![("tab".to_string(), "twice".to_string())]);
        assert!(handed_back.lock().is_empty(), "one of them ran: neither is typed again");
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
