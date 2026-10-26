//! Batched land checks (docs/hive.md §6.8, §8.1): a merge queue per repo. A one-repo land that
//! finds its repo busy (another land holds its lock, or a batch run is under way there) waits in
//! that repo's queue (`queued_batch`) instead of on the lock alone. Once the lock is free and two
//! or more wait, maiTerm builds an integration chain in a temporary worktree (Base, then each
//! member's pinned commit merged `--no-ff`, in `hiveLand` order), runs the land check once on its
//! tip, and fast-forwards Base to exactly that tip under the lock. A failing chain is split in half
//! and each half rebuilt from Base and checked, down to the culprit, whose land is refused with the
//! output. A singleton keeps its chain form (plan D11).
//!
//! Step 1 is skipped for members and their branches aren't moved: a member landed once its own
//! merge commit is an ancestor of the new Base, and its log line (`batch_member`) names that merge,
//! which Undo reverts with `revert -m 1`. A member that conflicts in the chain, or whose own merge
//! trips the config rule, leaves and lands alone (the conflict meets its own step 1, the config its
//! card). Multi-repo workers (plan D10), the integration mode, Landing = Ask me, fix-forward lands
//! and folders without a land check never batch.
//!
//! The run lives in memory: a restart holds every member (`land::reconcile`), and the
//! coordinator's next `hiveLand` resumes each. The chain's check runs under the land profile: it
//! holds several workers' work, so no one worker's folders are its to write.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, LazyLock, Mutex, MutexGuard};
use std::time::Instant;

use serde_json::{json, Value};

use super::driver::{self, CheckRequest, CheckStart, Opts, Outcome};
use super::git::{self as lg, Merged, Step2, Temp};
use super::lock::{self, LockInfo, Taken};
use super::{LandRepo, LandState, MAX_ATTEMPTS, REPO_ADVANCING, REPO_CHECKING, REPO_LANDED, REPO_PENDING};
use super::{STATE_ADVANCING, STATE_CHECKING, STATE_LOCKING, STATE_QUEUED_BATCH};
use crate::hive::deliver::one_line;
use crate::hive::paths::{HivePaths, LAND_SLUG};
use crate::hive::record::Hive;
use crate::hive::rpc::HiveWindow;
use crate::hive::tools::{blocking, worker_label, Caller, Ctx};
use crate::hive::worker::{self, WorkerRecord};

/// Batch ids: `Q-` and 8 hex digits, beside lands' `L-` under `wt.noindex/land/`.
pub const ID_PREFIX: &str = "Q-";

pub fn new_id() -> String {
    format!("{ID_PREFIX}{}", &uuid::Uuid::new_v4().simple().to_string()[..8])
}

/// A repo's queue has a run: (Hive id, folder).
type Key = (String, String);

static RUNNERS: LazyLock<Mutex<HashSet<Key>>> = LazyLock::new(|| Mutex::new(HashSet::new()));
/// Batches whose check this process may still hear the end of.
static ACTIVE: LazyLock<Mutex<HashSet<String>>> = LazyLock::new(|| Mutex::new(HashSet::new()));
/// A queued check's end, met by the run awaiting it whichever comes first.
static ENDS: LazyLock<Mutex<HashMap<String, Slot>>> = LazyLock::new(|| Mutex::new(HashMap::new()));

fn runners() -> MutexGuard<'static, HashSet<Key>> {
    RUNNERS.lock().unwrap_or_else(|e| e.into_inner())
}

fn active() -> MutexGuard<'static, HashSet<String>> {
    ACTIVE.lock().unwrap_or_else(|e| e.into_inner())
}

fn ends() -> MutexGuard<'static, HashMap<String, Slot>> {
    ENDS.lock().unwrap_or_else(|e| e.into_inner())
}

fn key(caller: &Caller, folder: &str) -> Key {
    (caller.hive.id.clone(), folder.to_string())
}

/// The Hive's queues have no run any more (a restart, `land::reconcile_in`).
pub(crate) fn forget_runs(hive_id: &str) {
    runners().retain(|(h, _)| h != hive_id);
}

/// How a batch's check ended. `passed` None: it didn't finish, so it says nothing of the chain.
#[derive(Debug, Clone)]
struct CheckEnd {
    passed: Option<bool>,
    tail: String,
    why: String,
}

enum Slot {
    Waiting(tokio::sync::oneshot::Sender<CheckEnd>),
    Ended(CheckEnd),
}

/// Monotonic, so two lands queued in one millisecond keep their `hiveLand` order.
fn next_seq() -> u64 {
    static LAST: AtomicU64 = AtomicU64::new(0);
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_millis() as u64) * 1000;
    let mut cur = LAST.load(Ordering::Relaxed);
    loop {
        let next = now.max(cur + 1);
        match LAST.compare_exchange(cur, next, Ordering::Relaxed, Ordering::Relaxed) {
            Ok(_) => return next,
            Err(c) => cur = c,
        }
    }
}

fn land_check(hive: &Hive, folder: &str) -> Option<String> {
    hive.folders.iter().find(|f| f.name == folder).and_then(|f| f.land_check.clone()).filter(|c| !c.trim().is_empty())
}

