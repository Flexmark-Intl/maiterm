//! The coordinator's hold on a worker it has started (docs/hive.md §7.2, §11.3): a message, a
//! stop, a restart. A reported worker counts against the cap until it lands, which is step 6, so
//! without a stop a Hive would lock up after `cap` reports (plan decision 3).
//!
//! A stop signals only the agent the terminal shows in front, never the shell and never by name;
//! a restart relaunches in the worker's own tab and worktrees through the launch builder, and
//! only once no agent holds the terminal, after the same reservation as a spawn.

use serde_json::{json, Value};

use super::deliver::{frame_escape, Sent};
use super::rpc::{self, Front, HiveWindow};
use super::tools::{blocking, find_worker, worker_label, Caller, Ctx, MAX_MESSAGE};
use super::worker::{self, WorkerRecord};
use crate::state::Task;

/// The worker an argument names, from the records on disk.
async fn named_worker(caller: &Caller, args: &Value) -> Result<WorkerRecord, String> {
    let name = match args.get("worker") {
        Some(Value::String(s)) if !s.trim().is_empty() => s.trim().to_string(),
        Some(Value::Number(n)) => format!("#{n}"),
        _ => return Err("worker is required: its badge, name or task id".to_string()),
    };
    let paths = caller.paths.clone();
    let workers = blocking(move || Ok(worker::list_workers(&paths))).await?;
    find_worker(&workers, &name).cloned().ok_or_else(|| format!("This Hive has no worker \"{name}\" (use its badge, name or task id)."))
}

/// The worker's tab when it is still in the Hive.
fn live_tab<W: HiveWindow>(ctx: &Ctx<'_, W>, caller: &Caller, w: &WorkerRecord) -> Option<String> {
    w.tab_id.clone().filter(|t| super::tools::has_tab(ctx.data, caller, t))
}

/// Patches the worker's task row in its window's store: a note, a status.
pub(crate) async fn task_note<W: HiveWindow>(ctx: &Ctx<'_, W>, caller: &Caller, task_id: &str, note: &str, status: Option<&str>) -> Result<(), String> {
    let args = json!({ "workspace_id": caller.workspace_id, "task_id": task_id, "note": note, "status": status });
    rpc::applied(ctx.window.request(rpc::VERB_TASK, args).await).map(|_| ())
}

// ── hiveMessageWorker ──

pub(crate) async fn message_worker<W: HiveWindow>(ctx: &Ctx<'_, W>, caller: &Caller, args: &Value) -> Result<Value, String> {
    let text = args.get("text").and_then(Value::as_str).map(str::trim).unwrap_or_default();
    if text.is_empty() {
        return Err("text is required".to_string());
    }
    if text.chars().count() > MAX_MESSAGE {
        return Err(format!("The message is over {MAX_MESSAGE} characters: put the detail in a file in the worker's folder and point to it."));
    }
    let w = named_worker(caller, args).await?;
    let tab = live_tab(ctx, caller, &w).ok_or_else(|| format!("Worker {} has no tab ({}): there's no agent to tell.", worker_label(&w), w.phase))?;
    // The coordinator's words are escaped too: only maiTerm opens a line with `[maiTerm]`.
    let framed = format!("[maiTerm] From your coordinator:\n{}", frame_escape(text));
    let sent = ctx.window.deliver(&tab, framed, "Hive message").await;
    log::info!("hive {}: message to worker {}: {}", caller.hive.id, worker_label(&w), sent.describe());
    match sent {
        Sent::Refused(why) => Err(format!(
            "Not delivered to worker {}: {why}. If its agent has exited, hiveRestartWorker brings it back.",
            worker_label(&w)
        )),
        s => Ok(json!({ "worker": w.slug, "badge": w.badge, "delivery": s.describe() })),
    }
}

// ── hiveStopWorker ──

pub(crate) const MAX_REASON: usize = 500;

