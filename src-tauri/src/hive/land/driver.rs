//! Running a land (docs/hive.md §8.1, §8.2): the locks in Hive folder order, step 1 and the land
//! check in every repo before any step 2 (anything failing there lands nothing, and the worker goes
//! back to Reported), then step 2 in order, stopping at the first held repo (plan D7). Once every
//! repo has landed the worker retires, its branch moves to what landed, the coordinator's read copy
//! follows Base, and with Push after land each repo was pushed right after its fast-forward.
//!
//! A land with a check pauses at `checking`: the queue runs it (`QueueChecker`), and its end
//! (`check_ended`) carries the land on, its result reaching the coordinator as `⟦HIVE LAND⟧`. A
//! lock busy past the tool's inline wait carries on in the background the same way. A hold ends
//! the run: its locks and temporary worktrees go, `tested` stays, and the coordinator's next
//! `hiveLand` resumes it (plan D6: never by polling).

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use serde_json::{json, Value};

use super::git::{self as lg, Merged, Step2, Temp};
use super::lock::{self, LockInfo, Taken};
use super::log::{self as landlog, LandLogEntry};
use super::{LandArgs, LandRepo, LandState, MAX_ATTEMPTS, REPO_ADVANCING, REPO_CHECKING, REPO_HELD, REPO_LANDED, REPO_PENDING, REPO_TESTED};
use super::{STATE_ADVANCING, STATE_CHECKING, STATE_HELD, STATE_LANDED, STATE_LOCKING};
use crate::hive::deliver::{frame_escape, one_line};
use crate::hive::paths::HivePaths;
use crate::hive::record::{Hive, HiveFolder};
use crate::hive::retire::{retire_worker, RetireSpec};
use crate::hive::rpc::HiveWindow;
use crate::hive::tools::{blocking, coordinator_tab, worker_label, Caller, Ctx};
use crate::hive::worker::{self, WorkerRecord};

/// How long `hiveLand` waits on a busy lock before it carries on in the background.
pub const LOCK_INLINE_WAIT: Duration = Duration::from_secs(10);
/// A land waiting this long on a lock tells the coordinator who holds it (§8.1).
pub const LOCK_NOTICE_AFTER: Duration = Duration::from_secs(3600);
/// Land checks go ahead of ordinary runs: a land holds its repo's lock while one waits.
pub const LAND_PRIORITY: i64 = 100;
const TAIL_LINES: usize = 30;

// ── The land check ──

/// One repo's land check, as the driver asks for it.
pub(crate) struct CheckRequest {
    pub caller: Caller,
    pub worker: WorkerRecord,
    pub folder: String,
    pub land_id: String,
    pub tmp: Temp,
    pub tested: String,
    /// The folder's land check: the human's own text. Never its setup, which needs the network
    /// the job's profile closes (`git::seed_ignored` stands in for it).
    pub command: String,
    /// Whose check: a land's (`CHECK_LAND`), an undo's or the post-land check (§8.6), which run
    /// under the land profile and end in their own modules.
    pub kind: &'static str,
}

pub(crate) enum CheckStart {
    /// The run queue has it; `check_ended` hears its end.
    Queued(String),
    /// It ran to its end already (a test's checker).
    Done { passed: bool, tail: String },
}

pub(crate) trait Checker: Send + Sync {
    fn start(&self, req: &CheckRequest) -> Result<CheckStart, String>;
}

/// `land:<folder>`, a resource of one: one land check per repo at a time (§6.8).
pub fn land_resource(folder: &str) -> String {
    let s: String = folder.to_lowercase().chars().map(|c| if c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-') { c } else { '-' }).collect();
    let mut r = format!("land:{s}");
    r.truncate(64);
    r
}

/// The queue's checker: a job under the worker's sandbox profile, its cwd the temporary worktree.
pub(crate) struct QueueChecker;

/// The job a land check is (§6.8): `/bin/sh -c "<land_check>"` in the temporary worktree, needing
/// `land:<folder>` and whatever the queued entry its check starts with needs.
pub(crate) fn check_spec(req: &CheckRequest) -> crate::hive::queue::JobSpec {
    use crate::hive::queue::{JobSpec, LandRun};
    let q = &req.caller.hive.settings.run_queue;
    let check = req.caller.hive.folders.iter().find(|f| f.name == req.folder).and_then(|f| f.land_check.clone()).unwrap_or_default();
    let words: Vec<String> = check.split_whitespace().map(str::to_string).collect();
    let mut needs = vec![land_resource(&req.folder)];
    if let Some(e) = crate::hive::run::match_entry(q, &req.folder, &words) {
        needs.extend(e.needs.iter().cloned());
    }
    let (entry, profile) = match req.kind {
        super::CHECK_UNDO => ("undo check", crate::hive::queue::PROFILE_LAND),
        super::CHECK_BASE => ("post-land check", crate::hive::queue::PROFILE_LAND),
        // A chain of several workers' work: no one worker's folders are its to write.
        super::CHECK_BATCH => ("batch land check", crate::hive::queue::PROFILE_LAND),
        _ => ("land check", crate::hive::queue::PROFILE_WORKER),
    };
    JobSpec {
        hive_id: req.caller.hive.id.clone(),
        workspace_id: req.caller.workspace_id.clone(),
        window: req.caller.label.clone(),
        task_id: req.worker.task_id.clone(),
        worker_slug: req.worker.slug.clone(),
        folder: req.folder.clone(),
        target: format!("{}{}", crate::stack::target::CHECKOUT, req.tmp.path.display()),
        argv: crate::hive::job_sandbox::service_argv(&req.command),
        entry: entry.to_string(),
        needs,
        shares: q.resources.clone(),
        commit: Some(req.tested.clone()),
        priority: LAND_PRIORITY,
        land: Some(LandRun {
            land_id: req.land_id.clone(),
            worktree: req.tmp.path.to_string_lossy().to_string(),
            admin: req.tmp.admin.to_string_lossy().to_string(),
            profile: profile.to_string(),
            kind: req.kind.to_string(),
        }),
        ..JobSpec::default()
    }
}

impl Checker for QueueChecker {
    fn start(&self, req: &CheckRequest) -> Result<CheckStart, String> {
        let job = crate::hive::queue::enqueue(check_spec(req))?;
        Ok(CheckStart::Queued(job.id))
    }
}

/// Called with the folder just before its step 2.
pub(crate) type Seam = Arc<dyn Fn(&str) + Send + Sync>;

#[derive(Clone)]
pub(crate) struct Opts {
    pub lock_wait: Duration,
    pub checker: Arc<dyn Checker>,
    /// A test's seam, run just before each step 2 (a commit to Base, to move it).
    pub before_advance: Option<Seam>,
    /// A test's seam, run just after an undo's step 2 moved the target and before its landed
    /// tree is read (a commit to Base, so that tree isn't the tested one).
    pub after_advance: Option<Seam>,
}

impl Opts {
    pub fn production() -> Self {
        Self { lock_wait: LOCK_INLINE_WAIT, checker: Arc::new(QueueChecker), before_advance: None, after_advance: None }
    }
}

/// How a run of the land ended.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Outcome {
    Landed(Value),
    Held(Value),
    Checking(Value),
    Waiting(Value),
    /// It waits on the human's card (`approval`), holding no lock.
    Awaiting(Value),
    /// The land is off and the worker back in Reported.
    Refused(String),
    /// Nothing of it to run (another path settled it meanwhile).
    Gone,
}

