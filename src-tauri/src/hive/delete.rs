//! Deleting a Hive (docs/hive.md §11.2): the workspace stays as an ordinary one with its Hive tabs
//! gone and its task rows kept as history (plan D13), and nothing that holds work is deleted.
//!
//! The rule is Claude's own worktree cleanup's: clean worktrees go, dirty ones stay and are listed,
//! no branch is ever deleted (`hive/<slug>` included), the gallery stays unless the human ticks it,
//! and a scratch folder with anything in it stays. A worktree whose `.git` file maiTerm can't vouch
//! for stays untouched (`retire::keep_reason`). Refused while a worker is landing.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde::Serialize;
use serde_json::json;

use super::git::{git_in, run, QUERY_TIMEOUT, WORKTREE_TIMEOUT};
use super::paths::{HivePaths, COORDINATOR_SLUG, LAND_SLUG};
use super::record::Hive;
use super::retire::{found_folder, git_file_names, keep_reason, remove_if_clean, remove_path, scratch_has_work, Removal};
use super::rpc::{self, Front, HiveWindow};
use super::tools::{blocking, worker_label, Caller, Ctx};
use super::worker::{self, WorkerRecord};
use crate::state::AppState;

/// A worker branch its landing target doesn't have all of.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct UnlandedBranch {
    /// `#badge slug`.
    pub worker: String,
    pub folder: String,
    pub branch: String,
    pub target: Option<String>,
    pub ahead: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ScratchFolder {
    pub worker: String,
    pub path: String,
    /// Files in it, maiTerm's brief aside.
    pub files: u64,
    /// Of those, the ones its report named.
    pub reported: u64,
}

