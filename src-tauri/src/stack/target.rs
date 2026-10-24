//! Which checkout a copy runs on, and the folder that is there (docs/hive.md §6.9).
//!
//! A target is a String: `""` (the service's own folder), `"checkout:<abs>"` (an existing
//! worktree of the same repo, a Hive worker's say) or `"branch:<name>"` (that branch's checkout:
//! one that exists, else a detached worktree maiTerm keeps under `<app data>/stack-wt.noindex/`).
//! The service's cwd is taken relative to its repo's top level and re-rooted there.
//!
//! Every git call is maiTerm's hardened one (`hive::git::git_in`: no hooks, no fsmonitor), and
//! never follows a worktree's `.git` file to find its repo: a worker's worktree is run on the git
//! dir its `worker.json` recorded (`git_in_worktree`), any other on the admin dir found from the
//! repo's own side. A worktree under a Hive's folder that isn't one of this workspace's workers'
//! is refused outright. Everything here blocks: callers run it under `spawn_blocking`.

use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use super::placeholders::valid_branch;
use crate::hive::git::{git_in, git_in_worktree, QUERY_TIMEOUT, WORKTREE_TIMEOUT};
use crate::hive::worker::{WorkerFolder, WorkerRecord};
use crate::share::canon;

pub const CHECKOUT: &str = "checkout:";
pub const BRANCH: &str = "branch:";
/// How many branches "Run on ▸ Other branch…" offers, newest commit first.
const BRANCH_LIMIT: usize = 50;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Target {
    Own,
    Checkout(PathBuf),
    Branch(String),
}

/// A stored target, checked without touching git. An unknown kind is refused here, when used;
/// it still loads, since the field is a String.
pub fn parse(s: &str) -> Result<Target, String> {
    if s.is_empty() {
        return Ok(Target::Own);
    }
    if let Some(p) = s.strip_prefix(CHECKOUT) {
        let path = PathBuf::from(p);
        if !path.is_absolute() || p.chars().any(char::is_control) {
            return Err(format!("{s:?} doesn't name an absolute folder"));
        }
        return Ok(Target::Checkout(path));
    }
    if let Some(b) = s.strip_prefix(BRANCH) {
        if !valid_branch(b) {
            return Err(format!("{b:?} can't be a branch to run on (allowed: letters, digits and . _ / + -)"));
        }
        return Ok(Target::Branch(b.to_string()));
    }
    Err(format!("{s:?} isn't a place this maiTerm can run a service (it knows checkout: and branch:)"))
}

/// The one door every git call in this module goes through: the shortcut test counts it.
fn git(cmd: &mut std::process::Command, timeout: Duration) -> Result<String, String> {
    #[cfg(test)]
    GIT_CALLS.with(|c| c.set(c.get() + 1));
    crate::hive::git::run(cmd, timeout)
}