pub(crate) async fn stop_worker<W: HiveWindow>(ctx: &Ctx<'_, W>, caller: &Caller, args: &Value) -> Result<Value, String> {
    let reason = args.get("reason").and_then(Value::as_str).map(str::trim).unwrap_or_default().to_string();
    if reason.is_empty() {
        return Err("reason is required: it goes on the worker's task".to_string());
    }
    if reason.chars().count() > MAX_REASON {
        return Err(format!("Keep the reason under {MAX_REASON} characters."));
    }
    let w = named_worker(caller, args).await?;
    if w.phase == worker::PHASE_STOPPED {
        return Err(format!("Worker {} is already stopped.", worker_label(&w)));
    }
    let tab = live_tab(ctx, caller, &w);
    let front = match &tab {
        Some(t) => ctx.window.front(t).await,
        None => Front::NoTerminal,
    };
    // `stopped` frees the slot: never while something maiTerm can't see may still start an agent.
    if front == Front::Unknown {
        return Err(format!("maiTerm can't tell what holds worker {}'s terminal, so nothing was stopped. Try again shortly.", worker_label(&w)));
    }
    // Stopped before the signal, so the agent's exit reads as a stop, never as a crash.
    let dir = worker::task_dir(&caller.paths, &w.task_id)?;
    let was = w.phase.clone();
    let d = dir.clone();
    blocking(move || {
        worker::update_worker(&d, |w| {
            w.phase = worker::PHASE_STOPPED.to_string();
            w.wait_reason = None;
            true
        })
    })
    .await?;
    let signalled = match (&tab, front) {
        (Some(t), Front::Job) => ctx.window.stop_agent(t).await,
        _ => Ok(false),
    };
    // Something other than its agent holds the terminal: on a first launch that is its setup,
    // with `&& … claude` typed after it. Marked stopped, that agent would start uncounted.
    if let Err(e) = &signalled {
        let back = was.clone();
        let restored = blocking(move || {
            worker::update_worker(&dir, |w| {
                let ours = w.phase == worker::PHASE_STOPPED;
                if ours {
                    w.phase = back;
                }
                ours
            })
        })
        .await;
        if let Err(re) = restored {
            log::warn!("hive {}: worker {} left stopped after a refused stop: {re}", caller.hive.id, worker_label(&w));
        }
        return Err(format!(
            "Worker {}: {e}. maiTerm signals only a worker's agent; on a first launch that program is usually its setup, \
             and its agent starts when that finishes. Stop it once its agent is running.",
            worker_label(&w)
        ));
    }
    // A stop is never held back by the board: its note is best effort.
    let note = format!("Stopped by the coordinator: {reason}");
    if let Err(e) = task_note(ctx, caller, &w.task_id, &note, None).await {
        log::warn!("hive {}: worker {} stop note not written: {e}", caller.hive.id, worker_label(&w));
    }
    ctx.window.workers_changed(&caller.workspace_id);
    log::info!("hive {}: worker {} stopped (was {was}): {reason}", caller.hive.id, worker_label(&w));
    let agent = match signalled {
        Ok(true) => "its agent was sent SIGTERM",
        _ => "no agent was running in its tab",
    };
    Ok(json!({
        "worker": w.slug,
        "badge": w.badge,
        "phase": worker::PHASE_STOPPED,
        "agent": agent,
        "note": "Its worktrees, branches and chat are kept; hiveRestartWorker brings it back.",
    }))
}

// ── hiveRestartWorker ──