/// What the delete sheet lists before anything is touched (§11.2).
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct HiveDeletePreview {
    pub unlanded: Vec<UnlandedBranch>,
    /// Worktrees that would stay, each with why.
    pub dirty: Vec<String>,
    pub scratch: Vec<ScratchFolder>,
    /// The gallery's assets that still have their file (the trash's included), from its index.
    pub gallery_files: u64,
    pub gallery_bytes: u64,
    /// Agents that would be stopped: `#badge slug`, and the coordinator.
    pub live_agents: Vec<String>,
    /// Workers landing now: while any is, delete is refused.
    pub lands_in_flight: Vec<String>,
    /// Every branch the Hive made, all kept: `folder: branch`.
    pub branches: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct PaneActive {
    pub pane_id: String,
    pub tab_id: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct HiveDeleteResult {
    pub worktrees_removed: usize,
    /// What stayed on disk, each with why.
    pub kept: Vec<String>,
    pub branches: Vec<String>,
    pub gallery_kept: bool,
    pub trust_removed: usize,
    pub requests_failed: usize,
    /// The Hive's folder, when anything stayed in it.
    pub folder_left: Option<String>,
    /// The tabs dropped, and each pane's active tab after: the window's mirror follows.
    pub dropped_tabs: Vec<String>,
    pub active_tabs: Vec<PaneActive>,
}

fn repo_of(hive: &Hive, folder: &str) -> Option<PathBuf> {
    hive.folders.iter().find(|h| h.name == folder).map(|h| PathBuf::from(&h.path))
}

fn branch_exists(repo: &Path, branch: &str) -> bool {
    run(git_in(repo).args(["rev-parse", "--verify", "--quiet"]).arg(format!("refs/heads/{branch}")), QUERY_TIMEOUT).is_ok()
}

/// Every branch the Hive made that still exists, as `folder: branch`. Asked of the human's repo,
/// whose config no worker writes. Blocks.
fn hive_branches(hive: &Hive, workers: &[WorkerRecord]) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut push = |s: String| {
        if !out.contains(&s) {
            out.push(s);
        }
    };
    if hive.is_integration() {
        let b = hive.integration_branch();
        for f in hive.git_folders() {
            if branch_exists(Path::new(&f.path), &b) {
                push(format!("{}: {b}", f.name));
            }
        }
    }
    for w in workers {
        for f in w.folders.iter() {
            let (Some(b), Some(repo)) = (f.branch.as_deref(), repo_of(hive, &f.name)) else { continue };
            if branch_exists(&repo, b) {
                push(format!("{}: {b}", f.name));
            }
        }
    }
    out
}

/// Files and bytes under `dir`, links counted as themselves and never followed.
fn tree_size(dir: &Path) -> (u64, u64) {
    let (mut files, mut bytes) = (0, 0);
    for e in fs::read_dir(dir).into_iter().flatten().flatten() {
        let Ok(m) = fs::symlink_metadata(e.path()) else { continue };
        if m.is_dir() {
            let (f, b) = tree_size(&e.path());
            files += f;
            bytes += b;
        } else {
            files += 1;
            bytes += m.len();
        }
    }
    (files, bytes)
}

fn scratch_folder(paths: &HivePaths, w: &WorkerRecord) -> Option<ScratchFolder> {
    let scratch = paths.worker_scratch(&w.slug);
    if !scratch_has_work(&scratch) {
        return None;
    }
    let files = tree_size(&scratch).0.saturating_sub(u64::from(scratch.join("brief.md").is_file()));
    let real = crate::share::canon(&scratch);
    let reported = w
        .last_report
        .iter()
        .flat_map(|r| r.receipts.files.iter())
        .filter(|f| {
            let p = Path::new(f.as_str());
            let p = if p.is_absolute() { p.to_path_buf() } else { scratch.join(p) };
            p.is_file() && crate::share::canon(&p).starts_with(&real)
        })
        .count() as u64;
    Some(ScratchFolder { worker: worker_label(w), path: scratch.to_string_lossy().to_string(), files, reported })
}

/// What under `wt.noindex/` no loaded record accounts for, to keep whole with why: a worker whose
/// `worker.json` didn't load still has its worktrees there, and nothing vouched they hold no work.
/// Its task folder stays with them, the only record of what they are. Blocks.
fn unaccounted(hive: &Hive, paths: &HivePaths, workers: &[WorkerRecord]) -> Vec<(String, PathBuf)> {
    let mut out = Vec::new();
    for e in fs::read_dir(paths.worktrees()).into_iter().flatten().flatten() {
        let (dir, name) = (e.path(), e.file_name().to_string_lossy().to_string());
        let known: Vec<PathBuf> = if name == LAND_SLUG {
            // Each land worktree was removed or kept on its own.
            continue;
        } else if name == COORDINATOR_SLUG {
            hive.git_folders().map(|f| paths.coordinator_worktree(&f.name)).collect()
        } else if let Some(w) = workers.iter().find(|w| w.slug == name) {
            let own = [paths.worker_scratch(&w.slug), paths.worker_tmp(&w.slug), paths.worker_cargo_target(&w.slug)];
            own.into_iter().chain(w.git_folders().filter_map(|f| f.worktree.as_deref().map(PathBuf::from))).collect()
        } else {
            out.push((format!("{} (no worker record maiTerm could read names it, so nothing vouched it holds no work)", dir.display()), dir));
            continue;
        };
        let known: Vec<PathBuf> = known.iter().map(|k| crate::share::canon(k)).collect();
        for c in fs::read_dir(&dir).into_iter().flatten().flatten() {
            let p = c.path();
            if !known.contains(&crate::share::canon(&p)) {
                out.push((format!("{} (not a folder this Hive's records name)", p.display()), p));
            }
        }
    }
    for e in fs::read_dir(paths.tasks()).into_iter().flatten().flatten() {
        let name = e.file_name().to_string_lossy().to_string();
        if name == COORDINATOR_SLUG || workers.iter().any(|w| w.task_id == name) {
            continue;
        }
        if fs::symlink_metadata(e.path().join(worker::WORKER_FILE)).is_ok() {
            out.push((format!("task record {} (it couldn't be read, so what it names is kept)", e.path().display()), e.path()));
        }
    }
    out
}

/// The gallery's assets and their bytes, from its index: the folder also holds the index, its WAL
/// files and the thumbnails. No index means nothing was ever filed.
fn gallery_totals(hive: &Hive, paths: &HivePaths) -> (u64, u64) {
    if !paths.gallery_index().is_file() {
        return (0, 0);
    }
    match super::gallery::for_paths(&hive.id, paths).and_then(|g| g.totals()) {
        Ok(t) => t,
        Err(e) => {
            log::warn!("hive {}: the gallery's size couldn't be read: {e}", hive.id);
            (0, 0)
        }
    }
}

/// The sheet's list (§11.2). Reads only: git runs in the human's repos, and in a worktree only on
/// the git dir maiTerm recorded, after its `.git` file checks out. Blocks.
pub(crate) fn preview(hive: &Hive, paths: &HivePaths, has_coordinator: bool) -> HiveDeletePreview {
    let workers = worker::list_workers(paths);
    let mut p = HiveDeletePreview::default();
    for w in &workers {
        for f in w.git_folders() {
            let (Some(b), Some(repo)) = (f.branch.as_deref(), repo_of(hive, &f.name)) else { continue };
            let base = match (&f.target, &f.base_sha) {
                (Some(t), _) => format!("refs/heads/{t}"),
                (None, Some(sha)) => sha.clone(),
                (None, None) => continue,
            };
            let count = run(super::git::verify_in(&repo).args(["rev-list", "--count"]).arg(format!("{base}..refs/heads/{b}")), QUERY_TIMEOUT);
            if let Some(ahead) = count.ok().and_then(|c| c.trim().parse::<u64>().ok()).filter(|n| *n > 0) {
                p.unlanded.push(UnlandedBranch { worker: worker_label(w), folder: f.name.clone(), branch: b.to_string(), target: f.target.clone(), ahead });
            }
            if f.worktree.as_deref().is_some_and(|wt| fs::symlink_metadata(wt).is_ok()) {
                p.dirty.extend(keep_reason(f));
            }
        }
        p.scratch.extend(scratch_folder(paths, w));
        if w.is_live() {
            p.live_agents.push(worker_label(w));
        }
        if super::land::in_flight(w) {
            p.lands_in_flight.push(worker_label(w));
        }
    }
    for u in super::land::undo::in_flight(paths) {
        p.lands_in_flight.push(format!("the undo of {}", u.undoes));
    }
    for f in hive.git_folders() {
        let wt = paths.coordinator_worktree(&f.name);
        if fs::symlink_metadata(&wt).is_err() {
            continue;
        }
        match found_folder(&f.name, Path::new(&f.path), &wt) {
            Some(rc) => p.dirty.extend(keep_reason(&rc).map(|why| format!("coordinator's copy of {why}"))),
            None => p.dirty.push(format!("coordinator's copy of {}: {} (not a worktree maiTerm can vouch for)", f.name, wt.display())),
        }
    }
    p.dirty.extend(unaccounted(hive, paths, &workers).into_iter().map(|(why, _)| why));
    if has_coordinator {
        p.live_agents.insert(0, "the coordinator".to_string());
    }
    (p.gallery_files, p.gallery_bytes) = gallery_totals(hive, paths);
    p.branches = hive_branches(hive, &workers);
    p
}

/// What the delete did on disk.
#[derive(Debug, Default)]
pub(crate) struct Disk {
    pub removed: usize,
    pub kept: Vec<String>,
    pub trust_removed: usize,
    pub folder_left: bool,
}

/// The Hive's disk, gone but for what holds work (§11.2). Blocks.
pub(crate) fn remove_disk(hive: &Hive, paths: &HivePaths, workers: &[WorkerRecord], delete_gallery: bool) -> Disk {
    let mut out = Disk::default();
    let mut keep: Vec<PathBuf> = Vec::new();
    let mut kept = |why: String, path: PathBuf, out: &mut Disk| {
        out.kept.push(why);
        keep.push(path);
    };

    for w in workers {
        for f in w.git_folders() {
            let wt = PathBuf::from(f.worktree.as_deref().unwrap_or_default());
            let Some(repo) = repo_of(hive, &f.name) else {
                kept(format!("{}: {} (this Hive has no such folder now)", f.name, wt.display()), wt, &mut out);
                continue;
            };
            match remove_if_clean(&repo, f) {
                Removal::Removed => out.removed += 1,
                Removal::Gone => {}
                Removal::Kept(why) => kept(why, wt, &mut out),
            }
        }
        if let Some(s) = scratch_folder(paths, w) {
            let n = if s.reported > 0 { format!("{} files, {} of them reported", s.files, s.reported) } else { format!("{} files", s.files) };
            kept(format!("scratch of {}: {} ({n})", s.worker, s.path), PathBuf::from(&s.path), &mut out);
        }
    }

    // The coordinator's read copies: maiTerm's own, removed by the same rule, its branch (in the
    // approval mode, `hive/<slug>`) kept.
    for f in hive.git_folders() {
        let wt = paths.coordinator_worktree(&f.name);
        if fs::symlink_metadata(&wt).is_err() {
            continue;
        }
        match found_folder(&f.name, Path::new(&f.path), &wt) {
            Some(rc) => match remove_if_clean(Path::new(&f.path), &rc) {
                Removal::Removed => out.removed += 1,
                Removal::Gone => {}
                Removal::Kept(why) => kept(format!("coordinator's copy of {why}"), wt, &mut out),
            },
            None => kept(format!("coordinator's copy of {}: {} (not a worktree maiTerm can vouch for)", f.name, wt.display()), wt, &mut out),
        }
    }

    // Temporary land worktrees: maiTerm's scratch, so forced, once their `.git` checks out.
    let land = paths.worktrees().join(LAND_SLUG);
    for l in fs::read_dir(&land).into_iter().flatten().flatten() {
        for e in fs::read_dir(l.path()).into_iter().flatten().flatten() {
            let wt = e.path();
            let name = e.file_name().to_string_lossy().to_string();
            let Some(repo) = repo_of(hive, &name) else {
                kept(format!("land worktree {} (this Hive has no such folder now)", wt.display()), wt, &mut out);
                continue;
            };
            let vouched = found_folder(&name, &repo, &wt).filter(|f| f.admin_dir.as_deref().is_some_and(|a| git_file_names(&wt, Path::new(a))));
            let res = match vouched {
                Some(_) => run(git_in(&repo).args(["worktree", "remove", "--force"]).arg(&wt), WORKTREE_TIMEOUT).map(|_| ()),
                None => Err("not a worktree maiTerm can vouch for".to_string()),
            };
            match res {
                Ok(()) => out.removed += 1,
                Err(e) => kept(format!("land worktree {}: {e}", wt.display()), wt, &mut out),
            }
        }
    }

    // Only what was vouched for and removed leaves `wt.noindex/`; the rest stays whole.
    for (why, p) in unaccounted(hive, paths, workers) {
        kept(why, p, &mut out);
    }

    out.trust_removed = super::trust::remove_all_trust(paths);
    // A key that couldn't be undone keeps its record, so it can be undone later.
    let left = super::trust::written_entries(paths);
    for (key, file) in &left {
        out.kept.push(format!("trust for {key} in {file} (it couldn't be undone; its record stays at {})", paths.trust_record().display()));
    }
    if !left.is_empty() {
        keep.push(paths.trust_record());
    }
    if !delete_gallery && fs::symlink_metadata(paths.gallery()).is_ok() {
        keep.push(paths.gallery());
    }
    for e in remove_except(&paths.root, &keep) {
        out.kept.push(e);
    }
    let _ = fs::remove_dir(&paths.root);
    out.folder_left = fs::symlink_metadata(&paths.root).is_ok();
    out
}

/// Everything under `dir` but the `keep` paths and the folders on the way to them, links removed
/// as links. Returns what couldn't be removed.
fn remove_except(dir: &Path, keep: &[PathBuf]) -> Vec<String> {
    let mut errors = Vec::new();
    for e in fs::read_dir(dir).into_iter().flatten().flatten() {
        let p = e.path();
        if keep.contains(&p) {
            continue;
        }
        let is_dir = fs::symlink_metadata(&p).is_ok_and(|m| m.is_dir());
        if is_dir && keep.iter().any(|k| k.starts_with(&p)) {
            errors.extend(remove_except(&p, keep));
            let _ = fs::remove_dir(&p);
            continue;
        }
        if let Err(err) = remove_path(&p) {
            errors.push(format!("{}: {err}", p.display()));
        }
    }
    errors
}

/// One pane's active tab after the Hive's tabs went: as `heal_pane_active_tab` decides.
fn heal(pane: &mut crate::state::Pane) -> PaneActive {
    let healthy = pane.active_tab_id.as_ref().and_then(|id| pane.tabs.iter().find(|t| &t.id == id)).is_some_and(|t| !t.is_strip_hidden());
    if !healthy {
        pane.active_tab_id = pane.tabs.iter().find(|t| !t.is_strip_hidden()).map(|t| t.id.clone());
    }
    PaneActive { pane_id: pane.id.clone(), tab_id: pane.active_tab_id.clone() }
}

/// Delete the Hive (§11.2): every agent stopped, the Hive's tabs (its job tabs too) dropped and its record cleared
/// (rows kept, their tabs cleared), its queue released, its open requests failed, then its disk.
pub(crate) async fn delete_hive<W: HiveWindow>(ctx: &Ctx<'_, W>, caller: &Caller, delete_gallery: bool) -> Result<HiveDeleteResult, String> {
    let paths = caller.paths.clone();
    let (workers, undos) = blocking(move || Ok((worker::list_workers(&paths), super::land::undo::in_flight(&paths)))).await?;
    if let Some(w) = workers.iter().find(|w| super::land::in_flight(w)) {
        return Err(format!("Worker {} is landing: delete the Hive once its land has finished or been held.", worker_label(w)));
    }
    if let Some(u) = undos.first() {
        return Err(format!("The undo of {} is running: delete the Hive once it has finished or been held.", u.undoes));
    }
    super::queue::park_workspace(&caller.workspace_id, "your Hive was deleted");

    // Stopped before the signal, so no exit reads as a crash.
    for w in workers.iter().filter(|w| w.is_live()) {
        let Ok(dir) = worker::task_dir(&caller.paths, &w.task_id) else { continue };
        let res = blocking(move || {
            worker::update_worker(&dir, |cur| {
                let live = cur.is_live();
                if live {
                    cur.phase = worker::PHASE_STOPPED.to_string();
                }
                live
            })
        })
        .await;
        if let Err(e) = res {
            log::warn!("hive {}: worker {} not marked stopped before the delete: {e}", caller.hive.id, worker_label(w));
        }
    }
    // Its run-queue job tabs go with it: hidden, bound to job ids nothing reads once the Hive is
    // gone, their shells in worktrees about to go. The queue forgets the finished ones it held.
    for w in &workers {
        super::queue::take_old_tabs(&w.task_id);
    }
    let (tabs, job_tabs): (Vec<String>, Vec<String>) = {
        let d = ctx.data.read();
        let all: Vec<&crate::state::Tab> = d
            .window(&caller.label)
            .and_then(|win| win.workspaces.iter().find(|ws| ws.id == caller.workspace_id))
            .map(|ws| ws.panes.iter().flat_map(|p| p.tabs.iter()).collect())
            .unwrap_or_default();
        let (hive, rest): (Vec<&crate::state::Tab>, Vec<&crate::state::Tab>) = all.into_iter().partition(|t| t.hive_role.is_some());
        (
            hive.iter().map(|t| t.id.clone()).collect(),
            rest.iter().filter(|t| t.service_id.as_deref().is_some_and(super::queue::is_job_id)).map(|t| t.id.clone()).collect(),
        )
    };
    for t in &tabs {
        if ctx.window.front(t).await != Front::Job {
            continue;
        }
        match ctx.window.stop_agent(t).await {
            Ok(true) => super::retire::wait_for_exit(ctx, t).await,
            Ok(false) => {}
            Err(e) => log::info!("hive {}: tab {t} at delete: {e}; its tab goes anyway", caller.hive.id),
        }
    }

    let active_tabs = {
        let mut d = ctx.data.write();
        let ws = super::tools::workspace_mut(&mut d, caller).map_err(|_| "This Hive is gone already.".to_string())?;
        for t in tabs.iter().chain(&job_tabs) {
            super::tools::drop_tab(ws, Some(t));
        }
        for row in ws.tasks.iter_mut().filter(|r| r.tab_id.as_ref().is_some_and(|t| tabs.contains(t))) {
            row.tab_id = None;
        }
        ws.hive = None;
        ws.panes.iter_mut().map(heal).collect::<Vec<_>>()
    };
    let args = json!({ "workspace_id": caller.workspace_id, "tab_ids": tabs, "job_tab_ids": job_tabs });
    if let Err(e) = rpc::applied(ctx.window.request(rpc::VERB_DELETE, args).await) {
        log::warn!("hive {}: deleted, its window's half not done: {e}", caller.hive.id);
    }
    let snapshot = ctx.data.read().clone();
    if let Err(e) = (ctx.save)(&snapshot) {
        log::warn!("hive {}: deleted, the state not saved: {e}", caller.hive.id);
    }
    let requests_failed = ctx.window.fail_requests(&tabs, "its Hive was deleted");
    // No rebuild may write into the folder as it goes, and no open connection may hold the
    // gallery's index or its WAL files.
    super::memory::unwatch(&caller.hive.id);
    super::gallery::forget(&caller.hive.id);

    let (hive, paths, recs) = (caller.hive.clone(), caller.paths.clone(), workers.clone());
    let (disk, branches) = blocking(move || {
        let disk = remove_disk(&hive, &paths, &recs, delete_gallery);
        Ok((disk, hive_branches(&hive, &recs)))
    })
    .await?;
    super::queue::unpark_workspace(&caller.workspace_id);
    for w in &workers {
        super::diffstat::forget(&caller.hive.id, &w.task_id);
    }
    for t in &tabs {
        super::feed::forget_meta(t);
    }
    log::info!(
        "hive {}: deleted from workspace {}: {} worktrees removed, kept {:?}, branches kept {:?}",
        caller.hive.id,
        caller.workspace_id,
        disk.removed,
        disk.kept,
        branches
    );
    Ok(HiveDeleteResult {
        worktrees_removed: disk.removed,
        kept: disk.kept,
        branches,
        gallery_kept: !delete_gallery && fs::symlink_metadata(caller.paths.gallery()).is_ok(),
        trust_removed: disk.trust_removed,
        requests_failed,
        folder_left: disk.folder_left.then(|| caller.paths.root.to_string_lossy().to_string()),
        dropped_tabs: tabs.into_iter().chain(job_tabs).collect(),
        active_tabs,
    })
}

fn caller_in(state: &AppState, label: &str, workspace_id: &str) -> Result<Caller, String> {
    super::tools::caller_for_workspace(&state.app_data.read(), workspace_id)
        .filter(|c| c.label == label)
        .ok_or_else(|| "That workspace isn't a Hive in this window.".to_string())
}

/// What deleting the Hive would keep and stop (§11.2), before anything is touched.
#[tauri::command]
pub async fn hive_delete_preview(
    window: tauri::Window,
    state: tauri::State<'_, Arc<AppState>>,
    workspace_id: String,
) -> Result<HiveDeletePreview, String> {
    let caller = caller_in(&state, window.label(), &workspace_id)?;
    let has_coordinator = !caller.tab_id.is_empty();
    blocking(move || Ok(preview(&caller.hive, &caller.paths, has_coordinator))).await
}

/// Delete the Hive (§11.2): the human's click on the delete sheet, the only way in.
#[tauri::command]
pub async fn hive_delete(
    window: tauri::Window,
    app: tauri::AppHandle,
    state: tauri::State<'_, Arc<AppState>>,
    workspace_id: String,
    delete_gallery: bool,
) -> Result<HiveDeleteResult, String> {
    let caller = caller_in(&state, window.label(), &workspace_id)?;
    let ctx = super::tools::app_ctx(state.inner(), &app, &caller);
    delete_hive(&ctx, &caller, delete_gallery).await
}