#[cfg(test)]
thread_local! {
    pub static GIT_CALLS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// What a resolve may look at besides the service: this workspace's Hive workers (read from
/// their `worker.json`), every Hive's folder, and where maiTerm keeps its branch worktrees.
#[derive(Debug, Default, Clone)]
pub struct Context {
    pub workers: Vec<WorkerRecord>,
    /// `<app data>/hive`.
    pub hives: Option<PathBuf>,
    /// `<app data>/stack-wt.noindex`.
    pub stack_wt: Option<PathBuf>,
}

impl Context {
    /// Both roots are canonicalized when asked, not when built: either may not exist yet.
    fn under_hives(&self, p: &Path) -> bool {
        self.hives.as_deref().is_some_and(|h| p.starts_with(canon(h)))
    }

    fn under_stack_wt(&self, p: &Path) -> bool {
        self.stack_wt.as_deref().is_some_and(|s| p.starts_with(canon(s)))
    }

    /// The worker whose recorded worktree is exactly `root`, with that folder.
    fn worker_at(&self, root: &Path) -> Option<(&WorkerRecord, &WorkerFolder)> {
        self.workers.iter().find_map(|w| {
            w.git_folders().find(|f| f.worktree.as_deref().is_some_and(|wt| canon(Path::new(wt)) == root)).map(|f| (w, f))
        })
    }
}

/// `<app data>/stack-wt.noindex`: `.noindex` so Spotlight leaves the checkouts alone (§6.2).
pub fn stack_wt_root() -> Option<PathBuf> {
    crate::state::persistence::app_data_dir().map(|d| d.join("stack-wt.noindex"))
}

/// The repo a service's folder is in.
#[derive(Debug, Clone)]
pub struct Repo {
    /// Its top level and common git dir, canonical.
    pub top: PathBuf,
    pub common: PathBuf,
    /// The service's cwd under `top`; empty at the top itself.
    pub rel: PathBuf,
}

pub fn repo_of(cwd: &Path, ctx: &Context) -> Result<Repo, String> {
    let at = canon(cwd);
    if !at.is_dir() {
        return Err(format!("{} doesn't exist", cwd.display()));
    }
    // A folder under a Hive is a worker's or a read copy, whose `.git` the worker may have
    // written: git is never run there by path.
    if ctx.under_hives(&at) {
        return Err(format!("{} is inside a Hive's folder, so it can't run on another checkout", cwd.display()));
    }
    let out = git(git_in(&at).args(["rev-parse", "--path-format=absolute", "--show-toplevel", "--git-common-dir"]), QUERY_TIMEOUT)
        .map_err(|_| format!("{} isn't in a git repository, so the service can only run in its own folder", cwd.display()))?;
    let mut lines = out.lines();
    let (Some(top), Some(common)) = (lines.next(), lines.next()) else {
        return Err(format!("git couldn't say which repository {} is in", cwd.display()));
    };
    let top = canon(Path::new(top.trim()));
    let common = canon(Path::new(common.trim()));
    let rel = at.strip_prefix(&top).map(Path::to_path_buf).map_err(|_| format!("{} isn't under {}", at.display(), top.display()))?;
    Ok(Repo { top, common, rel })
}

/// The branch the service's own folder is on; None when detached.
pub fn own_branch(repo: &Repo) -> Option<String> {
    git(git_in(&repo.top).args(["symbolic-ref", "--short", "-q", "HEAD"]), QUERY_TIMEOUT).ok().filter(|b| !b.is_empty())
}

/// How maiTerm runs git in a place.
#[derive(Debug, Clone)]
enum PlaceGit {
    /// On the git dir the worker's record holds.
    Worker(WorkerFolder),
    /// On an admin dir found from the repo's side, never from the worktree's `.git` file.
    Dirs(PathBuf),
}

/// A checkout a copy runs in.
#[derive(Debug, Clone)]
pub struct Place {
    pub root: PathBuf,
    git: PlaceGit,
    pub branch: Option<String>,
    /// The worker's slug, when it is a worker's worktree.
    pub worker: Option<String>,
}

impl Place {
    fn git(&self) -> Result<std::process::Command, String> {
        match &self.git {
            PlaceGit::Worker(f) => git_in_worktree(f),
            PlaceGit::Dirs(admin) => {
                let mut cmd = git_in(&self.root);
                cmd.arg(format!("--git-dir={}", admin.display())).arg(format!("--work-tree={}", self.root.display()));
                Ok(cmd)
            }
        }
    }

    pub fn head_short(&self) -> Result<String, String> {
        git(self.git()?.args(["rev-parse", "--short", "HEAD"]), QUERY_TIMEOUT)
    }

    fn current_branch(&self) -> Option<String> {
        git(self.git().ok()?.args(["symbolic-ref", "--short", "-q", "HEAD"]), QUERY_TIMEOUT).ok().filter(|b| !b.is_empty())
    }

    /// What a row calls it: the worker, the branch, else the folder.
    pub fn label(&self) -> String {
        self.worker.clone().or_else(|| self.branch.clone()).unwrap_or_else(|| folder_name(&self.root))
    }
}

fn folder_name(p: &Path) -> String {
    p.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default()
}

/// The admin dir of the worktree at `root`, from the repo's side: the common dir itself for the
/// main worktree, else the `worktrees/<id>` entry whose `gitdir` names `root/.git`.
fn admin_dir_for(common: &Path, root: &Path) -> Option<PathBuf> {
    let dot_git = root.join(".git");
    if dot_git.is_dir() && canon(&dot_git) == common {
        return Some(common.to_path_buf());
    }
    for e in fs::read_dir(common.join("worktrees")).ok()?.flatten() {
        let admin = e.path();
        if !admin.join("commondir").is_file() {
            continue;
        }
        let Ok(gitdir) = fs::read_to_string(admin.join("gitdir")) else { continue };
        let named = PathBuf::from(gitdir.trim());
        let named = if named.is_absolute() { named } else { admin.join(named) };
        if named.parent().is_some_and(|p| canon(p) == root) {
            return Some(canon(&admin));
        }
    }
    None
}

/// `checkout:<path>`: an existing worktree of the service's repo.
pub fn checkout_place(repo: &Repo, path: &Path, ctx: &Context) -> Result<Place, String> {
    let root = canon(path);
    if !root.is_dir() {
        return Err(format!("{} doesn't exist", path.display()));
    }
    if let Some((w, f)) = ctx.worker_at(&root) {
        if f.common_dir.as_deref().map(|c| canon(Path::new(c))) != Some(repo.common.clone()) {
            return Err(format!("{}'s worktree at {} is of another repository", w.slug, root.display()));
        }
        return Ok(Place { root, git: PlaceGit::Worker(f.clone()), branch: f.branch.clone(), worker: Some(w.slug.clone()) });
    }
    if ctx.under_hives(&root) {
        return Err(format!("{} belongs to a Hive and isn't a worktree of one of this workspace's workers", root.display()));
    }
    if ctx.under_stack_wt(&root) {
        return Err(format!("{} is maiTerm's own checkout for branch copies: run on the branch instead", root.display()));
    }
    let admin = admin_dir_for(&repo.common, &root).ok_or_else(|| format!("{} isn't a worktree of {}", root.display(), repo.top.display()))?;
    let mut place = Place { root, git: PlaceGit::Dirs(admin), branch: None, worker: None };
    place.branch = place.current_branch();
    Ok(place)
}

#[derive(Debug, Default)]
struct WorktreeEntry {
    path: PathBuf,
    branch: Option<String>,
    bare: bool,
}

/// `git worktree list` from the repo's own side, NUL-separated (a path can hold a newline).
fn worktrees(repo: &Repo) -> Result<Vec<WorktreeEntry>, String> {
    let out = git(git_in(&repo.top).args(["worktree", "list", "--porcelain", "-z"]), QUERY_TIMEOUT)?;
    let mut list = Vec::new();
    let mut cur: Option<WorktreeEntry> = None;
    for field in out.split('\0') {
        if field.is_empty() {
            list.extend(cur.take());
        } else if let Some(p) = field.strip_prefix("worktree ") {
            list.extend(cur.take());
            cur = Some(WorktreeEntry { path: PathBuf::from(p), ..WorktreeEntry::default() });
        } else if let Some(entry) = cur.as_mut() {
            if let Some(b) = field.strip_prefix("branch refs/heads/") {
                entry.branch = Some(b.to_string());
            } else if field == "bare" {
                entry.bare = true;
            }
        }
    }
    list.extend(cur);
    Ok(list.into_iter().filter(|e| !e.bare).collect())
}

fn sha_hex(s: &str, n: usize) -> String {
    Sha256::digest(s.as_bytes()).iter().map(|b| format!("{b:02x}")).collect::<String>()[..n].to_string()
}

/// `<sha256(common dir)[..12]>`: one folder per repo.
fn repo_dir(common: &Path) -> String {
    sha_hex(&common.to_string_lossy(), 12)
}

/// The branch's slug plus a hash of its exact name, so `a/b` and `a-b` never share a checkout.
fn branch_dir(branch: &str) -> String {
    let slug = crate::hive::create::slugify(branch);
    format!("{}-{}", if slug.is_empty() { "branch" } else { &slug }, sha_hex(branch, 6))
}

/// Beside each of maiTerm's branch worktrees: what it is for, so a prune knows.
#[derive(Debug, Serialize, Deserialize)]
struct Sidecar {
    branch: String,
    common_dir: String,
}

/// One lock per maiTerm checkout folder, for as long as the process runs (a handful of paths).
fn checkout_lock(dir: &Path) -> Arc<Mutex<()>> {
    static LOCKS: Mutex<Option<HashMap<PathBuf, Arc<Mutex<()>>>>> = Mutex::new(None);
    let mut locks = LOCKS.lock().unwrap_or_else(|e| e.into_inner());
    locks.get_or_insert_with(HashMap::new).entry(dir.to_path_buf()).or_default().clone()
}

/// `branch:<name>`: the checkout that has it, else maiTerm's own detached one, brought to the
/// branch's tip (refused while it has tracked changes). Returns the place and whether it is
/// maiTerm's own.
pub fn branch_place(repo: &Repo, branch: &str, ctx: &Context) -> Result<(Place, bool), String> {
    // git allows `$`, `(` and backticks in a ref name: the charset is what keeps them out.
    if !valid_branch(branch) {
        return Err(format!("{branch:?} can't be a branch to run on (allowed: letters, digits and . _ / + -)"));
    }
    git(git_in(&repo.top).args(["check-ref-format", "--branch", branch]), QUERY_TIMEOUT)
        .map_err(|_| format!("{branch:?} isn't a branch name git accepts"))?;
    let tip = git(git_in(&repo.top).args(["rev-parse", "--verify", "--quiet"]).arg(format!("refs/heads/{branch}^{{commit}}")), QUERY_TIMEOUT)
        .map_err(|_| format!("there is no branch {branch} in {}", repo.top.display()))?;

    for wt in worktrees(repo)? {
        if wt.branch.as_deref() != Some(branch) {
            continue;
        }
        let root = canon(&wt.path);
        if let Some((w, f)) = ctx.worker_at(&root) {
            // A worker can rewrite its own HEAD to claim any branch: its worktree counts only for
            // the branch maiTerm recorded for it.
            let same_repo = f.common_dir.as_deref().map(|c| canon(Path::new(c))) == Some(repo.common.clone());
            if same_repo && f.branch.as_deref() == Some(branch) {
                return Ok((Place { root, git: PlaceGit::Worker(f.clone()), branch: Some(branch.to_string()), worker: Some(w.slug.clone()) }, false));
            }
            continue;
        }
        if ctx.under_hives(&root) || ctx.under_stack_wt(&root) {
            continue;
        }
        if let Some(admin) = admin_dir_for(&repo.common, &root) {
            return Ok((Place { root, git: PlaceGit::Dirs(admin), branch: Some(branch.to_string()), worker: None }, false));
        }
    }

    let base = ctx.stack_wt.as_deref().ok_or("maiTerm has no data folder to keep a checkout in")?;
    let dir = base.join(repo_dir(&repo.common)).join(branch_dir(branch));
    // Every service of the repo shares this folder, and two may start at once (an agent's
    // parallel startService, two windows restoring): the add, the dirty check and the switch run
    // one at a time per folder.
    let lock = checkout_lock(&dir);
    let _held = lock.lock().unwrap_or_else(|e| e.into_inner());
    let parent = dir.parent().ok_or("no parent folder")?;
    fs::create_dir_all(parent).map_err(|e| format!("{}: {e}", parent.display()))?;
    // `create_dir` is the claim: only the call that made the folder may add into it, and only
    // that call may take it back. Losing the claim means the folder is there to reuse.
    if fs::create_dir(&dir).is_ok() {
        let added = git(git_in(&repo.top).args(["worktree", "add", "--detach", "-q"]).arg(&dir).arg(&tip), WORKTREE_TIMEOUT);
        if let Err(e) = added {
            crate::hive::git::clean_failed_add(&repo.top, &dir, None);
            return Err(format!("couldn't make a checkout of {branch}: {e}"));
        }
        let side = Sidecar { branch: branch.to_string(), common_dir: repo.common.to_string_lossy().to_string() };
        let json = serde_json::to_vec_pretty(&side).map_err(|e| e.to_string())?;
        if let Err(e) = fs::write(dir.with_extension("json"), json) {
            log::warn!("stack: no record beside {}, so it won't be pruned: {e}", dir.display());
        }
        log::info!("stack: made a checkout of {branch} at {}", dir.display());
    }
    let root = canon(&dir);
    let admin = admin_dir_for(&repo.common, &root).ok_or_else(|| format!("{} isn't a worktree of {}", root.display(), repo.top.display()))?;
    let place = Place { root, git: PlaceGit::Dirs(admin), branch: Some(branch.to_string()), worker: None };
    let dirty = git(place.git()?.args(["status", "--porcelain", "--untracked-files=no"]), QUERY_TIMEOUT)?;
    if !dirty.is_empty() {
        return Err(format!(
            "maiTerm's checkout of {branch} at {} has changes to tracked files: commit or discard them there first",
            place.root.display()
        ));
    }
    // `switch` never reads its argument as a path, and the full sha can't name another ref.
    git(place.git()?.args(["switch", "--detach", "-q"]).arg(&tip), WORKTREE_TIMEOUT)?;
    Ok((place, true))
}

/// Remove maiTerm's branch checkouts whose branch is gone, that are clean, and that nothing in
/// `keep` (every tab's target and every service's running target) still names. Returns how
/// many went.
pub fn prune(base: &Path, keep: &HashSet<String>) -> usize {
    let mut removed = 0;
    let Ok(repos) = fs::read_dir(base) else { return 0 };
    for repo in repos.flatten().filter(|e| e.file_type().is_ok_and(|t| t.is_dir())) {
        let Ok(entries) = fs::read_dir(repo.path()) else { continue };
        for e in entries.flatten() {
            let side_path = e.path();
            if side_path.extension().and_then(|x| x.to_str()) != Some("json") {
                continue;
            }
            let Some(side) = fs::read(&side_path).ok().and_then(|b| serde_json::from_slice::<Sidecar>(&b).ok()) else { continue };
            if !valid_branch(&side.branch) || keep.contains(&format!("{BRANCH}{}", side.branch)) {
                continue;
            }
            let common = PathBuf::from(&side.common_dir);
            if !common.is_dir() {
                log::debug!("stack: {} outlived its repository; left in place", side_path.display());
                continue;
            }
            let on_common = || {
                let mut cmd = git_in(&common);
                cmd.arg(format!("--git-dir={}", common.display()));
                cmd
            };
            let dir = side_path.with_extension("");
            let lock = checkout_lock(&dir);
            let _held = lock.lock().unwrap_or_else(|e| e.into_inner());
            if fs::symlink_metadata(&dir).is_err() {
                let _ = fs::remove_file(&side_path);
                continue;
            }
            let exists = git(on_common().args(["rev-parse", "--verify", "--quiet"]).arg(format!("refs/heads/{}", side.branch)), QUERY_TIMEOUT);
            if exists.is_ok() {
                continue;
            }
            let root = canon(&dir);
            let Some(admin) = admin_dir_for(&common, &root) else { continue };
            let place = Place { root, git: PlaceGit::Dirs(admin), branch: None, worker: None };
            // A file the human made there counts as a change: only ignored output (node_modules,
            // dist) may go with the folder. No `--force` either, so git refuses as a second guard.
            let clean = place.git().and_then(|mut c| git(c.args(["status", "--porcelain", "--untracked-files=all"]), QUERY_TIMEOUT));
            if !matches!(clean.as_deref(), Ok("")) {
                continue;
            }
            match git(on_common().args(["worktree", "remove"]).arg(&dir), WORKTREE_TIMEOUT) {
                Ok(_) => {
                    let _ = fs::remove_file(&side_path);
                    log::info!("stack: removed the checkout of deleted branch {} at {}", side.branch, dir.display());
                    removed += 1;
                }
                Err(e) => log::warn!("stack: checkout at {} left in place: {e}", dir.display()),
            }
        }
    }
    removed
}

/// One "Run on ▸" choice.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct CopyTarget {
    pub target: String,
    /// `"own"` | `"worktree"` | `"branch"`.
    pub kind: String,
    pub label: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub branch: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub worker: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
}

