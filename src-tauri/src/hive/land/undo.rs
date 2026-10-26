//! Undoing a land (docs/hive.md §8.6): a revert commit on the target, landed through the same
//! guarded steps as a land (the repo's lock, a temporary worktree, the config rule, the land
//! check, a fast-forward or compare-and-swap, the dirty hold, Base-moved retries). History is
//! never reset or rewritten: the land stays in the log and in git, with a commit on top.
//!
//! - A fast-forward land is reverted as one commit holding the inverse of
//!   `base_before..base_after` applied three-way onto the target's tip (`git merge-tree`, git
//!   2.40+). A conflict means later work builds on it: refused, fix forward.
//! - A batch member's or an integration land's own merge commit: `git revert -m 1`.
//!
//! The coordinator's undo whose revert changes agent or execution config files a `hive_undo` card
//! the human approves; the human's own Undo (the land log, the worker column's menu) is a direct
//! command whose inline confirmation already named those files (plan D15). In the integration
//! mode only the human undoes (§8.3). Undo never appears on a card of the board (§4.3). The worker
//! isn't reopened: it is marked `undone`, so its waiters block, and the coordinator is told.
//!
//! An undo's state is `logs/undos/<U-…>.json` while it runs, waits or holds, persisted before
//! Base moves, so a restart settles it from the repo (`reconcile_in`).

use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::Instant;

use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};

use super::driver::{self, CheckRequest, CheckStart, Opts, Outcome};
use super::git::{self as lg, Inverse, Reverted, Step2, Temp};
use super::lock::{self, LockInfo, Taken};
use super::log::{self as landlog, FoldedEntry, LandLogEntry};
use super::{config_rule, MAX_ATTEMPTS, MAX_SUMMARY, STATE_ADVANCING, STATE_AWAITING_HUMAN, STATE_CHECKING, STATE_HELD, STATE_LOCKING};
use crate::hive::deliver::{frame_escape, one_line};
use crate::hive::paths::HivePaths;
use crate::hive::record::Hive;
use crate::hive::rpc::{HiveRequest, HiveWindow};
use crate::hive::tools::{blocking, coordinator_tab, Caller, Ctx};
use crate::hive::worker::{self, WorkerRecord};

pub const KIND: &str = "hive_undo";
/// Undo ids: `U-` and 8 hex digits, beside lands' `L-` under `wt.noindex/land/`.
pub const ID_PREFIX: &str = "U-";
pub const STATE_DONE: &str = "done";
pub const STATE_REFUSED: &str = "refused";
/// How many of a land's commits the human's confirmation lists.
const PREVIEW_COMMITS: usize = 20;

/// An undo in `logs/undos/`. Every kind is a String (§3.2).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct UndoState {
    pub id: String,
    /// The land log line it reverts, `L-…/<folder>`.
    pub undoes: String,
    pub land: String,
    pub folder: String,
    pub target: String,
    pub task_id: String,
    pub worker: String,
    pub title: String,
    /// `coordinator` | `human`: who asked.
    pub by: String,
    pub reason: String,
    /// `locking` | `checking` | `advancing` | `held` | `awaiting_human` | `done` | `refused`.
    pub state: String,
    /// The target's tip the revert was made on, and the revert commit and its tree.
    pub tip: Option<String>,
    pub revert: Option<String>,
    pub tree: Option<String>,
    /// Its land check passed on `revert`.
    pub checked: bool,
    /// Times the target moved under its step 2.
    pub attempts: u32,
    pub job: Option<String>,
    pub tmp: Option<String>,
    pub tmp_admin: Option<String>,
    /// The config rule's catch from `tip` to `revert`, and its fingerprint (`config_rule`).
    pub config_hits: Vec<String>,
    pub config_fp: Option<String>,
    /// The fingerprint the human approved on a card, or saw in their confirmation.
    pub config_approved: Option<String>,
    /// The human's own Undo: the fingerprint their confirmation showed (None: no config files).
    pub shown_fp: Option<String>,
    pub request: Option<String>,
    pub request_at: u64,
    /// `desktop` | `phone`: who approved its card.
    pub approved_by: Option<String>,
    pub held_reason: Option<String>,
    pub base_after: Option<String>,
    pub tree_landed: Option<String>,
    pub pushed: Option<String>,
    pub logged: bool,
    pub started_at: String,
    pub updated_at: String,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

impl UndoState {
    pub fn in_progress(&self) -> bool {
        matches!(self.state.as_str(), STATE_LOCKING | STATE_CHECKING | STATE_ADVANCING)
    }

    /// Not over: running, waiting on the human, or held for a retry.
    pub fn open(&self) -> bool {
        self.in_progress() || matches!(self.state.as_str(), STATE_AWAITING_HUMAN | STATE_HELD)
    }

    fn config_unapproved(&self) -> bool {
        !self.config_hits.is_empty() && (self.config_fp.is_none() || self.config_approved.as_deref() != self.config_fp.as_deref())
    }

    fn by_human(&self) -> bool {
        self.by == landlog::BY_HUMAN
    }
}

pub fn new_id() -> String {
    format!("{ID_PREFIX}{}", &uuid::Uuid::new_v4().simple().to_string()[..8])
}

fn is_id(s: &str) -> bool {
    s.strip_prefix(ID_PREFIX).is_some_and(|h| h.len() == 8 && h.bytes().all(|b| b.is_ascii_hexdigit()))
}

fn now_ms() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_millis() as u64)
}

// ── On disk ──

static WRITE: Mutex<()> = Mutex::new(());
/// One undo starts at a time in this process, so two can't both pass "none in flight".
static START: Mutex<()> = Mutex::new(());

fn file(paths: &HivePaths, id: &str) -> Result<PathBuf, String> {
    if !is_id(id) {
        return Err(format!("{} isn't an undo id", one_line(id, 40)));
    }
    Ok(paths.undos().join(format!("{id}.json")))
}

pub fn read_one(paths: &HivePaths, id: &str) -> Result<UndoState, String> {
    let p = file(paths, id)?;
    let text = std::fs::read_to_string(&p).map_err(|e| format!("{}: {e}", p.display()))?;
    serde_json::from_str(&text).map_err(|e| format!("{}: {e}", p.display()))
}

/// Every undo on record, oldest first.
pub fn read_all(paths: &HivePaths) -> Vec<UndoState> {
    let Ok(dir) = std::fs::read_dir(paths.undos()) else { return Vec::new() };
    let mut out: Vec<UndoState> = dir
        .flatten()
        .filter_map(|e| e.file_name().to_str().and_then(|n| n.strip_suffix(".json")).map(str::to_string))
        .filter(|id| is_id(id))
        .filter_map(|id| read_one(paths, &id).ok())
        .collect();
    out.sort_by(|a, b| a.started_at.cmp(&b.started_at).then(a.id.cmp(&b.id)));
    out
}

fn write(paths: &HivePaths, u: &mut UndoState) -> Result<(), String> {
    let _held = WRITE.lock().unwrap_or_else(|e| e.into_inner());
    u.updated_at = crate::commands::workspace::iso_now();
    let v = serde_json::to_value(&*u).map_err(|e| e.to_string())?;
    crate::hive::launch::write_json(&file(paths, &u.id)?, &v)
}

async fn save(paths: &HivePaths, u: &mut UndoState) -> Result<(), String> {
    let (p, mut x) = (paths.clone(), u.clone());
    let x = blocking(move || write(&p, &mut x).map(|_| x)).await?;
    *u = x;
    Ok(())
}

