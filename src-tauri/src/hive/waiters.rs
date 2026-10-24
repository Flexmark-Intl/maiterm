//! Workers waiting on `depends_on` (docs/hive.md §3.1): a waiter launches once every dependency's
//! task is done (landing sets that in step 6; a human can by hand now), re-checked against the
//! cap, the fuses and the breaker at that moment. Refused, it keeps waiting with the reason on
//! its card. A dependency that failed, was dropped or whose land was undone blocks it, and the
//! coordinator is told once.
//!
//! Polled after every task write of a Hive workspace (`set_workspace_tasks`) and after a worker's
//! phase changes (a report, a stop, a crash, a resumed Hive). One poll per Hive at a time; a
//! request meanwhile runs it once more after, so no change is missed and no waiter launches twice.
//! The same poll releases the queued and running jobs of a worker whose task was dropped, and
//! retires a reviewer that gave its verdict but wasn't retired (`review::retire_owed`).

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use tauri::AppHandle;

use super::deliver::one_line;
use super::rpc::HiveWindow;
use super::tools::{blocking, launch_waiting, worker_label, Caller, Ctx};
use super::worker::{self, WorkerRecord};
use crate::state::{AppState, Task};

/// A launch that failed past its reservation (a worktree, the trust check, the window) is tried
/// again after this, not on every task write: each try makes and removes worktrees.
const BACKOFF_MS: i64 = 5 * 60_000;

/// Hives with a poll running → whether another was asked for meanwhile.
static POLLING: Mutex<Option<HashMap<String, bool>>> = Mutex::new(None);
/// Waiter task id → no launch try before this (ms).
static BACKOFF: Mutex<Option<HashMap<String, i64>>> = Mutex::new(None);

/// Polls the Hive in `workspace_id` off the caller's path. Nothing for a workspace that isn't one.
pub fn poll_soon(state: Arc<AppState>, handle: AppHandle, workspace_id: String) {
    tauri::async_runtime::spawn(async move {
        let Some(caller) = super::tools::caller_for_workspace(&state.app_data.read(), &workspace_id) else { return };
        let ctx = super::tools::app_ctx(&state, &handle, &caller);
        poll(&ctx, &caller).await;
    });
}

pub(crate) async fn poll<W: HiveWindow>(ctx: &Ctx<'_, W>, caller: &Caller) {
    {
        let mut g = POLLING.lock().unwrap_or_else(|e| e.into_inner());
        let running = g.get_or_insert_with(HashMap::new);
        if let Some(again) = running.get_mut(&caller.hive.id) {
            *again = true;
            return;
        }
        running.insert(caller.hive.id.clone(), false);
    }
    // A poll that panics or is dropped mid-await must not leave its Hive marked as polling forever.
    struct Running<'a>(&'a str, bool);
    impl Drop for Running<'_> {
        fn drop(&mut self) {
            if !self.1 {
                if let Some(m) = POLLING.lock().unwrap_or_else(|e| e.into_inner()).as_mut() {
                    m.remove(self.0);
                }
            }
        }
    }
    let mut guard = Running(&caller.hive.id, false);
    loop {
        poll_once(ctx, caller).await;
        let mut g = POLLING.lock().unwrap_or_else(|e| e.into_inner());
        let running = g.get_or_insert_with(HashMap::new);
        if running.get(&caller.hive.id).copied().unwrap_or(false) {
            running.insert(caller.hive.id.clone(), false);
            continue;
        }
        // Removed under the same lock that read "no more asked", so no request falls between.
        running.remove(&caller.hive.id);
        guard.1 = true;
        break;
    }
}

/// Where a waiter stands against its dependencies.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Standing {
    /// Every dependency's task is done (or gone, which no longer blocks).
    Ready,
    /// Some aren't done yet: "waits on #1 schema, #2 api".
    Waits(String),
    /// One failed, was dropped or was undone: "blocked: #1 schema failed".
    Blocked(String),
}