impl Outcome {
    pub fn answer(self) -> Result<Value, String> {
        match self {
            Outcome::Refused(e) => Err(e),
            Outcome::Gone => Err("The land isn't running any more: look with hiveListWorkers.".to_string()),
            Outcome::Landed(v) | Outcome::Held(v) | Outcome::Checking(v) | Outcome::Waiting(v) | Outcome::Awaiting(v) => Ok(v),
        }
    }

    /// The `⟦HIVE LAND⟧` for a result that comes after the tool's own answer.
    pub fn message(&self, w: &WorkerRecord) -> Option<String> {
        let says = match self {
            Outcome::Refused(e) => e.clone(),
            Outcome::Landed(v) | Outcome::Held(v) | Outcome::Waiting(v) | Outcome::Awaiting(v) => v.get("result").and_then(Value::as_str)?.to_string(),
            Outcome::Checking(_) | Outcome::Gone => return None,
        };
        Some(format!("⟦HIVE LAND · {}⟧ {says}", one_line(&w.title, 120)))
    }
}

pub(super) fn hive_folder<'a>(hive: &'a Hive, folder: &str) -> Result<&'a HiveFolder, String> {
    hive.folders.iter().find(|f| f.name == folder && f.is_git()).ok_or_else(|| format!("This Hive has no repo called \"{folder}\" now"))
}

fn short(sha: &str) -> &str {
    &sha[..sha.len().min(10)]
}

pub(super) fn files_line(files: &[String]) -> String {
    let shown: Vec<String> = files.iter().take(10).map(|f| format!("`{}`", one_line(f, 200))).collect();
    let more = files.len().saturating_sub(10);
    if more > 0 { format!("{} and {more} more", shown.join(", ")) } else { shown.join(", ") }
}

/// The land written into `worker.json`, if it is still this land there.
pub(super) async fn save(dir: &Path, land: &LandState) -> Result<(), String> {
    let (d, mut l) = (dir.to_path_buf(), land.clone());
    l.updated_at = crate::commands::workspace::iso_now();
    blocking(move || {
        let id = l.id.clone();
        let mut ours = false;
        worker::update_worker(&d, |cur| {
            ours = cur.land.as_ref().is_some_and(|x| x.id == id);
            if ours {
                cur.land = Some(l);
            }
            ours
        })?;
        if ours { Ok(()) } else { Err(format!("land {id} is no longer this worker's")) }
    })
    .await
}

fn warn_save(caller: &Caller, res: Result<(), String>) {
    if let Err(e) = res {
        log::warn!("hive {}: land state not saved: {e}", caller.hive.id);
    }
}

// ── Locks ──

/// Every lock the land's unlanded repos need, in Hive folder order, one per git common dir. Busy
/// past `wait`: the locks taken so far go back, and the answer is who holds the busy one.
async fn take_locks(caller: &Caller, land: &LandState, wait: Duration) -> Result<Result<(), String>, String> {
    let (hive, land) = (caller.hive.clone(), land.clone());
    blocking(move || {
        let mut commons: Vec<(String, PathBuf)> = Vec::new();
        for r in land.repos.iter().filter(|r| r.status != REPO_LANDED) {
            let c = lg::common_dir(Path::new(&hive_folder(&hive, &r.folder)?.path))?;
            if !commons.iter().any(|(_, x)| *x == c) {
                commons.push((r.folder.clone(), c));
            }
        }
        let info = LockInfo::ours(&hive.id, &land.id);
        let mut taken: Vec<PathBuf> = Vec::new();
        for (folder, c) in &commons {
            match lock::take(c, &info, Instant::now() + wait, |_| {})? {
                Taken::Taken => taken.push(c.clone()),
                Taken::Busy(who) => {
                    for t in &taken {
                        lock::release(t, &land.id);
                    }
                    let who = who.map_or_else(|| "a land whose lock file can't be read yet".to_string(), |i| i.describe());
                    return Ok(Err(format!("{who} in {folder}")));
                }
            }
        }
        Ok(Ok(()))
    })
    .await
}

/// The land's temporary worktrees removed, its queued checks stopped and its locks released.
/// Blocks.
pub(super) fn cleanup(hive: &Hive, paths: &HivePaths, land: &mut LandState) {
    for r in land.repos.iter_mut() {
        if let Some(job) = r.job.take().filter(|_| r.status == REPO_CHECKING) {
            crate::hive::queue::interrupt(&job, crate::hive::queue::BY_RELEASE, "its land ended");
        }
        let Ok(hf) = hive_folder(hive, &r.folder) else { continue };
        let repo = PathBuf::from(&hf.path);
        if let Some(tmp) = r.tmp.take() {
            lg::remove_temp(&repo, Path::new(&tmp));
        }
        r.tmp_admin = None;
        if let Ok(c) = lg::common_dir(&repo) {
            lock::release(&c, &land.id);
        }
    }
    // Its check's temp folder, then its folder once nothing kept is left in it.
    let _ = crate::hive::retire::remove_path(&paths.land_tmp(&land.id));
    let _ = std::fs::remove_dir(paths.land_dir(&land.id));
}

// ── Step 1 ──

/// Step 1 in one repo (§8.1): a temporary worktree at the pinned sha, Base's sha merged in, and
/// the config rule on what that changes from Base. Err is why the land can't go on. Blocks.
fn step1(hive: &Hive, paths: &HivePaths, w: &WorkerRecord, land_id: &str, r: &mut LandRepo) -> Result<(), String> {
    let hf = hive_folder(hive, &r.folder)?;
    let repo = PathBuf::from(&hf.path);
    if let Some(t) = r.tmp.take() {
        lg::remove_temp(&repo, Path::new(&t));
    }
    let base = r.target.clone();
    let base_sha = lg::branch_tip(&repo, &base).ok_or_else(|| format!("{}: its landing target, {base}, doesn't exist any more", r.folder))?;
    // The integration mode merges exactly the pinned sha `--no-ff` into its branch's tip (§8.3);
    // land to Base merges Base's sha into the pinned commit (§8.1).
    let integration = hive.is_integration();
    let start = if integration { base_sha.as_str() } else { r.pinned.as_str() };
    let tmp = lg::add_temp(&repo, &paths.land_worktree(land_id, &r.folder), start).map_err(|e| format!("{}: {e}", r.folder))?;
    r.tmp = Some(tmp.path.to_string_lossy().to_string());
    r.tmp_admin = Some(tmp.admin.to_string_lossy().to_string());
    let title = w.title.replace(['\n', '\r'], " ");
    let merged = if integration {
        let message = format!("Land {title} ({}) [maiTerm {land_id}]", &r.pinned[..r.pinned.len().min(7)]);
        lg::merge_pinned(&repo, &tmp, &base_sha, &r.pinned, &message)
    } else {
        lg::merge_base(&repo, &tmp, &base_sha, &r.pinned, &format!("Land {title} onto {base} [maiTerm {land_id}]"))
    };
    match merged.map_err(|e| format!("{}: {e}", r.folder))? {
        Merged::Conflict(files) if integration => {
            return Err(format!(
                "Merging the reviewed commit into {base} conflicts in {}: {}. Tell the worker to merge {base} into its branch, resolve \
                 them and report done again; review what it commits.",
                r.folder,
                files_line(&files)
            ))
        }
        Merged::Conflict(files) => {
            return Err(format!(
                "Merging {base} into the reviewed commit conflicts in {}: {}. Tell the worker to merge {base} into its branch, resolve \
                 them and report done again; review what it commits.",
                r.folder,
                files_line(&files)
            ))
        }
        Merged::Tested { sha, merged } => {
            r.tree_tested = lg::tree_of(&repo, &sha);
            r.tested = Some(sha);
            r.merged = merged;
            r.checked = false;
            r.base_before = Some(base_sha.clone());
            r.status = REPO_PENDING.to_string();
        }
    }
    // What it changes from its target: a catch goes to the human before step 2 (`advance_all`).
    let tested = r.tested.clone().unwrap_or_default();
    let caught = super::config_rule::caught(&repo, &base_sha, &tested, &hive.settings.land.config_paths).map_err(|e| format!("{}: {e}", r.folder))?;
    r.config_fp = Some(caught.fingerprint).filter(|f| !f.is_empty());
    r.config_hits = caught.files;
    Ok(())
}