/// An undo that runs, waits on the human or holds a lock now: a delete waits for it.
pub fn in_flight(paths: &HivePaths) -> Vec<UndoState> {
    read_all(paths).into_iter().filter(|u| u.in_progress()).collect()
}

// ── What can be undone ──

/// The line `name` (`L-…/<folder>`, or `L-…` for a land with one repo) names, if it can be
/// undone: a land, batch member or integration land not undone already.
pub(crate) fn resolve(folded: &landlog::Folded, name: &str) -> Result<LandLogEntry, String> {
    let name = name.trim();
    let shown = one_line(name, 60);
    let exact: Vec<&FoldedEntry> = folded.entries.iter().filter(|e| e.entry.id == name).collect();
    let hits: Vec<&FoldedEntry> = if exact.is_empty() {
        folded.entries.iter().filter(|e| e.entry.land == name && e.entry.moved_target()).collect()
    } else {
        exact
    };
    let e = match hits.as_slice() {
        [] => return Err(format!("No land {shown} in this Hive's land log.")),
        [one] => *one,
        many => {
            let ids: Vec<&str> = many.iter().map(|e| e.entry.id.as_str()).collect();
            return Err(format!("Land {shown} landed in several repos: undo each by its own id ({}).", ids.join(", ")));
        }
    };
    match e.entry.kind.as_str() {
        landlog::KIND_UNDO => return Err(format!("{} is an undo itself: to bring that work back, land it again.", e.entry.id)),
        landlog::KIND_BASE_CHECK => return Err(format!("{} is a post-land check, not a land.", e.entry.id)),
        _ if !e.entry.moved_target() => return Err(format!("{} isn't a land maiTerm can undo.", e.entry.id)),
        _ => {}
    }
    if let Some(u) = &e.undone_by {
        return Err(format!("{} is already undone ({u}).", e.entry.id));
    }
    Ok(e.entry.clone())
}

/// A new undo of `e`, or a held one of it resumed; refused while another of it is open.
fn start_state(paths: &HivePaths, e: &LandLogEntry, by: &str, reason: &str, shown_fp: Option<String>) -> Result<UndoState, String> {
    let now = crate::commands::workspace::iso_now();
    let prior = read_all(paths).into_iter().rev().find(|u| u.undoes == e.id && u.open());
    let mut u = match prior {
        Some(u) if u.state == STATE_HELD => u,
        Some(u) if u.state == STATE_AWAITING_HUMAN => {
            return Err(format!("The undo of {} ({}) waits for the human's approval on its card.", e.id, u.id));
        }
        Some(u) => return Err(format!("{} is being undone now ({}, {}).", e.id, u.id, u.state)),
        None => UndoState {
            id: new_id(),
            undoes: e.id.clone(),
            land: e.land.clone(),
            folder: e.folder.clone(),
            target: e.target.clone(),
            task_id: e.task_id.clone(),
            worker: e.worker.clone(),
            title: e.title.clone(),
            started_at: now,
            ..UndoState::default()
        },
    };
    u.by = by.to_string();
    u.reason = reason.to_string();
    u.state = STATE_LOCKING.to_string();
    u.attempts = 0;
    u.held_reason = None;
    u.shown_fp = shown_fp;
    // A resumed undo builds its revert again: the target may have moved while it held.
    u.revert = None;
    u.checked = false;
    Ok(u)
}

// ── Building the revert ──

/// The revert commit of `e` on the target's tip, its config catch, and (with `needs_tmp`, or for
/// a merge revert) a temporary worktree at it for the land check. Err is why it can't be undone.
/// Blocks on git.
fn build(hive: &Hive, paths: &HivePaths, e: &LandLogEntry, u: &mut UndoState, needs_tmp: bool) -> Result<(), String> {
    let repo = PathBuf::from(&driver::hive_folder(hive, &e.folder)?.path);
    if let Some(t) = u.tmp.take() {
        lg::remove_temp(&repo, Path::new(&t));
    }
    u.tmp_admin = None;
    let tip = lg::branch_tip(&repo, &e.target).ok_or_else(|| format!("{}: {} doesn't exist any more", e.folder, e.target))?;
    let title = e.title.replace(['\n', '\r'], " ");
    let message = format!("Revert land {} ({}) [maiTerm {}]", e.id, title.trim(), u.id);
    let later = |files: &[String]| {
        format!(
            "reverting {} conflicts with later work on {} in {}: {}. Later work builds on it, so a revert can't take it back: fix \
             forward instead (a worker that repairs or removes it).",
            e.id,
            e.target,
            e.folder,
            driver::files_line(files)
        )
    };
    let gone = || format!("its change is already gone from {} in {}: there is nothing to revert", e.target, e.folder);
    let revert = match e.merge_commit.as_deref().filter(|m| !m.is_empty()) {
        Some(merge) => {
            let tmp = lg::add_temp(&repo, &paths.land_worktree(&u.id, &e.folder), &tip)?;
            u.tmp = Some(tmp.path.to_string_lossy().to_string());
            u.tmp_admin = Some(tmp.admin.to_string_lossy().to_string());
            match lg::revert_merge(&repo, &tmp, merge, &message)? {
                Reverted::Commit(sha) => sha,
                Reverted::Conflict(files) => return Err(later(&files)),
                Reverted::Empty => return Err(gone()),
            }
        }
        None => {
            if e.base_before.is_empty() || e.base_after.is_empty() || e.base_before == e.base_after {
                return Err(format!("{} moved nothing on {}: there is nothing to revert", e.id, e.target));
            }
            lg::merge_tree_supported()?;
            match lg::inverse_tree(&repo, &e.base_after, &tip, &e.base_before)? {
                Inverse::Conflict(files) => return Err(later(&files)),
                Inverse::Tree(tree) if lg::tree_of(&repo, &tip).as_deref() == Some(tree.as_str()) => return Err(gone()),
                Inverse::Tree(tree) => lg::commit_tree(&repo, &tree, &tip, &message)?,
            }
        }
    };
    u.tree = lg::tree_of(&repo, &revert);
    let caught = config_rule::caught(&repo, &tip, &revert, &hive.settings.land.config_paths)?;
    u.config_fp = Some(caught.fingerprint).filter(|f| !f.is_empty());
    u.config_hits = caught.files;
    if needs_tmp && u.tmp.is_none() {
        let tmp = lg::add_temp(&repo, &paths.land_worktree(&u.id, &e.folder), &revert)?;
        u.tmp = Some(tmp.path.to_string_lossy().to_string());
        u.tmp_admin = Some(tmp.admin.to_string_lossy().to_string());
    }
    u.tip = Some(tip);
    u.revert = Some(revert);
    u.checked = false;
    Ok(())
}

/// Its temporary worktree and lock gone. Blocks.
fn cleanup(hive: &Hive, paths: &HivePaths, u: &mut UndoState) {
    if let Some(job) = u.job.take().filter(|_| u.state == STATE_CHECKING) {
        crate::hive::queue::interrupt(&job, crate::hive::queue::BY_RELEASE, "its undo ended");
    }
    let Ok(hf) = driver::hive_folder(hive, &u.folder) else { return };
    let repo = PathBuf::from(&hf.path);
    if let Some(t) = u.tmp.take() {
        lg::remove_temp(&repo, Path::new(&t));
    }
    u.tmp_admin = None;
    if let Ok(c) = lg::common_dir(&repo) {
        lock::release(&c, &u.id);
    }
    let _ = crate::hive::retire::remove_path(&paths.land_tmp(&u.id));
    let _ = std::fs::remove_dir(paths.land_dir(&u.id));
}

