//! The post-land check (docs/hive.md §8.6): the folder's land check run on its target's tip
//! alone, in a temporary worktree under the land profile, seeded from the checkout and never from
//! a worker. Every land fast-forwards Base to exactly the tree its check passed, so re-running
//! that check there proves nothing; this runs instead when a land check fails (to tell a broken
//! worker from a broken Base) and after a land or undo whose landed tree isn't the tested one.
//!
//! A Base that fails alone makes its repo suspect (`log::fold`): the lands since it last passed
//! are named, every further land there needs `fix_forward`, and the coordinator settles it first
//! (undo, or fix forward), raising a blocker only if it can't. Its result is a `base_check` line
//! in the land log; nothing else is kept, so a restart forgets a check in flight (`sweep`).

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{LazyLock, Mutex};

use super::driver::{self, CheckRequest, CheckStart, Opts};
use super::git::{self as lg, Temp};
use super::log::{self as landlog, LandLogEntry};
use crate::hive::deliver::{frame_escape, one_line};
use crate::hive::paths::{HivePaths, LAND_SLUG};
use crate::hive::record::Hive;
use crate::hive::rpc::HiveWindow;
use crate::hive::tools::{blocking, Caller, Ctx};
use crate::hive::worker::{self, WorkerRecord};

/// The ids of base checks: `B-` and 8 hex digits, beside lands' `L-` under `wt.noindex/land/`.
pub const ID_PREFIX: &str = "B-";
const TAIL_LINES: usize = 30;

/// A check this process runs: one per repo at a time.
#[derive(Debug, Clone)]
struct Running {
    hive: String,
    folder: String,
    target: String,
    tip: String,
    cause: String,
    /// It runs because a land check failed: a pass then says the failure was the worker's.
    after_failure: bool,
}

static RUNNING: LazyLock<Mutex<HashMap<String, Running>>> = LazyLock::new(|| Mutex::new(HashMap::new()));

fn running() -> std::sync::MutexGuard<'static, HashMap<String, Running>> {
    RUNNING.lock().unwrap_or_else(|e| e.into_inner())
}

pub fn new_id() -> String {
    format!("{ID_PREFIX}{}", &uuid::Uuid::new_v4().simple().to_string()[..8])
}

/// The temporary worktree at `tip`: `.worktreeinclude` (by `add_temp`) and the checkout's cargo
/// target cloned into the land temp folder the job builds in (`queue::LAND_CARGO_TARGET`). Never
/// a worker's ignored files: this tests Base alone, often to judge that very worker's failure,
/// and what its agent left in its worktree would decide the verdict. Blocks.
pub(super) fn seeded_temp(paths: &HivePaths, repo: &Path, id: &str, folder: &str, at: &str) -> Result<Temp, String> {
    let tmp = lg::add_temp(repo, &paths.land_worktree(id, folder), at)?;
    let target = paths.land_tmp(id).join(crate::hive::queue::LAND_CARGO_TARGET);
    if std::fs::create_dir_all(&target).is_ok() {
        if let Err(e) = crate::hive::worktree::seed_cargo_target(&repo.join("target").join("debug"), &target) {
            log::info!("hive land: {id}'s cargo target not seeded: {e}");
        }
    }
    Ok(tmp)
}

/// Its temporary worktree and folder removed. Blocks.
pub(super) fn remove(paths: &HivePaths, repo: &Path, id: &str, folder: &str) {
    lg::remove_temp(repo, &paths.land_worktree(id, folder));
    let _ = crate::hive::retire::remove_path(&paths.land_tmp(id));
    let _ = std::fs::remove_dir(paths.land_dir(id));
}