/// Whether a land may wait in its repo's batch queue (§6.8): batching on, the land-to-Base mode on
/// the coordinator's checkpoint, one repo with a land check, its step 1 not started, and neither
/// the human's approval nor a fix forward to keep apart.
pub(crate) fn eligible(hive: &Hive, w: &WorkerRecord, land: &LandState) -> bool {
    let [r] = land.repos.as_slice() else { return false };
    hive.settings.land.batch
        && !hive.is_integration()
        && super::approval::asks_first(hive).is_none()
        && w.git_folders().count() == 1
        && !w.is_reviewer()
        && land.approved_by.is_none()
        && !land.fix_forward
        && r.status == REPO_PENDING
        && land_check(hive, &r.folder).is_some()
}

/// A repo's chain fields cleared: it lands alone from its pin, never from a merge that sits on
/// other members' work.
pub(crate) fn detach_repo(r: &mut LandRepo) {
    r.chain_tip = None;
    r.tested = None;
    r.base_before = None;
    r.tree_tested = None;
    r.merged = false;
    r.checked = false;
    r.tmp = None;
    r.tmp_admin = None;
    r.job = None;
    r.status = REPO_PENDING.to_string();
}

fn detach(l: &mut LandState) {
    l.batch = None;
    for r in l.repos.iter_mut().filter(|r| r.status != REPO_LANDED) {
        detach_repo(r);
    }
}

// ── Joining ──

/// `hiveLand`'s land waits in its repo's batch queue when that repo is busy: another land holds
/// its lock, or a batch run is under way there. None: it lands now, alone.
pub(crate) async fn try_join<W: HiveWindow>(ctx: &Ctx<'_, W>, caller: &Caller, w: &WorkerRecord, land: &LandState) -> Option<Outcome> {
    if !eligible(&caller.hive, w, land) {
        return None;
    }
    let folder = land.repos[0].folder.clone();
    let repo = PathBuf::from(&driver::hive_folder(&caller.hive, &folder).ok()?.path);
    let dir = worker::task_dir(&caller.paths, &w.task_id).ok()?;
    let (k, id) = (key(caller, &folder), land.id.clone());
    let joined = blocking(move || {
        let common = lg::common_dir(&repo)?;
        // Under the runners' lock, so a run that finds its queue empty and ends can't miss this.
        let mut runs = runners();
        let running = runs.contains(&k);
        let holder = lock::holder(&common);
        if !running && holder.is_none() {
            return Ok(None);
        }
        let mut ok = false;
        worker::update_worker(&dir, |cur| {
            let landing = cur.phase == worker::PHASE_LANDING;
            if let Some(l) = cur.land.as_mut().filter(|l| landing && l.id == id && l.state == STATE_LOCKING) {
                l.state = STATE_QUEUED_BATCH.to_string();
                l.queued_at = next_seq();
                l.batch = None;
                l.updated_at = crate::commands::workspace::iso_now();
                ok = true;
            }
            ok
        })?;
        if !ok {
            return Ok(None);
        }
        let start = runs.insert(k);
        Ok(Some((start, holder.flatten().map(|i| i.describe()))))
    })
    .await;
    let (start, who) = match joined {
        Ok(Some(j)) => j,
        Ok(None) => return None,
        Err(e) => {
            log::info!("hive {}: land {} not queued for a batch ({e}); it lands alone", caller.hive.id, land.id);
            return None;
        }
    };
    if start {
        ctx.window.continue_batch(caller, &folder);
    }
    ctx.window.workers_changed(&caller.workspace_id);
    log::info!("hive {}: land {} of {} queued for a batch in {folder}", caller.hive.id, land.id, worker_label(w));
    let busy = who.map_or_else(|| "other lands are landing there now".to_string(), |who| format!("{who} holds it"));
    Some(Outcome::Waiting(json!({
        "land": land.id,
        "state": STATE_QUEUED_BATCH,
        "folder": folder,
        "result": format!("Worker {}'s land {} waits to land in a batch in {folder}: {busy}.", worker_label(w), land.id),
        "note": "Once the repo is free, maiTerm merges every land waiting there onto Base in turn and runs the land check once on \
                 the lot; a batch that fails is split until the culprit is found. Each land's result arrives as ⟦HIVE LAND⟧.",
    })))
}

// ── The run ──

/// The queue's members as they stand, in `hiveLand` order. Blocks on the disk.
fn queued(paths: &HivePaths, folder: &str) -> Vec<WorkerRecord> {
    let mut v: Vec<WorkerRecord> = worker::list_workers(paths)
        .into_iter()
        .filter(|w| {
            w.phase == worker::PHASE_LANDING
                && w.land.as_ref().is_some_and(|l| l.state == STATE_QUEUED_BATCH && l.repos.len() == 1 && l.repos[0].folder == folder)
        })
        .collect();
    v.sort_by_key(|w| w.land.as_ref().map_or(0, |l| l.queued_at));
    v
}

/// The land of `task_id`, changed by `f` if it is still landing in a queue or a batch; the
/// record as written. Blocks.
fn edit(paths: &HivePaths, task_id: &str, f: impl FnOnce(&mut LandState)) -> Option<WorkerRecord> {
    edit_where(paths, task_id, |l| matches!(l.state.as_str(), STATE_QUEUED_BATCH | STATE_CHECKING | STATE_ADVANCING | STATE_LOCKING), f)
}