pub(crate) async fn restart_worker<W: HiveWindow>(ctx: &Ctx<'_, W>, caller: &Caller, args: &Value) -> Result<Value, String> {
    let fresh = args.get("fresh").and_then(Value::as_bool).unwrap_or(false);
    let w = named_worker(caller, args).await?;
    let label = worker_label(&w);
    match w.phase.as_str() {
        worker::PHASE_FAILED | worker::PHASE_STOPPED | worker::PHASE_RUNNING => {}
        worker::PHASE_WAITING => {
            return Err(format!(
                "Worker {label} hasn't launched yet: it launches once its dependencies are done ({}).",
                w.wait_reason.as_deref().unwrap_or("waiting")
            ))
        }
        worker::PHASE_LAUNCHING => return Err(format!("Worker {label} is starting now: its card shows when it runs.")),
        worker::PHASE_REPORTED | worker::PHASE_BLOCKED => {
            return Err(format!(
                "Worker {label} has reported ({}): hiveMessageWorker asks it for more. To start it over, hiveStopWorker it first.",
                w.phase
            ))
        }
        other => return Err(format!("Worker {label} is {other}: only a failed, stopped or exited worker restarts.")),
    }
    // Stopped while it waited: it never launched, so it has no worktrees or launch record. It goes
    // back to waiting, and the waiters launch it as a spawn once its dependencies are done.
    if w.launched_at.is_none() {
        return rewait(ctx, caller, &w).await;
    }
    let tab = live_tab(ctx, caller, &w);
    // §11.3: only where nothing would be typed over. No terminal is fine for a crashed or stopped
    // worker: the mount launches it. A running one restarts only at its shell's empty prompt (its
    // agent exited), or when its tab is gone and no agent can be left.
    if let Some(t) = &tab {
        let front = ctx.window.front(t).await;
        let clear = front == Front::Shell || (front == Front::NoTerminal && w.phase != worker::PHASE_RUNNING);
        if !clear {
            return Err(format!(
                "Worker {label}'s agent (or another program) still holds its terminal{}. hiveStopWorker stops it; restart it after.",
                match front {
                    Front::Unknown => ", as far as maiTerm can tell",
                    Front::NoTerminal => ", or its terminal isn't up yet",
                    _ => "",
                }
            ));
        }
    }
    let dir = worker::task_dir(&caller.paths, &w.task_id)?;
    let (paths, record, preflight) = (caller.paths.clone(), w.clone(), ctx.preflight);
    blocking(move || preflight(&paths, &record)).await?;

    // Counted as a spawn. Its own slot is the one it takes: a live worker doesn't count twice.
    let (hive, paths, fuses, task_id) = (caller.hive.clone(), caller.paths.clone(), ctx.fuses, w.task_id.clone());
    let me = task_id.clone();
    let live = move || super::tools::live_task_ids(&paths).into_iter().filter(|t| *t != me).collect::<Vec<_>>();
    let reservation = blocking(move || fuses.reserve(&hive, &task_id, live, super::fuses::now_ms())).await?;

    // `launching` before the relaunch: the builder starts only a live worker, and the slot moves
    // from pending to live with no gap, as for a spawn.
    let new_tab = tab.is_none().then(|| super::tools::worker_tab(&w));
    let (d, before, new_tab_id) = (dir.clone(), w.clone(), new_tab.as_ref().map(|t| t.id.clone()));
    let now = crate::commands::workspace::iso_now();
    blocking(move || {
        let mut moved = true;
        worker::update_worker(&d, |w| {
            // Stopped, restarted or reported meanwhile: this restart was decided on what's gone.
            moved = w.phase == before.phase && w.tab_id == before.tab_id;
            if moved {
                w.phase = worker::PHASE_LAUNCHING.to_string();
                w.fresh_next = fresh;
                w.launched_at = Some(now);
                w.wait_reason = None;
                if new_tab_id.is_some() {
                    w.tab_id = new_tab_id;
                }
            }
            moved
        })?;
        if moved { Ok(()) } else { Err("The worker changed while this restart was checked: look again with hiveListWorkers.".to_string()) }
    })
    .await?;

    // Whether the window may still act on what was sent: one that didn't answer may wake later.
    let (relaunched, answered) = match &new_tab {
        // A late spawn is undone by the unspawn queued after it, and its tab is named by no record.
        Some(t) => (relaunch_in_new_tab(ctx, caller, &w, t).await, true),
        None => {
            let tab_id = tab.clone().unwrap_or_default();
            let outcome = ctx.window.request(rpc::VERB_RELAUNCH, json!({ "workspace_id": caller.workspace_id, "tab_id": tab_id })).await;
            let answered = rpc::window_answered(&outcome);
            (rpc::applied(outcome).map(|_| ()), answered)
        }
    };
    if let Err(e) = relaunched {
        drop(reservation);
        put_back(dir, w.clone(), answered).await;
        return Err(e);
    }
    if let Err(e) = reservation.commit(super::fuses::now_ms()) {
        // The line is already typed: the worker runs, uncounted. Said, not undone.
        log::warn!("hive {}: worker {} restarted, spawn not counted: {e}", caller.hive.id, worker_label(&w));
    }
    ctx.window.workers_changed(&caller.workspace_id);
    let how = if fresh { "a fresh session" } else { "resuming its session" };
    if let Err(e) = task_note(ctx, caller, &w.task_id, &format!("Restarted by the coordinator ({how})"), None).await {
        log::info!("hive {}: worker {} restart note not written: {e}", caller.hive.id, worker_label(&w));
    }
    log::info!("hive {}: worker {} restarted ({how}, was {})", caller.hive.id, worker_label(&w), w.phase);
    Ok(json!({
        "worker": w.slug,
        "badge": w.badge,
        "state": worker::PHASE_LAUNCHING,
        "session": if fresh { "new" } else { "resumed" },
        "tab": if new_tab.is_some() { "new (its old tab was gone)" } else { "its own" },
    }))
}

