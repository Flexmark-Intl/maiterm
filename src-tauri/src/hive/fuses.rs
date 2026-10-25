//! Fuses (docs/hive.md §7.8): what Rust checks before a worker's agent starts. The Hive's cap,
//! spawns per hour and per day across every Hive on the machine (the Claude plan they draw on is
//! shared), and the quota breaker a usage limit trips.
//!
//! One reservation, under one lock: the cap slot, both spawn counts and the breaker are checked
//! and taken together before any worktree or tab exists, and a spawn that fails or isn't
//! confirmed in time gives the slot back (`Reservation`'s Drop). The counts and the breaker's
//! expiry live in `<app data>/hive/fuses.json`, Rust's alone and outside the frontend's whole-list
//! writes, so a restart neither resets the day's count nor lifts a pause. Reservations are memory
//! only: after a restart the workers they became are counted from `worker.json`.
//!
//! Every function here takes the lock and may do file I/O under it (the first load, a commit's
//! write, the caller's live count): call them from a blocking context, never a tokio worker.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Mutex, MutexGuard};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use super::record::{Hive, STATUS_ACTIVE};
use crate::state::workspace::{TaskBlocker, TaskNote, TASK_NOTE_CAP};
use crate::state::{AppData, Task, Workspace, Workstream};

pub const FUSES_VERSION: u32 = 1;
pub const FUSES_FILE: &str = "fuses.json";
/// Defaults of §7.8; machine-wide, not per Hive.
pub const PER_HOUR: usize = 20;
pub const PER_DAY: usize = 100;
/// How long a spawn's window has to confirm the new tab before its slot is given back (§7.2).
pub const RESERVATION_TTL: Duration = Duration::from_secs(120);
pub const STATUS_PAUSED: &str = "paused";

const HOUR_MS: i64 = 3_600_000;
const DAY_MS: i64 = 24 * HOUR_MS;

fn ttl_ms() -> i64 {
    RESERVATION_TTL.as_millis() as i64
}

pub fn now_ms() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis() as i64).unwrap_or(0)
}

/// `fuses.json`. Every field defaults and a later build's fields survive in `extra`.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct FuseState {
    pub version: u32,
    /// When each spawn was committed, ms since the epoch; the last 24 h only.
    pub spawns: Vec<i64>,
    /// No worker starts in any Hive before this (ms).
    pub breaker_until: Option<i64>,
    pub breaker_reason: Option<String>,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

#[derive(Debug, Clone)]
struct Pending {
    hive_id: String,
    /// The worker's task id: once its `worker.json` says `launching` it is counted live, and
    /// this slot no longer counts on top.
    worker: String,
    at: i64,
}

pub struct FuseBook {
    path: PathBuf,
    state: FuseState,
    pending: HashMap<String, Pending>,
    /// tab → when it hit a limit Claude waits out, the last 24 h. Memory only, like Claude's own
    /// re-arm count: a restart kills the PTY, and the resumed agent starts that count over.
    waited: HashMap<String, Vec<i64>>,
    /// tab → the reset its usage-limit wait runs to: any delivery there before it would end the
    /// wait (§7.8), so Hive deliveries hold until then. Memory only, as `waited`.
    waiting: HashMap<String, i64>,
}

impl FuseBook {
    /// A missing file is a first start. One that won't read starts the counts over, logged: an
    /// atomic write makes that a hand edit, and refusing every spawn forever would be worse.
    fn load(path: PathBuf) -> Self {
        let state = match std::fs::read(&path) {
            Ok(bytes) => serde_json::from_slice(&bytes).unwrap_or_else(|e| {
                log::warn!("hive fuses: {} unreadable ({e}); spawn counts start over", path.display());
                FuseState::default()
            }),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => FuseState::default(),
            Err(e) => {
                log::warn!("hive fuses: {}: {e}; spawn counts start over", path.display());
                FuseState::default()
            }
        };
        Self { path, state, pending: HashMap::new(), waited: HashMap::new(), waiting: HashMap::new() }
    }

    fn save(&mut self) -> Result<(), String> {
        self.state.version = FUSES_VERSION;
        let v = serde_json::to_value(&self.state).map_err(|e| e.to_string())?;
        super::launch::write_json(&self.path, &v)
    }

    /// Expired reservations go; their owners find `is_live` false and roll back.
    fn prune(&mut self, now: i64) {
        self.pending.retain(|_, p| now - p.at < ttl_ms());
    }

    fn committed_within(&self, now: i64, window: i64) -> usize {
        self.state.spawns.iter().filter(|t| now - **t < window).count()
    }

    /// `hive_id`'s reservations whose worker isn't among `live` (task ids), so a worker written
    /// `launching` before its commit is counted once.
    fn pending_for(&self, hive_id: &str, live: &[String]) -> usize {
        self.pending.values().filter(|p| p.hive_id == hive_id && !live.contains(&p.worker)).count()
    }

    fn breaker(&self, now: i64) -> Option<i64> {
        self.state.breaker_until.filter(|u| now < *u)
    }

    /// When the oldest committed spawn inside `window` leaves it.
    fn frees_at(&self, now: i64, window: i64) -> Option<i64> {
        self.state.spawns.iter().filter(|t| now - **t < window).min().map(|t| t + window)
    }
}

/// The book and where it lives. The app has one (`BOOK`); tests make their own over a tempdir.
pub struct Fuses {
    book: Mutex<Option<FuseBook>>,
    path: Option<PathBuf>,
    /// Memory pressure as a reservation sees it (§6.8): the machine's for the app's book.
    gate: fn() -> super::pressure::Gate,
}

/// `<app data>/hive/fuses.json`, loaded on first use.
static BOOK: Fuses = Fuses { book: Mutex::new(None), path: None, gate: super::pressure::spawn_gate };

fn no_pressure() -> super::pressure::Gate {
    super::pressure::Gate::Open
}

impl Fuses {
    /// A book over `path` that never feels memory pressure: tests decide that themselves.
    pub fn at(path: PathBuf) -> Self {
        Self { book: Mutex::new(None), path: Some(path), gate: no_pressure }
    }

    pub fn with_gate(path: PathBuf, gate: fn() -> super::pressure::Gate) -> Self {
        Self { book: Mutex::new(None), path: Some(path), gate }
    }