/// `edit`, for a land still landing that `ok` accepts. Blocks.
fn edit_where(paths: &HivePaths, task_id: &str, ok: impl Fn(&LandState) -> bool, f: impl FnOnce(&mut LandState)) -> Option<WorkerRecord> {
    let dir = worker::task_dir(paths, task_id).ok()?;
    let mut ours = false;
    let w = worker::update_worker(&dir, |cur| {
        let landing = cur.phase == worker::PHASE_LANDING;
        if let Some(l) = cur.land.as_mut().filter(|l| landing && ok(l)) {
            f(l);
            l.updated_at = crate::commands::workspace::iso_now();
            ours = true;
        }
        ours
    })
    .ok()?;
    ours.then_some(w)
}

/// Runs a repo's batch queue until it is empty: the lock taken, then two or more members landed as
/// a batch, or a lone one alone; members that leave a batch land alone after it. `landed` hears
/// each land that landed, so waiters launch from the new Base without waiting for the queue.
pub(crate) async fn run<W: HiveWindow>(ctx: &Ctx<'_, W>, caller: &Caller, folder: &str, opts: &Opts, landed: &(dyn Fn() + Send + Sync)) {
    let k = key(caller, folder);
    runners().insert(k.clone());
    let mut told = false;
    loop {
        let (p, f) = (caller.paths.clone(), folder.to_string());
        let waiting = blocking(move || Ok(queued(&p, &f))).await.unwrap_or_default();
        if waiting.is_empty() {
            let (p, f, k) = (caller.paths.clone(), folder.to_string(), k.clone());
            // Under the runners' lock: a land joining after this finds no run, and starts one.
            let done = blocking(move || {
                let mut runs = runners();
                let empty = queued(&p, &f).is_empty();
                if empty {
                    runs.remove(&k);
                }
                Ok(empty)
            })
            .await
            .unwrap_or(true);
            if done {
                return;
            }
            continue;
        }
        let id = new_id();
        match take_lock(caller, folder, &id, opts).await {
            Ok(Ok(())) => told = false,
            Ok(Err(who)) => {
                if !told {
                    told = true;
                    let n = waiting.len();
                    let text = format!(
                        "⟦HIVE LAND · {folder}⟧ {n} land{} waiting to land in a batch in {folder} still wait for the land lock: held by {who}.",
                        if n == 1 { "" } else { "s" }
                    );
                    driver::tell_text(ctx, caller, &waiting[0], text).await;
                }
                continue;
            }
            Err(e) => {
                // The repo can't be read: every member is refused rather than spun on.
                for w in waiting {
                    let out = refuse_member(ctx, caller, &w.task_id, format!("{folder} couldn't be locked for its batch: {e}")).await;
                    driver::tell(ctx, caller, &w, &out).await;
                }
                continue;
            }
        }
        let (p, f) = (caller.paths.clone(), folder.to_string());
        let members = blocking(move || Ok(queued(&p, &f))).await.unwrap_or_default();
        let alone = if members.len() >= 2 {
            land_batch(ctx, caller, folder, &id, members, opts, landed).await
        } else {
            members.into_iter().map(|w| w.task_id).collect()
        };
        release(caller, folder, &id).await;
        for task in alone {
            land_alone(ctx, caller, &task, opts, landed).await;
        }
    }
}

/// A run outside the tool call (`HiveWindow::continue_batch`): a busy lock waited on an hour at a
/// time, the coordinator told once who holds it.
pub(crate) fn run_background(app: Arc<crate::state::AppState>, handle: tauri::AppHandle, caller: Caller, folder: String) {
    tauri::async_runtime::spawn(async move {
        let ctx = crate::hive::tools::app_ctx(&app, &handle, &caller);
        let mut opts = Opts::production();
        opts.lock_wait = driver::LOCK_NOTICE_AFTER;
        let (a, h, ws) = (app.clone(), handle.clone(), caller.workspace_id.clone());
        let landed = move || crate::hive::waiters::poll_soon(a.clone(), h.clone(), ws.clone());
        run(&ctx, &caller, &folder, &opts, &landed).await;
    });
}

async fn take_lock(caller: &Caller, folder: &str, id: &str, opts: &Opts) -> Result<Result<(), String>, String> {
    let repo = PathBuf::from(&driver::hive_folder(&caller.hive, folder)?.path);
    let (info, wait) = (LockInfo::ours(&caller.hive.id, id), opts.lock_wait);
    blocking(move || {
        let common = lg::common_dir(&repo)?;
        match lock::take(&common, &info, Instant::now() + wait, |_| {})? {
            Taken::Taken => Ok(Ok(())),
            Taken::Busy(who) => Ok(Err(who.map_or_else(|| "a land whose lock file can't be read yet".to_string(), |i| i.describe()))),
        }
    })
    .await
}

/// The batch's lock, temporary worktree and folder gone.
async fn release(caller: &Caller, folder: &str, id: &str) {
    let (hive, paths, f, i) = (caller.hive.clone(), caller.paths.clone(), folder.to_string(), id.to_string());
    let _ = blocking(move || {
        remove(&hive, &paths, &i, &f);
        Ok(())
    })
    .await;
}