fn has_check(hive: &Hive, folder: &str) -> Option<String> {
    hive.folders.iter().find(|f| f.name == folder).and_then(|f| f.land_check.clone()).filter(|c| !c.trim().is_empty())
}

/// The land's worker as its record has it, or as the log line names it once the record is gone.
fn worker_of(paths: &HivePaths, u: &UndoState) -> WorkerRecord {
    worker::task_dir(paths, &u.task_id).ok().and_then(|d| worker::read_worker(&d).ok()).unwrap_or_else(|| WorkerRecord {
        task_id: u.task_id.clone(),
        slug: u.worker.clone(),
        title: u.title.clone(),
        ..WorkerRecord::default()
    })
}

fn entry_of(paths: &HivePaths, u: &UndoState) -> Result<LandLogEntry, String> {
    landlog::read(paths).into_iter().find(|e| e.id == u.undoes).ok_or_else(|| format!("{} is no longer in the land log", u.undoes))
}

// ── The run ──

/// Starts an undo of the line `name` names (or resumes a held one): `by` the coordinator or the
/// human, `shown_fp` the config fingerprint the human's confirmation showed. Answers the undo as
/// it started (None when refused before it did) and what it came to.
pub(crate) async fn start<W: HiveWindow>(
    ctx: &Ctx<'_, W>,
    caller: &Caller,
    name: &str,
    by: &str,
    reason: &str,
    shown_fp: Option<String>,
    opts: &Opts,
) -> (Option<UndoState>, Outcome) {
    let (paths, n, b, r) = (caller.paths.clone(), name.to_string(), by.to_string(), reason.to_string());
    let started = blocking(move || {
        let _one = START.lock().unwrap_or_else(|e| e.into_inner());
        let e = resolve(&landlog::folded(&paths), &n)?;
        let mut u = start_state(&paths, &e, &b, &r, shown_fp)?;
        write(&paths, &mut u)?;
        Ok(u)
    })
    .await;
    let u = match started {
        Ok(u) => u,
        Err(e) => return (None, Outcome::Refused(e)),
    };
    // Read after the record is on disk: a hiveFinish racing this one either finds the record in
    // its late check and takes Done back, or saved Done before this read. The human's own undo
    // stays theirs in a Done Hive.
    if by == landlog::BY_COORDINATOR && crate::hive::autonomy::current_hive(ctx, caller).is_some_and(|h| h.status == crate::hive::record::STATUS_DONE) {
        let out = refuse(ctx, caller, u.clone(), "This Hive is done: its coordinator undoes nothing. A message from your human wakes it.".to_string()).await;
        return (Some(u), out);
    }
    log::info!("hive {}: undo {} of {} started by the {by}", caller.hive.id, u.id, u.undoes);
    ctx.window.workers_changed(&caller.workspace_id);
    let out = drive(ctx, caller, &u.id, opts).await;
    (Some(u), out)
}

/// Runs the undo from where it stands: the lock, the revert (reused while the target is where it
/// was made and its check passed), the config rule, the check, step 2, the finish.
pub(crate) async fn drive<W: HiveWindow>(ctx: &Ctx<'_, W>, caller: &Caller, id: &str, opts: &Opts) -> Outcome {
    let (paths, i) = (caller.paths.clone(), id.to_string());
    let read = blocking(move || {
        let u = read_one(&paths, &i)?;
        let e = entry_of(&paths, &u);
        Ok((u, e))
    })
    .await;
    let (mut u, entry) = match read {
        Ok((u, _)) if u.state != STATE_LOCKING => {
            log::info!("hive {}: undo {} isn't to run now ({})", caller.hive.id, u.id, u.state);
            return Outcome::Gone;
        }
        Ok((u, Ok(e))) => (u, e),
        Ok((u, Err(e))) => return refuse(ctx, caller, u, e).await,
        Err(e) => return Outcome::Refused(e),
    };
    let repo = match driver::hive_folder(&caller.hive, &u.folder) {
        Ok(hf) => PathBuf::from(&hf.path),
        Err(e) => return refuse(ctx, caller, u, e).await,
    };

    // The repo's land lock, as a land takes it: an undo is a land of a revert.
    let (rp, info, wait) = (repo.clone(), LockInfo::ours(&caller.hive.id, &u.id), opts.lock_wait);
    let taken = blocking(move || {
        let c = lg::common_dir(&rp)?;
        lock::take(&c, &info, Instant::now() + wait, |_| {})
    })
    .await;
    match taken {
        Ok(Taken::Taken) => {}
        Ok(Taken::Busy(who)) => {
            let who = who.map_or_else(|| "another land".to_string(), |i| i.describe());
            let why = format!("{who} holds {}'s land lock: undo again once it's done", u.folder);
            return hold(ctx, caller, u, why).await;
        }
        Err(e) => return refuse(ctx, caller, u, e).await,
    }

    let check = has_check(&caller.hive, &u.folder);
    loop {
        let (rp, target) = (repo.clone(), u.target.clone());
        let tip_now = blocking(move || Ok(lg::branch_tip(&rp, &target))).await.unwrap_or(None);
        let reusable = u.revert.is_some() && (u.checked || check.is_none()) && tip_now.is_some() && tip_now == u.tip;
        if !reusable {
            let (hive, paths, e, mut x, needs) = (caller.hive.clone(), caller.paths.clone(), entry.clone(), u.clone(), check.is_some());
            let built = blocking(move || {
                let r = build(&hive, &paths, &e, &mut x, needs);
                Ok((x, r))
            })
            .await;
            match built {
                Ok((x, Ok(()))) => u = x,
                Ok((x, Err(e))) => return refuse(ctx, caller, x, e).await,
                Err(e) => return refuse(ctx, caller, u, e).await,
            }
            // The config rule (§8.1): the human's own Undo goes on if its confirmation named
            // exactly these files; the coordinator's goes to a card.
            if u.config_unapproved() {
                if u.by_human() {
                    if u.shown_fp.is_some() && u.shown_fp == u.config_fp {
                        u.config_approved = u.config_fp.clone();
                    } else {
                        let why = format!(
                            "its revert now changes agent or execution config ({}), which your confirmation didn't show: look at it again",
                            driver::files_line(&u.config_hits)
                        );
                        return refuse(ctx, caller, u, why).await;
                    }
                } else {
                    return await_human(ctx, caller, u, &entry).await;
                }
            }
            if let Some(command) = check.clone() {
                let req = CheckRequest {
                    caller: caller.clone(),
                    worker: worker_of(&caller.paths, &u),
                    folder: u.folder.clone(),
                    land_id: u.id.clone(),
                    tmp: Temp { path: PathBuf::from(u.tmp.clone().unwrap_or_default()), admin: PathBuf::from(u.tmp_admin.clone().unwrap_or_default()) },
                    tested: u.revert.clone().unwrap_or_default(),
                    command,
                    kind: super::CHECK_UNDO,
                };
                let (checker, paths, rp, uid) = (opts.checker.clone(), caller.paths.clone(), repo.clone(), u.id.clone());
                let started = blocking(move || {
                    // Seeded like a worker's: the checkout's cargo target, in its own temp folder.
                    let target = paths.land_tmp(&uid).join(crate::hive::queue::LAND_CARGO_TARGET);
                    if std::fs::create_dir_all(&target).is_ok() {
                        if let Err(e) = crate::hive::worktree::seed_cargo_target(&rp.join("target").join("debug"), &target) {
                            log::info!("hive undo: {uid}'s cargo target not seeded: {e}");
                        }
                    }
                    checker.start(&req)
                })
                .await;
                match started {
                    Ok(CheckStart::Queued(job)) => {
                        u.state = STATE_CHECKING.to_string();
                        u.job = Some(job.clone());
                        if let Err(e) = save(&caller.paths, &mut u).await {
                            return refuse(ctx, caller, u, e).await;
                        }
                        ctx.window.workers_changed(&caller.workspace_id);
                        return Outcome::Checking(json!({
                            "undo": u.id,
                            "undoes": u.undoes,
                            "state": STATE_CHECKING,
                            "jobs": [job],
                            "note": "The land check runs in the queue on the revert commit. The result arrives as ⟦HIVE UNDO⟧.",
                        }));
                    }
                    Ok(CheckStart::Done { passed: true, .. }) => u.checked = true,
                    Ok(CheckStart::Done { passed: false, tail }) => return check_refused(ctx, caller, u, &tail, opts).await,
                    Err(e) => return refuse(ctx, caller, u, format!("its land check couldn't be queued: {e}")).await,
                }
            }
        }

        // Step 2: persisted before the target moves, so a restart reads the repo to know.
        u.state = STATE_ADVANCING.to_string();
        if let Err(e) = save(&caller.paths, &mut u).await {
            return refuse(ctx, caller, u, e).await;
        }
        if let Some(seam) = opts.before_advance.clone() {
            let f = u.folder.clone();
            let _ = blocking(move || {
                seam(&f);
                Ok(())
            })
            .await;
        }
        let (rp, target, from, to) = (repo.clone(), u.target.clone(), u.tip.clone().unwrap_or_default(), u.revert.clone().unwrap_or_default());
        match blocking(move || lg::advance(&rp, &target, &from, &to)).await {
            Ok(Step2::Advanced) => {
                if let Some(seam) = opts.after_advance.clone() {
                    let f = u.folder.clone();
                    let _ = blocking(move || {
                        seam(&f);
                        Ok(())
                    })
                    .await;
                }
                return finish(ctx, caller, u, &entry, opts).await;
            }
            Ok(Step2::Held(why)) => return hold(ctx, caller, u, why).await,
            Ok(Step2::BaseMoved) => {
                u.attempts += 1;
                log::info!("hive {}: undo {}: {} moved under it ({}/{MAX_ATTEMPTS})", caller.hive.id, u.id, u.target, u.attempts);
                if u.attempts >= MAX_ATTEMPTS {
                    let why = format!("{} keeps moving ({} tries)", u.target, u.attempts);
                    return hold(ctx, caller, u, why).await;
                }
                u.state = STATE_LOCKING.to_string();
                u.revert = None;
                u.checked = false;
            }
            Err(e) => return refuse(ctx, caller, u, e).await,
        }
    }
}

