//! A worker gone quiet (docs/hive.md §7.4): its turn ended, nothing has happened in its tab since,
//! and nothing it could be waiting for is on its way. The coordinator is told once, so a Hive never
//! stalls on a worker that is waiting for something that won't come.
//!
//! The coordinator is event-driven: it wakes only when maiTerm types into it. A worker that ends
//! its turn saying "I'll give the verdict once my run reports", when the report was lost, leaves
//! both idle forever (2026-11-04: a reviewer and its coordinator sat 19 hours). Rust watches
//! instead of the coordinator polling with follow-ups: no turn is spent while the Hive is healthy.
//!
//! Every activity event of a worker's tab bumps its generation; a Stop or a SessionStart (a
//! relaunch after a restart) also arms a check `QUIET` later, which tells only if the generation
//! hasn't moved. Memory only: a restart relaunches the worker, whose SessionStart arms it again.

use std::collections::HashMap;
use std::sync::{Arc, LazyLock, Mutex};
use std::time::Duration;

use serde_json::Value;
use tauri::AppHandle;

use super::deliver::one_line;
use super::tools::{blocking, worker_label};
use super::worker::{self, WorkerRecord, PHASE_RUNNING};
use crate::state::AppState;

/// How long a worker's tab stays quiet before its coordinator is told.
pub const QUIET: Duration = Duration::from_secs(10 * 60);

/// Worker tab → generation, bumped by every activity event.
static GENERATIONS: LazyLock<Mutex<HashMap<String, u64>>> = LazyLock::new(|| Mutex::new(HashMap::new()));

/// Events that mean the agent did something (or something reached it). Not `Notification`:
/// Claude's own "waiting for your input" nudge comes a minute after every Stop.
fn is_activity(event: &str) -> bool {
    matches!(
        event,
        "SessionStart" | "SessionEnd" | "UserPromptSubmit" | "PreToolUse" | "PostToolUse" | "PostToolUseFailure" | "PermissionRequest" | "SubagentStop" | "PreCompact" | "Stop" | "StopFailure"
    )
}

/// Events after which the agent sits at its prompt until something is typed into it.
fn arms(event: &str) -> bool {
    matches!(event, "Stop" | "SessionStart")
}

fn bump(tab: &str) -> u64 {
    let mut g = GENERATIONS.lock().unwrap_or_else(|e| e.into_inner());
    let n = g.entry(tab.to_string()).or_insert(0);
    *n += 1;
    *n
}

fn generation(tab: &str) -> u64 {
    GENERATIONS.lock().unwrap_or_else(|e| e.into_inner()).get(tab).copied().unwrap_or(0)
}

/// Whether a quiet worker is stalled rather than waiting on something that will come: it is still
/// `running` (not reported, landing…), its task carries no blocker (a question to the human or
/// the coordinator keeps it `running`: the answer is on its way, and a nudge would invite the
/// coordinator to decide what is the human's), and no run of its own is open or has a result on
/// its way. Whether its Hive is active is the caller's: a paused one defers the check.
pub fn stalled(w: &WorkerRecord, task_blocked: bool) -> bool {
    w.phase == PHASE_RUNNING && !task_blocked && !w.pending_runs.iter().any(|r| r.awaited())
}

/// The Stop says the session wakes itself: background work or a scheduled wakeup (Claude's own
/// fields, 2.1.318: "session is paused waiting for background work to wake it").
fn wakes_itself(event: &Value) -> bool {
    ["background_tasks", "session_crons"].iter().any(|k| event.get(*k).and_then(Value::as_array).is_some_and(|a| !a.is_empty()))
}

/// The worker's task row has a blocker, as the state has it now.
fn task_blocked(state: &AppState, caller: &super::tools::Caller, task_id: &str) -> bool {
    let d = state.app_data.read();
    d.window(&caller.label)
        .and_then(|w| w.workspaces.iter().find(|ws| ws.id == caller.workspace_id))
        .and_then(|ws| ws.tasks.iter().find(|t| t.id == task_id))
        .is_some_and(|t| t.blocker.is_some())
}

/// To the coordinator.
pub fn notice(label: &str, minutes: u64, last_words: Option<&str>) -> String {
    let said = last_words.map(|t| format!(" Its last words: \"{t}\".")).unwrap_or_default();
    format!(
        "[maiTerm] Worker {label} ended its turn {minutes} min ago and has done nothing since. It hasn't reported, it has no run \
         open, and nothing is on its way to it, so it may be waiting for something that won't come.{said} Tell it what to do \
         with hiveMessageWorker, or restart it with hiveRestartWorker if it's stuck."
    )
}