/// Blocks.
fn remove(hive: &Hive, paths: &HivePaths, id: &str, folder: &str) {
    if let Ok(hf) = driver::hive_folder(hive, folder) {
        let repo = PathBuf::from(&hf.path);
        lg::remove_temp(&repo, &paths.land_worktree(id, folder));
        if let Ok(c) = lg::common_dir(&repo) {
            lock::release(&c, id);
        }
    }
    let _ = crate::hive::retire::remove_path(&paths.land_tmp(id));
    let _ = std::fs::remove_dir(paths.land_dir(id));
}

/// At startup: what a batch run left under `wt.noindex/land/` goes, since none survives a restart.
/// Blocks.
pub fn sweep(hive: &Hive, paths: &HivePaths) {
    let Ok(dir) = std::fs::read_dir(paths.worktrees().join(LAND_SLUG)) else { return };
    for e in dir.flatten() {
        let name = e.file_name().to_string_lossy().to_string();
        if !name.starts_with(ID_PREFIX) || !crate::hive::paths::is_component(&name) {
            continue;
        }
        for f in hive.folders.iter().filter(|f| f.is_git()) {
            remove(hive, paths, &name, &f.name);
        }
    }
}

/// A member landing on its own: the queue's lone one, or one that left its batch. The coordinator
/// hears its result.
async fn land_alone<W: HiveWindow>(ctx: &Ctx<'_, W>, caller: &Caller, task_id: &str, opts: &Opts, landed: &(dyn Fn() + Send + Sync)) {
    let (p, t) = (caller.paths.clone(), task_id.to_string());
    // Waiting, or left for this: a land already driven (its check queued, its temporary worktree
    // made) is never detached and driven again under the check it runs.
    let waiting = |l: &LandState| {
        l.state == STATE_QUEUED_BATCH || (l.state == STATE_LOCKING && l.repos.iter().all(|r| r.job.is_none() && r.tmp.is_none()))
    };
    let ready = blocking(move || {
        Ok(edit_where(&p, &t, waiting, |l| {
            detach(l);
            l.state = STATE_LOCKING.to_string();
        }))
    })
    .await
    .ok()
    .flatten();
    if ready.is_none() {
        return;
    }
    let out = driver::drive_to_end(ctx, caller, task_id, opts).await;
    let (p, t) = (caller.paths.clone(), task_id.to_string());
    if let Ok(w) = blocking(move || worker::read_worker(&worker::task_dir(&p, &t)?)).await {
        driver::tell(ctx, caller, &w, &out).await;
    }
    if matches!(out, Outcome::Landed(_)) {
        landed();
    }
}

/// A member's land refused (nothing of it landed): back in Reported. Its outcome.
async fn refuse_member<W: HiveWindow>(ctx: &Ctx<'_, W>, caller: &Caller, task_id: &str, why: String) -> Outcome {
    let (p, t) = (caller.paths.clone(), task_id.to_string());
    let Some(w) = blocking(move || Ok(edit(&p, &t, detach))).await.ok().flatten() else { return Outcome::Gone };
    let Some(land) = w.land.clone() else { return Outcome::Gone };
    driver::fail(ctx, caller, &w, land, why).await
}

/// A member's land held at step 2: `hiveLand` with the same commits resumes it, alone or in a
/// batch, from its pin.
async fn hold_member<W: HiveWindow>(ctx: &Ctx<'_, W>, caller: &Caller, task_id: &str, folder: &str, why: String) -> Outcome {
    let (p, t) = (caller.paths.clone(), task_id.to_string());
    let Some(w) = blocking(move || Ok(edit(&p, &t, detach))).await.ok().flatten() else { return Outcome::Gone };
    let Some(land) = w.land.clone() else { return Outcome::Gone };
    driver::hold(ctx, caller, &w, land, folder, why).await
}

/// The members of a batch, landed by halves: what doesn't pass is split until each failing land
/// stands alone. Answers the members that left it, to land alone.
async fn land_batch<W: HiveWindow>(
    ctx: &Ctx<'_, W>,
    caller: &Caller,
    folder: &str,
    id: &str,
    members: Vec<WorkerRecord>,
    opts: &Opts,
    landed: &(dyn Fn() + Send + Sync),
) -> Vec<String> {
    active().insert(id.to_string());
    log::info!("hive {}: batch {id} in {folder}: {} members", caller.hive.id, members.len());
    let mut alone: Vec<String> = Vec::new();
    let mut todo: Vec<Vec<String>> = vec![members.iter().map(|w| w.task_id.clone()).collect()];
    let mut base_checked = false;
    while let Some(set) = todo.pop() {
        match land_set(ctx, caller, folder, id, &set, opts, &mut alone, landed).await {
            SetEnd::Done => {}
            SetEnd::Split(first, second) => {
                todo.push(second);
                todo.push(first);
            }
            SetEnd::Culprit(w) => {
                // Once per batch: tell a broken Base from broken members (§8.6).
                if !base_checked {
                    base_checked = true;
                    super::base_check::run(ctx, caller, &w, folder, &format!("batch {id}'s check failed on {}'s land alone", worker_label(&w)), true, opts).await;
                }
            }
            SetEnd::Held(why) => {
                // Every set left would fast-forward the same checkout: held with it.
                for task in todo.drain(..).flatten() {
                    let out = hold_member(ctx, caller, &task, folder, format!("held with its batch {id}: {why}")).await;
                    let (p, t) = (caller.paths.clone(), task.clone());
                    if let Ok(w) = blocking(move || worker::read_worker(&worker::task_dir(&p, &t)?)).await {
                        driver::tell(ctx, caller, &w, &out).await;
                    }
                }
            }
        }
    }
    active().remove(id);
    alone
}