/// Step 1 for every repo that needs it (or only `only`): pending ones, and tested ones whose
/// Base moved since. The land comes back with what step 1 made even when it stops, so its
/// temporary worktrees are cleaned up.
async fn prepare(caller: &Caller, w: &WorkerRecord, land: &mut LandState, only: Option<&str>) -> Result<(), String> {
    let (hive, paths, rec, mut l) = (caller.hive.clone(), caller.paths.clone(), w.clone(), land.clone());
    let only = only.map(str::to_string);
    let (l, res) = blocking(move || {
        let id = l.id.clone();
        let mut res = Ok(());
        for r in l.repos.iter_mut().filter(|r| only.as_deref().is_none_or(|o| o == r.folder)) {
            if r.status == REPO_LANDED {
                continue;
            }
            if r.status == REPO_TESTED {
                let unmoved = hive_folder(&hive, &r.folder)
                    .map(|hf| r.base_before.is_some() && lg::branch_tip(Path::new(&hf.path), &r.target) == r.base_before)
                    .unwrap_or(false);
                if unmoved {
                    continue;
                }
            }
            res = step1(&hive, &paths, &rec, &id, r);
            if res.is_err() {
                break;
            }
        }
        Ok((l, res))
    })
    .await?;
    *land = l;
    res
}

/// Why a land can't go on, and the folder whose land check failed, if that is why: the post-land
/// check then tells a broken worker from a broken Base (§8.6).
type Stop = (String, Option<String>);

/// The land checks of the repos step 1 just prepared. A check that ran inline and failed is
/// Err; queued ones leave their repo `checking`.
async fn start_checks(caller: &Caller, w: &WorkerRecord, land: &mut LandState, opts: &Opts) -> Result<(), Stop> {
    for r in land.repos.iter_mut().filter(|r| r.status == REPO_PENDING && r.tested.is_some()) {
        let check = caller.hive.folders.iter().find(|f| f.name == r.folder).and_then(|f| f.land_check.clone()).filter(|c| !c.trim().is_empty());
        let Some(check) = check else {
            r.status = REPO_TESTED.to_string();
            continue;
        };
        let req = CheckRequest {
            caller: caller.clone(),
            worker: w.clone(),
            folder: r.folder.clone(),
            land_id: land.id.clone(),
            tmp: Temp { path: PathBuf::from(r.tmp.clone().unwrap_or_default()), admin: PathBuf::from(r.tmp_admin.clone().unwrap_or_default()) },
            tested: r.tested.clone().unwrap_or_default(),
            command: check,
            kind: super::CHECK_LAND,
        };
        let checker = opts.checker.clone();
        let own = w.git_folders().find(|f| f.name == r.folder).cloned();
        let started = blocking(move || {
            // The check's network is closed (§13.1), so the folder's setup (`npm ci`) can't run
            // there: what the worker's setup and work left in its worktree stands in.
            if let Some(f) = own {
                let seeded = lg::seed_ignored(&f, &req.tmp.path, &req.caller.hive.settings.land.config_paths);
                if !seeded.is_empty() {
                    log::info!("hive {}: land {}: seeded {} from worker {}'s worktree: {}", req.caller.hive.id, req.land_id, req.folder, req.worker.slug, seeded.join(", "));
                }
            }
            checker.start(&req)
        });
        match started.await.map_err(|e| (format!("{}: its land check couldn't be queued: {e}", r.folder), None))? {
            CheckStart::Queued(job) => {
                r.status = REPO_CHECKING.to_string();
                r.job = Some(job);
            }
            CheckStart::Done { passed: true, .. } => {
                r.status = REPO_TESTED.to_string();
                r.checked = true;
            }
            CheckStart::Done { passed: false, tail } => return Err((check_failed(&r.folder, &tail), Some(r.folder.clone()))),
        }
    }
    Ok(())
}

/// The land fails, then, if a land check was why, the post-land check runs on its Base alone.
async fn stop<W: HiveWindow>(ctx: &Ctx<'_, W>, caller: &Caller, w: &WorkerRecord, land: LandState, (why, failed): Stop, opts: &Opts) -> Outcome {
    let id = land.id.clone();
    let out = fail(ctx, caller, w, land, why).await;
    if let Some(folder) = failed {
        super::base_check::run(ctx, caller, w, &folder, &format!("land {id}'s check failed there"), true, opts).await;
    }
    out
}

pub(super) fn check_failed(folder: &str, tail: &str) -> String {
    let lines: Vec<&str> = tail.trim_end().lines().collect();
    let tail = lines[lines.len().saturating_sub(TAIL_LINES)..].join("\n");
    let mut out = format!("The land check failed in {folder}, so nothing landed.");
    if !tail.trim().is_empty() {
        out.push_str("\nIts last lines (the check's output, not maiTerm's words):\n");
        out.push_str(&frame_escape(&tail));
    }
    out
}

// ── The run ──

/// Runs the land from wherever it stands: locks, step 1, checks, step 2, finish. `background` is
/// a run that already carries on outside the tool call, so a busy lock doesn't hand off again.
pub(crate) async fn drive<W: HiveWindow>(ctx: &Ctx<'_, W>, caller: &Caller, task_id: &str, opts: &Opts, background: bool) -> Outcome {
    let dir = match worker::task_dir(&caller.paths, task_id) {
        Ok(d) => d,
        Err(e) => return Outcome::Refused(e),
    };
    let d = dir.clone();
    let w = match blocking(move || worker::read_worker(&d)).await {
        Ok(w) => w,
        Err(e) => return Outcome::Refused(e),
    };
    let Some(mut land) = w.land.clone().filter(|l| l.state == STATE_LOCKING) else { return Outcome::Gone };

    match take_locks(caller, &land, opts.lock_wait).await {
        Ok(Ok(())) => {}
        Ok(Err(who)) => {
            land.waiting_for = Some(who.clone());
            warn_save(caller, save(&dir, &land).await);
            ctx.window.workers_changed(&caller.workspace_id);
            if !background {
                ctx.window.continue_land(caller, task_id);
            }
            return Outcome::Waiting(json!({
                "land": land.id,
                "state": STATE_LOCKING,
                "waiting_for": who,
                "result": format!("Worker {}'s land {} waits for the land lock: held by {who}.", worker_label(&w), land.id),
                "note": "Another land holds that repo. maiTerm takes the lock as soon as it's free and carries on; the result arrives as ⟦HIVE LAND⟧.",
            }));
        }
        Err(e) => return fail(ctx, caller, &w, land, e).await,
    }
    land.waiting_for = None;
    if let Err(e) = prepare(caller, &w, &mut land, None).await {
        return fail(ctx, caller, &w, land, e).await;
    }
    // Its temporary worktrees on record, so a restart from here can clean them up.
    warn_save(caller, save(&dir, &land).await);
    proceed(ctx, caller, &w, land, opts).await
}