    fn lock(&self) -> Result<Guard<'_>, String> {
        let mut g = self.book.lock().unwrap_or_else(|e| e.into_inner());
        if g.is_none() {
            let path = self
                .path
                .clone()
                .or_else(|| super::paths::hives_dir().map(|d| d.join(FUSES_FILE)))
                .ok_or("maiTerm couldn't find its data folder, so no worker can start")?;
            *g = Some(FuseBook::load(path));
        }
        Ok(Guard(g))
    }

    /// Takes a slot for the launch of `hive`'s worker `worker` (its task id), or says why not.
    /// `live` lists the task ids of the Hive's workers in `LIVE_PHASES`; it's called under the
    /// lock, so two spawns can't both read a count from before either committed. It must not touch
    /// the fuses itself.
    pub fn reserve(
        &self,
        hive: &Hive,
        worker: &str,
        live: impl FnOnce() -> Vec<String>,
        now: i64,
    ) -> Result<Reservation<'_>, String> {
        // Read before the lock: a sample may take a moment. Under pressure the effective cap is
        // the live count, so no new worker starts in any Hive until it eases (§6.8).
        if let Some(why) = (self.gate)().throttled() {
            return Err(format!("Throttled · {why}: no worker starts until the machine has memory to spare."));
        }
        let mut g = self.lock()?;
        let b = g.book();
        b.prune(now);
        if hive.status != STATUS_ACTIVE {
            return Err(match hive.status.as_str() {
                STATUS_PAUSED => "This Hive is paused: no worker starts until the human resumes it.".to_string(),
                s => format!("This Hive is {s}: it starts no workers."),
            });
        }
        if let Some(until) = b.breaker(now) {
            return Err(format!(
                "Paused · usage limit until {}: no worker starts in any Hive before then.",
                reset_hhmm(until)
            ));
        }
        let live = live();
        let held = live.len() + b.pending_for(&hive.id, &live);
        if held >= hive.cap as usize {
            return Err(format!("The Hive's cap is reached: {held} of {} workers at once.", hive.cap));
        }
        let pending = b.pending.len();
        for (limit, window, what) in [(PER_HOUR, HOUR_MS, "hour"), (PER_DAY, DAY_MS, "day")] {
            if b.committed_within(now, window) + pending >= limit {
                let when = b.frees_at(now, window).map(|t| format!(" The next can start after {}.", local_hhmm(t)));
                return Err(format!(
                    "Spawn fuse: {limit} workers started in the last {what} across every Hive.{}",
                    when.unwrap_or_default()
                ));
            }
        }
        let id = uuid::Uuid::new_v4().to_string();
        b.pending.insert(id.clone(), Pending { hive_id: hive.id.clone(), worker: worker.to_string(), at: now });
        Ok(Reservation { fuses: self, id, done: false })
    }

    /// `live` as for `reserve`.
    pub fn headroom(&self, hive: &Hive, live: &[String], now: i64) -> Headroom {
        let mut h = Headroom { cap: hive.cap, live: live.len(), ..Headroom::default() };
        let Ok(mut g) = self.lock() else { return h };
        let b = g.book();
        b.prune(now);
        h.pending = b.pending_for(&hive.id, live);
        let all = b.pending.len();
        h.hour_left = PER_HOUR.saturating_sub(b.committed_within(now, HOUR_MS) + all);
        h.day_left = PER_DAY.saturating_sub(b.committed_within(now, DAY_MS) + all);
        h.breaker_until = b.breaker(now);
        h.breaker_reason = h.breaker_until.and(b.state.breaker_reason.clone());
        h.changes_at = [h.breaker_until, b.frees_at(now, HOUR_MS), b.frees_at(now, DAY_MS)].into_iter().flatten().min();
        h
    }

    /// Stops every Hive starting workers until `until`. True when that's news: the breaker was
    /// off or ended sooner. Kept in memory when the write fails, the conservative side.
    pub fn trip_breaker(&self, until: i64, reason: &str) -> bool {
        let Ok(mut g) = self.lock() else { return false };
        let b = g.book();
        if b.state.breaker_until.is_some_and(|u| u >= until) {
            return false;
        }
        b.state.breaker_until = Some(until);
        b.state.breaker_reason = Some(reason.to_string());
        if let Err(e) = b.save() {
            log::warn!("hive fuses: breaker until {until} not saved, so a restart would lift it: {e}");
        }
        true
    }

    /// What a StopFailure on Hive tab `tab` does (§7.8), or None when it's no usage limit to act
    /// on. A reset Claude waits out is counted against the tab: past `REARMS` waits in a day,
    /// Claude's auto-continue has stopped, so the Hive pauses as well as the breaker tripping.
    pub fn assess_limit(&self, tab: &str, event: &Value, now: i64, on_subscription: bool) -> Option<Assessment> {
        let text = usage_limit_text(event)?;
        if is_transient(&text) {
            // A plan's capacity 429 that Claude retried is no limit. Without a known plan it may
            // be an API key's, and nothing auto-continues those.
            return (!on_subscription).then_some(Assessment { text, until: None, pause: Some(PauseCause::NoSubscription) });
        }
        let Some(until) = parse_reset_text(&text, now) else {
            return Some(Assessment { text, until: None, pause: Some(PauseCause::NoWait) });
        };
        let Ok(mut g) = self.lock() else {
            return Some(Assessment { text, until: Some(until), pause: None });
        };
        let b = g.book();
        b.waited.retain(|_, hits| {
            hits.retain(|t| now - *t < DAY_MS);
            !hits.is_empty()
        });
        let hits = b.waited.entry(tab.to_string()).or_default();
        hits.push(now);
        let pause = (hits.len() > REARMS).then_some(PauseCause::Rearmed);
        b.waiting.retain(|_, u| now < *u);
        b.waiting.insert(tab.to_string(), until);
        Some(Assessment { text, until: Some(until), pause })
    }

    /// When `tab`'s usage-limit wait ends, while it hasn't.
    pub fn limit_wait(&self, tab: &str, now: i64) -> Option<i64> {
        let mut g = self.lock().ok()?;
        g.book().waiting.get(tab).copied().filter(|u| now < *u)
    }
}

/// How many usage-limit waits Claude's auto-continue makes on one tab before it stops (§7.8:
/// "re-arms at most twice"). Read as two waits, so the third limit pauses: the conservative side
/// of an unmeasured count.
pub const REARMS: usize = 2;

/// Why a limit pauses its Hive rather than only tripping the breaker: nothing will continue the
/// tab that hit it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PauseCause {
    /// Weekly, monthly, dated (over 24 h) or unparseable: Claude doesn't wait for it.
    NoWait,
    /// A reset Claude would wait for, but it already has `REARMS` times on this tab today.
    Rearmed,
    /// A 429 Claude retried and gave up on, on a tab maiTerm can't show runs on a Claude plan.
    NoSubscription,
}

impl PauseCause {
    /// maiTerm's words for it, which the human's blocker and the coordinator's notice carry.
    fn explain(self) -> &'static str {
        match self {
            PauseCause::NoWait => "Claude doesn't wait out a limit like this one, so nothing will continue it.",
            PauseCause::Rearmed => {
                "Claude has already waited out a usage limit on that agent's tab twice today and won't wait again."
            }
            PauseCause::NoSubscription => {
                "Claude retried and stopped, and maiTerm can't tell the agent runs on a Claude plan, so nothing will \
                 continue it."
            }
        }
    }
}

/// A usage limit on one Hive tab: Claude's words, the reset the breaker holds to (end of the
/// shown minute), and whether the Hive pauses.
#[derive(Debug, Clone, PartialEq)]
pub struct Assessment {
    pub text: String,
    pub until: Option<i64>,
    pub pause: Option<PauseCause>,
}

struct Guard<'a>(MutexGuard<'a, Option<FuseBook>>);

impl Guard<'_> {
    fn book(&mut self) -> &mut FuseBook {
        self.0.as_mut().expect("loaded by Fuses::lock")
    }
}

/// One launch's slot. Commit it once the window has confirmed the tab and the worker's phase is
/// `launching` on disk, so the slot moves from pending to live with no gap; meanwhile the worker
/// is counted once, live, since the slot is keyed by its task id. Dropped uncommitted
/// (an error, a panic, a rollback) it is given back.
///
/// A crash between the window's confirmation and the commit undercounts the spawns by one; the
/// worker itself still counts against the cap, from its `worker.json`.
pub struct Reservation<'a> {
    fuses: &'a Fuses,
    id: String,
    done: bool,
}