enum SetEnd {
    Done,
    /// Its check failed: each half is tried on its own, the first first.
    Split(Vec<String>, Vec<String>),
    /// A lone member failed the check: its land was refused.
    Culprit(Box<WorkerRecord>),
    /// Step 2 held, for this reason: its members were held.
    Held(String),
}

/// A chain built in the temporary worktree.
struct Chain {
    base: String,
    tip: String,
    tmp: Temp,
    /// (task, its merge commit), in order.
    merges: Vec<(String, String)>,
    /// (task, why it left).
    left: Vec<(String, String)>,
}

/// The chain (§6.8): the target's tip, then each member's pinned commit merged `--no-ff` in order.
/// A member whose merge conflicts, merges nothing, or whose own merge (`<merge>^1..<merge>`) trips
/// the config rule leaves, the chain as it was before it. Blocks on git.
fn build(hive: &Hive, paths: &HivePaths, id: &str, folder: &str, members: &[WorkerRecord]) -> Result<Chain, String> {
    let repo = PathBuf::from(&driver::hive_folder(hive, folder)?.path);
    let target = super::land_target(hive, folder);
    let base = lg::branch_tip(&repo, &target).ok_or_else(|| format!("{folder}: its landing target, {target}, doesn't exist any more"))?;
    // Its own cargo target seeded once from the checkout's, as the post-land check's is.
    let tmp = super::base_check::seeded_temp(paths, &repo, id, folder, &base).map_err(|e| format!("{folder}: {e}"))?;
    let mut chain = Chain { base: base.clone(), tip: base, tmp, merges: Vec::new(), left: Vec::new() };
    for w in members {
        let Some(r) = w.land.as_ref().and_then(|l| l.repos.first()) else { continue };
        let land_id = w.land.as_ref().map(|l| l.id.clone()).unwrap_or_default();
        let title = w.title.replace(['\n', '\r'], " ");
        let message = format!("Land {} ({}) [maiTerm {land_id}]", title.trim(), &r.pinned[..r.pinned.len().min(7)]);
        match lg::merge_pinned(&repo, &chain.tmp, &chain.tip, &r.pinned, &message) {
            Err(e) => chain.left.push((w.task_id.clone(), format!("its merge failed ({})", one_line(&e, 200)))),
            Ok(Merged::Conflict(files)) => chain.left.push((w.task_id.clone(), format!("it conflicts with the lands before it in {}", driver::files_line(&files)))),
            Ok(Merged::Tested { merged: false, .. }) => chain.left.push((w.task_id.clone(), format!("{target} already has its commit"))),
            Ok(Merged::Tested { sha, .. }) => {
                let caught = super::config_rule::caught(&repo, &chain.tip, &sha, &hive.settings.land.config_paths)?;
                if !caught.files.is_empty() {
                    lg::reset_temp(&chain.tmp, &chain.tip)?;
                    chain.left.push((w.task_id.clone(), format!("it changes agent or execution config ({})", driver::files_line(&caught.files))));
                    continue;
                }
                chain.merges.push((w.task_id.clone(), sha.clone()));
                chain.tip = sha;
            }
        }
    }
    Ok(chain)
}