/// After step 1: the checks, then step 2 once none is still running.
async fn proceed<W: HiveWindow>(ctx: &Ctx<'_, W>, caller: &Caller, w: &WorkerRecord, mut land: LandState, opts: &Opts) -> Outcome {
    let Ok(dir) = worker::task_dir(&caller.paths, &w.task_id) else { return Outcome::Gone };
    if let Err(e) = start_checks(caller, w, &mut land, opts).await {
        return stop(ctx, caller, w, land, e, opts).await;
    }
    let jobs: Vec<String> = land.repos.iter().filter(|r| r.status == REPO_CHECKING).filter_map(|r| r.job.clone()).collect();
    if !jobs.is_empty() {
        land.state = STATE_CHECKING.to_string();
        warn_save(caller, save(&dir, &land).await);
        ctx.window.workers_changed(&caller.workspace_id);
        return Outcome::Checking(json!({
            "land": land.id,
            "state": STATE_CHECKING,
            "jobs": jobs,
            "note": "The land check runs in the queue on the merged commit. The result arrives as ⟦HIVE LAND⟧: landed, held, or refused with the check's output.",
        }));
    }
    advance_all(ctx, caller, w, land, opts).await
}

/// Step 2 in order (§8.1, §8.2), then the finish. A repo whose landed tree isn't the tree its
/// check passed (Base moved between its fast-forward and the read) gets the post-land check
/// once the land's run ends, whatever it ended as (§8.6).
async fn advance_all<W: HiveWindow>(ctx: &Ctx<'_, W>, caller: &Caller, w: &WorkerRecord, land: LandState, opts: &Opts) -> Outcome {
    let id = land.id.clone();
    let mut post: Vec<String> = Vec::new();
    let out = advance_inner(ctx, caller, w, land, opts, &mut post).await;
    for folder in post {
        super::base_check::run(ctx, caller, w, &folder, &format!("land {id} landed a tree other than the one its check passed"), false, opts).await;
    }
    out
}

async fn advance_inner<W: HiveWindow>(ctx: &Ctx<'_, W>, caller: &Caller, w: &WorkerRecord, mut land: LandState, opts: &Opts, post: &mut Vec<String>) -> Outcome {
    let Ok(dir) = worker::task_dir(&caller.paths, &w.task_id) else { return Outcome::Gone };
    land.state = STATE_ADVANCING.to_string();
    for folder in land.ordered() {
        while let Some(r) = land.repo_mut(&folder) {
            if r.status == REPO_LANDED {
                break;
            }
            if r.status != REPO_TESTED {
                let why = format!("{folder} isn't ready to land ({})", r.status);
                return fail(ctx, caller, w, land, why).await;
            }
            // The config rule (§8.1), before anything moves: a change the human hasn't approved
            // as it now stands sends the whole land to one card, whatever the checkpoint says.
            if land.repos.iter().any(LandRepo::config_unapproved) {
                return super::approval::await_human(ctx, caller, w, land, super::approval::WHY_CONFIG).await;
            }
            if let Some(seam) = opts.before_advance.clone() {
                let f = folder.clone();
                let _ = blocking(move || {
                    seam(&f);
                    Ok(())
                })
                .await;
            }
            let r = land.repo_mut(&folder).expect("just found");
            r.status = REPO_ADVANCING.to_string();
            // Persisted before Base moves: a restart reads Base to know whether it did.
            if let Err(e) = save(&dir, &land).await {
                return fail(ctx, caller, w, land, e).await;
            }
            let r = land.repo_mut(&folder).expect("just found").clone();
            let repo = match hive_folder(&caller.hive, &folder) {
                Ok(hf) => PathBuf::from(&hf.path),
                Err(e) => return fail(ctx, caller, w, land, e).await,
            };
            let tested = r.tested.clone().unwrap_or_default();
            let (rp, base, from, to) = (repo.clone(), r.target.clone(), r.base_before.clone().unwrap_or_default(), tested.clone());
            let res = blocking(move || lg::advance(&rp, &base, &from, &to)).await;
            match res {
                Ok(Step2::Advanced) => {
                    let (hive, paths, rec, id, summary, review, request) =
                        (caller.hive.clone(), caller.paths.clone(), w.clone(), land.id.clone(), land.summary.clone(), land.review.clone(), approved_request(&land));
                    let decision = land.decision.clone();
                    let mut done = r.clone();
                    let done = blocking(move || {
                        done.status = REPO_LANDED.to_string();
                        done.held_reason = None;
                        done.base_after = done.tested.clone();
                        done.tree_landed = lg::tree_of(&repo, &format!("refs/heads/{}", done.target));
                        done.pushed = Some(if hive.push_after_land && !hive.is_integration() { lg::push(&repo, &done.target) } else { landlog::PUSHED_OFF.to_string() });
                        log_repo(&paths, &hive, &rec, &id, &summary, &review, request, decision, &mut done);
                        Ok(done)
                    })
                    .await;
                    match done {
                        Ok(done) => {
                            if done.tree_landed != done.tree_tested {
                                post.push(folder.clone());
                            }
                            *land.repo_mut(&folder).expect("just found") = done
                        }
                        Err(e) => return fail(ctx, caller, w, land, e).await,
                    }
                    warn_save(caller, save(&dir, &land).await);
                    log::info!("hive {}: land {} landed {folder} at {}", caller.hive.id, land.id, short(&tested));
                    break;
                }
                Ok(Step2::Held(why)) => return hold(ctx, caller, w, land, &folder, why).await,
                Ok(Step2::BaseMoved) => {
                    let id = land.id.clone();
                    let r = land.repo_mut(&folder).expect("just found");
                    r.attempts += 1;
                    let attempts = r.attempts;
                    log::info!("hive {}: land {id}: {} moved under step 2 ({attempts}/{MAX_ATTEMPTS})", caller.hive.id, r.target);
                    if attempts >= MAX_ATTEMPTS {
                        let base = r.target.clone();
                        return hold(ctx, caller, w, land, &folder, format!("{base} keeps moving ({attempts} tries)")).await;
                    }
                    r.status = REPO_PENDING.to_string();
                    if let Err(e) = prepare(caller, w, &mut land, Some(&folder)).await {
                        return fail(ctx, caller, w, land, e).await;
                    }
                    if let Err(e) = start_checks(caller, w, &mut land, opts).await {
                        return stop(ctx, caller, w, land, e, opts).await;
                    }
                    if land.repo_mut(&folder).is_some_and(|r| r.status == REPO_CHECKING) {
                        land.state = STATE_CHECKING.to_string();
                        warn_save(caller, save(&dir, &land).await);
                        ctx.window.workers_changed(&caller.workspace_id);
                        let jobs: Vec<String> = land.repos.iter().filter_map(|r| r.job.clone().filter(|_| r.status == REPO_CHECKING)).collect();
                        return Outcome::Checking(json!({
                            "land": land.id,
                            "state": STATE_CHECKING,
                            "jobs": jobs,
                            "note": format!("{} moved meanwhile, so maiTerm merged it again and re-runs the land check. The result arrives as ⟦HIVE LAND⟧.", r_target(&land, &folder)),
                        }));
                    }
                }
                Err(e) => return fail(ctx, caller, w, land, e).await,
            }
        }
    }
    finish(ctx, caller, w, land).await
}