impl Reservation<'_> {
    pub fn is_live(&self, now: i64) -> bool {
        let Ok(mut g) = self.fuses.lock() else { return false };
        g.book().pending.get(&self.id).is_some_and(|p| now - p.at < ttl_ms())
    }

    /// Counts the spawn. Refused when the reservation expired meanwhile (its slot may be someone
    /// else's by now) or the count can't be saved: either way the caller rolls the spawn back.
    pub fn commit(mut self, now: i64) -> Result<(), String> {
        let mut g = self.fuses.lock()?;
        let b = g.book();
        if !b.pending.get(&self.id).is_some_and(|p| now - p.at < ttl_ms()) {
            return Err("The spawn took too long to confirm, so its slot was given back.".to_string());
        }
        let before = b.state.spawns.clone();
        b.state.spawns.push(now);
        b.state.spawns.retain(|t| now - *t < DAY_MS);
        if let Err(e) = b.save() {
            b.state.spawns = before;
            return Err(format!("The spawn count couldn't be saved: {e}"));
        }
        b.pending.remove(&self.id);
        self.done = true;
        Ok(())
    }
}

impl Drop for Reservation<'_> {
    fn drop(&mut self) {
        if self.done {
            return;
        }
        if let Ok(mut g) = self.fuses.lock() {
            g.book().pending.remove(&self.id);
        }
    }
}

/// What `hiveListWorkers` tells the coordinator and the header shows (§7.8).
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct Headroom {
    pub cap: u32,
    pub live: usize,
    /// This Hive's launches reserved and not yet confirmed.
    pub pending: usize,
    /// Machine-wide, pending launches included.
    pub hour_left: usize,
    pub day_left: usize,
    /// The end of the minute Claude showed as the reset: show it with `reset_hhmm`.
    pub breaker_until: Option<i64>,
    pub breaker_reason: Option<String>,
    /// When this changes with no event to say so: the breaker ends, or a spawn leaves the hour's
    /// or the day's window. The board refetches then (§4.4: a clock, not a poll).
    pub changes_at: Option<i64>,
}

/// The app's book, for a caller that holds a reservation across an await (a spawn).
pub fn book() -> &'static Fuses {
    &BOOK
}

pub fn reserve(hive: &Hive, worker: &str, live: impl FnOnce() -> Vec<String>, now: i64) -> Result<Reservation<'static>, String> {
    BOOK.reserve(hive, worker, live, now)
}

pub fn headroom(hive: &Hive, live: &[String], now: i64) -> Headroom {
    BOOK.headroom(hive, live, now)
}

pub fn trip_breaker(until: i64, reason: &str) -> bool {
    BOOK.trip_breaker(until, reason)
}

pub fn assess_limit(tab: &str, event: &Value, now: i64, on_subscription: bool) -> Option<Assessment> {
    BOOK.assess_limit(tab, event, now, on_subscription)
}

pub fn limit_wait(tab: &str, now: i64) -> Option<i64> {
    BOOK.limit_wait(tab, now)
}

/// The words of a rate limit a StopFailure reports, or None for another error kind. An empty
/// string is a limit Claude gave no words for.
pub fn usage_limit_text(event: &Value) -> Option<String> {
    if event.get("error").and_then(|v| v.as_str()) != Some("rate_limit") {
        return None;
    }
    let s = |k: &str| event.get(k).and_then(|v| v.as_str()).map(str::trim).filter(|s| !s.is_empty());
    Some(s("last_assistant_message").or_else(|| s("error_details")).unwrap_or_default().to_string())
}

/// A 429 Claude retried ("API Error: Request rejected (429) · …"), not a usage limit with a
/// reset: on a plan it's capacity, on an API key the key's own rate limit.
fn is_transient(text: &str) -> bool {
    let lower = text.to_lowercase();
    lower.starts_with("api error") && !lower.contains("usage") && !lower.contains("credit")
}

/// Whether `tab` is known to run on a Claude plan: it spawned under a managed account whose plan
/// was read back. Unmanaged is unknown (an API key may be what it runs on), so this says false.
pub fn on_subscription(app: &crate::state::AppState, tab: &str) -> bool {
    let Some(id) = app.tab_accounts.read().get(tab).and_then(|r| r.local.get(crate::accounts::Runtime::Claude.slug())).map(|a| a.id.clone()) else {
        return false;
    };
    app.app_data.read().preferences.managed_accounts.iter().any(|a| a.id == id && a.plan.is_some())
}

/// Limits whose reset is a week or a month off: nothing waits for those (§7.8).
const LONG_LIMITS: [&str; 6] = ["weekly limit", "opus limit", "sonnet limit", "fable limit", "monthly", "spend limit"];

/// When a usage limit resets (ms), from Claude's own words: "You've hit your session limit ·
/// resets 3pm (Europe/London)". None for a weekly or monthly limit, a reset more than 24 h away
/// (Claude then writes a date, "Oct 25, 3pm"), no reset at all, or words that don't parse: the
/// caller pauses the Hive instead, since nothing should wait on a resume that won't come.
pub fn parse_reset(stop_failure_event: &Value, now: i64) -> Option<i64> {
    parse_reset_text(&usage_limit_text(stop_failure_event)?, now)
}

fn parse_reset_text(text: &str, now: i64) -> Option<i64> {
    let lower = text.to_lowercase();
    if LONG_LIMITS.iter().any(|l| lower.contains(l)) {
        return None;
    }
    let (_, after) = lower.split_once("resets ")?;
    let when = after.split(" \u{b7} ").next()?.trim();
    // The zone in brackets is the agent's, and it runs in maiTerm's own PTY on this machine:
    // it's the local zone, and aliases ("US/Eastern") would make comparing names refuse it.
    let time = when.split_once(" (").map_or(when, |(t, _)| t).trim();
    let (h, m) = parse_clock(time)?;
    let at = next_local(now, h, m)?;
    (at - now <= DAY_MS).then_some(at)
}

/// "3pm", "3:05am", "12pm" → 24-hour (hour, minute). A date ("oct 25, 3pm") is refused.
fn parse_clock(s: &str) -> Option<(i32, i32)> {
    let (digits, pm) = if let Some(d) = s.strip_suffix("pm") {
        (d, true)
    } else {
        (s.strip_suffix("am")?, false)
    };
    let (h, m) = match digits.trim().split_once(':') {
        Some((h, m)) => (h, m),
        None => (digits.trim(), "0"),
    };
    let ok = |p: &str| !p.is_empty() && p.len() <= 2 && p.bytes().all(|b| b.is_ascii_digit());
    if !ok(h) || !ok(m) {
        return None;
    }
    let (h, m): (i32, i32) = (h.parse().ok()?, m.parse().ok()?);
    if !(1..=12).contains(&h) || m > 59 {
        return None;
    }
    Some((h % 12 + if pm { 12 } else { 0 }, m))
}

/// The first local `h:m` whose minute has ended after `now`, as its end: Claude drops the
/// seconds, so a reset shown as 3pm comes during 15:00, and one that just passed is tomorrow's.
#[cfg(unix)]
fn next_local(now: i64, h: i32, m: i32) -> Option<i64> {
    (0..=1).filter_map(|days| local_at(now, h, m, days)).map(|t| t + 60_000).find(|t| *t > now)
}

#[cfg(unix)]
fn local_at(now: i64, h: i32, m: i32, days_ahead: i32) -> Option<i64> {
    let secs = now.div_euclid(1000) as libc::time_t;
    // SAFETY: a zeroed `tm` is valid; localtime_r and mktime only touch the struct given.
    let mut tm: libc::tm = unsafe { std::mem::zeroed() };
    if unsafe { libc::localtime_r(&secs, &mut tm) }.is_null() {
        return None;
    }
    tm.tm_hour = h;
    tm.tm_min = m;
    tm.tm_sec = 0;
    tm.tm_mday += days_ahead;
    tm.tm_isdst = -1;
    let t = unsafe { libc::mktime(&mut tm) };
    (t != -1).then(|| t as i64 * 1000)
}

/// No Hive runs on native Windows (§14.16); a reset there is read as unparseable.
#[cfg(not(unix))]
fn next_local(_now: i64, _h: i32, _m: i32) -> Option<i64> {
    None
}