/// One set of a batch to its end: built from the target's tip, checked once, and fast-forwarded
/// there, rebuilt when the target moves before that (three times, then held).
#[allow(clippy::too_many_arguments)]
async fn land_set<W: HiveWindow>(
    ctx: &Ctx<'_, W>,
    caller: &Caller,
    folder: &str,
    id: &str,
    set: &[String],
    opts: &Opts,
    alone: &mut Vec<String>,
    landed: &(dyn Fn() + Send + Sync),
) -> SetEnd {
    let mut attempts = 0u32;
    loop {
        // The set's members, still waiting, as they stand on disk: this batch's now. One that left
        // on an earlier try stays out (it lands alone after the batch), and only a queued or
        // advancing land is this set's to take back.
        let s: Vec<String> = set.iter().filter(|t| !alone.contains(t)).cloned().collect();
        let (p, i) = (caller.paths.clone(), id.to_string());
        let members: Vec<WorkerRecord> = blocking(move || {
            Ok(s.iter()
                .filter_map(|t| {
                    edit_where(&p, t, |l| matches!(l.state.as_str(), STATE_QUEUED_BATCH | STATE_ADVANCING), |l| {
                        detach(l);
                        l.state = STATE_QUEUED_BATCH.to_string();
                        l.batch = Some(i.clone());
                    })
                })
                .collect())
        })
        .await
        .unwrap_or_default();
        if members.is_empty() {
            return SetEnd::Done;
        }
        let (hive, paths, i, f, ms) = (caller.hive.clone(), caller.paths.clone(), id.to_string(), folder.to_string(), members.clone());
        let chain = match blocking(move || build(&hive, &paths, &i, &f, &ms)).await {
            Ok(c) => c,
            Err(e) => {
                for w in &members {
                    let out = refuse_member(ctx, caller, &w.task_id, format!("its batch couldn't be built: {e}")).await;
                    driver::tell(ctx, caller, w, &out).await;
                }
                return SetEnd::Done;
            }
        };
        for (task, why) in &chain.left {
            log::info!("hive {}: batch {id}: {task} leaves the batch and lands alone: {why}", caller.hive.id);
            let (p, t) = (caller.paths.clone(), task.clone());
            let _ = blocking(move || {
                Ok(edit(&p, &t, |l| {
                    detach(l);
                    l.state = STATE_LOCKING.to_string();
                }))
            })
            .await;
            if !alone.contains(task) {
                alone.push(task.clone());
            }
        }
        let inchain: Vec<WorkerRecord> = chain.merges.iter().filter_map(|(t, _)| members.iter().find(|w| &w.task_id == t).cloned()).collect();
        if inchain.is_empty() {
            return SetEnd::Done;
        }
        let tasks: Vec<String> = inchain.iter().map(|w| w.task_id.clone()).collect();
        mark(caller, &chain, &tasks, None).await;

        let end = check(caller, folder, id, &chain, &inchain, opts).await;
        let checked = end.passed.is_some() && land_check(&caller.hive, folder).is_some();
        match end.passed {
            None => {
                for w in &inchain {
                    let why = format!("The batch land check in {folder} didn't finish ({}), so nothing landed.", one_line(&end.why, 200));
                    let out = refuse_member(ctx, caller, &w.task_id, why).await;
                    driver::tell(ctx, caller, w, &out).await;
                }
                return SetEnd::Done;
            }
            Some(false) if inchain.len() == 1 => {
                let w = inchain[0].clone();
                let why = format!(
                    "Its batch {id}'s land check failed, and split down to this land alone it still fails. {}",
                    driver::check_failed(folder, &end.tail)
                );
                let out = refuse_member(ctx, caller, &w.task_id, why).await;
                driver::tell(ctx, caller, &w, &out).await;
                return SetEnd::Culprit(Box::new(w));
            }
            Some(false) => {
                log::info!("hive {}: batch {id}'s check failed on {} members; split", caller.hive.id, tasks.len());
                let (p, s) = (caller.paths.clone(), tasks.clone());
                let _ = blocking(move || {
                    for t in &s {
                        edit(&p, t, |l| {
                            detach(l);
                            l.state = STATE_QUEUED_BATCH.to_string();
                        });
                    }
                    Ok(())
                })
                .await;
                let mid = tasks.len().div_ceil(2);
                return SetEnd::Split(tasks[..mid].to_vec(), tasks[mid..].to_vec());
            }
            Some(true) => {}
        }

        let repo = match driver::hive_folder(&caller.hive, folder) {
            Ok(hf) => PathBuf::from(&hf.path),
            Err(e) => {
                for w in &inchain {
                    let out = refuse_member(ctx, caller, &w.task_id, e.clone()).await;
                    driver::tell(ctx, caller, w, &out).await;
                }
                return SetEnd::Done;
            }
        };
        // Persisted before Base moves: a restart reads Base to know whether each merge landed, and
        // logs one that did as `land_members` would, its check passed on the chain's tree.
        let (rp, tip) = (repo.clone(), chain.tip.clone());
        let tree = blocking(move || Ok(lg::tree_of(&rp, &tip))).await.ok().flatten();
        mark_job(caller, &chain, &tasks, Some(STATE_ADVANCING), None, Some((checked, tree))).await;
        if let Some(seam) = opts.before_advance.clone() {
            let f = folder.to_string();
            let _ = blocking(move || {
                seam(&f);
                Ok(())
            })
            .await;
        }
        let target = super::land_target(&caller.hive, folder);
        let (rp, t, from, to) = (repo.clone(), target.clone(), chain.base.clone(), chain.tip.clone());
        match blocking(move || lg::advance(&rp, &t, &from, &to)).await {
            Ok(Step2::Advanced) => {
                land_members(ctx, caller, folder, id, &chain, &inchain, checked, opts, landed).await;
                return SetEnd::Done;
            }
            Ok(Step2::Held(why)) => {
                for w in &inchain {
                    let out = hold_member(ctx, caller, &w.task_id, folder, why.clone()).await;
                    driver::tell(ctx, caller, w, &out).await;
                }
                return SetEnd::Held(why);
            }
            Ok(Step2::BaseMoved) => {
                attempts += 1;
                log::info!("hive {}: batch {id}: {target} moved before its fast-forward ({attempts}/{MAX_ATTEMPTS})", caller.hive.id);
                if attempts >= MAX_ATTEMPTS {
                    let why = format!("{target} keeps moving ({attempts} tries)");
                    for w in &inchain {
                        let out = hold_member(ctx, caller, &w.task_id, folder, why.clone()).await;
                        driver::tell(ctx, caller, w, &out).await;
                    }
                    return SetEnd::Held(why);
                }
            }
            Err(e) => {
                for w in &inchain {
                    let out = refuse_member(ctx, caller, &w.task_id, e.clone()).await;
                    driver::tell(ctx, caller, w, &out).await;
                }
                return SetEnd::Done;
            }
        }
    }
}