/// The own folder, the repo's other worktrees (workers' named from their records; read copies
/// and maiTerm's own checkouts left out), then up to 50 branches by newest commit.
pub fn copy_targets(repo: Option<&Repo>, ctx: &Context) -> Vec<CopyTarget> {
    let mut out = vec![CopyTarget { target: String::new(), kind: "own".into(), label: "main".into(), branch: repo.and_then(own_branch), worker: None, path: None }];
    let Some(repo) = repo else { return out };
    match worktrees(repo) {
        Ok(list) => {
            for wt in list {
                let root = canon(&wt.path);
                if root == repo.top || ctx.under_stack_wt(&root) {
                    continue;
                }
                let target = format!("{CHECKOUT}{}", root.display());
                if let Some((w, f)) = ctx.worker_at(&root) {
                    if f.common_dir.as_deref().map(|c| canon(Path::new(c))) == Some(repo.common.clone()) {
                        out.push(CopyTarget { target, kind: "worktree".into(), label: format!("{} (worker {})", w.slug, w.badge), branch: f.branch.clone(), worker: Some(w.slug.clone()), path: Some(root.display().to_string()) });
                    }
                    continue;
                }
                if ctx.under_hives(&root) || parse(&target).is_err() {
                    continue;
                }
                let label = wt.branch.clone().unwrap_or_else(|| folder_name(&root));
                out.push(CopyTarget { target, kind: "worktree".into(), label, branch: wt.branch, worker: None, path: Some(root.display().to_string()) });
            }
        }
        Err(e) => log::warn!("stack: worktrees of {}: {e}", repo.top.display()),
    }
    let refs = git(git_in(&repo.top).args(["for-each-ref", "--sort=-committerdate", "--format=%(refname)", "refs/heads"]), QUERY_TIMEOUT).unwrap_or_default();
    for b in refs.lines().filter_map(|r| r.strip_prefix("refs/heads/")).filter(|b| valid_branch(b)).take(BRANCH_LIMIT) {
        out.push(CopyTarget { target: format!("{BRANCH}{b}"), kind: "branch".into(), label: b.to_string(), branch: Some(b.to_string()), worker: None, path: None });
    }
    out
}

