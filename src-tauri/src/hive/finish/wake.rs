//! Waking a Done Hive (docs/hive.md §11.4): a message from the human to its coordinator sets the
//! Hive back to `active` and relaunches the coordinator through the launch builder, resuming its
//! session. The message itself is owed to that session's start (`owed.wake`), typed through the
//! delivery gate like the kickoff, so a refusal or a restart mid-wake doesn't lose it (plan D11).
//!
//! Only the human's own send paths call this: the desktop composer (`mailink::send_tab_message`)
//! and, from 9d, the phone's `POST /message`. No agent, mesh message, Overlord directive or
//! follow-up reaches it, and a Done Hive launches nothing until it has run (`launch::DONE_REFUSAL`).

use std::sync::Mutex;
use std::time::{Duration, Instant};

use serde_json::{json, Value};

use super::super::deliver::frame_escape;
use super::super::owed::{self, OwedWake};
use super::super::record::{Hive, STATUS_ACTIVE, STATUS_DONE};
use super::super::rpc::{self, Front, HiveWindow, Outcome, Sent};
use super::super::tools::{blocking, caller_for_tab, coordinator_tab, Caller, Ctx, MAX_MESSAGE};
use super::{current_hive, save_status, RECORD_VERSION};
use crate::state::AppData;

pub const BY_DESKTOP: &str = "desktop";
pub const BY_PHONE: &str = "phone";
/// `logs/finishes.jsonl` lines beside the finishes: the history `done_at` no longer holds.
pub const KIND_WAKE: &str = "wake";
pub const KIND_WAKE_FAILED: &str = "wake_failed";
/// `wake`'s refusal when the Hive isn't done (any more): the caller sends as usual.
pub const NOT_DONE: &str = "not done";

pub const FILES_REFUSAL: &str = "This Hive is done. Send a message first to wake it, then attach files once its coordinator is back.";
pub const FILES_STARTING: &str = "This Hive's coordinator is still starting: attach files once it's back.";
/// While the coordinator's session start is typing the message that woke it: anything sent
/// meanwhile could land ahead of it.
pub const TAKING_REFUSAL: &str = "This Hive's coordinator is taking the message that woke it right now: send this again in a moment.";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WakeOutcome {
    /// The coordinator is being relaunched; the message goes out at its session start.
    Relaunching,
    /// The window was asked to relaunch it and didn't answer: the Hive stays active and the
    /// message owed, since a coordinator that did come up gets it at its start.
    Unconfirmed,
    /// It was still running (inside `exit_coordinator`'s wait) and has the message now.
    Delivered,
}

/// What a human's message to `tab` does.
#[derive(Debug)]
pub(crate) enum Route {
    Send,
    Wake(Box<Caller>),
    Refuse(&'static str),
}

/// The Hive whose coordinator `tab_id` is, whatever its status.
pub(super) fn coordinator_of(data: &AppData, tab_id: &str) -> Option<Caller> {
    // `caller_for_tab` fills in the coordinator's tab, whichever of the Hive's tabs it was given.
    caller_for_tab(data, tab_id).filter(|c| !tab_id.is_empty() && c.tab_id == tab_id)
}

/// The Hive whose Done coordinator `tab_id` is.
pub(crate) fn done_coordinator(data: &AppData, tab_id: &str) -> Option<Caller> {
    coordinator_of(data, tab_id).filter(|c| c.hive.status == STATUS_DONE)
}

/// A message to a Done coordinator wakes its Hive; one with attachments is refused, since a file
/// staged for an agent that isn't there yet would be typed into the shell. So does one to a
/// coordinator a wake is still relaunching: it joins the first message (plan 9c step 4), never the
/// ordinary path, which would type it (or `/maiterm init`) ahead of that message.
pub(crate) fn route_send(data: &AppData, tab_id: &str, files_empty: bool) -> Route {
    route(coordinator_of(data, tab_id), files_empty)
}

/// `route_send` once the tab's coordinator caller, if it is one, is known.
pub(super) fn route(c: Option<Caller>, files_empty: bool) -> Route {
    let Some(c) = c else { return Route::Send };
    let task = c.paths.coordinator_task();
    let waking = c.hive.status == STATUS_DONE || owed::wake_relaunching(&task);
    if c.hive.status != STATUS_DONE && owed::claim_in_flight(&task, owed::KIND_WAKE) {
        Route::Refuse(TAKING_REFUSAL)
    } else if !waking {
        Route::Send
    } else if !files_empty {
        Route::Refuse(if c.hive.status == STATUS_DONE { FILES_REFUSAL } else { FILES_STARTING })
    } else {
        Route::Wake(Box::new(c))
    }
}

/// What the coordinator's next session is told: maiTerm's frame, then the human's words escaped
/// with their line breaks kept, so only maiTerm opens a `[maiTerm]` line.
pub fn wake_text(done_since: Option<i64>, by: &str, text: &str) -> String {
    let since = done_since.map(|ms| format!(", which was Done since {}", super::super::autonomy::local_when(ms))).unwrap_or_else(|| ", which was Done".to_string());
    let from = if by == BY_PHONE { " (from their phone)" } else { "" };
    format!("[maiTerm] Your human has woken this Hive{since}. Their message{from}:\n\n{}", frame_escape(text))
}

/// A command as the column reads one (ChatColumn `neverEchoed`): `/name` then a space or the end.
/// A message that opens with a path is words.
fn is_command(t: &str) -> bool {
    let name = t.strip_prefix('/').and_then(|r| r.split(char::is_whitespace).next()).unwrap_or_default();
    !name.is_empty() && name.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | ':' | '.' | '-'))
}