/// The chain on each member's record: its own merge as `tested`, the chain's tip and Base, and
/// `state` (the check's job, or step 2) when given, and a passed check as (`checked`, the tree it
/// ran on).
async fn mark(caller: &Caller, chain: &Chain, tasks: &[String], state: Option<&'static str>) {
    mark_job(caller, chain, tasks, state, None, None).await;
}

async fn mark_job(caller: &Caller, chain: &Chain, tasks: &[String], state: Option<&'static str>, job: Option<String>, passed: Option<(bool, Option<String>)>) {
    let (p, merges, base, tip, tasks) = (caller.paths.clone(), chain.merges.clone(), chain.base.clone(), chain.tip.clone(), tasks.to_vec());
    let _ = blocking(move || {
        for t in &tasks {
            let Some(merge) = merges.iter().find(|(x, _)| x == t).map(|(_, m)| m.clone()) else { continue };
            edit(&p, t, |l| {
                if let Some(s) = state {
                    l.state = s.to_string();
                }
                if let Some(r) = l.repos.first_mut() {
                    r.tested = Some(merge);
                    r.merged = true;
                    r.chain_tip = Some(tip.clone());
                    r.base_before = Some(base.clone());
                    match state {
                        Some(STATE_CHECKING) => {
                            r.status = REPO_CHECKING.to_string();
                            r.job = job.clone();
                        }
                        Some(STATE_ADVANCING) => r.status = REPO_ADVANCING.to_string(),
                        _ => {}
                    }
                    if let Some((checked, tree)) = passed.clone() {
                        r.checked = checked;
                        r.tree_tested = tree;
                    }
                }
            });
        }
        Ok(())
    })
    .await;
}

/// The chain's one land check: inline (a test's checker), or queued and awaited. Seeded like a
/// lone land's from its first member's worktree, since the job's network is closed (§13.1).
async fn check(caller: &Caller, folder: &str, id: &str, chain: &Chain, inchain: &[WorkerRecord], opts: &Opts) -> CheckEnd {
    let Some(command) = land_check(&caller.hive, folder) else {
        // Its check was taken off meanwhile: there is nothing to run, as for a lone land.
        return CheckEnd { passed: Some(true), tail: String::new(), why: String::new() };
    };
    let first = inchain[0].clone();
    let own = first.git_folders().find(|f| f.name == folder).cloned();
    let req = CheckRequest {
        caller: caller.clone(),
        worker: first,
        folder: folder.to_string(),
        land_id: id.to_string(),
        tmp: chain.tmp.clone(),
        tested: chain.tip.clone(),
        command,
        kind: super::CHECK_BATCH,
    };
    let checker = opts.checker.clone();
    let started = blocking(move || {
        if let Some(f) = own {
            let seeded = lg::seed_ignored(&f, &req.tmp.path, &req.caller.hive.settings.land.config_paths);
            if !seeded.is_empty() {
                log::info!("hive {}: batch {}: seeded {} from worker {}'s worktree: {}", req.caller.hive.id, req.land_id, req.folder, req.worker.slug, seeded.join(", "));
            }
        }
        checker.start(&req)
    })
    .await;
    match started {
        Err(e) => CheckEnd { passed: None, tail: String::new(), why: format!("it couldn't be queued: {e}") },
        Ok(CheckStart::Done { passed, tail }) => CheckEnd { passed: Some(passed), tail, why: String::new() },
        Ok(CheckStart::Queued(job)) => {
            let tasks: Vec<String> = inchain.iter().map(|w| w.task_id.clone()).collect();
            mark_job(caller, chain, &tasks, Some(STATE_CHECKING), Some(job.clone()), None).await;
            await_check(&job).await
        }
    }
}

async fn await_check(job: &str) -> CheckEnd {
    let rx = {
        let mut e = ends();
        if let Some(Slot::Ended(end)) = e.remove(job) {
            return end;
        }
        let (tx, rx) = tokio::sync::oneshot::channel();
        e.insert(job.to_string(), Slot::Waiting(tx));
        rx
    };
    rx.await.unwrap_or_else(|_| CheckEnd { passed: None, tail: String::new(), why: "its result was lost".to_string() })
}

/// A batch's queued check ended (`driver::check_ended`): its run, awaiting it, carries on. A check
/// that moved its tested commit, or ran past its time, failed; one stopped otherwise didn't finish.
pub(crate) fn on_check_end(job: &crate::hive::queue::Job, tail: &str) {
    use crate::hive::queue::{BY_TIMEOUT, DONE};
    let Some(run) = &job.land else { return };
    let err = job.error.as_deref().map(|e| one_line(e, 200)).unwrap_or_else(|| job.state.clone());
    let end = if job.state == DONE && job.began && job.dirty {
        CheckEnd { passed: Some(false), tail: format!("{tail}\n(maiTerm: the check moved the commit it tested: {err})"), why: String::new() }
    } else if job.state == DONE && job.began {
        CheckEnd { passed: Some(job.exit_code == Some(0)), tail: tail.to_string(), why: String::new() }
    } else if job.stopped_by.as_deref() == Some(BY_TIMEOUT) {
        CheckEnd { passed: Some(false), tail: format!("{tail}\n(maiTerm: the check ran past its time)"), why: String::new() }
    } else {
        CheckEnd { passed: None, tail: tail.to_string(), why: err }
    };
    let mut e = ends();
    match e.remove(&job.id) {
        Some(Slot::Waiting(tx)) => {
            let _ = tx.send(end);
        }
        _ if active().contains(&run.land_id) => {
            e.insert(job.id.clone(), Slot::Ended(end));
        }
        _ => log::info!("hive: batch check {} of {} ended unknown to this run; nothing carried on", job.id, run.land_id),
    }
}