/// An agent's `on`: `"main"` is the own folder, a worker of this workspace's Hive (by badge or
/// name) its worktree of this repo, anything else a branch.
pub fn target_for(on: &str, repo: Option<&Repo>, ctx: &Context) -> Result<String, String> {
    let on = on.trim();
    if on.is_empty() || on == "main" {
        return Ok(String::new());
    }
    let badge = on.strip_prefix('#').unwrap_or(on).parse::<u32>().ok();
    let worker = ctx.workers.iter().find(|w| badge.is_some_and(|b| b == w.badge) || w.slug == on || w.title.eq_ignore_ascii_case(on));
    if let Some(w) = worker {
        let repo = repo.ok_or("this service's folder isn't in a git repository, so it runs only in its own folder")?;
        let f = w
            .git_folders()
            .find(|f| f.common_dir.as_deref().map(|c| canon(Path::new(c))) == Some(repo.common.clone()))
            .ok_or_else(|| format!("worker {} has no worktree of {}", w.slug, repo.top.display()))?;
        return Ok(format!("{CHECKOUT}{}", f.worktree.as_deref().unwrap_or_default()));
    }
    if !valid_branch(on) {
        return Err(format!("{on:?} isn't main, a worker of this Hive, or a branch name (letters, digits and . _ / + -)"));
    }
    Ok(format!("{BRANCH}{on}"))
}

/// Every target something still names: each tab's, and each service's running intent.
pub fn targeted(data: &crate::state::AppData) -> HashSet<String> {
    let mut keep = HashSet::new();
    for ws in data.windows.iter().flat_map(|w| w.workspaces.iter()) {
        keep.extend(ws.panes.iter().flat_map(|p| p.tabs.iter()).filter_map(|t| t.service_target.clone()));
        keep.extend(ws.stack.iter().flat_map(|s| s.running_targets.iter().cloned()));
    }
    keep
}