fn check_text(text: &str) -> Result<String, String> {
    let t = text.trim();
    if t.is_empty() {
        return Err("Write a message to wake this Hive.".to_string());
    }
    if t.chars().count() > MAX_MESSAGE {
        return Err(format!("That message is over {MAX_MESSAGE} characters: put the detail in a file and point to it."));
    }
    // A picker's `/model` or `/effort` would land as words inside maiTerm's frame, not as a command.
    if is_command(t) {
        return Err("This Hive is done: a slash command can't wake it. Send a message first; its coordinator takes commands once it's back.".to_string());
    }
    Ok(t.to_string())
}

/// One of a wake or the coordinator's exit at a time per Hive: the phone and the desktop at once
/// would both relaunch, and an exit's status read and signal must not straddle a wake's status
/// flip and delivery.
static WAKING: Mutex<Vec<String>> = Mutex::new(Vec::new());

/// Longer than anything that holds it: a wake's relaunch request, a delivery, an exit's grace.
const TURN_WAIT: Duration = Duration::from_secs(30);
const TURN_POLL: Duration = Duration::from_millis(50);

pub(super) struct Waking(String);

impl Waking {
    pub(super) fn take(key: &str) -> Option<Self> {
        let mut w = WAKING.lock().unwrap_or_else(|e| e.into_inner());
        if w.iter().any(|k| k == key) {
            return None;
        }
        w.push(key.to_string());
        Some(Self(key.to_string()))
    }

    async fn wait(key: &str, within: Duration) -> Option<Self> {
        let start = Instant::now();
        loop {
            if let Some(w) = Self::take(key) {
                return Some(w);
            }
            if start.elapsed() >= within {
                return None;
            }
            tokio::time::sleep(TURN_POLL).await;
        }
    }
}

impl Drop for Waking {
    fn drop(&mut self) {
        WAKING.lock().unwrap_or_else(|e| e.into_inner()).retain(|k| k != &self.0);
    }
}

/// Hives whose coordinator was signalled and hadn't yet given its terminal back: a wake must not
/// type into the dying agent.
static ENDING: Mutex<Vec<String>> = Mutex::new(Vec::new());

/// `exit_coordinator`, holding the Hive's turn, after the signal went: the turn is kept until the
/// agent has left its terminal (or `grace` ran out, which marks it ending), so a wake that waited
/// for the turn finds a shell and relaunches.
pub(super) async fn hold_until_ended<W: HiveWindow>(ctx: &Ctx<'_, W>, caller: &Caller, tab: &str, grace: Duration) {
    let start = Instant::now();
    while ctx.window.front(tab).await == Front::Job {
        if start.elapsed() >= grace {
            log::warn!("hive {}: the coordinator still holds its terminal after its signal", caller.hive.id);
            let key = caller.paths.root.to_string_lossy().to_string();
            let mut ending = ENDING.lock().unwrap_or_else(|e| e.into_inner());
            if !ending.contains(&key) {
                ending.push(key);
            }
            return;
        }
        tokio::time::sleep(Duration::from_millis(250).min(grace)).await;
    }
}