/// A dependency is met when its task is done, or its row is gone (a deleted prerequisite no
/// longer blocks, docs/tasks.md). Failed or dropped blocks the waiter outright.
pub(crate) fn standing(w: &WorkerRecord, workers: &[WorkerRecord], tasks: &[Task]) -> Standing {
    let mut waits: Vec<String> = Vec::new();
    for dep in &w.depends_on {
        let rec = workers.iter().find(|x| &x.task_id == dep);
        let row = tasks.iter().find(|t| &t.id == dep);
        // A title is any agent's to write and the reason reaches the coordinator's notice: escaped.
        let name = rec
            .map(worker_label)
            .or_else(|| row.map(|t| format!("\"{}\"", one_line(&t.title, 120))))
            .unwrap_or_else(|| one_line(dep, 80));
        if row.is_some_and(|t| t.status == "dropped") {
            return Standing::Blocked(format!("blocked: {name} was dropped"));
        }
        if rec.is_some_and(|x| x.phase == worker::PHASE_FAILED) && row.is_none_or(|t| t.status != "done") {
            return Standing::Blocked(format!("blocked: {name} failed"));
        }
        // Its task stays done, but what it landed was taken back (§8.6).
        if rec.is_some_and(|x| x.undone) {
            return Standing::Blocked(format!("blocked: {name}'s land was undone"));
        }
        if row.is_some_and(|t| t.status != "done") {
            waits.push(name);
        }
    }
    if waits.is_empty() {
        Standing::Ready
    } else {
        Standing::Waits(format!("waits on {}", waits.join(", ")))
    }
}

/// Sets a waiter's reason; true when it changed. Only while it still waits.
fn set_reason(caller: &Caller, w: &WorkerRecord, reason: &str) -> bool {
    let Ok(dir) = worker::task_dir(&caller.paths, &w.task_id) else { return false };
    let mut changed = false;
    let res = worker::update_worker(&dir, |cur| {
        changed = cur.phase == worker::PHASE_WAITING && cur.wait_reason.as_deref() != Some(reason);
        if changed {
            cur.wait_reason = Some(reason.to_string());
        }
        changed
    });
    if let Err(e) = res {
        log::warn!("hive {}: worker {} wait reason not written: {e}", caller.hive.id, worker_label(w));
        return false;
    }
    changed
}

fn backed_off(task_id: &str, now: i64) -> bool {
    let mut g = BACKOFF.lock().unwrap_or_else(|e| e.into_inner());
    let map = g.get_or_insert_with(HashMap::new);
    map.retain(|_, until| now < *until);
    map.contains_key(task_id)
}

fn back_off(task_id: &str, now: i64) {
    BACKOFF.lock().unwrap_or_else(|e| e.into_inner()).get_or_insert_with(HashMap::new).insert(task_id.to_string(), now + BACKOFF_MS);
}

/// Workers whose own task row was dropped: what they have in the run queue is released (§6.8),
/// told once. Only an explicit `dropped`: a row missing from a read never releases anyone.
/// Answers how many runs went.
pub(crate) fn release_dropped(workers: &[WorkerRecord], tasks: &[Task]) -> usize {
    workers
        .iter()
        .filter(|w| tasks.iter().any(|t| t.id == w.task_id && t.status == "dropped"))
        .map(|w| {
            let n = super::queue::release_worker(&w.task_id, "your task was dropped");
            if n > 0 {
                log::info!("hive: {n} run(s) of {} released: its task was dropped", worker_label(w));
            }
            n
        })
        .sum()
}