/// A worker stopped before it ever launched goes back to waiting (the caller polls the waiters).
async fn rewait<W: HiveWindow>(ctx: &Ctx<'_, W>, caller: &Caller, w: &WorkerRecord) -> Result<Value, String> {
    let dir = worker::task_dir(&caller.paths, &w.task_id)?;
    let before = w.phase.clone();
    blocking(move || {
        let mut moved = false;
        worker::update_worker(&dir, |w| {
            moved = w.phase == before && w.launched_at.is_none();
            if moved {
                w.phase = worker::PHASE_WAITING.to_string();
                w.wait_reason = None;
            }
            moved
        })?;
        if moved { Ok(()) } else { Err("The worker changed while this restart was checked: look again with hiveListWorkers.".to_string()) }
    })
    .await?;
    ctx.window.workers_changed(&caller.workspace_id);
    if let Err(e) = task_note(ctx, caller, &w.task_id, "Put back to waiting by the coordinator", None).await {
        log::info!("hive {}: worker {} rewait note not written: {e}", caller.hive.id, worker_label(w));
    }
    log::info!("hive {}: worker {} waits again (was {}, never launched)", caller.hive.id, worker_label(w), w.phase);
    Ok(json!({
        "worker": w.slug,
        "badge": w.badge,
        "state": worker::PHASE_WAITING,
        "note": "It never launched, so it waits again: it launches once every dependency's task is done, as a spawn.",
    }))
}

/// A refused restart: the record as it was, while it is still this restart's `launching`. A
/// relaunch the window never answered may still run when it wakes, and the builder types only
/// for a live worker: an exited `running` one goes back `failed`, so that late relaunch types
/// nothing (`failed` and `stopped` already refuse it).
async fn put_back(dir: std::path::PathBuf, before: WorkerRecord, answered: bool) {
    let res = blocking(move || {
        worker::update_worker(&dir, |w| {
            let ours = w.phase == worker::PHASE_LAUNCHING;
            if ours {
                w.phase = if !answered && before.phase == worker::PHASE_RUNNING { worker::PHASE_FAILED.to_string() } else { before.phase };
                w.fresh_next = before.fresh_next;
                w.launched_at = before.launched_at;
                w.tab_id = before.tab_id;
            }
            ours
        })
    })
    .await;
    if let Err(e) = res {
        log::warn!("hive: a refused restart may have left its worker launching: {e}");
    }
}

/// The worker's tab is gone: a new hidden one takes over its task (as a reload's does) and is
/// mounted, which types the builder's line. Everything put is taken back on a refusal.
async fn relaunch_in_new_tab<W: HiveWindow>(ctx: &Ctx<'_, W>, caller: &Caller, w: &WorkerRecord, tab: &crate::state::Tab) -> Result<(), String> {
    let (row, was) = super::tools::insert_replacement_tab(ctx.data, caller, &w.task_id, tab)?;
    // `sent`: whether `hive.spawn` went out, and if so whether the window answered it.
    let back = |why: String, sent: Option<bool>| async move {
        super::tools::remove_replacement_tab(ctx.data, caller, &tab.id, was.as_ref());
        let snapshot = ctx.data.read().clone();
        if let Err(e) = (ctx.save)(&snapshot) {
            log::warn!("hive {}: worker {}'s new tab taken out of the state, not saved: {e}", caller.hive.id, worker_label(w));
        }
        if let Some(answered) = sent {
            let restore: Option<&Task> = was.as_ref();
            let args = json!({ "workspace_id": caller.workspace_id, "tab_id": tab.id, "restore_task": restore });
            // As a spawn's Undo: a window that didn't answer may wake later, and then handles the
            // spawn and this after it, in order, so it isn't waited on.
            if answered {
                let _ = ctx.window.request(rpc::VERB_UNSPAWN, args).await;
            } else {
                let window = ctx.window.clone();
                tokio::spawn(async move {
                    let _ = window.request(rpc::VERB_UNSPAWN, args).await;
                });
            }
        }
        why
    };
    let Some(row) = row else {
        return Err(back(format!("Worker {}'s task is gone from the board, so it can't take a new tab.", worker_label(w)), None).await);
    };
    let outcome = ctx.window.request(rpc::VERB_SPAWN, json!({ "workspace_id": caller.workspace_id, "task": row, "tab": tab })).await;
    let answered = rpc::window_answered(&outcome);
    if let Err(e) = rpc::applied(outcome).and_then(|_| super::tools::save(ctx)) {
        return Err(back(e, Some(answered)).await);
    }
    Ok(())
}