/// `14:20`, local, as the header and the coordinator's notice show it.
pub fn local_hhmm(ms: i64) -> String {
    #[cfg(unix)]
    {
        let secs = ms.div_euclid(1000) as libc::time_t;
        // SAFETY: as in `local_at`.
        let mut tm: libc::tm = unsafe { std::mem::zeroed() };
        if !unsafe { libc::localtime_r(&secs, &mut tm) }.is_null() {
            return format!("{:02}:{:02}", tm.tm_hour, tm.tm_min);
        }
    }
    let mins = ms.div_euclid(60_000).rem_euclid(24 * 60);
    format!("{:02}:{:02} UTC", mins / 60, mins % 60)
}

/// A breaker's `until` as Claude showed it: the stored value is the end of that minute.
pub fn reset_hhmm(until: i64) -> String {
    local_hhmm(until - 60_000)
}

/// Claude's words for a limit, quoted to a coordinator: one line, short, and nothing that reads
/// as a maiTerm frame.
fn quote_limit(text: &str) -> String {
    let line = text.lines().next().unwrap_or_default().replace(['\u{27e6}', '\u{27e7}'], "");
    let line = line.trim().trim_start_matches("[maiTerm]").trim();
    let mut q: String = line.chars().take(160).collect();
    if q.is_empty() {
        q = "a usage limit".to_string();
    }
    q
}

/// To every coordinator but the one that hit the limit (typing there would end Claude's own wait).
pub fn breaker_notice(until: i64) -> String {
    format!(
        "[maiTerm] Paused \u{b7} usage limit until {}. A Hive agent hit Claude's usage limit, which every Hive on this \
         machine draws on, so no worker starts in any Hive before then. Starts resume on their own at the reset.",
        reset_hhmm(until)
    )
}

/// To the paused Hive's coordinator when another of its agents hit the limit. maiTerm has
/// already asked the human (`file_pause_blocker`), so the coordinator mustn't ask again.
pub fn pause_notice(limit: &str, cause: PauseCause) -> String {
    format!(
        "[maiTerm] This Hive is paused. One of its agents hit a usage limit (Claude said: \"{}\"). {} No worker starts \
         until your human resumes the Hive. maiTerm has asked them to, on your task \"{PAUSE_TASK_TITLE}\": don't ask \
         them again. Leave running workers as they are.",
        quote_limit(limit),
        cause.explain()
    )
}

/// The row the human's pause blocker sits on, on the coordinator's tab.
pub const PAUSE_TASK_TITLE: &str = "Hive paused: usage limit";

fn is_pause_task(t: &Task, coordinator: Option<&str>) -> bool {
    t.tab_id.as_deref() == coordinator && t.normalized_title == Task::normalize_title(PAUSE_TASK_TITLE)
}

/// Rust asks the human itself (§7.8 "files a blocker for the human"): the coordinator is often
/// the agent at the limit, and a weekly limit is every agent's on that plan, so no agent could.
/// An `action` blocker on the coordinator's row, reused across pauses, so it waits where every
/// question does (the Loom's Decisions, the phone).
pub fn file_pause_blocker(ws: &mut Workspace, coordinator: Option<&str>, limit: &str, cause: PauseCause, now: &str) {
    let blocker = TaskBlocker {
        kind: "action".into(),
        question: "Resume this Hive once Claude's usage limit allows: it is paused and starts no workers until you do."
            .into(),
        context: Some(format!("Claude said: \"{}\". {}", quote_limit(limit), cause.explain())),
        options: Vec::new(),
        command: None,
        asked_at: now.into(),
        asked_by: "agent".into(),
    };
    let note = format!("Needs you: {}", blocker.question);
    file_row_blocker(ws, coordinator, PAUSE_TASK_TITLE, blocker, &note, now);
}

/// A question maiTerm asks the human itself (a pause, an autonomy offer, the plan): a blocker on
/// the coordinator's row titled `title`, found or made, so it waits where every question does.
/// The caller saves and sends the tasks store the list (`mailink-tasks-changed`). Answers the row's
/// id and the blocker's `asked_at`.
pub fn file_row_blocker(ws: &mut Workspace, coordinator: Option<&str>, title: &str, blocker: TaskBlocker, note: &str, now: &str) -> (String, String) {
    let normalized = Task::normalize_title(title);
    let t = match ws.tasks.iter().position(|t| t.tab_id.as_deref() == coordinator && t.normalized_title == normalized) {
        Some(i) => &mut ws.tasks[i],
        None => {
            ws.tasks.push(Task {
                id: uuid::Uuid::new_v4().to_string(),
                title: title.into(),
                normalized_title: normalized,
                detail: None,
                status: "blocked".into(),
                tab_id: coordinator.map(str::to_string),
                blocked_by: Vec::new(),
                origin: "agent".into(),
                created_at: now.into(),
                updated_at: now.into(),
                workstream_id: None,
                notes: Vec::new(),
                blocker: None,
            });
            ws.tasks.last_mut().expect("just pushed")
        }
    };
    let asked_at = blocker.asked_at.clone();
    t.status = "blocked".into();
    t.blocker = Some(blocker);
    t.notes.push(TaskNote { at: now.into(), text: note.to_string(), by: "agent".into() });
    let over = t.notes.len().saturating_sub(TASK_NOTE_CAP);
    t.notes.drain(..over);
    t.updated_at = now.into();
    (t.id.clone(), asked_at)
}

/// A resume answers the pause: its row leaves Blocked, which clears the question everywhere.
/// False when no row was waiting.
pub fn close_pause_blocker(ws: &mut Workspace, coordinator: Option<&str>, now: &str) -> bool {
    let Some(t) = ws.tasks.iter_mut().find(|t| is_pause_task(t, coordinator) && t.status == "blocked") else {
        return false;
    };
    t.status = "done".into();
    t.settle_blocker();
    t.notes.push(TaskNote { at: now.into(), text: "Resumed by the human".into(), by: "human".into() });
    let over = t.notes.len().saturating_sub(TASK_NOTE_CAP);
    t.notes.drain(..over);
    t.updated_at = now.into();
    true
}

/// The coordinator tabs of every Hive still at work, in every window.
pub fn coordinator_tabs(data: &AppData) -> Vec<String> {
    data.windows
        .iter()
        .flat_map(|w| w.workspaces.iter())
        .filter(|ws| ws.hive.as_ref().is_some_and(|h| h.status != "done"))
        .flat_map(|ws| ws.panes.iter().flat_map(|p| p.tabs.iter()))
        .filter(|t| t.hive_role.as_deref() == Some(super::ROLE_COORDINATOR))
        .map(|t| t.id.clone())
        .collect()
}

/// A Hive changed status in the state: its window, workspace and record as they now are.
pub struct StatusChange {
    pub window_label: String,
    pub workspace_id: String,
    pub hive: Hive,
    pub coordinator: Option<String>,
    /// The workspace's tasks and workstreams when the change wrote the pause row.
    pub tasks: Option<(Vec<Task>, Vec<Workstream>)>,
}

/// Sets the Hive's status when it's `from` (any status when None). None when the Hive is gone
/// or already elsewhere, so a second limit doesn't pause or notify twice.
pub fn set_status(data: &mut AppData, hive_id: &str, from: Option<&str>, to: &str) -> Option<StatusChange> {
    for w in &mut data.windows {
        for ws in &mut w.workspaces {
            let Some(h) = ws.hive.as_mut().filter(|h| h.id == hive_id) else { continue };
            if h.status == to || from.is_some_and(|f| h.status != f) {
                return None;
            }
            h.status = to.to_string();
            let coordinator = ws
                .panes
                .iter()
                .flat_map(|p| p.tabs.iter())
                .find(|t| t.hive_role.as_deref() == Some(super::ROLE_COORDINATOR))
                .map(|t| t.id.clone());
            return Some(StatusChange {
                window_label: w.label.clone(),
                workspace_id: ws.id.clone(),
                hive: h.clone(),
                coordinator,
                tasks: None,
            });
        }
    }
    None
}