fn r_target(land: &LandState, folder: &str) -> String {
    land.repos.iter().find(|r| r.folder == folder).map(|r| r.target.clone()).unwrap_or_default()
}

/// The agent request a land was approved through, for its log line (`by: human`).
pub(crate) fn approved_request(land: &LandState) -> Option<String> {
    land.approved_by.as_ref().and(land.request.clone())
}

/// One repo's line in the land log, unless it has one: an integration-mode land and a batch
/// member name their own merge commit (a member's check ran on its chain's tip), and a land the
/// human approved names the request, and the coordinator's call the decision. Blocks.
#[allow(clippy::too_many_arguments)]
pub(crate) fn log_repo(
    paths: &HivePaths,
    hive: &Hive,
    w: &WorkerRecord,
    land_id: &str,
    summary: &str,
    review: &str,
    request: Option<String>,
    decision: Option<String>,
    r: &mut LandRepo,
) {
    if r.logged {
        return;
    }
    let id = format!("{land_id}/{}", r.folder);
    if landlog::has(paths, &id) {
        r.logged = true;
        return;
    }
    let integration = hive.is_integration();
    let member = r.chain_tip.is_some();
    let kind = if member {
        landlog::KIND_BATCH_MEMBER
    } else if integration {
        landlog::KIND_INTEGRATION
    } else {
        landlog::KIND_LAND
    };
    let entry = LandLogEntry {
        id,
        land: land_id.to_string(),
        kind: kind.to_string(),
        merge_commit: r.tested.clone().filter(|_| member || (integration && r.merged)),
        by: if request.is_some() { landlog::BY_HUMAN } else { landlog::BY_COORDINATOR }.to_string(),
        request,
        decision,
        hive_id: hive.id.clone(),
        task_id: w.task_id.clone(),
        worker: w.slug.clone(),
        title: w.title.clone(),
        folder: r.folder.clone(),
        target: r.target.clone(),
        reviewed_sha: r.pinned.clone(),
        tested_sha: r.chain_tip.clone().or_else(|| r.tested.clone()).unwrap_or_default(),
        base_before: r.base_before.clone().unwrap_or_default(),
        base_after: r.base_after.clone().unwrap_or_default(),
        tree_tested: r.tree_tested.clone().unwrap_or_default(),
        tree_landed: r.tree_landed.clone().unwrap_or_default(),
        summary: summary.to_string(),
        review: if review.is_empty() { landlog::REVIEW_NONE.to_string() } else { review.to_string() },
        pushed: r.pushed.clone().unwrap_or_else(|| landlog::PUSHED_OFF.to_string()),
        check: if r.checked { landlog::CHECK_PASSED.to_string() } else { String::new() },
        at: crate::commands::workspace::iso_now(),
        ..LandLogEntry::default()
    };
    match landlog::append(paths, &entry) {
        Ok(()) => r.logged = true,
        Err(e) => log::warn!("hive {}: land log line {} not written: {e}", hive.id, entry.id),
    }
}

/// The land's result on the decisions about it (§7.6): the coordinator's call, and its card.
pub(super) async fn log_outcome<W: HiveWindow>(ctx: &Ctx<'_, W>, caller: &Caller, land: &LandState, outcome: String) {
    if land.decision.is_none() && land.card_decision.is_none() {
        return;
    }
    let (paths, hive_id, l) = (caller.paths.clone(), caller.hive.id.clone(), land.clone());
    let _ = blocking(move || {
        crate::hive::decisions::land_outcome(&paths, &hive_id, &l, &outcome);
        Ok(())
    })
    .await;
    ctx.window.decisions_changed(&caller.workspace_id);
}

/// A failure before anything landed refuses the land; after, it holds what's left.
pub(super) async fn fail<W: HiveWindow>(ctx: &Ctx<'_, W>, caller: &Caller, w: &WorkerRecord, mut land: LandState, why: String) -> Outcome {
    if land.landed().is_empty() {
        return refuse(ctx, caller, w, land, why).await;
    }
    // What didn't pass step 1 and its check is done again on resume, never reused.
    for r in land.repos.iter_mut().filter(|r| !matches!(r.status.as_str(), REPO_LANDED | REPO_TESTED)) {
        r.tested = None;
        r.base_before = None;
    }
    let failed = land.repos.iter().find(|r| r.status != REPO_LANDED && r.status != REPO_TESTED).or_else(|| land.repos.iter().find(|r| r.status != REPO_LANDED));
    let failed = failed.map(|r| r.folder.clone()).unwrap_or_default();
    hold(ctx, caller, w, land, &failed, why).await
}

/// Nothing landed: the land is off, the worker back in Reported with its branch as it was.
pub(super) async fn refuse<W: HiveWindow>(ctx: &Ctx<'_, W>, caller: &Caller, w: &WorkerRecord, land: LandState, why: String) -> Outcome {
    let label = worker_label(w);
    if let Ok(dir) = worker::task_dir(&caller.paths, &w.task_id) {
        let id = land.id.clone();
        let res = blocking(move || {
            worker::update_worker(&dir, |cur| {
                let ours = cur.phase == worker::PHASE_LANDING && cur.land.as_ref().is_some_and(|l| l.id == id);
                if ours {
                    cur.phase = worker::PHASE_REPORTED.to_string();
                    cur.land = None;
                }
                ours
            })
        })
        .await;
        if let Err(e) = res {
            log::warn!("hive {}: worker {label} not put back after its land was refused: {e}", caller.hive.id);
        }
    }
    let (hive, paths, mut l) = (caller.hive.clone(), caller.paths.clone(), land.clone());
    let _ = blocking(move || {
        cleanup(&hive, &paths, &mut l);
        Ok(())
    })
    .await;
    end_hold(w);
    // Git's words can carry the worker's file names: quoted like any of its text.
    let why = frame_escape(&why);
    log_outcome(ctx, caller, &land, format!("refused: {}", one_line(&why, 200))).await;
    let note = format!("Land refused: {}", one_line(&why, 300));
    if let Err(e) = crate::hive::control::task_note(ctx, caller, &w.task_id, &note, None).await {
        log::info!("hive {}: land refusal note for {label} not written: {e}", caller.hive.id);
    }
    ctx.window.workers_changed(&caller.workspace_id);
    log::info!("hive {}: worker {label}'s land refused: {}", caller.hive.id, one_line(&why, 300));
    Outcome::Refused(format!("Worker {label} didn't land: {why}\n[maiTerm] It is back in Reported; its branch is as it was."))
}