async fn check_refused<W: HiveWindow>(ctx: &Ctx<'_, W>, caller: &Caller, u: UndoState, tail: &str, opts: &Opts) -> Outcome {
    let lines: Vec<&str> = tail.trim_end().lines().collect();
    let tail = lines[lines.len().saturating_sub(30)..].join("\n");
    let mut why = format!("its land check failed on the revert in {}", u.folder);
    if !tail.trim().is_empty() {
        why.push_str(".\nIts last lines (the check's output, not maiTerm's words):\n");
        why.push_str(&tail);
    }
    let (w, folder, id) = (worker_of(&caller.paths, &u), u.folder.clone(), u.id.clone());
    let out = refuse(ctx, caller, u, why).await;
    super::base_check::run(ctx, caller, &w, &folder, &format!("undo {id}'s check failed there"), true, opts).await;
    out
}

/// Nothing was written: the undo is over, its lock and temporary worktree gone.
async fn refuse<W: HiveWindow>(ctx: &Ctx<'_, W>, caller: &Caller, mut u: UndoState, why: String) -> Outcome {
    let why = frame_escape(&why);
    let (hive, paths, mut x) = (caller.hive.clone(), caller.paths.clone(), u.clone());
    u = blocking(move || {
        cleanup(&hive, &paths, &mut x);
        Ok(x)
    })
    .await
    .unwrap_or(u);
    u.state = STATE_REFUSED.to_string();
    u.held_reason = Some(one_line(&why, 600));
    if let Err(e) = save(&caller.paths, &mut u).await {
        log::warn!("hive {}: undo {} not recorded as refused: {e}", caller.hive.id, u.id);
    }
    ctx.window.workers_changed(&caller.workspace_id);
    log::info!("hive {}: undo {} of {} refused: {}", caller.hive.id, u.id, u.undoes, one_line(&why, 300));
    Outcome::Refused(format!("{} wasn't undone: {why}\n[maiTerm] Nothing was written to {} in {}.", u.undoes, u.target, u.folder))
}

/// Held (§8.1's holds, or a busy lock): nothing written, and undoing it again resumes it.
async fn hold<W: HiveWindow>(ctx: &Ctx<'_, W>, caller: &Caller, mut u: UndoState, why: String) -> Outcome {
    let why = one_line(&why, 600);
    let (hive, paths, mut x) = (caller.hive.clone(), caller.paths.clone(), u.clone());
    u = blocking(move || {
        cleanup(&hive, &paths, &mut x);
        Ok(x)
    })
    .await
    .unwrap_or(u);
    u.state = STATE_HELD.to_string();
    u.held_reason = Some(why.clone());
    if let Err(e) = save(&caller.paths, &mut u).await {
        log::warn!("hive {}: undo {} not recorded as held: {e}", caller.hive.id, u.id);
    }
    ctx.window.workers_changed(&caller.workspace_id);
    log::info!("hive {}: undo {} of {} held: {why}", caller.hive.id, u.id, u.undoes);
    Outcome::Held(json!({
        "undo": u.id,
        "undoes": u.undoes,
        "state": STATE_HELD,
        "reason": why,
        "result": format!("The undo of {} holds: {why}.", u.undoes),
        "note": "Nothing was stashed, overwritten or written. Once the reason is gone, undo it again: it makes its revert afresh on the target as it then is.",
    }))
}

