//! What a worker's own hooks tell maiTerm (docs/hive.md §6.7, §11.3), off the hook's path.
//!
//! A crash: the agent left the foreground with its shell at its prompt, or the tab's terminal
//! died, while the worker was running with no report since its launch. It goes `failed` and the
//! coordinator is told; a stop is marked before its signal, a suspend or reload kills the PTY
//! (never read as a death), and a report moves the phase off `running`, so none of those count.
//!
//! A sandbox refusal: a failed call whose output names a host or path the sandbox refused
//! becomes an `external` blocker on the worker's task, once per target, and the coordinator,
//! which can't widen a worker's sandbox, is told to raise its own blocker to the human.

use std::sync::Arc;
use std::time::Duration;

use serde_json::{json, Value};
use tauri::AppHandle;

use super::deliver::one_line;
use super::rpc::{self, Front, HiveWindow};
use super::sandbox_events::{self, Violation};
use super::tools::{blocking, worker_label, Caller, Ctx};
use super::worker::{self, WorkerRecord};
use crate::state::AppState;

/// The phases a crash moves to `failed`: a launch whose agent never reported.
const CRASHABLE: [&str; 2] = [worker::PHASE_LAUNCHING, worker::PHASE_RUNNING];
/// A SessionEnd comes as the agent exits: the shell has its prompt back a moment later.
const SETTLE: Duration = Duration::from_secs(2);

/// Every event a Hive tab's own command hook forwards (`via=hive`). Called before the hooks
/// handler's mod dedup, which would drop this copy for the mod's: only this one says it is a
/// Hive tab's, and the tab is checked against the state here.
pub fn observed(state: &Arc<AppState>, handle: &AppHandle, tab_id: &str, event_name: &str, event: &Value) {
    if !matches!(event_name, "SessionEnd" | "PostToolUseFailure") {
        return;
    }
    let Some(caller) = worker_caller(state, tab_id) else { return };
    let (state, handle, tab, event, name) = (state.clone(), handle.clone(), tab_id.to_string(), event.clone(), event_name.to_string());
    tauri::async_runtime::spawn(async move {
        let ctx = super::tools::app_ctx(&state, &handle, &caller);
        if name == "SessionEnd" {
            // A `/clear` ends one session and starts the next in the same process.
            if event.get("reason").and_then(Value::as_str) == Some("clear") {
                return;
            }
            tokio::time::sleep(SETTLE).await;
            if session_ended(&ctx, &caller, &tab).await {
                super::waiters::poll_soon(state.clone(), handle.clone(), caller.workspace_id.clone());
            }
        } else {
            tool_failed(&ctx, &caller, &tab, &event).await;
        }
    });
}

/// A worker tab's terminal died on its own (not killed by maiTerm: a suspend, close or reload
/// sends its own kill, which never comes here).
///
/// The tab may be gone already: the window closes a tab whose terminal closed, and its delete
/// can land before the PTY's thread sees the exit. So a tab the state no longer has is looked
/// up in the Hives' worker records, which name it until a restart gives the worker another.
pub fn pty_exited(state: &Arc<AppState>, handle: &AppHandle, tab_id: &str) {
    let candidates = {
        let data = state.app_data.read();
        if data.windows.iter().flat_map(|w| w.workspaces.iter()).flat_map(|ws| ws.panes.iter()).any(|p| p.tabs.iter().any(|t| t.id == tab_id)) {
            match worker_caller(state, tab_id) {
                Some(c) => vec![c],
                None => return,
            }
        } else {
            super::tools::hive_callers(&data)
        }
    };
    if candidates.is_empty() {
        return;
    }
    let (state, handle, tab) = (state.clone(), handle.clone(), tab_id.to_string());
    tauri::async_runtime::spawn(async move {
        let t = tab.clone();
        let Ok(Some(caller)) = blocking(move || Ok(owner_of(candidates, &t))).await else { return };
        let ctx = super::tools::app_ctx(&state, &handle, &caller);
        if mark_failed(&ctx, &caller, &tab, "its terminal closed").await {
            super::waiters::poll_soon(state.clone(), handle.clone(), caller.workspace_id.clone());
        }
    });
}

/// The Hive among `candidates` whose worker records name `tab_id`. Blocks.
pub(crate) fn owner_of(candidates: Vec<Caller>, tab_id: &str) -> Option<Caller> {
    candidates.into_iter().find(|c| worker::worker_for_tab(&c.paths, tab_id).is_some())
}

fn worker_caller(state: &Arc<AppState>, tab_id: &str) -> Option<Caller> {
    let data = state.app_data.read();
    let (_, tab) = super::find_hive_tab(&data, tab_id)?;
    if tab.hive_role.as_deref() != Some(super::ROLE_WORKER) {
        return None;
    }
    super::tools::caller_for_tab(&data, tab_id)
}

/// The agent's session ended: a crash only if its shell is back at its prompt. A terminal that's
/// gone was killed (a suspend, a reload) or died, which `pty_exited` hears; another job in front
/// is something else's. True when the worker went `failed`.
pub(crate) async fn session_ended<W: HiveWindow>(ctx: &Ctx<'_, W>, caller: &Caller, tab: &str) -> bool {
    if ctx.window.front(tab).await != Front::Shell {
        return false;
    }
    mark_failed(ctx, caller, tab, "its agent exited").await
}

