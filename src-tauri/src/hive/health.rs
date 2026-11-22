//! The queen's regular look at her workers (Darryl, 2026-11-21: "since stalls are so common and
//! all our efforts to prevent them have failed, maybe queen needs a 15 min followup timer to check
//! the worker health and resolve any stalls").
//!
//! Every `settings.autonomy.health_check_minutes` (15 by default, 0 off), each active Hive with a
//! queen tab is looked at. The workers that may be stalled are named to the queen in one framed
//! notice (⟦HIVE CHECK⟧) with what to do: a worker failed or stopped, a prompt on its screen, a
//! blocker on its task, or nothing from it for the whole interval (a reported worker nobody has
//! landed or reviewed included). A Hive whose unfinished workers all worked within the interval
//! gets nothing, so a healthy Hive spends no turn, and each worker's concern is told once, again
//! only when it changes (`TOLD`), so a stall the queen knows of or can't act on isn't repeated. Workers waiting on another's work are expected
//! to wait, and landed or retired ones are done.
//!
//! The event-driven checks stay (`quiet`: a worker quiet after its turn ended, told once); this is
//! the net under them for whatever they miss. The interval is the behaviour here, so it is a clock,
//! not a wait on an event (docs/timing-audit.md class C). Memory only: after a restart the first
//! check comes one interval later, and a worker with no activity since then reads as "nothing
//! since maiTerm started".

use std::collections::HashMap;
use std::sync::{Arc, LazyLock, Mutex};
use std::time::Duration;

use super::tools::{blocking, worker_label};
use super::worker::{self, WorkerRecord, PHASE_BLOCKED, PHASE_FAILED, PHASE_LANDED, PHASE_RETIRED, PHASE_STOPPED, PHASE_WAITING};

pub const DEFAULT_MINUTES: u32 = 15;
pub const MAX_MINUTES: u32 = 240;

/// Hive id → when its queen was last sent a check (ms).
static LAST_SENT: LazyLock<Mutex<HashMap<String, i64>>> = LazyLock::new(|| Mutex::new(HashMap::new()));
/// Worker task id → the concern last reported for it (`signature`). A concern is told once, and
/// again only when it changes: a worker the queen stopped, a land held for the human, a blocker
/// the human hasn't answered or one long tool call would otherwise cost her a turn every interval
/// about something she already knows or can't act on (the review).
static TOLD: LazyLock<Mutex<HashMap<String, String>>> = LazyLock::new(|| Mutex::new(HashMap::new()));

/// What a worker's concern is, without its minutes: told again only when this changes.
pub fn signature(w: &WorkerRecord, s: &Seen, now: i64, every_ms: i64) -> String {
    let idle = s.last_active.map_or(true, |t| now - t >= every_ms);
    format!("{}|{}|{}|{}|{}", w.phase, s.prompt, s.blocker, idle, s.last_active.is_some())
}

/// The workers whose concern wasn't told yet, as it stands now; the rest are remembered as told.
/// A worker with no concern is forgotten, so its next stall is told.
fn untold(workers: Vec<(WorkerRecord, Seen)>, now: i64, every_ms: i64) -> Vec<(WorkerRecord, Seen)> {
    let mut told = TOLD.lock().unwrap_or_else(|e| e.into_inner());
    workers
        .into_iter()
        .filter_map(|(w, s)| {
            if concern(&w, &s, now, every_ms).is_none() {
                told.remove(&w.task_id);
                return Some((w, s));
            }
            let sig = signature(&w, &s, now, every_ms);
            if told.get(&w.task_id) == Some(&sig) {
                return None;
            }
            told.insert(w.task_id.clone(), sig);
            Some((w, s))
        })
        .collect()
}

/// What the check knows of one worker beyond its record.
#[derive(Debug, Clone, Default)]
pub struct Seen {
    /// Its tab's last agent activity (ms), None when none since maiTerm started.
    pub last_active: Option<i64>,
    /// A permission prompt or question is on its screen.
    pub prompt: bool,
    /// Its task row carries a blocker.
    pub blocker: bool,
}

/// Why a worker may be stalled, in the queen's words, or None while it looks fine.
pub fn concern(w: &WorkerRecord, s: &Seen, now: i64, every_ms: i64) -> Option<String> {
    if matches!(w.phase.as_str(), PHASE_LANDED | PHASE_RETIRED | PHASE_WAITING) {
        return None;
    }
    let mut why = Vec::new();
    match w.phase.as_str() {
        PHASE_FAILED => why.push("it failed".to_string()),
        PHASE_STOPPED => why.push("it is stopped".to_string()),
        PHASE_BLOCKED => why.push("it is blocked".to_string()),
        _ => {}
    }
    if s.prompt {
        why.push("a prompt is on its screen".to_string());
    }
    if s.blocker {
        why.push("its task waits on a blocker".to_string());
    }
    match s.last_active {
        Some(t) if now - t >= every_ms => why.push(format!("nothing from it for {} min", (now - t) / 60_000)),
        None => why.push("nothing from it since maiTerm started".to_string()),
        _ => {}
    }
    (!why.is_empty()).then(|| why.join("; "))
}