/// A repo held (§8.1): the land stops there, what landed stays landed, its locks and temporary
/// worktrees go, and `hiveLand` with the same commits resumes it.
pub(super) async fn hold<W: HiveWindow>(ctx: &Ctx<'_, W>, caller: &Caller, w: &WorkerRecord, mut land: LandState, folder: &str, why: String) -> Outcome {
    let label = worker_label(w);
    let why = one_line(&why, 600);
    if let Some(r) = land.repo_mut(folder) {
        r.status = REPO_HELD.to_string();
        r.held_reason = Some(why.clone());
    }
    let landed: Vec<String> = land.landed().iter().map(|r| r.folder.clone()).collect();
    let line = if landed.is_empty() { format!("holding {folder}: {why}") } else { format!("Landed {}; holding {folder}: {why}", landed.join(", ")) };
    land.state = STATE_HELD.to_string();
    land.held_reason = Some(line.clone());
    land.waiting_for = None;
    if let Ok(dir) = worker::task_dir(&caller.paths, &w.task_id) {
        warn_save(caller, save(&dir, &land).await);
    }
    let (hive, paths, mut l) = (caller.hive.clone(), caller.paths.clone(), land.clone());
    let l = blocking(move || {
        cleanup(&hive, &paths, &mut l);
        Ok(l)
    })
    .await
    .unwrap_or(land);
    if let Ok(dir) = worker::task_dir(&caller.paths, &w.task_id) {
        warn_save(caller, save(&dir, &l).await);
    }
    end_hold(w);
    log_outcome(ctx, caller, &l, format!("held: {line}")).await;
    if let Err(e) = crate::hive::control::task_note(ctx, caller, &w.task_id, &format!("Landing held: {}", one_line(&line, 300)), None).await {
        log::info!("hive {}: land hold note for {label} not written: {e}", caller.hive.id);
    }
    ctx.window.workers_changed(&caller.workspace_id);
    log::info!("hive {}: land {} of {label} held: {line}", caller.hive.id, l.id);
    Outcome::Held(json!({
        "land": l.id,
        "state": STATE_HELD,
        "landed": landed,
        "holding": folder,
        "reason": why,
        "result": format!("Worker {label}'s land {}: {line}.", l.id),
        "note": "Nothing was stashed or overwritten. Once the reason is gone, hiveLand again with the same commits: it lands only what's left. \
                 If the worker has to change its work instead (a conflict), it reports again and you land its new, reviewed commits; \
                 what landed stays as it landed. hiveRetireWorker abandons the rest.",
    }))
}

/// Every repo landed: the locks and temporary worktrees go, the worker retires as landed (its
/// task done, so its waiters launch from the new Base), its branch moves to what landed, and the
/// coordinator's read copies follow Base.
pub(super) async fn finish<W: HiveWindow>(ctx: &Ctx<'_, W>, caller: &Caller, w: &WorkerRecord, mut land: LandState) -> Outcome {
    let label = worker_label(w);
    land.held_reason = None;
    let (hive, paths, mut l) = (caller.hive.clone(), caller.paths.clone(), land.clone());
    land = blocking(move || {
        cleanup(&hive, &paths, &mut l);
        Ok(l)
    })
    .await
    .unwrap_or(land);
    // Still `advancing`, every repo landed, until the worker has retired: a restart before then
    // holds it (`reconcile`), and the coordinator's hiveLand retires it.
    let dir = worker::task_dir(&caller.paths, &w.task_id).ok();
    if let Some(dir) = &dir {
        warn_save(caller, save(dir, &land).await);
    }
    end_hold(w);

    let shas: Vec<String> = land.repos.iter().map(|r| format!("{} {} → {}", r.folder, short(r.base_after.as_deref().unwrap_or_default()), r.target)).collect();
    // Approved hours ago, perhaps: a worker whose branch moved on since its pin landed only the
    // pin, and keeps its newer commits for a land of their own (plan 6d).
    if land.approved_by.is_some() {
        let (hive, l) = (caller.hive.clone(), land.clone());
        let moved = blocking(move || Ok(branches_moved(&hive, &l))).await.unwrap_or_default();
        if !moved.is_empty() {
            return landed_unretired(ctx, caller, w, land, &shas, moved).await;
        }
    }
    let spec = RetireSpec {
        phase: worker::PHASE_LANDED,
        from: vec![worker::PHASE_LANDING],
        reason: format!("landed {}", land.id),
        task_status: "done",
        note: format!("Landed ({}): {}", land.id, shas.join(", ")),
        landed: land.repos.first().and_then(|r| r.base_after.clone()),
        held_land: false,
    };
    let mut notes: Vec<String> = Vec::new();
    let (removed, retired) = match retire_worker(ctx, caller, w, spec).await {
        Ok(out) => (out.disk.removed.clone(), json!({ "worktrees_removed": out.disk.removed, "kept": out.disk.kept })),
        Err(e) => {
            log::warn!("hive {}: worker {label} landed but wasn't retired: {e}", caller.hive.id);
            // Its phase unmoved, nothing else would ever retire it: the land holds for hiveLand.
            let d = dir.clone();
            let phase = blocking(move || d.ok_or_else(|| "no task folder".to_string()).and_then(|d| worker::read_worker(&d)).map(|r| r.phase)).await;
            if phase.as_deref().is_ok_and(|p| p == worker::PHASE_LANDING) {
                return unretired(ctx, caller, w, land, e).await;
            }
            notes.push(format!("It landed, but maiTerm couldn't finish retiring it: {e}"));
            (Vec::new(), Value::Null)
        }
    };
    land.state = STATE_LANDED.to_string();
    if let Some(dir) = &dir {
        warn_save(caller, save(dir, &land).await);
    }
    let (hive, paths, rec, l) = (caller.hive.clone(), caller.paths.clone(), w.clone(), land.clone());
    let after = blocking(move || Ok(after_land(&hive, &paths, &rec, &l, &removed))).await.unwrap_or_default();
    notes.extend(after);
    let repos: Vec<Value> = land
        .repos
        .iter()
        .map(|r| json!({ "folder": r.folder, "target": r.target, "landed": r.base_after, "merged_base": r.merged, "pushed": r.pushed }))
        .collect();
    let pushes: Vec<String> = land.repos.iter().filter_map(|r| r.pushed.as_ref().filter(|p| p.starts_with("held")).map(|p| format!("{}: push {p}", r.folder))).collect();
    let mut result = format!("Worker {label} landed: {}.", shas.join(", "));
    if !pushes.is_empty() {
        result.push_str(&format!(" {} (the land stands; the next land pushes both).", pushes.join("; ")));
    }
    result.push_str(" It is retired; workers waiting on it launch from the new Base.");
    for n in &notes {
        result.push_str(&format!(" {n}"));
    }
    log::info!("hive {}: land {} of {label} finished", caller.hive.id, land.id);
    log_outcome(ctx, caller, &land, "landed".to_string()).await;
    Outcome::Landed(json!({
        "land": land.id,
        "state": STATE_LANDED,
        "repos": repos,
        "review": land.review,
        "retired": retired,
        "notes": notes,
        "result": result,
    }))
}

/// Every repo landed but the worker's retirement didn't happen: the land holds, so hiveLand with
/// the same commits retires it (nothing lands twice), and it doesn't sit in Landing for good.
async fn unretired<W: HiveWindow>(ctx: &Ctx<'_, W>, caller: &Caller, w: &WorkerRecord, mut land: LandState, e: String) -> Outcome {
    let label = worker_label(w);
    let folders: Vec<String> = land.repos.iter().map(|r| r.folder.clone()).collect();
    let line = format!("Landed {}, but maiTerm couldn't retire the worker ({}): hiveLand again with the same commits to retire it", folders.join(", "), one_line(&e, 200));
    land.state = STATE_HELD.to_string();
    land.held_reason = Some(line.clone());
    if let Ok(dir) = worker::task_dir(&caller.paths, &w.task_id) {
        warn_save(caller, save(&dir, &land).await);
    }
    log_outcome(ctx, caller, &land, format!("held: {line}")).await;
    if let Err(e) = crate::hive::control::task_note(ctx, caller, &w.task_id, &format!("Landing held: {}", one_line(&line, 300)), None).await {
        log::info!("hive {}: land hold note for {label} not written: {e}", caller.hive.id);
    }
    ctx.window.workers_changed(&caller.workspace_id);
    Outcome::Held(json!({
        "land": land.id,
        "state": STATE_HELD,
        "landed": folders,
        "reason": line,
        "result": format!("Worker {label}'s land {}: {line}.", land.id),
    }))
}