/// Wakes the caller's Done Hive with the human's `text` (§11.4, plan D10–D12). `by` is
/// `BY_DESKTOP` or `BY_PHONE`. Err(`NOT_DONE`) when the Hive isn't done: send as usual.
pub(crate) async fn wake<W: HiveWindow>(ctx: &Ctx<'_, W>, caller: &Caller, text: &str, by: &str) -> Result<WakeOutcome, String> {
    // By folder, not id: a test's scratch Hives share ids.
    let key = caller.paths.root.to_string_lossy().to_string();
    let _held = Waking::wait(&key, TURN_WAIT).await.ok_or("A wake of this Hive is already under way: send this again in a moment.")?;
    let before = current_hive(ctx, caller)?;
    if before.status != STATUS_DONE {
        // Woken and still relaunching: this joins the message its session start will type.
        if !owed::wake_relaunching(&caller.paths.coordinator_task()) {
            return Err(NOT_DONE.to_string());
        }
        owe(caller, check_text(text)?, by, None, true).await?;
        log::info!("hive {}: another message from the human ({by}) joins the wake", caller.hive.id);
        return Ok(WakeOutcome::Relaunching);
    }
    let text = check_text(text)?;
    let tab = coordinator_tab(ctx.data, caller).ok_or("This Hive has no coordinator tab to wake.")?;
    match ctx.window.front(&tab).await {
        Front::Job if ENDING.lock().unwrap_or_else(|e| e.into_inner()).contains(&key) => {
            return Err("This Hive's coordinator is still being ended: send this again in a moment.".to_string())
        }
        Front::Job => return wake_running(ctx, caller, &before, &tab, &text, by).await,
        Front::Unknown => return Err("maiTerm can't tell what holds the coordinator's terminal, so the Hive wasn't woken. Try again in a moment.".to_string()),
        Front::Shell | Front::NoTerminal => ENDING.lock().unwrap_or_else(|e| e.into_inner()).retain(|k| k != &key),
    }

    // Owed before the flip, so a restart between them still delivers it once the Hive runs.
    let task = caller.paths.coordinator_task();
    let prev = owe(caller, text, by, before.done_at, false).await?;

    // Saved, not yet announced: if the relaunch fails, the window never hears `active` and the
    // human gets no second "Hive done" notice when it goes back.
    let change = match save_status(ctx, caller, &[STATUS_DONE], Some(STATUS_ACTIVE), |h| h.done_at = None) {
        Ok(c) => c,
        Err(e) => {
            restore_wake(caller, prev).await;
            return Err(e);
        }
    };
    log_line(caller, KIND_WAKE, by, None).await;
    // Before the request: the session can start before the window answers.
    owed::mark_wake_relaunch(&task);
    // The status flipped first: the builder refuses every launch in a Done Hive.
    let outcome = ctx.window.request(rpc::VERB_RELAUNCH, json!({ "workspace_id": caller.workspace_id, "tab_id": tab })).await;
    // A window that was asked and didn't answer may still have typed the line: rolling back under
    // a coordinator that comes up would leave an agent in a Done Hive and its message dropped.
    let unanswered = matches!(outcome, Outcome::Timeout | Outcome::Disconnected);
    match rpc::applied(outcome) {
        Ok(_) => {
            ctx.window.hive_changed(&change.workspace_id, &change.hive);
            log::info!("hive {}: woken by the human ({by}); the coordinator is relaunching", caller.hive.id);
            Ok(WakeOutcome::Relaunching)
        }
        Err(e) if unanswered => {
            ctx.window.hive_changed(&change.workspace_id, &change.hive);
            log::warn!("hive {}: woken by the human ({by}), the coordinator's relaunch unconfirmed: {e}", caller.hive.id);
            Ok(WakeOutcome::Unconfirmed)
        }
        Err(e) => {
            log::warn!("hive {}: wake failed, the coordinator wasn't relaunched: {e}", caller.hive.id);
            owed::end_wake_relaunch(&task);
            if roll_back(ctx, caller, &before, by, &e).await {
                restore_wake(caller, prev).await;
            }
            Err(format!("This Hive stays done: its coordinator couldn't be relaunched. {e}"))
        }
    }
}