/// The notice, or None when no worker needs a look.
pub fn check_text(workers: &[(WorkerRecord, Seen)], now: i64, minutes: u32) -> Option<String> {
    let every_ms = i64::from(minutes) * 60_000;
    let mut lines = Vec::new();
    let mut fine = 0;
    for (w, s) in workers {
        match concern(w, s, now, every_ms) {
            Some(why) => lines.push(format!("- {} ({}): {why}.", worker_label(w), w.phase)),
            None if !matches!(w.phase.as_str(), PHASE_LANDED | PHASE_RETIRED) => fine += 1,
            None => {}
        }
    }
    if lines.is_empty() {
        return None;
    }
    let others = match fine {
        0 => String::new(),
        1 => "\n1 other worker is working or waiting on another's work.".to_string(),
        n => format!("\n{n} other workers are working or waiting on another's work."),
    };
    Some(format!(
        "[maiTerm] ⟦HIVE CHECK⟧ Your {minutes}-minute look at the workers. These may be stalled:\n{}{others}\n\nFor each, find out what \
         it is waiting for and unblock it now: answer its prompt or blocker (hiveAnswerWorker), grant what it lacks \
         (hiveChangeSettings: a host, a database socket, a tool), tell it what to do (hiveMessageWorker), land or review a \
         reported worker, or restart a stuck one (hiveRestartWorker; its session resumes). Raise a blocker to your human only \
         for what only they can decide. If nothing needs doing, end your turn without a reply.",
        lines.join("\n")
    ))
}

/// Whether a Hive's check is due now, and if so marks it sent.
fn due(hive_id: &str, now: i64, minutes: u32, started: i64) -> bool {
    let every = i64::from(minutes) * 60_000;
    let mut m = LAST_SENT.lock().unwrap_or_else(|e| e.into_inner());
    let last = *m.get(hive_id).unwrap_or(&started);
    if now - last < every {
        return false;
    }
    m.insert(hive_id.to_string(), now);
    true
}

/// One Hive looked at, its queen told when a worker needs a look.
async fn check_one(state: &Arc<crate::state::AppState>, handle: &tauri::AppHandle, caller: &super::tools::Caller, now: i64) {
    let paths = caller.paths.clone();
    let chapter = caller.hive.chapter();
    let Ok(workers) = blocking(move || Ok(worker::list_workers(&paths))).await else { return };
    let tasks: Vec<(String, bool)> = {
        let d = state.app_data.read();
        d.window(&caller.label)
            .and_then(|w| w.workspaces.iter().find(|ws| ws.id == caller.workspace_id))
            .map(|ws| ws.tasks.iter().map(|t| (t.id.clone(), t.blocker.is_some())).collect())
            .unwrap_or_default()
    };
    let seen: Vec<(WorkerRecord, Seen)> = workers
        .into_iter()
        .filter(|w| w.chapter == chapter)
        .map(|w| {
            let tab = w.tab_id.clone().unwrap_or_default();
            let s = Seen {
                last_active: super::quiet::last_active(&tab),
                prompt: !tab.is_empty() && super::prompts::has_screen_routes(&tab),
                blocker: tasks.iter().any(|(id, b)| *id == w.task_id && *b),
            };
            (w, s)
        })
        .collect();
    let minutes = caller.hive.settings.autonomy.health_check_minutes;
    let seen = untold(seen, now, i64::from(minutes) * 60_000);
    let Some(text) = check_text(&seen, now, minutes) else { return };
    log::info!("hive {}: {minutes}-minute check: telling the queen", caller.hive.id);
    let ctx = super::tools::app_ctx(state, handle, caller);
    super::events::notify(&ctx, caller, text).await;
}