/// A set whose chain passed and whose target fast-forwarded to its tip: each member whose merge is
/// now in the target is logged (`batch_member`, its merge commit named), pushed once for all, and
/// retired as landed, its branch left at its pin. The post-land check runs once if the landed tree
/// isn't the tested one (§8.6).
#[allow(clippy::too_many_arguments)]
async fn land_members<W: HiveWindow>(
    ctx: &Ctx<'_, W>,
    caller: &Caller,
    folder: &str,
    id: &str,
    chain: &Chain,
    inchain: &[WorkerRecord],
    checked: bool,
    opts: &Opts,
    landed: &(dyn Fn() + Send + Sync),
) {
    let Ok(hf) = driver::hive_folder(&caller.hive, folder) else { return };
    let (hive, repo, target, tip, merges) = (caller.hive.clone(), PathBuf::from(&hf.path), super::land_target(&caller.hive, folder), chain.tip.clone(), chain.merges.clone());
    let facts = blocking(move || {
        let tree_tested = lg::tree_of(&repo, &tip);
        let tree_landed = lg::tree_of(&repo, &format!("refs/heads/{target}"));
        let now = lg::branch_tip(&repo, &target).unwrap_or_default();
        // Once per batch (§8.5).
        let pushed = if hive.push_after_land && !hive.is_integration() { lg::push(&repo, &target) } else { super::log::PUSHED_OFF.to_string() };
        let in_target: Vec<String> = merges.iter().filter(|(_, m)| lg::is_ancestor(&repo, m, &now)).map(|(t, _)| t.clone()).collect();
        Ok((tree_tested, tree_landed, pushed, in_target))
    })
    .await;
    let Ok((tree_tested, tree_landed, pushed, in_target)) = facts else { return };
    let names: Vec<String> = inchain.iter().map(worker_label).collect();
    log::info!("hive {}: batch {id} landed {} in {folder} at {}", caller.hive.id, names.join(", "), &chain.tip[..chain.tip.len().min(10)]);
    for w in inchain {
        if !in_target.contains(&w.task_id) {
            let out = hold_member(ctx, caller, &w.task_id, folder, format!("its merge isn't in {} after its batch's fast-forward", super::land_target(&caller.hive, folder))).await;
            driver::tell(ctx, caller, w, &out).await;
            continue;
        }
        let (p, h, t, tip, tt, tl, ps) = (caller.paths.clone(), caller.hive.clone(), w.task_id.clone(), chain.tip.clone(), tree_tested.clone(), tree_landed.clone(), pushed.clone());
        let rec = blocking(move || {
            let dir = worker::task_dir(&p, &t)?;
            let cur = worker::read_worker(&dir)?;
            let Some(mut land) = cur.land.clone().filter(|_| cur.phase == worker::PHASE_LANDING) else { return Ok(None) };
            let (lid, summary, review, decision) = (land.id.clone(), land.summary.clone(), land.review.clone(), land.decision.clone());
            if let Some(r) = land.repos.first_mut() {
                r.status = REPO_LANDED.to_string();
                r.held_reason = None;
                r.job = None;
                r.base_after = Some(tip);
                r.tree_tested = tt;
                r.tree_landed = tl;
                r.pushed = Some(ps);
                r.checked = checked;
                driver::log_repo(&p, &h, &cur, &lid, &summary, &review, None, decision, r);
            }
            Ok(Some((cur, land)))
        })
        .await;
        let Ok(Some((cur, land))) = rec else { continue };
        if let Ok(dir) = worker::task_dir(&caller.paths, &w.task_id) {
            if let Err(e) = driver::save(&dir, &land).await {
                log::warn!("hive {}: batch {id}: {}'s land not saved: {e}", caller.hive.id, worker_label(w));
            }
        }
        let mut out = driver::finish(ctx, caller, &cur, land).await;
        if let Outcome::Landed(v) = &mut out {
            let others: Vec<&String> = names.iter().filter(|n| **n != worker_label(w)).collect();
            let with = if others.is_empty() { String::new() } else { format!(" with {}", others.iter().map(|s| s.as_str()).collect::<Vec<_>>().join(", ")) };
            let said = v.get("result").and_then(Value::as_str).unwrap_or_default().to_string();
            v["result"] = json!(format!(
                "{said} It landed in batch {id}{with}: one land check on the chain of their merges passed, and its own merge commit is what landed (its branch stays at the commit you reviewed)."
            ));
            v["batch"] = json!(id);
        }
        driver::tell(ctx, caller, &cur, &out).await;
        if matches!(out, Outcome::Landed(_)) {
            landed();
        }
    }
    if tree_landed != tree_tested {
        if let Some(w) = inchain.first() {
            super::base_check::run(ctx, caller, w, folder, &format!("batch {id} landed a tree other than the one its check passed"), false, opts).await;
        }
    }
}