/// `failed`, when the worker is still running with no report since its launch. True when it moved.
pub(crate) async fn mark_failed<W: HiveWindow>(ctx: &Ctx<'_, W>, caller: &Caller, tab: &str, how: &str) -> bool {
    let (paths, tab_id) = (caller.paths.clone(), tab.to_string());
    let moved = blocking(move || {
        let Some(w) = worker::worker_for_tab(&paths, &tab_id) else { return Ok(None) };
        let dir = worker::task_dir(&paths, &w.task_id)?;
        let mut moved = false;
        let after = worker::update_worker(&dir, |w| {
            moved = CRASHABLE.contains(&w.phase.as_str()) && w.tab_id.as_deref() == Some(tab_id.as_str());
            if moved {
                w.phase = worker::PHASE_FAILED.to_string();
            }
            moved
        })?;
        Ok(moved.then_some(after))
    })
    .await;
    let w = match moved {
        Ok(Some(w)) => w,
        Ok(None) => return false,
        Err(e) => {
            log::warn!("hive {}: tab {tab}: crash not recorded: {e}", caller.hive.id);
            return false;
        }
    };
    log::warn!("hive {}: worker {} failed: {how} without a report", caller.hive.id, worker_label(&w));
    ctx.window.workers_changed(&caller.workspace_id);
    let note = format!("Failed: {how} without a report");
    if let Err(e) = super::control::task_note(ctx, caller, &w.task_id, &note, None).await {
        log::info!("hive {}: worker {} failure note not written: {e}", caller.hive.id, worker_label(&w));
    }
    notify(
        ctx,
        caller,
        format!(
            "[maiTerm] Worker {} stopped without reporting: {how}. Its worktrees and branch are kept. hiveRestartWorker resumes \
             it (fresh: true starts a new session that reads its brief again); hiveStopWorker retires it.",
            worker_label(&w)
        ),
    )
    .await;
    true
}

/// A worker's failed call: each target the sandbox refused that it hasn't been told about yet.
pub(crate) async fn tool_failed<W: HiveWindow>(ctx: &Ctx<'_, W>, caller: &Caller, tab: &str, event: &Value) {
    let (paths, tab_id, ev) = (caller.paths.clone(), tab.to_string(), event.clone());
    let found = blocking(move || {
        let Some(w) = worker::worker_for_tab(&paths, &tab_id) else { return Ok(None) };
        let dir = worker::task_dir(&paths, &w.task_id)?;
        let settings = super::launch::read_record(&dir)
            .ok()
            .and_then(|r| std::fs::read(&r.settings_path).ok())
            .and_then(|b| serde_json::from_slice::<Value>(&b).ok())
            .unwrap_or(Value::Null);
        let writable = sandbox_events::Writable::from_settings(&settings, &w.cwd(&paths));
        let all = sandbox_events::parse_violations(&ev, &writable);
        if all.is_empty() {
            return Ok(None);
        }
        Ok(Some((w, dir, all)))
    })
    .await;
    let (w, dir, all) = match found {
        Ok(Some(f)) => f,
        Ok(None) => return,
        Err(e) => {
            log::warn!("hive {}: tab {tab}: sandbox refusal not read: {e}", caller.hive.id);
            return;
        }
    };
    // Recorded before it is filed, under the record's lock: two hooks reporting one refusal
    // file one blocker.
    let fresh = blocking(move || {
        let mut fresh: Vec<Violation> = Vec::new();
        worker::update_worker(&dir, |w| {
            fresh = all.into_iter().filter(|v| !w.violations_seen.contains(&v.key())).collect();
            w.violations_seen.extend(fresh.iter().map(Violation::key));
            !fresh.is_empty()
        })?;
        Ok(fresh)
    })
    .await
    .unwrap_or_default();
    if fresh.is_empty() {
        return;
    }
    file_sandbox_blocker(ctx, caller, &w, &fresh).await;
}

fn describe(v: &Violation) -> String {
    format!("{} {}", v.kind, one_line(&v.target, 200))
}

async fn file_sandbox_blocker<W: HiveWindow>(ctx: &Ctx<'_, W>, caller: &Caller, w: &WorkerRecord, fresh: &[Violation]) {
    let names: Vec<String> = fresh.iter().map(describe).collect();
    let now = crate::commands::workspace::iso_now();
    let context = (names.len() > 1).then(|| format!("Also refused: {}", names[1..].join(", ")));
    let blocker = json!({
        "kind": "external",
        "question": format!("The sandbox refused {}", names[0]),
        "context": context,
        "asked_at": now,
        "asked_by": "agent",
    });
    let args = json!({
        "workspace_id": caller.workspace_id,
        "task_id": w.task_id,
        "status": "blocked",
        "blocker": blocker,
        "note": format!("The sandbox refused {}", names.join(", ")),
    });
    if let Err(e) = rpc::applied(ctx.window.request(rpc::VERB_TASK, args).await) {
        log::warn!("hive {}: worker {} sandbox blocker not filed: {e}", caller.hive.id, worker_label(w));
    }
    log::info!("hive {}: worker {}: the sandbox refused {}", caller.hive.id, worker_label(w), names.join(", "));
    ctx.window.workers_changed(&caller.workspace_id);
    notify(
        ctx,
        caller,
        format!(
            "[maiTerm] Worker {}'s sandbox refused {}. You can't widen a worker's sandbox. If the work needs it, raise your own \
             blocker to the human naming it; if not, tell the worker (hiveMessageWorker) to do without it.",
            worker_label(w),
            names.join(", ")
        ),
    )
    .await;
}

async fn notify<W: HiveWindow>(ctx: &Ctx<'_, W>, caller: &Caller, text: String) {
    let Some(c) = super::tools::coordinator_tab(ctx.data, caller) else { return };
    let sent = ctx.window.deliver(&c, text, "Hive notice").await;
    log::info!("hive {}: notice to the coordinator: {}", caller.hive.id, sent.describe());
}