fn changed_workspace<'a>(data: &'a mut AppData, c: &StatusChange) -> Option<&'a mut Workspace> {
    data.windows
        .iter_mut()
        .filter(|w| w.label == c.window_label)
        .flat_map(|w| w.workspaces.iter_mut())
        .find(|ws| ws.id == c.workspace_id)
}

/// An active Hive pauses, and the human is asked to resume it, in one write.
pub fn pause_in(data: &mut AppData, hive_id: &str, limit: &str, cause: PauseCause, now: &str) -> Option<StatusChange> {
    let mut c = set_status(data, hive_id, Some(STATUS_ACTIVE), STATUS_PAUSED)?;
    if let Some(ws) = changed_workspace(data, &c) {
        file_pause_blocker(ws, c.coordinator.as_deref(), limit, cause, now);
        c.tasks = Some((ws.tasks.clone(), ws.workstreams.clone()));
    }
    Some(c)
}

/// A paused Hive is active again, and the question that asked for it is closed.
pub fn resume_in(data: &mut AppData, hive_id: &str, now: &str) -> Option<StatusChange> {
    let mut c = set_status(data, hive_id, Some(STATUS_PAUSED), STATUS_ACTIVE)?;
    if let Some(ws) = changed_workspace(data, &c) {
        if close_pause_blocker(ws, c.coordinator.as_deref(), now) {
            c.tasks = Some((ws.tasks.clone(), ws.workstreams.clone()));
        }
    }
    Some(c)
}

/// The frontend's mirror of `Workspace.hive`, which Rust alone writes, follows its window's event.
pub const HIVE_CHANGED_EVENT: &str = "hive-changed";

fn announce(handle: &tauri::AppHandle, c: &StatusChange) {
    use tauri::Emitter;
    let payload = serde_json::json!({ "workspace_id": c.workspace_id, "hive": c.hive });
    if let Err(e) = handle.emit_to(c.window_label.as_str(), HIVE_CHANGED_EVENT, payload) {
        log::warn!("hive {}: status change not sent to window {}: {e}", c.hive.id, c.window_label);
    }
    // The tasks store's channel for a write Rust made, or its next whole-list persist drops it.
    if let Some((tasks, workstreams)) = &c.tasks {
        let payload = serde_json::json!({ "workspaceId": c.workspace_id, "tasks": tasks, "workstreams": workstreams });
        if let Err(e) = handle.emit("mailink-tasks-changed", payload) {
            log::warn!("hive {}: pause task not sent to the tasks store: {e}", c.hive.id);
        }
    }
}

/// Applies a status change to the state and saves it. A failed save leaves it changed in
/// memory, logged: for a pause that's the safe side, and the next save carries it.
fn commit_change(
    app: &crate::state::AppState,
    handle: &tauri::AppHandle,
    hive_id: &str,
    change: impl FnOnce(&mut AppData) -> Option<StatusChange>,
) -> Option<StatusChange> {
    let (change, snapshot) = {
        let mut data = app.app_data.write();
        let c = change(&mut data)?;
        (c, data.clone())
    };
    let to = change.hive.status.as_str();
    if let Err(e) = crate::state::save_state(&snapshot) {
        log::warn!("hive {hive_id}: status {to} not saved: {e}");
    }
    log::info!("hive {hive_id}: status {to}");
    announce(handle, &change);
    Some(change)
}

pub fn pause(
    app: &crate::state::AppState,
    handle: &tauri::AppHandle,
    hive_id: &str,
    limit: &str,
    cause: PauseCause,
) -> Option<StatusChange> {
    let now = crate::mailink::board::iso_now();
    commit_change(app, handle, hive_id, |data| pause_in(data, hive_id, limit, cause, &now))
}