/// The hook's way in (`events::observed`), for every event of a Hive tab. Cheap off the path: a
/// lock and, on a Stop or a start, a timer.
pub fn seen(state: &Arc<AppState>, handle: &AppHandle, tab_id: &str, event_name: &str, event: &Value) {
    if !is_activity(event_name) {
        return;
    }
    let is_worker = {
        let data = state.app_data.read();
        super::find_hive_tab(&data, tab_id).is_some_and(|(_, t)| t.hive_role.as_deref() == Some(super::ROLE_WORKER))
    };
    if !is_worker {
        return;
    }
    let armed = bump(tab_id);
    if !arms(event_name) || wakes_itself(event) {
        return;
    }
    let last_words = event.get("last_assistant_message").and_then(Value::as_str).map(str::trim).filter(|s| !s.is_empty()).map(|t| one_line(t, 300));
    let (state, handle, tab) = (state.clone(), handle.clone(), tab_id.to_string());
    tauri::async_runtime::spawn(async move {
        // A paused Hive defers the check, not ends it: nothing may reach the worker at Resume.
        let (caller, w) = loop {
            tokio::time::sleep(QUIET).await;
            if generation(&tab) != armed || super::prompts::has_screen_routes(&tab) {
                return;
            }
            let Some(caller) = super::tools::caller_for_tab(&state.app_data.read(), &tab) else { return };
            match caller.hive.status.as_str() {
                super::record::STATUS_ACTIVE => {}
                super::fuses::STATUS_PAUSED => continue,
                _ => return,
            }
            let (paths, t) = (caller.paths.clone(), tab.clone());
            let Ok(Some(w)) = blocking(move || Ok(worker::worker_for_tab(&paths, &t))).await else { return };
            break (caller, w);
        };
        if !stalled(&w, task_blocked(&state, &caller, &w.task_id)) {
            return;
        }
        // Told once per quiet spell: the next activity starts a new one.
        if generation(&tab) != armed {
            return;
        }
        bump(&tab);
        log::info!("hive {}: worker {} quiet for {} min: telling the coordinator", caller.hive.id, worker_label(&w), QUIET.as_secs() / 60);
        let ctx = super::tools::app_ctx(&state, &handle, &caller);
        super::events::notify(&ctx, &caller, notice(&worker_label(&w), QUIET.as_secs() / 60, last_words.as_deref())).await;
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hive::worker::{PendingRun, RUN_OPEN, RUN_RESULT};

    fn w(phase: &str, runs: &[&str]) -> WorkerRecord {
        WorkerRecord {
            phase: phase.into(),
            pending_runs: runs.iter().map(|s| PendingRun { state: (*s).into(), ..Default::default() }).collect(),
            ..Default::default()
        }
    }

    /// The live stall: a running reviewer, its `cargo test` result lost, nothing on its way.
    #[test]
    fn a_running_worker_with_nothing_coming_is_stalled() {
        assert!(stalled(&w(PHASE_RUNNING, &[]), false));
        assert!(!stalled(&w(PHASE_RUNNING, &[RUN_OPEN]), false), "its run will report");
        assert!(!stalled(&w(PHASE_RUNNING, &[RUN_RESULT]), false), "its result is on its way");
        assert!(!stalled(&w(PHASE_RUNNING, &[]), true), "its task's blocker will be answered");
        // A served app or service copy runs for hours and reports nothing the worker waits on.
        let mut serving = w(PHASE_RUNNING, &[RUN_OPEN]);
        serving.pending_runs[0].server = true;
        assert!(stalled(&serving, false), "a server is no run it waits on");
        for p in [worker::PHASE_REPORTED, worker::PHASE_BLOCKED, worker::PHASE_LANDING, worker::PHASE_RETIRED, worker::PHASE_WAITING] {
            assert!(!stalled(&w(p, &[]), false), "{p}");
        }
    }

    /// Claude's Stop says when background work or a scheduled wakeup will wake the session.
    #[test]
    fn a_session_that_wakes_itself_isnt_armed() {
        use serde_json::json;
        assert!(wakes_itself(&json!({ "background_tasks": [{ "id": "b1" }] })));
        assert!(wakes_itself(&json!({ "session_crons": [{ "id": "c1" }] })));
        assert!(!wakes_itself(&json!({ "background_tasks": [], "session_crons": [] })));
        assert!(!wakes_itself(&json!({})), "an older Claude sends neither");
    }

    #[test]
    fn only_activity_counts_and_only_an_end_arms() {
        assert!(!is_activity("Notification"), "Claude's idle nudge after every Stop");
        assert!(!is_activity("PreModelSwitch"));
        assert!(is_activity("PreToolUse") && is_activity("UserPromptSubmit") && is_activity("Stop"));
        assert!(arms("Stop") && arms("SessionStart"));
        assert!(!arms("StopFailure"), "turn_failure tells of it");
        assert!(!arms("PreToolUse"));
    }

    #[test]
    fn a_new_event_cancels_the_check() {
        let tab = format!("t-{}", uuid::Uuid::new_v4());
        let armed = bump(&tab);
        assert_eq!(generation(&tab), armed);
        bump(&tab);
        assert_ne!(generation(&tab), armed);
    }

    #[test]
    fn the_notice_says_what_to_do_and_quotes_the_worker() {
        let n = notice("#18 review", 10, Some("I'll give the verdict once the cargo test job reports"));
        assert!(n.starts_with("[maiTerm] Worker #18 review ended its turn 10 min ago"), "{n}");
        assert!(n.contains("hiveMessageWorker") && n.contains("hiveRestartWorker") && n.contains("cargo test job reports"), "{n}");
        assert!(!notice("#2 a", 10, None).contains("last words"));
    }
}