/// The revert landed: logged, pushed with Push after land, the worker marked undone (its waiters
/// block), the read copy moved, the coordinator's task noted. A landed tree that isn't the one
/// its check passed (Base moved between the fast-forward and the read) gets the post-land
/// check, as a land's does (§8.6).
async fn finish<W: HiveWindow>(ctx: &Ctx<'_, W>, caller: &Caller, mut u: UndoState, e: &LandLogEntry, opts: &Opts) -> Outcome {
    let (hive, paths, mut x, entry) = (caller.hive.clone(), caller.paths.clone(), u.clone(), e.clone());
    let done = blocking(move || {
        let repo = PathBuf::from(&driver::hive_folder(&hive, &x.folder)?.path);
        x.base_after = x.revert.clone();
        x.tree_landed = lg::tree_of(&repo, &format!("refs/heads/{}", x.target));
        x.pushed = Some(if hive.push_after_land && !hive.is_integration() { lg::push(&repo, &x.target) } else { landlog::PUSHED_OFF.to_string() });
        log_undo(&paths, &hive, &entry, &mut x);
        cleanup(&hive, &paths, &mut x);
        let mut notes = Vec::new();
        if !hive.is_integration() {
            if let Some(to) = x.revert.as_deref() {
                if let Err(e) = lg::move_read_copy(&repo, &paths.coordinator_worktree(&x.folder), to) {
                    notes.push(format!("Your read copy of {} stays where it was ({e}).", x.folder));
                }
            }
        }
        mark_undone(&paths, &x.task_id);
        x.state = STATE_DONE.to_string();
        write(&paths, &mut x)?;
        Ok((x, notes))
    })
    .await;
    let notes = match done {
        Ok((x, notes)) => {
            u = x;
            notes
        }
        Err(e) => {
            log::warn!("hive {}: undo {} landed but its bookkeeping failed: {e}", caller.hive.id, u.id);
            vec![format!("It landed, but maiTerm couldn't finish recording it: {e}")]
        }
    };
    let who = if u.by_human() { "the human" } else { "the coordinator" };
    // The human's own Undo of the coordinator's land overrides that decision (plan D17).
    if u.by_human() {
        crate::hive::decisions::land_undone_by_human(ctx, caller, &u.land, e.decision.as_deref()).await;
    }
    crate::hive::finish::undone_after_done(ctx, caller, &u.undoes).await;
    let note = format!("Land {} undone by {who}: {}", u.undoes, one_line(&u.reason, 300));
    if let Err(err) = crate::hive::control::task_note(ctx, caller, &u.task_id, &note, None).await {
        log::info!("hive {}: undo note for {} not written: {err}", caller.hive.id, u.task_id);
    }
    ctx.window.workers_changed(&caller.workspace_id);
    let revert = u.revert.clone().unwrap_or_default();
    let mut result = format!(
        "Land {} (“{}”) is undone: revert commit {} on {} in {}. Worker {} is marked undone, so workers waiting on it stay \
         blocked; it isn't reopened, so spawn a new task if the work should come back.",
        u.undoes,
        one_line(&frame_escape(&u.title), 120),
        &revert[..revert.len().min(10)],
        u.target,
        u.folder,
        u.worker
    );
    if let Some(p) = u.pushed.as_deref().filter(|p| p.starts_with("held")) {
        result.push_str(&format!(" Push {p} (the undo stands; the next land pushes both)."));
    }
    for n in &notes {
        result.push_str(&format!(" {n}"));
    }
    log::info!("hive {}: undo {} of {} landed at {}", caller.hive.id, u.id, u.undoes, &revert[..revert.len().min(10)]);
    if u.tree_landed.is_some() && u.tree_landed != u.tree {
        let cause = format!("undo {} landed a tree other than the one its check passed", u.id);
        super::base_check::run(ctx, caller, &worker_of(&caller.paths, &u), &u.folder, &cause, false, opts).await;
    }
    Outcome::Landed(json!({
        "undo": u.id,
        "undoes": u.undoes,
        "state": STATE_DONE,
        "folder": u.folder,
        "target": u.target,
        "revert": revert,
        "pushed": u.pushed,
        "result": result,
    }))
}

/// Its line in the land log, unless it has one. Blocks.
fn log_undo(paths: &HivePaths, hive: &Hive, e: &LandLogEntry, u: &mut UndoState) {
    if u.logged {
        return;
    }
    let id = format!("{}/{}", u.id, u.folder);
    if landlog::has(paths, &id) {
        u.logged = true;
        return;
    }
    let human = u.by_human() || u.approved_by.is_some();
    let entry = LandLogEntry {
        id,
        land: u.id.clone(),
        kind: landlog::KIND_UNDO.to_string(),
        hive_id: hive.id.clone(),
        task_id: u.task_id.clone(),
        worker: u.worker.clone(),
        title: u.title.clone(),
        folder: u.folder.clone(),
        target: u.target.clone(),
        reviewed_sha: e.tested_sha.clone(),
        tested_sha: u.revert.clone().unwrap_or_default(),
        base_before: u.tip.clone().unwrap_or_default(),
        base_after: u.base_after.clone().unwrap_or_default(),
        tree_tested: u.tree.clone().unwrap_or_default(),
        tree_landed: u.tree_landed.clone().unwrap_or_default(),
        summary: u.reason.clone(),
        by: if human { landlog::BY_HUMAN } else { landlog::BY_COORDINATOR }.to_string(),
        request: u.request.clone().filter(|_| u.approved_by.is_some()),
        undoes: Some(u.undoes.clone()),
        pushed: u.pushed.clone().unwrap_or_else(|| landlog::PUSHED_OFF.to_string()),
        check: if u.checked { landlog::CHECK_PASSED.to_string() } else { String::new() },
        at: crate::commands::workspace::iso_now(),
        ..LandLogEntry::default()
    };
    match landlog::append(paths, &entry) {
        Ok(()) => u.logged = true,
        Err(err) => log::warn!("hive {}: land log line {} not written: {err}", hive.id, entry.id),
    }
}

/// The land's worker marked undone (§8.6): its waiters block with "undone". Blocks.
fn mark_undone(paths: &HivePaths, task_id: &str) {
    let Ok(dir) = worker::task_dir(paths, task_id) else { return };
    if let Err(e) = worker::update_worker(&dir, |cur| !std::mem::replace(&mut cur.undone, true)) {
        log::info!("hive: worker of task {task_id} not marked undone: {e}");
    }
}

// ── The human's card (the coordinator's undo of config) ──

/// What the card says and carries. Every word maiTerm's but the reason. Blocks on git.
pub(crate) fn card(hive: &Hive, workspace_id: &str, u: &UndoState, e: &LandLogEntry) -> (String, Vec<String>, Value) {
    let title = one_line(&frame_escape(&u.title), 120);
    let summary = format!("Undo land “{title}” on {} in {}", u.target, u.folder);
    let repo = driver::hive_folder(hive, &u.folder).map(|hf| PathBuf::from(&hf.path));
    let (from, to) = match e.merge_commit.as_deref().filter(|m| !m.is_empty()) {
        Some(m) => (format!("{m}^1"), m.to_string()),
        None => (e.base_before.clone(), e.base_after.clone()),
    };
    let stat = repo
        .as_ref()
        .map_err(|e| e.clone())
        .and_then(|r| lg::shortstat_between(r, &from, &to))
        .map(|s| if s.trim().is_empty() { "no file changes".to_string() } else { s.trim().to_string() })
        .unwrap_or_else(|err| format!("its diff couldn't be read ({})", one_line(&err, 120)));
    let mut effects = vec![
        format!(
            "It writes a revert commit to {} in {}, and that revert changes files that configure agents or run code on this machine, \
             outside any sandbox: those always come to you.",
            u.target, u.folder
        ),
        format!("Reverts land {} ({stat}), which landed {} on {}.", u.undoes, &e.base_after[..e.base_after.len().min(10)], e.target),
        format!("Changes agent or execution config in {}: {}.", u.folder, driver::files_line(&u.config_hits)),
    ];
    effects.push(if has_check(hive, &u.folder).is_some() {
        "Its land check runs on the revert before it lands.".to_string()
    } else {
        format!("{} has no land check, so nothing tests the revert before it lands.", u.folder)
    });
    effects.push("Nothing is reset or rewritten: the land stays in the log and in git, with a commit on top that takes it back.".to_string());
    let args = json!({
        "hive_id": hive.id,
        "workspace_id": workspace_id,
        "undo_id": u.id,
        // One card per undo (`agent_requests::same_pending`); no task id, so two undos of one
        // worker's repos don't supersede each other.
        "land_id": u.id,
        "undoes": u.undoes,
        "folder": u.folder,
        "config_fp": u.config_fp,
    });
    (summary, effects, args)
}