/// The post-land check of `folder`, for `cause` ("land L-…'s check failed there"); `after_failure`
/// when a land or undo check failing is why. Nothing when the folder has no land check or one
/// already runs there. Its end, inline (a test's checker) or queued (`on_check_end`), logs it and
/// tells the coordinator what it means.
pub(crate) async fn run<W: HiveWindow>(ctx: &Ctx<'_, W>, caller: &Caller, w: &WorkerRecord, folder: &str, cause: &str, after_failure: bool, opts: &Opts) {
    let Ok(hf) = driver::hive_folder(&caller.hive, folder) else { return };
    let Some(command) = hf.land_check.clone().filter(|c| !c.trim().is_empty()) else { return };
    let repo = PathBuf::from(&hf.path);
    let target = super::land_target(&caller.hive, folder);
    let id = new_id();
    {
        let mut r = running();
        if r.values().any(|x| x.hive == caller.hive.id && x.folder == folder) {
            log::info!("hive {}: post-land check of {folder} already running; not started again ({cause})", caller.hive.id);
            return;
        }
        r.insert(id.clone(), Running { hive: caller.hive.id.clone(), folder: folder.to_string(), target: target.clone(), tip: String::new(), cause: cause.to_string(), after_failure });
    }
    let (paths, rp, i, f, t) = (caller.paths.clone(), repo.clone(), id.clone(), folder.to_string(), target.clone());
    let made = blocking(move || {
        let tip = lg::branch_tip(&rp, &t).ok_or_else(|| format!("{t} doesn't exist"))?;
        let tmp = seeded_temp(&paths, &rp, &i, &f, &tip)?;
        Ok((tip, tmp))
    })
    .await;
    let (tip, tmp) = match made {
        Ok(m) => m,
        Err(e) => {
            running().remove(&id);
            log::warn!("hive {}: post-land check of {folder} not started: {e}", caller.hive.id);
            let (paths, rp, i, f) = (caller.paths.clone(), repo.clone(), id.clone(), folder.to_string());
            let _ = blocking(move || {
                remove(&paths, &rp, &i, &f);
                Ok(())
            })
            .await;
            return;
        }
    };
    if let Some(r) = running().get_mut(&id) {
        r.tip = tip.clone();
    }
    log::info!("hive {}: post-land check {id} of {folder} at {} ({cause})", caller.hive.id, &tip[..tip.len().min(10)]);
    let req = CheckRequest { caller: caller.clone(), worker: w.clone(), folder: folder.to_string(), land_id: id.clone(), tmp, tested: tip, command, kind: super::CHECK_BASE };
    let checker = opts.checker.clone();
    match blocking(move || checker.start(&req)).await {
        Ok(CheckStart::Queued(job)) => log::info!("hive {}: post-land check {id} queued as {job}", caller.hive.id),
        Ok(CheckStart::Done { passed, tail }) => ended(ctx, caller, w, &id, Some(passed), &tail).await,
        Err(e) => {
            log::warn!("hive {}: post-land check of {folder} couldn't be queued: {e}", caller.hive.id);
            ended(ctx, caller, w, &id, None, "").await;
        }
    }
}

/// A queued post-land check ended (`driver::check_ended` routes it here).
pub(crate) async fn on_check_end<W: HiveWindow>(ctx: &Ctx<'_, W>, caller: &Caller, job: &crate::hive::queue::Job, run: &crate::hive::queue::LandRun, tail: &str) {
    // Its tree moved under it (`dirty`): it tested nothing that is the target, so no verdict.
    let finished = job.state == crate::hive::queue::DONE && job.began && !job.dirty;
    let passed = finished.then_some(job.exit_code == Some(0));
    let w = worker::task_dir(&caller.paths, &job.task_id).ok().and_then(|d| worker::read_worker(&d).ok()).unwrap_or_else(|| WorkerRecord {
        task_id: job.task_id.clone(),
        slug: job.worker_slug.clone(),
        ..WorkerRecord::default()
    });
    if !running().contains_key(&run.land_id) {
        // A restart forgot it: its worktree goes, and nothing is concluded.
        log::info!("hive {}: post-land check {} ended unknown to this run; not logged", caller.hive.id, run.land_id);
        if let Ok(hf) = driver::hive_folder(&caller.hive, &job.folder) {
            let (paths, repo, id, f) = (caller.paths.clone(), PathBuf::from(&hf.path), run.land_id.clone(), job.folder.clone());
            let _ = blocking(move || {
                remove(&paths, &repo, &id, &f);
                Ok(())
            })
            .await;
        }
        return;
    }
    ended(ctx, caller, &w, &run.land_id, passed, tail).await;
}