/// The repos whose worker branch isn't at its pin any more. Blocks on git.
fn branches_moved(hive: &Hive, land: &LandState) -> Vec<String> {
    land.repos
        .iter()
        .filter(|r| !r.branch.is_empty())
        .filter(|r| hive_folder(hive, &r.folder).is_ok_and(|hf| lg::branch_tip(Path::new(&hf.path), &r.branch).as_deref() != Some(r.pinned.as_str())))
        .map(|r| r.folder.clone())
        .collect()
}

/// An approved land whose worker's branch moved on since its pin: the pin landed, and the worker
/// stays, back in Reported with its branch as it is, the land kept as history.
async fn landed_unretired<W: HiveWindow>(ctx: &Ctx<'_, W>, caller: &Caller, w: &WorkerRecord, mut land: LandState, shas: &[String], moved: Vec<String>) -> Outcome {
    let label = worker_label(w);
    land.state = STATE_LANDED.to_string();
    land.updated_at = crate::commands::workspace::iso_now();
    if let Ok(dir) = worker::task_dir(&caller.paths, &w.task_id) {
        let l = land.clone();
        let res = blocking(move || {
            worker::update_worker(&dir, |cur| {
                let ours = cur.phase == worker::PHASE_LANDING && cur.land.as_ref().is_some_and(|x| x.id == l.id);
                if ours {
                    cur.phase = worker::PHASE_REPORTED.to_string();
                    cur.land = Some(l);
                }
                ours
            })
        })
        .await;
        if let Err(e) = res {
            log::warn!("hive {}: worker {label} landed but wasn't put back in Reported: {e}", caller.hive.id);
        }
    }
    let (hive, paths, l) = (caller.hive.clone(), caller.paths.clone(), land.clone());
    let notes = blocking(move || Ok(move_read_copies(&hive, &paths, &l))).await.unwrap_or_default();
    let line = format!(
        "Landed ({}): {}. Its branch moved since the human approved it, in {}: those newer commits need a land of their own.",
        land.id,
        shas.join(", "),
        moved.join(", ")
    );
    if let Err(e) = crate::hive::control::task_note(ctx, caller, &w.task_id, &line, None).await {
        log::info!("hive {}: land note for {label} not written: {e}", caller.hive.id);
    }
    ctx.window.workers_changed(&caller.workspace_id);
    log::info!("hive {}: land {} of {label} landed its pin; the branch moved since, so the worker stays", caller.hive.id, land.id);
    log_outcome(ctx, caller, &land, format!("landed the approved commits; its branch moved on since, in {}", moved.join(", "))).await;
    let mut result = format!(
        "Worker {label} landed the commits the human approved: {}. Its branch has moved since you named them, in {}: those newer \
         commits didn't land and need a land of their own (review them, then hiveLand). It is back in Reported, not retired.",
        shas.join(", "),
        moved.join(", ")
    );
    for n in &notes {
        result.push_str(&format!(" {n}"));
    }
    Outcome::Landed(json!({
        "land": land.id,
        "state": STATE_LANDED,
        "repos": land.repos.iter().map(|r| json!({ "folder": r.folder, "target": r.target, "landed": r.base_after, "pushed": r.pushed })).collect::<Vec<_>>(),
        "review": land.review,
        "retired": Value::Null,
        "branch_moved": moved,
        "notes": notes,
        "result": result,
    }))
}

/// The coordinator's read copies moved to Base's new tip unless one has changes (§8.1). In the
/// integration mode a read copy holds `hive/<slug>` itself, so step 2's fast-forward there
/// already moved it, and a detached checkout would take it off its branch. Blocks.
fn move_read_copies(hive: &Hive, paths: &HivePaths, land: &LandState) -> Vec<String> {
    let mut notes = Vec::new();
    if hive.is_integration() {
        return notes;
    }
    for r in &land.repos {
        let (Ok(hf), Some(to)) = (hive_folder(hive, &r.folder), r.base_after.as_deref()) else { continue };
        if let Err(e) = lg::move_read_copy(Path::new(&hf.path), &paths.coordinator_worktree(&r.folder), to) {
            log::info!("hive {}: read copy of {} not moved: {e}", hive.id, r.folder);
            notes.push(format!("Your read copy of {} stays where it was ({e}).", r.folder));
        }
    }
    notes
}

/// After a land: the worker's branch moved to exactly what landed (compare-and-swap from the pin
/// when its worktree went, a fast-forward in a kept one), and the coordinator's read copy moved to
/// Base's new tip unless it has changes. What didn't happen, in words. Blocks.
fn after_land(hive: &Hive, paths: &HivePaths, w: &WorkerRecord, land: &LandState, removed: &[String]) -> Vec<String> {
    let mut notes = Vec::new();
    for r in &land.repos {
        let Ok(hf) = hive_folder(hive, &r.folder) else { continue };
        let repo = PathBuf::from(&hf.path);
        // A batch member's branch stays at its pin: its merge commit carries other members' work.
        let (Some(tested), false, None) = (r.tested.as_deref(), r.branch.is_empty(), r.chain_tip.as_ref()) else { continue };
        let kept = w.git_folders().find(|f| f.name == r.folder).filter(|f| f.worktree.as_deref().is_some_and(|wt| Path::new(wt).exists()));
        let moved = match kept.filter(|_| !removed.contains(&r.folder)) {
            Some(f) if f.admin_dir.as_deref().is_some_and(|a| crate::hive::retire::git_file_names(Path::new(f.worktree.as_deref().unwrap_or_default()), Path::new(a))) => {
                lg::ff_worktree(f, tested).is_ok()
            }
            Some(_) => false,
            None => lg::cas_branch(&repo, &r.branch, tested, &r.pinned),
        };
        if !moved && lg::branch_tip(&repo, &r.branch).as_deref() != Some(tested) {
            notes.push(format!("{}'s branch {} wasn't moved to what landed: it has moved past the commit you reviewed.", r.folder, r.branch));
        }
    }
    notes.extend(move_read_copies(hive, paths, land));
    notes
}

pub(super) fn end_hold(w: &WorkerRecord) {
    if let Some(t) = &w.tab_id {
        crate::hive::deliver::end_land_hold(t);
    }
}

/// A worker with no git folders has nothing to land (§8): it retires as landed, its task done.
pub(crate) async fn land_without_repos<W: HiveWindow>(ctx: &Ctx<'_, W>, caller: &Caller, w: &WorkerRecord, a: &LandArgs, precedent: Option<String>) -> Result<Value, String> {
    // Still the coordinator's call, logged as one (§7.6), with nothing to merge.
    let land = LandState { id: super::new_land_id(), decision: Some(crate::hive::decisions::new_id()), precedent, ..LandState::default() };
    let spec = RetireSpec {
        phase: worker::PHASE_LANDED,
        from: vec![worker::PHASE_REPORTED],
        reason: "landed: no repos".to_string(),
        task_status: "done",
        note: format!("Landed (no repos): {}", one_line(&a.summary, 300)),
        landed: None,
        held_land: false,
    };
    let out = retire_worker(ctx, caller, w, spec).await?;
    let (paths, hive_id, rec) = (caller.paths.clone(), caller.hive.id.clone(), w.clone());
    let _ = blocking(move || {
        crate::hive::decisions::land_decided(&paths, &hive_id, &rec, &land);
        crate::hive::decisions::land_outcome(&paths, &hive_id, &land, "landed: no repos, nothing merged");
        crate::hive::precedents::count_citation(&paths, land.precedent.as_deref());
        Ok(())
    })
    .await;
    ctx.window.decisions_changed(&caller.workspace_id);
    Ok(json!({
        "worker": w.slug,
        "state": STATE_LANDED,
        "repos": [],
        "kept": out.disk.kept,
        "result": format!("Worker {} had no repos, so nothing merged: it is retired, its task done.", worker_label(w)),
    }))
}