/// Every Hive looked at once a minute for a check that is due, for the app's life.
pub fn start_timer(app: Arc<crate::state::AppState>, handle: tauri::AppHandle) {
    tauri::async_runtime::spawn(async move {
        let started = super::fuses::now_ms();
        loop {
            tokio::time::sleep(Duration::from_secs(60)).await;
            let now = super::fuses::now_ms();
            let callers = super::tools::hive_callers(&app.app_data.read());
            for caller in callers.into_iter().filter(|c| !c.tab_id.is_empty() && c.hive.status == super::record::STATUS_ACTIVE) {
                let minutes = caller.hive.settings.autonomy.health_check_minutes.min(MAX_MINUTES);
                if minutes == 0 || !due(&caller.hive.id, now, minutes, started) {
                    continue;
                }
                check_one(&app, &handle, &caller, now).await;
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hive::worker::{PHASE_REPORTED, PHASE_RUNNING};

    fn w(badge: u32, phase: &str) -> WorkerRecord {
        WorkerRecord { badge, slug: format!("w{badge}"), phase: phase.into(), ..WorkerRecord::default() }
    }

    const NOW: i64 = 10_000_000;
    const MIN: i64 = 60_000;

    #[test]
    fn a_healthy_hive_gets_no_check() {
        let busy = Seen { last_active: Some(NOW - 2 * MIN), ..Default::default() };
        let ws = vec![(w(1, PHASE_RUNNING), busy.clone()), (w(2, PHASE_WAITING), Seen::default()), (w(3, PHASE_LANDED), Seen::default())];
        assert!(check_text(&ws, NOW, 15).is_none(), "working, waiting on another and landed: nothing to look at");
        assert!(check_text(&[], NOW, 15).is_none());
    }

    #[test]
    fn a_stall_names_each_worker_and_why() {
        let ws = vec![
            (w(1, PHASE_RUNNING), Seen { last_active: Some(NOW - 42 * MIN), blocker: true, ..Default::default() }),
            (w(2, PHASE_REPORTED), Seen { last_active: Some(NOW - 20 * MIN), ..Default::default() }),
            (w(3, PHASE_RUNNING), Seen { last_active: Some(NOW - MIN), prompt: true, ..Default::default() }),
            (w(4, PHASE_FAILED), Seen { last_active: Some(NOW - MIN), ..Default::default() }),
            (w(5, PHASE_RUNNING), Seen { last_active: None, ..Default::default() }),
            (w(6, PHASE_RUNNING), Seen { last_active: Some(NOW - MIN), ..Default::default() }),
        ];
        let t = check_text(&ws, NOW, 15).unwrap();
        assert!(t.contains("⟦HIVE CHECK⟧") && t.contains("15-minute"), "{t}");
        assert!(t.contains("#1 w1 (running): its task waits on a blocker; nothing from it for 42 min."), "{t}");
        assert!(t.contains("#2 w2 (reported): nothing from it for 20 min."), "a reported worker nobody landed: {t}");
        assert!(t.contains("#3 w3 (running): a prompt is on its screen."), "{t}");
        assert!(t.contains("#4 w4 (failed): it failed."), "{t}");
        assert!(t.contains("#5 w5 (running): nothing from it since maiTerm started."), "{t}");
        assert!(!t.contains("#6 w6") && t.contains("1 other worker is working"), "{t}");
        assert!(t.contains("hiveChangeSettings") && t.contains("hiveRestartWorker") && t.contains("end your turn without a reply"), "{t}");
    }

    /// A held land, a stopped worker or an unanswered blocker is told once, not every interval;
    /// a change in it (or a fresh stall after it recovered) is told again.
    #[test]
    fn a_concern_is_told_once_until_it_changes() {
        let id = uuid::Uuid::new_v4().to_string();
        let held = WorkerRecord { task_id: id.clone(), ..w(1, crate::hive::worker::PHASE_LANDING) };
        let idle = |mins: i64| Seen { last_active: Some(NOW - mins * MIN), ..Default::default() };
        let every = 15 * MIN;
        assert_eq!(untold(vec![(held.clone(), idle(20))], NOW, every).len(), 1, "told the first time");
        assert!(untold(vec![(held.clone(), idle(35))], NOW, every).is_empty(), "not again: only its minutes grew");
        let blocked = Seen { blocker: true, ..idle(50) };
        assert_eq!(untold(vec![(held.clone(), blocked)], NOW, every).len(), 1, "a new blocker changes the concern");
        assert_eq!(untold(vec![(held.clone(), idle(1))], NOW, every).len(), 1, "busy again: forgotten, and passed through as fine");
        assert_eq!(untold(vec![(held, idle(20))], NOW, every).len(), 1, "so its next stall is told");
    }

    #[test]
    fn a_check_is_due_once_per_interval() {
        let id = format!("h-{}", uuid::Uuid::new_v4());
        assert!(!due(&id, NOW, 15, NOW - 14 * MIN), "not before an interval since maiTerm started");
        assert!(due(&id, NOW, 15, NOW - 15 * MIN));
        assert!(!due(&id, NOW + 14 * MIN, 15, 0), "marked sent");
        assert!(due(&id, NOW + 15 * MIN, 15, 0));
    }
}