async fn await_human<W: HiveWindow>(ctx: &Ctx<'_, W>, caller: &Caller, mut u: UndoState, e: &LandLogEntry) -> Outcome {
    if caller.tab_id.is_empty() {
        return refuse(ctx, caller, u, "its revert changes agent or execution config, which needs the human's approval, and this Hive has no coordinator tab to ask from".to_string()).await;
    }
    let (hive, paths, ws, mut x, entry) = (caller.hive.clone(), caller.paths.clone(), caller.workspace_id.clone(), u.clone(), e.clone());
    let built = blocking(move || {
        // No lock and no temporary worktree while the human decides (plan D4).
        cleanup(&hive, &paths, &mut x);
        let c = card(&hive, &ws, &x, &entry);
        Ok((x, c))
    })
    .await;
    let (x, (summary, effects, args)) = match built {
        Ok(b) => b,
        Err(err) => return refuse(ctx, caller, u, err).await,
    };
    u = x;
    let req = HiveRequest { tab_id: caller.tab_id.clone(), kind: KIND, args, summary, effects, reason: Some(u.reason.clone()), keep: None };
    let id = match ctx.window.file_request(req) {
        Ok(id) => id,
        Err(err) => return refuse(ctx, caller, u, format!("maiTerm couldn't put it to the human: {err}")).await,
    };
    u.state = STATE_AWAITING_HUMAN.to_string();
    u.request = Some(id.clone());
    u.request_at = now_ms();
    u.checked = false;
    if let Err(err) = save(&caller.paths, &mut u).await {
        ctx.window.withdraw_request(&id, "maiTerm couldn't record the undo it asks about");
        return refuse(ctx, caller, u, err).await;
    }
    ctx.window.workers_changed(&caller.workspace_id);
    log::info!("hive {}: undo {} of {} waits for the human (request {id})", caller.hive.id, u.id, u.undoes);
    Outcome::Awaiting(json!({
        "undo": u.id,
        "undoes": u.undoes,
        "state": STATE_AWAITING_HUMAN,
        "request": id,
        "result": format!(
            "The undo of {} waits for the human's approval: its revert changes agent or execution config ({}). Nothing has been written.",
            u.undoes,
            driver::files_line(&u.config_hits)
        ),
        "note": "The card is in their Decisions and on their phone; don't ask them yourself. The result arrives as ⟦HIVE UNDO⟧ once \
                 they answer; a denial, or no answer within 24 hours, leaves the land as it is.",
    }))
}

/// The human approved the coordinator's undo: it goes on with their say-so, the target re-read
/// (a moved target makes its revert again, and a config change other than the approved one asks
/// again), and the request is settled here.
pub(crate) async fn approved<W: HiveWindow>(ctx: &Ctx<'_, W>, caller: &Caller, d: &super::approval::Decision, opts: &Opts) -> Outcome {
    let id = d.args.get("undo_id").and_then(Value::as_str).unwrap_or_default().to_string();
    let (paths, i, rid, by, fp) = (caller.paths.clone(), id.clone(), d.id.clone(), d.by.clone(), d.args.get("config_fp").and_then(Value::as_str).map(str::to_string));
    let taken = blocking(move || {
        let _one = START.lock().unwrap_or_else(|e| e.into_inner());
        let mut u = read_one(&paths, &i)?;
        if u.state != STATE_AWAITING_HUMAN || u.request.as_deref() != Some(rid.as_str()) {
            return Err(format!("the undo of {} isn't waiting on this card any more ({})", u.undoes, u.state));
        }
        u.state = STATE_LOCKING.to_string();
        u.approved_by = Some(by);
        u.config_approved = fp;
        write(&paths, &mut u)?;
        Ok(u)
    })
    .await;
    let u = match taken {
        Ok(u) => u,
        Err(e) => {
            ctx.window.settle_request(&d.id, false, e);
            return Outcome::Gone;
        }
    };
    log::info!("hive {}: undo {} of {} approved by the human ({})", caller.hive.id, u.id, u.undoes, d.by);
    let out = drive(ctx, caller, &u.id, opts).await;
    let result = |v: &Value| v.get("result").and_then(Value::as_str).unwrap_or_default().to_string();
    let (ok, outcome) = match &out {
        Outcome::Landed(v) | Outcome::Held(v) | Outcome::Waiting(v) => (true, result(v)),
        Outcome::Checking(_) => (true, "approved: its land check runs now; the coordinator hears the result".to_string()),
        Outcome::Awaiting(_) => (false, "its config change is no longer the one you approved, so a new card asks again".to_string()),
        Outcome::Refused(e) => (false, e.clone()),
        Outcome::Gone => (false, "the undo isn't there to carry out any more".to_string()),
    };
    ctx.window.settle_request(&d.id, ok, outcome);
    let from = if d.by == "phone" { " from their phone" } else { "" };
    let rest = match &out {
        Outcome::Checking(_) => Some("Its land check runs now; the result follows as ⟦HIVE UNDO⟧.".to_string()),
        Outcome::Refused(e) => Some(e.clone()),
        Outcome::Landed(v) | Outcome::Held(v) | Outcome::Waiting(v) | Outcome::Awaiting(v) => v.get("result").and_then(Value::as_str).map(str::to_string),
        Outcome::Gone => None,
    };
    if let Some(rest) = rest {
        tell(ctx, caller, &u, format!("The human approved undoing {}{from}. {rest}", u.undoes)).await;
    }
    out
}

/// A denial or a lapse: nothing is written, and the coordinator is told.
pub(crate) async fn closed<W: HiveWindow>(ctx: &Ctx<'_, W>, caller: &Caller, d: &super::approval::Decision, status: &str) -> Option<Outcome> {
    let id = d.args.get("undo_id").and_then(Value::as_str).unwrap_or_default().to_string();
    let (paths, i) = (caller.paths.clone(), id.clone());
    let u = blocking(move || read_one(&paths, &i)).await.ok()?;
    if u.state != STATE_AWAITING_HUMAN || u.request.as_deref() != Some(d.id.as_str()) {
        return None;
    }
    let why = match status {
        "denied" => format!("the human denied it{}", if d.by == "phone" { " from their phone" } else { "" }),
        "expired" => "the human didn't answer its card within 24 hours, so it lapsed".to_string(),
        other => format!("its card ended without an approval ({other})"),
    };
    let out = refuse(ctx, caller, u.clone(), why).await;
    if let Outcome::Refused(text) = &out {
        tell(ctx, caller, &u, text.clone()).await;
    }
    Some(out)
}