/// Owes `text` to the coordinator's next session, after any message still waiting there (both
/// go, in order, refused together over the cap). Answers what was owed before. `joining`: only
/// while a wake's relaunch still holds messages, checked under the file's lock, since the session
/// start ends that hold as it reads what it is owed.
async fn owe(caller: &Caller, text: String, by: &str, since: Option<i64>, joining: bool) -> Result<Option<OwedWake>, String> {
    let (task, by, at) = (caller.paths.coordinator_task(), by.to_string(), crate::mailink::board::iso_now());
    blocking(move || {
        let mut res = Ok(None);
        owed::update(&task, |o| {
            if joining && !owed::wake_relaunching(&task) {
                res = Err(TAKING_REFUSAL.to_string());
                return false;
            }
            let joined = match &o.wake {
                Some(w) => format!("{}\n\n{text}", w.text),
                None => text,
            };
            if joined.chars().count() > MAX_MESSAGE {
                res = Err(format!("Your earlier message to wake this Hive is still waiting, and with this one it is over {MAX_MESSAGE} characters. Send this once the coordinator is back."));
                return false;
            }
            res = Ok(o.wake.clone());
            let w = o.wake.get_or_insert_with(OwedWake::default);
            w.text = joined;
            w.by = by;
            w.done_since = w.done_since.or(since);
            w.at = at;
            true
        })?;
        res
    })
    .await
}

/// The coordinator is still up (Done, and `exit_coordinator` hasn't ended it yet): the Hive goes
/// active, which keeps it from being ended, and the message is typed now (plan D12). The caller
/// holds the Hive's turn, so the exit can't read the status or signal in between; after a refused
/// delivery it finds Done again and ends the agent as before.
async fn wake_running<W: HiveWindow>(ctx: &Ctx<'_, W>, caller: &Caller, before: &Hive, tab: &str, text: &str, by: &str) -> Result<WakeOutcome, String> {
    let change = save_status(ctx, caller, &[STATUS_DONE], Some(STATUS_ACTIVE), |h| h.done_at = None)?;
    match ctx.window.deliver(tab, wake_text(before.done_at, by, text), "Hive wake").await {
        // A held one is typed once its draft or dialog clears: the agent is there to take it.
        Sent::Delivered | Sent::Held(_) => {
            ctx.window.hive_changed(&change.workspace_id, &change.hive);
            log_line(caller, KIND_WAKE, by, None).await;
            log::info!("hive {}: woken by the human ({by}) while its coordinator was still up", caller.hive.id);
            Ok(WakeOutcome::Delivered)
        }
        Sent::Refused(why) => {
            roll_back(ctx, caller, before, by, &why).await;
            Err(format!("The coordinator didn't take your message ({why}), so this Hive stays done. Try again in a moment."))
        }
    }
}

/// Back to Done with its old `done_at`, unannounced (the window never heard `active`). False when
/// that couldn't be saved: the Hive stays active, which the window then hears, and the owed
/// message stays for the coordinator's next start.
async fn roll_back<W: HiveWindow>(ctx: &Ctx<'_, W>, caller: &Caller, before: &Hive, by: &str, why: &str) -> bool {
    let done_at = before.done_at;
    match save_status(ctx, caller, &[STATUS_ACTIVE], Some(STATUS_DONE), move |h| h.done_at = done_at) {
        Ok(_) => {
            log_line(caller, KIND_WAKE_FAILED, by, Some(why)).await;
            true
        }
        Err(e) => {
            log::warn!("hive {}: a failed wake left the Hive active: {e}", caller.hive.id);
            if let Some(h) = current_hive(ctx, caller).ok().filter(|h| h.status == STATUS_ACTIVE) {
                ctx.window.hive_changed(&caller.workspace_id, &h);
            }
            false
        }
    }
}

async fn restore_wake(caller: &Caller, prev: Option<OwedWake>) {
    let task = caller.paths.coordinator_task();
    let res = blocking(move || {
        owed::update(&task, |o| {
            o.wake = prev;
            true
        })
        .map(|_| ())
    })
    .await;
    if let Err(e) = res {
        log::warn!("hive {}: a failed wake's message is still owed: {e}", caller.hive.id);
    }
}

async fn log_line(caller: &Caller, kind: &str, by: &str, why: Option<&str>) {
    let mut v = json!({ "v": RECORD_VERSION, "kind": kind, "at": crate::mailink::board::iso_now(), "by": by });
    if let (Some(why), Value::Object(o)) = (why, &mut v) {
        o.insert("why".to_string(), Value::String(super::one_line(why, 500)));
    }
    let path = caller.paths.finishes();
    if let Err(e) = blocking(move || super::super::queue::append_line(&path, &v)).await {
        log::warn!("hive {}: {kind} not logged: {e}", caller.hive.id);
    }
}