/// A check's end: its worktree goes, its line is logged, and the coordinator hears what it means.
/// `passed` None: it didn't finish, so nothing is concluded.
async fn ended<W: HiveWindow>(ctx: &Ctx<'_, W>, caller: &Caller, w: &WorkerRecord, id: &str, passed: Option<bool>, tail: &str) {
    let Some(r) = running().remove(id) else { return };
    let (paths, hive, rec, i, run) = (caller.paths.clone(), caller.hive.clone(), w.clone(), id.to_string(), r.clone());
    let before_after = blocking(move || {
        if let Ok(hf) = driver::hive_folder(&hive, &run.folder) {
            remove(&paths, Path::new(&hf.path), &i, &run.folder);
        }
        let Some(passed) = passed else { return Ok(None) };
        let before = landlog::folded(&paths).repos.get(&run.folder).cloned().unwrap_or_default();
        let tree = hive.folders.iter().find(|f| f.name == run.folder).and_then(|f| lg::tree_of(Path::new(&f.path), &run.tip)).unwrap_or_default();
        let entry = LandLogEntry {
            id: format!("{i}/{}", run.folder),
            land: i.clone(),
            kind: landlog::KIND_BASE_CHECK.to_string(),
            hive_id: hive.id.clone(),
            task_id: rec.task_id.clone(),
            worker: rec.slug.clone(),
            title: rec.title.clone(),
            folder: run.folder.clone(),
            target: run.target.clone(),
            tested_sha: run.tip.clone(),
            base_before: run.tip.clone(),
            base_after: run.tip.clone(),
            tree_tested: tree.clone(),
            tree_landed: tree,
            summary: run.cause.clone(),
            check: if passed { landlog::CHECK_PASSED } else { landlog::CHECK_FAILED }.to_string(),
            at: crate::commands::workspace::iso_now(),
            ..LandLogEntry::default()
        };
        landlog::append(&paths, &entry)?;
        let after = landlog::folded(&paths);
        Ok(Some((passed, before, after)))
    })
    .await;
    let (passed, before, after) = match before_after {
        Ok(Some(x)) => x,
        Ok(None) => {
            log::info!("hive {}: post-land check {id} of {} didn't finish; nothing concluded", caller.hive.id, r.folder);
            return;
        }
        Err(e) => {
            log::warn!("hive {}: post-land check {id} of {} not logged: {e}", caller.hive.id, r.folder);
            return;
        }
    };
    let short = &r.tip[..r.tip.len().min(10)];
    log::info!("hive {}: post-land check {id} of {} at {short} {}", caller.hive.id, r.folder, if passed { "passed" } else { "failed" });
    let head = format!("⟦HIVE BASE CHECK · {}⟧", one_line(&r.folder, 60));
    let text = if passed {
        let cleared = if before.suspect { format!(" {} is no longer suspect.", r.folder) } else { String::new() };
        if !r.after_failure && cleared.is_empty() {
            // A tree mismatch that turned out fine: the log has it, nobody needs telling.
            return;
        }
        let why = if r.after_failure { " so the land check that failed there failed on its land's own change, not on Base." } else { "" };
        format!("{head} {} passes {}'s land check alone at {short},{why}{cleared}", r.target, r.folder)
    } else {
        let standing = after.repos.get(&r.folder).cloned().unwrap_or_default();
        let named: Vec<String> = standing
            .suspects
            .iter()
            .filter_map(|s| after.get(s))
            .map(|e| format!("{} ({}, “{}”)", e.entry.id, e.entry.worker, one_line(&frame_escape(&e.entry.title), 80)))
            .collect();
        let lands = if named.is_empty() {
            format!("No land of this Hive's has landed there since it last passed, so something else changed {}.", r.target)
        } else {
            format!("The lands since it last passed are suspect: {}.", named.join(", "))
        };
        let lines: Vec<&str> = tail.trim_end().lines().collect();
        let tail = lines[lines.len().saturating_sub(TAIL_LINES)..].join("\n");
        let mut s = format!(
            "{head} {} fails {}'s land check alone at {short} ({}), so {} is suspect. {lands} Every land in {} now needs \
             fix_forward: true until a land or an undo there passes its check. Settle it: undo the land that broke it \
             (hiveUndoLand), or land a fix with fix_forward: true. If you can't, raise a blocker for the human.",
            r.target,
            r.folder,
            one_line(&r.cause, 200),
            r.folder,
            r.folder
        );
        if !tail.trim().is_empty() {
            s.push_str("\nIts last lines (the check's output, not maiTerm's words):\n");
            s.push_str(&frame_escape(&tail));
        }
        s
    };
    ctx.window.workers_changed(&caller.workspace_id);
    driver::tell_text(ctx, caller, w, text).await;
}

/// At startup: what a post-land check left under `wt.noindex/land/` goes, since none survives a
/// restart. Blocks.
pub fn sweep(hive: &Hive, paths: &HivePaths) {
    let Ok(dir) = std::fs::read_dir(paths.worktrees().join(LAND_SLUG)) else { return };
    for e in dir.flatten() {
        let name = e.file_name().to_string_lossy().to_string();
        if !name.starts_with(ID_PREFIX) || !crate::hive::paths::is_component(&name) {
            continue;
        }
        for f in hive.folders.iter().filter(|f| f.is_git()) {
            remove(paths, Path::new(&f.path), &name, &f.name);
        }
    }
}