/// Every undo waiting on the human filed again under its id and first filing time, after a
/// restart. Blocks on git.
pub(crate) fn refile_in<W: HiveWindow>(window: &W, caller: &Caller) -> usize {
    let mut n = 0;
    for u in read_all(&caller.paths).into_iter().filter(|u| u.state == STATE_AWAITING_HUMAN) {
        let (Some(id), Ok(e)) = (u.request.clone(), entry_of(&caller.paths, &u)) else { continue };
        let (summary, effects, args) = card(&caller.hive, &caller.workspace_id, &u, &e);
        let req = HiveRequest { tab_id: caller.tab_id.clone(), kind: KIND, args, summary, effects, reason: Some(u.reason.clone()), keep: Some((id, u.request_at)) };
        match window.file_request(req) {
            Ok(_) => n += 1,
            Err(err) => log::warn!("hive {}: undo {} not put to the human again: {err}", caller.hive.id, u.id),
        }
    }
    n
}

/// `⟦HIVE UNDO · <title>⟧` typed into the coordinator through the delivery gate.
async fn tell<W: HiveWindow>(ctx: &Ctx<'_, W>, caller: &Caller, u: &UndoState, says: String) {
    let text = format!("⟦HIVE UNDO · {}⟧ {says}", one_line(&frame_escape(&u.title), 120));
    match coordinator_tab(ctx.data, caller) {
        Some(c) => {
            let sent = ctx.window.deliver(&c, text.clone(), "Hive undo").await;
            log::info!("hive {}: undo result for {}: {}", caller.hive.id, u.undoes, sent.describe());
            // The human's Undo in a Done Hive finds no agent there: it hears once it is woken.
            crate::hive::owed::owe_if_refused(caller, &sent, "Hive undo", text).await;
        }
        None => log::info!("hive {}: undo result for {} not delivered: no coordinator tab", caller.hive.id, u.undoes),
    }
}

/// The text the coordinator hears of an outcome that comes after its own call (a queued check,
/// the human's undo).
pub(crate) fn outcome_text(o: &Outcome) -> Option<String> {
    match o {
        Outcome::Refused(e) => Some(e.clone()),
        Outcome::Landed(v) | Outcome::Held(v) | Outcome::Waiting(v) | Outcome::Awaiting(v) => v.get("result").and_then(Value::as_str).map(str::to_string),
        Outcome::Checking(_) | Outcome::Gone => None,
    }
}

/// A queued undo check ended (`driver::check_ended`): the undo goes on to step 2 or is refused,
/// and the coordinator hears the result.
pub(crate) async fn on_check_end<W: HiveWindow>(
    ctx: &Ctx<'_, W>,
    caller: &Caller,
    job: &crate::hive::queue::Job,
    run: &crate::hive::queue::LandRun,
    tail: &str,
    opts: &Opts,
) -> Option<Outcome> {
    let (paths, i) = (caller.paths.clone(), run.land_id.clone());
    let mut u = blocking(move || read_one(&paths, &i)).await.ok()?;
    if u.state != STATE_CHECKING || u.job.as_deref() != Some(job.id.as_str()) {
        return None;
    }
    let passed = job.state == crate::hive::queue::DONE && job.began && job.exit_code == Some(0) && !job.dirty;
    u.job = None;
    let out = if passed {
        u.checked = true;
        u.state = STATE_LOCKING.to_string();
        if let Err(e) = save(&caller.paths, &mut u).await {
            refuse(ctx, caller, u.clone(), e).await
        } else {
            drive(ctx, caller, &u.id, opts).await
        }
    } else if job.state == crate::hive::queue::DONE && job.began && !job.dirty {
        u.state = STATE_LOCKING.to_string();
        check_refused(ctx, caller, u.clone(), tail, opts).await
    } else {
        let why = format!("its land check didn't finish ({})", job.error.as_deref().map(|e| one_line(e, 200)).unwrap_or_else(|| job.state.clone()));
        refuse(ctx, caller, u.clone(), why).await
    };
    if let Some(text) = outcome_text(&out) {
        tell(ctx, caller, &u, text).await;
    }
    Some(out)
}

/// At startup: every undo a stopped maiTerm left running is settled from the repo. One caught in
/// step 2 whose revert is on the target landed (its line and the worker's mark are written now);
/// any other holds, and undoing it again resumes it. Blocks on git and the disk.
pub(crate) fn reconcile_in(hive: &Hive, paths: &HivePaths) -> usize {
    let mut n = 0;
    for mut u in read_all(paths).into_iter().filter(UndoState::in_progress) {
        let advancing = u.state == STATE_ADVANCING;
        cleanup(hive, paths, &mut u);
        let repo = driver::hive_folder(hive, &u.folder).map(|hf| PathBuf::from(&hf.path));
        let revert = u.revert.clone().unwrap_or_default();
        let landed = advancing
            && !revert.is_empty()
            && repo.as_ref().is_ok_and(|r| lg::branch_tip(r, &u.target).is_some_and(|t| lg::is_ancestor(r, &revert, &t)));
        if landed {
            let repo = repo.expect("checked");
            u.base_after = Some(revert.clone());
            u.tree_landed = lg::tree_of(&repo, &revert);
            u.pushed.get_or_insert_with(|| "held: maiTerm restarted before its push".to_string());
            if let Ok(e) = entry_of(paths, &u) {
                log_undo(paths, hive, &e, &mut u);
            }
            mark_undone(paths, &u.task_id);
            u.state = STATE_DONE.to_string();
        } else {
            u.state = STATE_HELD.to_string();
            u.held_reason = Some("interrupted by a maiTerm restart: undo it again to resume".to_string());
        }
        match write(paths, &mut u) {
            Ok(()) => n += 1,
            Err(e) => log::warn!("hive {}: undo {} not reconciled: {e}", hive.id, u.id),
        }
    }
    n
}

// ── hiveUndoLand ──

pub fn tool_definition(tab_id: &Value) -> Value {
    json!({
        "name": "hiveUndoLand",
        "description": format!(
            "Undo a land: maiTerm makes a revert commit on the land's target (Base) and lands it like a land (under the repo's \
             lock, through its land check, by fast-forward), never resetting history. land names a land log line, \"L-…/folder\", \
             or \"L-…\" for a land in one repo (the ids are in hiveLand's answers and ⟦HIVE LAND⟧). Refused if later work on the \
             target builds on it (a revert would conflict: fix forward instead), if it is already undone or being undone, and \
             in the integration mode, where Undo is the human's. A revert that changes agent or execution config waits for the \
             human's approval on a card. The worker isn't reopened: it is marked undone, workers waiting on it stay blocked, \
             and the work comes back only as a new task. An uncommitted change in Base's checkout holds it, as for a land; \
             undo again once it's gone. The reason (at most {MAX_SUMMARY} characters) goes in the land log."
        ),
        "inputSchema": {
            "type": "object",
            "properties": {
                "tabId": tab_id,
                "land": { "type": "string", "description": "The land log line: \"L-1a2b3c4d/web\", or \"L-1a2b3c4d\" for a land in one repo" },
                "reason": { "type": "string", "description": "Why it is undone, for the land log" }
            },
            "required": ["land", "reason"]
        }
    })
}

pub(crate) async fn undo_tool<W: HiveWindow>(ctx: &Ctx<'_, W>, caller: &Caller, args: &Value) -> Result<Value, String> {
    undo_tool_with(ctx, caller, args, &Opts::production()).await
}