// ── Carried on outside the tool call ──

/// A land check ended (`run::job_ended` routes every land job here): its repo is tested or the
/// land fails, and the last check in carries the land on to step 2. The result reaches the
/// coordinator as `⟦HIVE LAND⟧`.
pub async fn check_ended(app: &Arc<crate::state::AppState>, handle: &tauri::AppHandle, job: &crate::hive::queue::Job, tail: &str) {
    let Some(run) = job.land.clone() else { return };
    let Some(caller) = crate::hive::tools::hive_callers(&app.app_data.read()).into_iter().find(|c| c.hive.id == job.hive_id) else { return };
    let ctx = crate::hive::tools::app_ctx(app, handle, &caller);
    match run.kind.as_str() {
        super::CHECK_UNDO => {
            if super::undo::on_check_end(&ctx, &caller, job, &run, tail, &Opts::production()).await.is_some() {
                crate::hive::waiters::poll_soon(app.clone(), handle.clone(), caller.workspace_id.clone());
            }
            return;
        }
        super::CHECK_BASE => return super::base_check::on_check_end(&ctx, &caller, job, &run, tail).await,
        // The batch's run awaits it (`batch::await_check`).
        super::CHECK_BATCH => return super::batch::on_check_end(job, tail),
        _ => {}
    }
    let outcome = on_check_end(&ctx, &caller, job, &run, tail, &Opts::production()).await;
    if let Some((w, o)) = outcome {
        tell(&ctx, &caller, &w, &o).await;
        crate::hive::waiters::poll_soon(app.clone(), handle.clone(), caller.workspace_id.clone());
    }
}

/// `check_ended` with the window and options given. None when the land carries on elsewhere (a
/// check still runs) or isn't this one's any more.
pub(crate) async fn on_check_end<W: HiveWindow>(
    ctx: &Ctx<'_, W>,
    caller: &Caller,
    job: &crate::hive::queue::Job,
    run: &crate::hive::queue::LandRun,
    tail: &str,
    opts: &Opts,
) -> Option<(WorkerRecord, Outcome)> {
    let passed = job.state == crate::hive::queue::DONE && job.began && job.exit_code == Some(0) && !job.dirty;
    let dir = worker::task_dir(&caller.paths, &job.task_id).ok()?;
    let (d, job_id, land_id) = (dir.clone(), job.id.clone(), run.land_id.clone());
    let marked = blocking(move || {
        let mut out: Option<(String, bool)> = None;
        let w = worker::update_worker(&d, |cur| {
            let Some(l) = cur.land.as_mut().filter(|l| l.id == land_id && l.state == STATE_CHECKING) else { return false };
            let Some(r) = l.repos.iter_mut().find(|r| r.status == REPO_CHECKING && r.job.as_deref() == Some(job_id.as_str())) else { return false };
            r.status = if passed { REPO_TESTED } else { REPO_PENDING }.to_string();
            r.checked = passed;
            out = Some((r.folder.clone(), l.repos.iter().all(|r| r.status != REPO_CHECKING)));
            true
        })?;
        Ok(out.map(|o| (w, o)))
    })
    .await
    .ok()
    .flatten();
    let (w, (folder, all_in)) = marked?;
    let land = w.land.clone()?;
    log::info!("hive {}: land {}: check {} in {folder} {}", caller.hive.id, land.id, job.id, if passed { "passed" } else { "failed" });
    if !passed {
        let why = match job.state.as_str() {
            crate::hive::queue::DONE if job.dirty => format!(
                "The land check in {folder} moved the tested commit ({}), so it tested nothing that would land.",
                job.error.as_deref().map(|e| one_line(e, 200)).unwrap_or_default()
            ),
            crate::hive::queue::DONE => return Some((w.clone(), stop(ctx, caller, &w, land, (check_failed(&folder, tail), Some(folder)), opts).await)),
            _ => format!(
                "The land check in {folder} didn't finish ({}), so nothing landed.",
                job.error.as_deref().map(|e| one_line(e, 200)).unwrap_or_else(|| job.state.clone())
            ),
        };
        return Some((w.clone(), fail(ctx, caller, &w, land, why).await));
    }
    if !all_in {
        return None;
    }
    Some((w.clone(), advance_all(ctx, caller, &w, land, opts).await))
}

pub(super) async fn tell<W: HiveWindow>(ctx: &Ctx<'_, W>, caller: &Caller, w: &WorkerRecord, o: &Outcome) {
    let Some(text) = o.message(w) else { return };
    tell_text(ctx, caller, w, text).await;
}

/// A `⟦HIVE LAND⟧` typed into the coordinator through the delivery gate.
pub(super) async fn tell_text<W: HiveWindow>(ctx: &Ctx<'_, W>, caller: &Caller, w: &WorkerRecord, text: String) {
    match coordinator_tab(ctx.data, caller) {
        Some(c) => {
            let sent = ctx.window.deliver(&c, text.clone(), "Hive land").await;
            log::info!("hive {}: land result for {}: {}", caller.hive.id, worker_label(w), sent.describe());
            crate::hive::owed::owe_if_refused(caller, &sent, "Hive land", text).await;
        }
        None => log::info!("hive {}: land result for {} not delivered: no coordinator tab", caller.hive.id, worker_label(w)),
    }
}

/// The land run to its end, a busy lock waited out `opts.lock_wait` at a time rather than handed
/// off: a batch's lone member, or one that left its batch, lands so (`batch`).
pub(crate) async fn drive_to_end<W: HiveWindow>(ctx: &Ctx<'_, W>, caller: &Caller, task_id: &str, opts: &Opts) -> Outcome {
    loop {
        match drive(ctx, caller, task_id, opts, true).await {
            Outcome::Waiting(_) => continue,
            o => return o,
        }
    }
}

/// A land whose lock was busy past `hiveLand`'s inline wait, carried on here: it waits on the
/// lock (telling the coordinator once, after an hour, who holds it), then runs to its result.
pub(crate) fn resume_background(app: Arc<crate::state::AppState>, handle: tauri::AppHandle, caller: Caller, task_id: String) {
    tauri::async_runtime::spawn(async move {
        let ctx = crate::hive::tools::app_ctx(&app, &handle, &caller);
        let mut opts = Opts::production();
        opts.lock_wait = LOCK_NOTICE_AFTER;
        let mut told = false;
        loop {
            let out = drive(&ctx, &caller, &task_id, &opts, true).await;
            let w = worker::task_dir(&caller.paths, &task_id).ok().and_then(|d| worker::read_worker(&d).ok());
            match (&out, w) {
                (Outcome::Waiting(_), Some(w)) => {
                    if !told {
                        told = true;
                        tell(&ctx, &caller, &w, &out).await;
                    }
                    continue;
                }
                (Outcome::Gone, _) | (_, None) => return,
                (_, Some(w)) => {
                    tell(&ctx, &caller, &w, &out).await;
                    crate::hive::waiters::poll_soon(app.clone(), handle.clone(), caller.workspace_id.clone());
                    return;
                }
            }
        }
    });
}