/// One pass over the Hive's waiters, in badge order. Returns the slugs launched.
pub(crate) async fn poll_once<W: HiveWindow>(ctx: &Ctx<'_, W>, caller: &Caller) -> Vec<String> {
    let Ok(tasks) = super::tools::workspace_tasks(ctx.data, caller) else { return Vec::new() };
    let paths = caller.paths.clone();
    let Ok(workers) = blocking(move || Ok(worker::list_workers(&paths))).await else { return Vec::new() };
    release_dropped(&workers, &tasks);
    let owed = super::review::retire_owed(ctx, caller, &workers).await;
    if owed > 0 {
        log::info!("hive {}: {owed} reviewer(s) retired that had given their verdict", caller.hive.id);
    }
    let mut launched: Vec<String> = Vec::new();
    for w in workers.iter().filter(|w| w.phase == worker::PHASE_WAITING) {
        // Its own row gone or retired: the coordinator or the human took it off the board.
        let Some(row) = tasks.iter().find(|t| t.id == w.task_id && !matches!(t.status.as_str(), "done" | "dropped")) else { continue };
        match standing(w, &workers, &tasks) {
            Standing::Blocked(reason) => {
                let (c, w2, r) = (caller.clone(), w.clone(), reason.clone());
                let news = blocking(move || Ok(set_reason(&c, &w2, &r))).await.unwrap_or(false);
                if news {
                    log::info!("hive {}: worker {} {reason}", caller.hive.id, worker_label(w));
                    tell_coordinator(
                        ctx,
                        caller,
                        format!(
                            "[maiTerm] Worker {} can't launch: it depends on work that won't come ({reason}). Stop it \
                             (hiveStopWorker), or spawn what it needs and a replacement that depends on that.",
                            worker_label(w)
                        ),
                    )
                    .await;
                    ctx.window.workers_changed(&caller.workspace_id);
                }
            }
            Standing::Waits(reason) => {
                let (c, w2) = (caller.clone(), w.clone());
                if blocking(move || Ok(set_reason(&c, &w2, &reason))).await.unwrap_or(false) {
                    ctx.window.workers_changed(&caller.workspace_id);
                }
            }
            Standing::Ready => {
                let now = super::fuses::now_ms();
                if backed_off(&w.task_id, now) {
                    continue;
                }
                match launch_waiting(ctx, caller, w.clone(), row.clone()).await {
                    Ok(_) => {
                        log::info!("hive {}: waiting worker {} launched", caller.hive.id, worker_label(w));
                        launched.push(w.slug.clone());
                    }
                    Err(r) => {
                        log::info!("hive {}: waiting worker {} not launched: {}", caller.hive.id, worker_label(w), r.why);
                        if !r.at_reserve {
                            back_off(&w.task_id, now);
                            // Stopped while it launched: the coordinator's own doing, no news to it.
                            let (paths, id) = (caller.paths.clone(), w.task_id.clone());
                            let waits = blocking(move || Ok(worker::read_worker(&worker::task_dir(&paths, &id)?)?.phase == worker::PHASE_WAITING))
                                .await
                                .unwrap_or(true);
                            // Not the cap or a fuse, which only wait: something the coordinator may fix.
                            if waits && w.wait_reason.as_deref() != Some(r.why.as_str()) {
                                tell_coordinator(
                                    ctx,
                                    caller,
                                    format!(
                                        "[maiTerm] Worker {}'s dependencies are done, but it couldn't launch: {} maiTerm tries again in a few minutes.",
                                        worker_label(w),
                                        one_line(&r.why, 400)
                                    ),
                                )
                                .await;
                            }
                        }
                        ctx.window.workers_changed(&caller.workspace_id);
                    }
                }
            }
        }
    }
    launched
}

async fn tell_coordinator<W: HiveWindow>(ctx: &Ctx<'_, W>, caller: &Caller, text: String) {
    let Some(c) = super::tools::coordinator_tab(ctx.data, caller) else { return };
    let sent = ctx.window.deliver(&c, text, "Hive notice").await;
    log::info!("hive {}: notice to the coordinator: {}", caller.hive.id, sent.describe());
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hive::queue::{self, JobSpec};

    fn row(id: &str, status: &str) -> Task {
        serde_json::from_value(serde_json::json!({
            "id": id, "title": "t", "status": status, "origin": "agent", "created_at": "", "updated_at": ""
        }))
        .unwrap()
    }

    /// A dropped task's runs leave the queue (the app's own, so the ids are this test's alone).
    #[test]
    fn a_dropped_task_releases_its_workers_runs() {
        let (dropped, kept) = (format!("t-drop-{}", uuid::Uuid::new_v4()), format!("t-keep-{}", uuid::Uuid::new_v4()));
        let enqueue = |task: &str| {
            let spec = JobSpec {
                hive_id: "h-drop".into(),
                window: "main".into(),
                task_id: task.into(),
                worker_slug: "w".into(),
                argv: vec!["cargo".into(), "test".into()],
                ..JobSpec::default()
            };
            queue::enqueue(spec).unwrap().id
        };
        let (a, b) = (enqueue(&dropped), enqueue(&kept));
        let worker = |task: &str| WorkerRecord { task_id: task.into(), slug: "w".into(), ..WorkerRecord::default() };
        let workers = vec![worker(&dropped), worker(&kept)];

        assert_eq!(release_dropped(&workers, &[row(&dropped, "active"), row(&kept, "active")]), 0, "nothing dropped");
        assert_eq!(release_dropped(&workers, &[row(&kept, "active")]), 0, "a row missing from a read releases nobody");
        assert_eq!(release_dropped(&workers, &[row(&dropped, "dropped"), row(&kept, "active")]), 1);
        let j = queue::job(&a).unwrap();
        assert_eq!((j.state.as_str(), j.stopped_by.as_deref()), (queue::INTERRUPTED, Some(queue::BY_RELEASE)));
        assert!(queue::job(&b).unwrap().is_active(), "another worker's run stays");
        assert_eq!(release_dropped(&workers, &[row(&dropped, "dropped")]), 0, "once");
        queue::release_worker(&kept, "test over");
    }
}