pub(crate) async fn undo_tool_with<W: HiveWindow>(ctx: &Ctx<'_, W>, caller: &Caller, args: &Value, opts: &Opts) -> Result<Value, String> {
    if caller.hive.is_integration() {
        return Err(format!(
            "This Hive lands into {} with the human's approval, so Undo is theirs (the land log, or the worker's column menu): \
             ask them if a land should be undone.",
            caller.hive.integration_branch()
        ));
    }
    let land = args.get("land").and_then(Value::as_str).map(str::trim).unwrap_or_default();
    if land.is_empty() {
        return Err("land is required: the land log line to undo, \"L-…/folder\"".to_string());
    }
    let reason = args.get("reason").and_then(Value::as_str).map(str::trim).unwrap_or_default();
    if reason.is_empty() {
        return Err("reason is required: why the land is undone, for the land log".to_string());
    }
    if reason.chars().count() > MAX_SUMMARY {
        return Err(format!("Keep the reason under {MAX_SUMMARY} characters."));
    }
    start(ctx, caller, land, landlog::BY_COORDINATOR, reason, None, opts).await.1.answer()
}

// ── The human's commands ──

/// What the human's Undo would do, for its inline confirmation (§8.6): the repo, the commits it
/// reverts, the branch it writes, any config files, or why it can't.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct UndoPreview {
    pub id: String,
    pub land: String,
    pub folder: String,
    pub target: String,
    pub title: String,
    pub worker: String,
    /// `fast-forward`: one commit holding the inverse; `merge`: `revert -m 1` of its merge commit.
    pub method: String,
    pub commits: Vec<lg::CommitLine>,
    pub commits_total: usize,
    /// The target's tip the revert would go on.
    pub tip: String,
    pub config_files: Vec<String>,
    pub config_fp: Option<String>,
    pub has_check: bool,
    /// Why it can't be undone now (a conflict, nothing to revert, old git), in words.
    pub refusal: Option<String>,
}

/// The preview, built as the undo would build it, on a scratch id whose worktree goes again.
/// Blocks on git.
pub fn preview(hive: &Hive, paths: &HivePaths, name: &str) -> Result<UndoPreview, String> {
    let folded = landlog::folded(paths);
    let e = resolve(&folded, name)?;
    if let Some(u) = read_all(paths).into_iter().find(|u| u.undoes == e.id && u.open() && u.state != STATE_HELD) {
        return Err(format!("{} is being undone now ({}, {}).", e.id, u.id, u.state));
    }
    let repo = PathBuf::from(&driver::hive_folder(hive, &e.folder)?.path);
    let (method, range) = match e.merge_commit.as_deref().filter(|m| !m.is_empty()) {
        Some(m) => ("merge", format!("{m}^1..{m}")),
        None => ("fast-forward", format!("{}..{}", e.base_before, e.base_after)),
    };
    let (commits, commits_total) = lg::commits_in(&repo, &range, PREVIEW_COMMITS).unwrap_or_default();
    let mut scratch = UndoState { id: new_id(), undoes: e.id.clone(), folder: e.folder.clone(), target: e.target.clone(), ..UndoState::default() };
    let built = build(hive, paths, &e, &mut scratch, false);
    cleanup(hive, paths, &mut scratch);
    let refusal = built.err();
    Ok(UndoPreview {
        id: e.id.clone(),
        land: e.land.clone(),
        folder: e.folder.clone(),
        target: e.target.clone(),
        title: e.title.clone(),
        worker: e.worker.clone(),
        method: method.to_string(),
        commits,
        commits_total,
        tip: scratch.tip.clone().or_else(|| lg::branch_tip(&repo, &e.target)).unwrap_or_default(),
        config_files: if refusal.is_none() { scratch.config_hits.clone() } else { Vec::new() },
        config_fp: scratch.config_fp.clone().filter(|_| refusal.is_none()),
        has_check: has_check(hive, &e.folder).is_some(),
        refusal,
    })
}

/// The land log as the human's view shows it: every line folded, each repo's standing, and the
/// undos that run, wait or hold.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct LandLogView {
    /// Oldest first.
    pub entries: Vec<FoldedEntry>,
    pub repos: Vec<landlog::RepoStanding>,
    pub undos: Vec<UndoView>,
    pub integration: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct UndoView {
    pub id: String,
    pub undoes: String,
    pub state: String,
    pub by: String,
    pub held_reason: Option<String>,
    pub request: Option<String>,
}

pub fn log_view(hive: &Hive, paths: &HivePaths) -> LandLogView {
    let folded = landlog::folded(paths);
    LandLogView {
        repos: folded.repos.values().cloned().collect(),
        entries: folded.entries,
        undos: read_all(paths)
            .into_iter()
            .filter(UndoState::open)
            .map(|u| UndoView { id: u.id, undoes: u.undoes, state: u.state, by: u.by, held_reason: u.held_reason, request: u.request })
            .collect(),
        integration: hive.is_integration(),
    }
}

/// The land log (§8.6), folded: what the land log view and the worker column's menu read.
#[tauri::command]
pub async fn hive_land_log(window: tauri::Window, state: tauri::State<'_, std::sync::Arc<crate::state::AppState>>, workspace_id: String) -> Result<LandLogView, String> {
    let caller = super::caller_in(&state, window.label(), &workspace_id)?;
    blocking(move || Ok(log_view(&caller.hive, &caller.paths))).await
}

/// What undoing `id` would do, before anything is written.
#[tauri::command]
pub async fn hive_undo_preview(
    window: tauri::Window,
    state: tauri::State<'_, std::sync::Arc<crate::state::AppState>>,
    workspace_id: String,
    id: String,
) -> Result<UndoPreview, String> {
    let caller = super::caller_in(&state, window.label(), &workspace_id)?;
    blocking(move || preview(&caller.hive, &caller.paths, &id)).await
}

/// The human's Undo (plan D15): their click on the confirmation is the approval, so no card.
/// `config_fp` is what that confirmation showed: a revert that has come to change other config
/// is refused, to be looked at again.
#[tauri::command]
pub async fn hive_undo_land(
    window: tauri::Window,
    app: tauri::AppHandle,
    state: tauri::State<'_, std::sync::Arc<crate::state::AppState>>,
    workspace_id: String,
    id: String,
    config_fp: Option<String>,
) -> Result<Value, String> {
    let caller = super::caller_in(&state, window.label(), &workspace_id)?;
    let ctx = crate::hive::tools::app_ctx(state.inner(), &app, &caller);
    let out = human_undo(&ctx, &caller, &id, config_fp, &Opts::production()).await;
    crate::hive::waiters::poll_soon(state.inner().clone(), app.clone(), caller.workspace_id.clone());
    out
}

/// The human's Undo, told to the coordinator whatever it came to but a refusal (which the human
/// sees, and which wrote nothing).
pub(crate) async fn human_undo<W: HiveWindow>(ctx: &Ctx<'_, W>, caller: &Caller, id: &str, config_fp: Option<String>, opts: &Opts) -> Result<Value, String> {
    let (u, out) = start(ctx, caller, id, landlog::BY_HUMAN, "the human's Undo, from the land log", config_fp, opts).await;
    let text = match &out {
        Outcome::Refused(_) | Outcome::Gone => None,
        Outcome::Checking(_) => Some("Its land check runs now; the result follows as ⟦HIVE UNDO⟧.".to_string()),
        o => outcome_text(o),
    };
    if let (Some(u), Some(text)) = (u, text) {
        tell(ctx, caller, &u, format!("The human started undoing {} from the land log. {text}", u.undoes)).await;
    }
    out.answer()
}