/// Resume button's command: a paused Hive starts workers again. The breaker, machine-wide and
/// timed, is not the Hive's to lift.
#[tauri::command]
pub async fn hive_resume(
    window: tauri::Window,
    state: tauri::State<'_, std::sync::Arc<crate::state::AppState>>,
    app: tauri::AppHandle,
    workspace_id: String,
) -> Result<Hive, String> {
    let hive_id = {
        let data = state.app_data.read();
        let ws = data
            .window(window.label())
            .and_then(|w| w.workspaces.iter().find(|w| w.id == workspace_id))
            .ok_or("Workspace not found")?;
        let hive = ws.hive.as_ref().ok_or("This workspace isn't a Hive")?;
        if hive.status != STATUS_PAUSED {
            return Err(format!("This Hive isn't paused (it's {})", hive.status));
        }
        hive.id.clone()
    };
    let now = crate::mailink::board::iso_now();
    let hive = commit_change(&state, &app, &hive_id, |data| resume_in(data, &hive_id, &now))
        .map(|c| c.hive)
        .ok_or_else(|| "This Hive isn't paused any more".to_string())?;
    // Workers that waited out the pause launch now, where their dependencies allow.
    super::waiters::poll_soon(state.inner().clone(), app, workspace_id);
    Ok(hive)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn scratch() -> (PathBuf, Fuses) {
        let dir = std::env::temp_dir().join(format!("hive-fuses-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let f = Fuses::at(dir.join(FUSES_FILE));
        (dir, f)
    }

    fn hive(id: &str, cap: u32) -> Hive {
        Hive { id: id.into(), cap, ..Hive::default() }
    }

    const T0: i64 = 1_800_000_000_000;
    /// A worker no live list names, for reservations whose worker doesn't matter.
    const W: &str = "a worker";

    fn ids(n: usize) -> Vec<String> {
        (0..n).map(|i| format!("live-{i}")).collect()
    }

    #[test]
    fn memory_pressure_makes_the_cap_the_live_count() {
        let dir = std::env::temp_dir().join(format!("hive-fuses-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        for gate in [|| crate::hive::pressure::Gate::Hold, || crate::hive::pressure::Gate::Critical] {
            let f = Fuses::with_gate(dir.join(FUSES_FILE), gate);
            let err = f.reserve(&hive("h1", 4), W, Vec::new, T0).err().unwrap();
            assert!(err.starts_with("Throttled · memory pressure"), "{err}");
            assert_eq!(f.headroom(&hive("h1", 4), &[], T0).pending, 0, "nothing was taken");
        }
        let f = Fuses::with_gate(dir.join(FUSES_FILE), || crate::hive::pressure::Gate::Open);
        assert!(f.reserve(&hive("h1", 4), W, Vec::new, T0).is_ok());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn sixteen_spawns_at_once_take_exactly_the_cap() {
        let (dir, f) = scratch();
        let h = hive("h1", 4);
        let won = AtomicUsize::new(0);
        let held = Mutex::new(Vec::new());
        std::thread::scope(|s| {
            for _ in 0..16 {
                s.spawn(|| {
                    if let Ok(r) = f.reserve(&h, W, Vec::new, T0) {
                        won.fetch_add(1, Ordering::SeqCst);
                        held.lock().unwrap().push(r);
                    }
                });
            }
        });
        assert_eq!(won.load(Ordering::SeqCst), 4);
        let mut held = held.into_inner().unwrap();
        let err = f.reserve(&h, W, Vec::new, T0).err().unwrap();
        assert!(err.contains("cap is reached: 4 of 4"), "{err}");
        assert!(f.reserve(&hive("h2", 4), W, Vec::new, T0).is_ok(), "the cap is per Hive");
        drop(held.pop());
        assert!(f.reserve(&h, W, Vec::new, T0).is_ok(), "a dropped reservation frees its slot");
        // Live workers count with the pending ones.
        drop(held);
        assert!(f.reserve(&h, W, || ids(4), T0).is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A spawn writes its worker `launching` before it commits (§7.8's no-gap rule): in between,
    /// that worker is live on disk AND holds its reservation, and must count once.
    #[test]
    fn a_launching_worker_holding_its_reservation_counts_once() {
        let (dir, f) = scratch();
        let h = hive("h1", 2);
        let a = f.reserve(&h, "task-a", Vec::new, T0).unwrap();
        // A's worker.json now says `launching`; its window hasn't answered yet.
        let on_disk = || vec!["task-a".to_string()];
        assert_eq!(f.headroom(&h, &on_disk(), T0).pending, 0, "A counts as live, not as starting too");
        let b = f.reserve(&h, "task-b", on_disk, T0).expect("one worker of two: B fits");
        let err = f.reserve(&h, "task-c", on_disk, T0).err().unwrap();
        assert!(err.contains("cap is reached: 2 of 2"), "A live and B pending: {err}");
        let hr = f.headroom(&h, &on_disk(), T0);
        assert_eq!((hr.live, hr.pending), (1, 1));
        drop((a, b));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_committed_spawn_moves_from_pending_to_live() {
        let (dir, f) = scratch();
        let h = hive("h1", 1);
        let r = f.reserve(&h, W, Vec::new, T0).unwrap();
        assert!(r.is_live(T0 + 1000));
        r.commit(T0 + 1000).unwrap();
        // The worker is now `launching` in its worker.json, which the caller counts as live.
        assert!(f.reserve(&h, W, Vec::new, T0 + 2000).is_ok(), "committed, so no longer pending");
        assert!(f.reserve(&h, W, || ids(1), T0 + 2000).is_err());
        let hr = f.headroom(&h, &ids(1), T0 + 2000);
        assert_eq!((hr.cap, hr.live, hr.pending, hr.hour_left, hr.day_left), (1, 1, 0, PER_HOUR - 1, PER_DAY - 1));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn an_expired_reservation_cannot_commit_and_its_slot_is_retaken() {
        let (dir, f) = scratch();
        let h = hive("h1", 1);
        let late = T0 + ttl_ms();
        let r = f.reserve(&h, W, Vec::new, T0).unwrap();
        assert!(r.is_live(late - 1) && !r.is_live(late));
        let again = f.reserve(&h, W, Vec::new, late).expect("the expired slot is free again");
        assert!(r.commit(late).is_err());
        assert!(again.is_live(late), "the old one's drop doesn't release the new one");
        again.commit(late + 1).unwrap();
        assert_eq!(f.headroom(&h, &[], late + 2).hour_left, PER_HOUR - 1, "only the confirmed spawn counted");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn hour_and_day_windows_count_across_hives_with_a_fake_clock() {
        let (dir, f) = scratch();
        let big = |id: &str| hive(id, 1000);
        let mut t = T0;
        for i in 0..PER_HOUR {
            let h = big(if i % 2 == 0 { "a" } else { "b" });
            f.reserve(&h, W, Vec::new, t).unwrap().commit(t).unwrap();
            t += 1000;
        }
        let err = f.reserve(&big("c"), W, Vec::new, t).err().unwrap();
        assert!(err.contains("in the last hour across every Hive") && err.contains("after"), "{err}");
        // A pending reservation counts too: with 19 committed, one pending fills the hour.
        let (dir2, g) = scratch();
        for _ in 0..PER_HOUR - 1 {
            g.reserve(&big("a"), W, Vec::new, T0).unwrap().commit(T0).unwrap();
        }
        let p = g.reserve(&big("a"), W, Vec::new, T0).unwrap();
        assert!(g.reserve(&big("b"), W, Vec::new, T0).is_err());
        drop(p);
        assert!(g.reserve(&big("b"), W, Vec::new, T0).is_ok());

        // An hour after the first, the hour has room; the day fills at PER_DAY.
        t = T0 + HOUR_MS;
        assert!(f.reserve(&big("c"), W, Vec::new, t).is_ok());
        let mut n = PER_HOUR;
        while n < PER_DAY {
            t += HOUR_MS / PER_HOUR as i64 + 1;
            f.reserve(&big("a"), W, Vec::new, t).unwrap().commit(t).unwrap();
            n += 1;
        }
        let err = f.reserve(&big("a"), W, Vec::new, t + HOUR_MS).err().unwrap();
        assert!(err.contains("in the last day"), "{err}");
        assert!(f.reserve(&big("a"), W, Vec::new, T0 + DAY_MS + HOUR_MS).is_ok(), "the first spawns left the day");
        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::remove_dir_all(&dir2);
    }

    #[test]
    fn headroom_says_when_it_next_changes_with_no_event() {
        let (dir, f) = scratch();
        let h = hive("h1", 4);
        assert_eq!(f.headroom(&h, &[], T0).changes_at, None, "nothing counted, nothing to wait for");
        f.reserve(&h, W, Vec::new, T0).unwrap().commit(T0).unwrap();
        assert_eq!(f.headroom(&h, &[], T0 + 1).changes_at, Some(T0 + HOUR_MS), "the spawn leaves the hour");
        // Past the hour, the day's window is next.
        assert_eq!(f.headroom(&h, &[], T0 + HOUR_MS).changes_at, Some(T0 + DAY_MS));
        f.trip_breaker(T0 + 60_000, "session limit");
        assert_eq!(f.headroom(&h, &[], T0 + 1).changes_at, Some(T0 + 60_000), "the breaker ends first");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_paused_or_done_hive_and_the_breaker_refuse_before_the_cap() {
        let (dir, f) = scratch();
        let mut h = hive("h1", 4);
        h.status = STATUS_PAUSED.into();
        assert!(f.reserve(&h, W, Vec::new, T0).err().unwrap().contains("paused"));
        h.status = "done".into();
        assert!(f.reserve(&h, W, Vec::new, T0).err().unwrap().contains("done"));
        h.status = STATUS_ACTIVE.into();
        assert!(f.trip_breaker(T0 + HOUR_MS, "session limit"));
        assert!(!f.trip_breaker(T0 + 60_000, "an earlier reset changes nothing"));
        let err = f.reserve(&h, W, Vec::new, T0).err().unwrap();
        assert!(err.starts_with("Paused \u{b7} usage limit until "), "{err}");
        assert_eq!(f.headroom(&h, &[], T0).breaker_until, Some(T0 + HOUR_MS));
        assert!(f.reserve(&h, W, Vec::new, T0 + HOUR_MS).is_ok(), "lifted at the reset");
        assert_eq!(f.headroom(&h, &[], T0 + HOUR_MS).breaker_until, None);
        assert!(f.trip_breaker(T0 + 2 * HOUR_MS, "later"), "a later reset extends it");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn fuses_json_survives_a_reload_with_the_breaker_and_unknown_fields() {
        let (dir, f) = scratch();
        let path = dir.join(FUSES_FILE);
        let h = hive("h1", 4);
        f.reserve(&h, W, Vec::new, T0).unwrap().commit(T0).unwrap();
        f.trip_breaker(T0 + HOUR_MS, "session limit");
        // A later build's field, written beside ours.
        let mut v: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        v["future"] = json!({ "kept": true });
        std::fs::write(&path, serde_json::to_vec(&v).unwrap()).unwrap();

        let reloaded = Fuses::at(path.clone());
        let hr = reloaded.headroom(&h, &[], T0 + 1);
        assert_eq!((hr.hour_left, hr.breaker_until), (PER_HOUR - 1, Some(T0 + HOUR_MS)));
        assert_eq!(hr.breaker_reason.as_deref(), Some("session limit"));
        assert!(reloaded.reserve(&h, W, Vec::new, T0 + 1).is_err(), "a restart doesn't lift a pause");
        reloaded.reserve(&h, W, Vec::new, T0 + HOUR_MS).unwrap().commit(T0 + HOUR_MS).unwrap();
        let back: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert_eq!(back["future"], json!({ "kept": true }));
        assert_eq!(back["version"], FUSES_VERSION);
        assert_eq!(back["spawns"].as_array().unwrap().len(), 2);
        // An old spawn leaves the file once it's a day old.
        reloaded.reserve(&h, W, Vec::new, T0 + DAY_MS + 5).unwrap().commit(T0 + DAY_MS + 5).unwrap();
        let back: FuseState = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert_eq!(back.spawns, vec![T0 + HOUR_MS, T0 + DAY_MS + 5]);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn an_unreadable_fuses_json_starts_over() {
        let (dir, f) = scratch();
        std::fs::write(dir.join(FUSES_FILE), b"{not json").unwrap();
        assert!(f.reserve(&hive("h1", 1), W, Vec::new, T0).is_ok());
        let _ = std::fs::remove_dir_all(&dir);
    }

    fn stop(error: &str, msg: &str) -> Value {
        json!({ "hook_event_name": "StopFailure", "error": error, "last_assistant_message": msg })
    }

    /// Local wall-clock `h:m` on `now`'s day, as ms.
    #[cfg(unix)]
    fn today_at(now: i64, h: i32, m: i32) -> i64 {
        local_at(now, h, m, 0).unwrap()
    }

    #[cfg(unix)]
    #[test]
    fn parse_reset_reads_claudes_session_limit() {
        let now = today_at(T0, 10, 0);
        let at = parse_reset(&stop("rate_limit", "You've hit your session limit \u{b7} resets 3pm (Europe/London)"), now);
        assert_eq!(at, Some(today_at(T0, 15, 0) + 60_000));
        let at = parse_reset(&stop("rate_limit", "You've hit your session limit \u{b7} resets 3:30pm (UTC) \u{b7} progress saved"), now);
        assert_eq!(at, Some(today_at(T0, 15, 30) + 60_000));
        let at = parse_reset(&stop("rate_limit", "You've hit your usage limit \u{b7} resets 12am (America/New_York)"), now);
        assert_eq!(at, Some(local_at(T0, 0, 0, 1).unwrap() + 60_000), "midnight is tomorrow's");
        // A time already past today is tomorrow's; one inside its own minute is now.
        let at = parse_reset(&stop("rate_limit", "You've hit your session limit \u{b7} resets 9am (UTC)"), now).unwrap();
        assert_eq!(at, local_at(T0, 9, 0, 1).unwrap() + 60_000);
        let at = parse_reset(&stop("rate_limit", "You've hit your session limit \u{b7} resets 10am (UTC)"), now + 30_000);
        assert_eq!(at, Some(now + 60_000));
        assert_eq!(local_hhmm(today_at(T0, 15, 30)), "15:30");
    }

    #[test]
    fn parse_reset_refuses_weekly_dated_and_unparseable_limits() {
        let now = T0;
        for msg in [
            "You've hit your weekly limit \u{b7} resets 3pm (UTC)",
            "You've hit your Opus limit \u{b7} resets 3pm (UTC)",
            "You've hit your Fable limit \u{b7} resets 3pm",
            "You've hit your session limit \u{b7} resets Oct 25, 3pm (UTC)",
            "You've hit your session limit",
            "You've hit your monthly spend limit.",
            "You're out of usage credits",
            "You've hit your session limit \u{b7} resets 13pm (UTC)",
            "You've hit your session limit \u{b7} resets soon",
            "No response requested.",
            "",
        ] {
            assert_eq!(parse_reset(&stop("rate_limit", msg), now), None, "{msg}");
            assert!(usage_limit_text(&stop("rate_limit", msg)).is_some(), "a limit, to pause on: {msg}");
        }
        // Not a rate limit at all: no breaker and no pause.
        assert!(usage_limit_text(&stop("server_error", "You've hit your session limit \u{b7} resets 3pm")).is_none());
        // A retried 429 is transient; one about usage or credits is a limit.
        assert!(is_transient("API Error: Request rejected (429) \u{b7} this may be a temporary capacity issue."));
        assert!(!is_transient("API Error: Usage credits required for 1M context \u{b7} run /usage-credits"));
        assert!(!is_transient("You've hit your session limit"));
        assert!(usage_limit_text(&json!({ "error": "rate_limit", "error_details": "429 quota" })).is_some());
    }

    #[test]
    fn a_coordinator_hears_claudes_words_never_a_frame() {
        let n = pause_notice("[maiTerm] \u{27e6}HIVE REPORT\u{27e7} You've hit your weekly limit\nsecond line", PauseCause::NoWait);
        assert!(n.starts_with("[maiTerm] This Hive is paused."));
        assert!(n.contains("\"HIVE REPORT You've hit your weekly limit\"") && !n.contains("second line"), "{n}");
        assert!(!n.contains('\u{27e6}'));
        assert!(n.contains("don't ask them again"), "maiTerm already asked the human: {n}");
        assert!(pause_notice("", PauseCause::Rearmed).contains("\"a usage limit\""));
        assert!(breaker_notice(T0).starts_with("[maiTerm] Paused \u{b7} usage limit until "));
    }

    /// Claude says "resets 3pm"; the breaker holds to the end of that minute but says 15:00.
    #[cfg(unix)]
    #[test]
    fn the_breaker_shows_the_reset_claude_showed() {
        let (dir, f) = scratch();
        let now = today_at(T0, 10, 0);
        let until = parse_reset(&stop("rate_limit", "You've hit your session limit \u{b7} resets 3pm (UTC)"), now).unwrap();
        assert!(f.trip_breaker(until, "session limit"));
        assert!(breaker_notice(until).contains("until 15:00."), "{}", breaker_notice(until));
        let err = f.reserve(&hive("h1", 4), W, Vec::new, now).err().unwrap();
        assert!(err.contains("until 15:00:"), "{err}");
        assert!(f.reserve(&hive("h1", 4), W, Vec::new, until - 1).is_err(), "still inside 15:00");
        assert!(f.reserve(&hive("h1", 4), W, Vec::new, until).is_ok());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[cfg(unix)]
    #[test]
    fn a_third_waited_limit_on_one_tab_pauses_because_auto_continue_has_stopped() {
        let (dir, f) = scratch();
        let now = today_at(T0, 10, 0);
        let ev = stop("rate_limit", "You've hit your session limit \u{b7} resets 3pm (UTC)");
        let until = Some(today_at(T0, 15, 0) + 60_000);
        for i in 0..REARMS as i64 {
            let a = f.assess_limit("w1", &ev, now + i, true).unwrap();
            assert_eq!((a.until, a.pause), (until, None), "Claude waits out wait {}", i + 1);
        }
        assert_eq!(f.assess_limit("w2", &ev, now, true).unwrap().pause, None, "counted per tab");
        let a = f.assess_limit("w1", &ev, now + 10, true).unwrap();
        assert_eq!((a.until, a.pause), (until, Some(PauseCause::Rearmed)), "the breaker trips and the Hive pauses");
        // A day on, the old waits have left the count.
        assert_eq!(f.assess_limit("w1", &ev, now + DAY_MS + 11, true).unwrap().pause, None);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Hive deliveries to a tab hold while its wait is on screen: typing there ends it (§7.8).
    #[cfg(unix)]
    #[test]
    fn a_tab_waiting_out_a_limit_is_known_until_its_reset() {
        let (dir, f) = scratch();
        let now = today_at(T0, 10, 0);
        let until = today_at(T0, 15, 0) + 60_000;
        assert_eq!(f.limit_wait("w1", now), None);
        f.assess_limit("w1", &stop("rate_limit", "You've hit your session limit \u{b7} resets 3pm (UTC)"), now, true).unwrap();
        assert_eq!(f.limit_wait("w1", now + 1), Some(until));
        assert_eq!(f.limit_wait("w2", now + 1), None, "per tab");
        assert_eq!(f.limit_wait("w1", until), None, "over at the reset");
        f.assess_limit("w3", &stop("rate_limit", "You've hit your weekly limit \u{b7} resets Oct 25, 3pm (UTC)"), now, true).unwrap();
        assert_eq!(f.limit_wait("w3", now), None, "nothing waits out a weekly limit, so nothing holds for it");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_429_pauses_unless_the_tab_is_known_to_run_on_a_plan() {
        let (dir, f) = scratch();
        let capacity = stop("rate_limit", "API Error: Request rejected (429) \u{b7} this may be a temporary capacity issue.");
        let api_key = stop("rate_limit", "API Error: 429 {\"type\":\"error\",\"error\":{\"type\":\"rate_limit_error\"}}");
        assert_eq!(f.assess_limit("t", &capacity, T0, true), None, "a plan's capacity 429: Claude retried, nothing to do");
        for ev in [&capacity, &api_key] {
            let a = f.assess_limit("t", ev, T0, false).unwrap();
            assert_eq!((a.until, a.pause), (None, Some(PauseCause::NoSubscription)));
        }
        let weekly = stop("rate_limit", "You've hit your weekly limit \u{b7} resets Oct 25, 3pm (UTC)");
        assert_eq!(f.assess_limit("t", &weekly, T0, true).unwrap().pause, Some(PauseCause::NoWait));
        assert_eq!(f.assess_limit("t", &stop("server_error", "boom"), T0, false), None);
        let _ = std::fs::remove_dir_all(&dir);
    }

    fn hive_state(with_coordinator: bool) -> AppData {
        use crate::state::{Tab, WindowData};
        let mut ws = Workspace::new("Hive".into());
        ws.id = "ws".into();
        ws.hive = Some(Hive { status: STATUS_ACTIVE.into(), ..hive("h1", 4) });
        if with_coordinator {
            let mut coord = Tab::new("c".into());
            coord.id = "coord".into();
            coord.hive_role = Some(super::super::ROLE_COORDINATOR.into());
            ws.panes[0].tabs = vec![coord];
        }
        let mut win = WindowData::new("main".into());
        win.workspaces = vec![ws];
        AppData { windows: vec![win], ..AppData::default() }
    }

    /// The review's case: the coordinator itself hits a weekly limit. Nobody is typed into, so
    /// the human's blocker has to come from Rust, in the same write as the pause.
    #[test]
    fn a_pause_files_the_humans_blocker_itself_and_a_resume_closes_it() {
        let mut data = hive_state(true);
        let limit = "You've hit your weekly limit \u{b7} resets Oct 25, 3pm (UTC)";
        let c = pause_in(&mut data, "h1", limit, PauseCause::NoWait, "2026-10-23T10:00:00.000Z").unwrap();
        assert_eq!((c.hive.status.as_str(), c.coordinator.as_deref()), (STATUS_PAUSED, Some("coord")));
        let (tasks, _) = c.tasks.expect("the pause row goes to the tasks store");
        assert_eq!(tasks.len(), 1);
        let t = &tasks[0];
        assert_eq!((t.title.as_str(), t.status.as_str(), t.tab_id.as_deref()), (PAUSE_TASK_TITLE, "blocked", Some("coord")));
        let b = t.blocker.as_ref().unwrap();
        assert_eq!(b.kind, "action", "a Needs-you card: it reaches the Loom's Decisions and the phone");
        assert!(b.question.contains("Resume this Hive"));
        assert!(b.context.as_deref().unwrap().contains("weekly limit"), "{:?}", b.context);
        let mut settled = t.clone();
        settled.settle_blocker();
        assert_eq!(settled.blocker, t.blocker, "valid by the one rule every writer applies");
        assert!(pause_in(&mut data, "h1", limit, PauseCause::NoWait, "x").is_none(), "already paused: not asked twice");

        let c = resume_in(&mut data, "h1", "2026-10-23T11:00:00.000Z").unwrap();
        assert_eq!(c.hive.status, STATUS_ACTIVE);
        let (tasks, _) = c.tasks.expect("the closed row goes to the tasks store");
        assert_eq!((tasks[0].status.as_str(), tasks[0].blocker.is_none()), ("done", true));

        // A second pause reuses the row and asks again.
        let c = pause_in(&mut data, "h1", limit, PauseCause::Rearmed, "2026-10-23T12:00:00.000Z").unwrap();
        let (tasks, _) = c.tasks.unwrap();
        assert_eq!(tasks.len(), 1);
        assert_eq!(tasks[0].status, "blocked");
        assert!(tasks[0].blocker.as_ref().unwrap().context.as_deref().unwrap().contains("twice today"));
    }

    #[test]
    fn a_hive_with_no_coordinator_still_asks_the_human() {
        let mut data = hive_state(false);
        let c = pause_in(&mut data, "h1", "", PauseCause::NoSubscription, "now").unwrap();
        let (tasks, _) = c.tasks.unwrap();
        assert_eq!((tasks[0].tab_id.as_deref(), tasks[0].status.as_str()), (None, "blocked"));
        assert!(tasks[0].blocker.as_ref().unwrap().context.as_deref().unwrap().contains("\"a usage limit\""));
    }

    #[test]
    fn set_status_pauses_once_and_names_the_coordinator() {
        use crate::state::{Tab, WindowData, Workspace};
        let mut ws = Workspace::new("Hive".into());
        ws.id = "ws".into();
        ws.hive = Some(hive("h1", 4));
        let mut coord = Tab::new("c".into());
        coord.id = "coord".into();
        coord.hive_role = Some(super::super::ROLE_COORDINATOR.into());
        ws.panes[0].tabs = vec![coord];
        let mut win = WindowData::new("main".into());
        win.workspaces = vec![ws];
        let mut data = AppData { windows: vec![win], ..AppData::default() };

        assert_eq!(coordinator_tabs(&data), vec!["coord".to_string()]);
        let c = set_status(&mut data, "h1", None, STATUS_PAUSED).unwrap();
        assert_eq!((c.window_label.as_str(), c.workspace_id.as_str(), c.coordinator.as_deref()), ("main", "ws", Some("coord")));
        assert_eq!(c.hive.status, STATUS_PAUSED);
        assert!(set_status(&mut data, "h1", None, STATUS_PAUSED).is_none(), "already paused: no second notice");
        assert!(set_status(&mut data, "h1", Some("done"), STATUS_ACTIVE).is_none(), "resume only from paused");
        assert!(set_status(&mut data, "h1", Some(STATUS_PAUSED), STATUS_ACTIVE).is_some());
        assert!(set_status(&mut data, "gone", None, STATUS_PAUSED).is_none());
        data.windows[0].workspaces[0].hive.as_mut().unwrap().status = "done".into();
        assert!(coordinator_tabs(&data).is_empty(), "a done Hive isn't told");
    }
}
