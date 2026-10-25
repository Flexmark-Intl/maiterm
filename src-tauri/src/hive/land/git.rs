//! The land's git (docs/hive.md §8.1): step 1 brings the pinned commit up to date with Base in a
//! temporary worktree, step 2 moves Base to exactly the tested commit by fast-forward only, and
//! neither can touch anyone's uncommitted work. maiTerm's merge commits are unsigned and use the
//! repo's identity, or `maiTerm Hive` when it has none (plan D8).
//!
//! Every call goes through `git_in` (no hooks, no fsmonitor) as a verifying read
//! (`hive::git::verifying`: no commit-graph, replace refs or grafts a worker could plant decide an
//! ancestry or a merge base), in the C locale (its messages are read), with rerere, signing and
//! autostash off. Everything here blocks.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use crate::hive::git::{git_in, git_in_worktree, run, QUERY_TIMEOUT, WORKTREE_TIMEOUT};
use crate::hive::worker::WorkerFolder;

/// A merge or a fast-forward in a big repo checks out files.
pub const GIT_TIMEOUT: Duration = Duration::from_secs(120);
pub const PUSH_TIMEOUT: Duration = Duration::from_secs(60);
const FALLBACK_NAME: &str = "maiTerm Hive";
const FALLBACK_EMAIL: &str = "hive@maiterm.local";
/// A busy `index.lock` in Base's checkout gets this many retries, this far apart.
pub const INDEX_LOCK_RETRIES: u32 = 3;
pub const INDEX_LOCK_WAIT: Duration = Duration::from_secs(2);

fn harden(cmd: &mut Command) {
    crate::hive::git::verifying(cmd)
        .env("LC_ALL", "C")
        .env("GIT_TERMINAL_PROMPT", "0")
        .args(["-c", "rerere.enabled=false", "-c", "commit.gpgsign=false", "-c", "merge.autostash=false"])
        // A human's `merge.ff=only` or signature check would refuse step 1's merge commit.
        .args(["-c", "merge.ff=true", "-c", "merge.verifySignatures=false"]);
}

/// maiTerm's land git in `dir`.
pub fn lgit(dir: &Path) -> Command {
    let mut cmd = git_in(dir);
    harden(&mut cmd);
    cmd
}

/// maiTerm's land git in a worktree, on its recorded git dir (`git_in_worktree`).
pub fn lgit_wt(f: &WorkerFolder) -> Result<Command, String> {
    let mut cmd = git_in_worktree(f)?;
    harden(&mut cmd);
    Ok(cmd)
}

/// `-c user.name=… -c user.email=…` when the repo has no identity of its own (plan D8).
pub(crate) fn identity(repo: &Path) -> Vec<String> {
    let has = |key: &str| run(git_in(repo).args(["config", "--get", key]), QUERY_TIMEOUT).is_ok_and(|v| !v.trim().is_empty());
    let mut out = Vec::new();
    if !has("user.name") {
        out.extend(["-c".to_string(), format!("user.name={FALLBACK_NAME}")]);
    }
    if !has("user.email") {
        out.extend(["-c".to_string(), format!("user.email={FALLBACK_EMAIL}")]);
    }
    out
}

pub fn rev_parse(repo: &Path, rev: &str) -> Option<String> {
    run(lgit(repo).args(["rev-parse", "--verify", "--quiet"]).arg(format!("{rev}^{{commit}}")), QUERY_TIMEOUT).ok().filter(|s| !s.is_empty())
}

pub fn branch_tip(repo: &Path, branch: &str) -> Option<String> {
    rev_parse(repo, &format!("refs/heads/{branch}"))
}

pub fn tree_of(repo: &Path, rev: &str) -> Option<String> {
    run(lgit(repo).args(["rev-parse", "--verify", "--quiet"]).arg(format!("{rev}^{{tree}}")), QUERY_TIMEOUT).ok().filter(|s| !s.is_empty())
}

/// Whether `a` is an ancestor of (or equal to) `b`.
pub fn is_ancestor(repo: &Path, a: &str, b: &str) -> bool {
    let out = crate::util::proc::output_timeout(lgit(repo).args(["merge-base", "--is-ancestor", a, b]), QUERY_TIMEOUT);
    out.is_ok_and(|o| o.status.success())
}

/// The best common ancestor of two commits.
pub fn merge_base_of(repo: &Path, a: &str, b: &str) -> Option<String> {
    run(lgit(repo).args(["merge-base", a, b]), QUERY_TIMEOUT).ok().filter(|s| !s.is_empty())
}

/// `git diff --shortstat <target>...<pinned>`: what the worker's commits change, for a card
/// ("3 files changed, 40 insertions(+), 2 deletions(-)").
pub fn shortstat(repo: &Path, target: &str, pinned: &str) -> Result<String, String> {
    run(lgit(repo).args(["diff", "--shortstat", "--no-ext-diff", "--no-textconv"]).arg(format!("{target}...{pinned}")).arg("--"), GIT_TIMEOUT)
}

/// The repo's git common dir, canonical: where its land lock lives.
pub fn common_dir(repo: &Path) -> Result<PathBuf, String> {
    let d = run(lgit(repo).args(["rev-parse", "--path-format=absolute", "--git-common-dir"]), QUERY_TIMEOUT)?;
    Ok(crate::share::canon(Path::new(d.trim())))
}

/// A temporary worktree, made by this land: its folder and the admin dir git gave it.
#[derive(Debug, Clone, PartialEq)]
pub struct Temp {
    pub path: PathBuf,
    pub admin: PathBuf,
}

impl Temp {
    pub fn folder(&self, name: &str) -> WorkerFolder {
        WorkerFolder {
            name: name.to_string(),
            kind: crate::hive::record::KIND_GIT.to_string(),
            worktree: Some(self.path.to_string_lossy().to_string()),
            admin_dir: Some(self.admin.to_string_lossy().to_string()),
            ..WorkerFolder::default()
        }
    }
}

/// `git worktree add --detach <path> <sha>`, the `.worktreeinclude` files copied in (§6.4). A
/// leftover at `path` from an earlier attempt of this land is removed first.
pub fn add_temp(repo: &Path, path: &Path, sha: &str) -> Result<Temp, String> {
    remove_temp(repo, path);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|e| format!("{}: {e}", parent.display()))?;
    }
    if let Err(e) = run(lgit(repo).args(["worktree", "add", "--detach", "--quiet"]).arg(path).arg(sha), WORKTREE_TIMEOUT) {
        crate::hive::git::clean_failed_add(repo, path, None);
        return Err(format!("the temporary worktree couldn't be made: {e}"));
    }
    let path = crate::share::canon(path);
    let Some(f) = crate::hive::retire::found_folder("land", repo, &path) else {
        crate::hive::git::clean_failed_add(repo, &path, None);
        return Err("git made the temporary worktree but maiTerm couldn't find its git folder".to_string());
    };
    if let Err(e) = crate::hive::worktree::copy_worktree_include(repo, &path) {
        log::warn!("hive land: .worktreeinclude not copied into {}: {e}", path.display());
    }
    Ok(Temp { path, admin: PathBuf::from(f.admin_dir.unwrap_or_default()) })
}

/// A worktree's ignored entries can be big (`node_modules`): the copy gets longer than git's.
const SEED_TIMEOUT: Duration = Duration::from_secs(300);

/// What the worker's setup and work left in its worktree, cloned into a land's temporary worktree
/// for its check: each top-level ignored entry git lists there (`node_modules/`, `.venv/`, `.env`).
/// The check runs under the worker's profile, whose network is closed (§13.1), so the folder's
/// setup can't run in it; the worker's own installed state, made under its sandbox and run here
/// under the same one, stands in. Nothing is written through a link: an entry whose place in the
/// temporary worktree is taken, or lies under anything but a real folder, is skipped, and links
/// inside an entry are copied as links. Answers what was copied; anything else is logged.
pub fn seed_ignored(f: &WorkerFolder, tmp: &Path) -> Vec<String> {
    let Some(wt) = f.worktree.as_deref().map(Path::new).filter(|p| p.is_dir()) else { return Vec::new() };
    let listed = lgit_wt(f).and_then(|mut c| {
        let out = crate::util::proc::output_timeout(c.args(["ls-files", "-z", "--others", "--ignored", "--exclude-standard", "--directory"]), QUERY_TIMEOUT)
            .map_err(|e| e.to_string())?;
        if out.status.success() { Ok(out.stdout) } else { Err(String::from_utf8_lossy(&out.stderr).trim().to_string()) }
    });
    let listed = match listed {
        Ok(l) => l,
        Err(e) => {
            log::info!("hive land: {}'s ignored files not listed, so none seeded: {e}", f.name);
            return Vec::new();
        }
    };
    let mut seeded = Vec::new();
    for rel in listed.split(|b| *b == 0).filter(|p| !p.is_empty()) {
        let rel = String::from_utf8_lossy(rel).trim_end_matches('/').to_string();
        let parts: Vec<&str> = rel.split('/').collect();
        if rel.is_empty() || parts.iter().any(|p| p.is_empty() || *p == "." || *p == ".." || p.eq_ignore_ascii_case(".git")) {
            continue;
        }
        let dest = tmp.join(&rel);
        let real_parents = (1..parts.len()).all(|k| fs::symlink_metadata(tmp.join(parts[..k].join("/"))).is_ok_and(|m| m.is_dir()));
        if !real_parents || fs::symlink_metadata(&dest).is_ok() {
            continue;
        }
        let src = wt.join(&rel);
        let copy = |clone: bool| {
            let mut cmd = Command::new("/bin/cp");
            cmd.arg(if clone { "-cRPp" } else { "-RPp" }).arg(&src).arg(&dest);
            crate::util::proc::output_timeout(&mut cmd, SEED_TIMEOUT).map_err(|e| e.to_string()).and_then(|o| {
                if o.status.success() { Ok(()) } else { Err(String::from_utf8_lossy(&o.stderr).trim().to_string()) }
            })
        };
        // APFS clones on macOS, so a big folder costs little; a plain copy where that fails.
        let mut res = if cfg!(target_os = "macos") { copy(true) } else { Err(String::new()) };
        if res.is_err() {
            let _ = crate::hive::retire::remove_path(&dest);
            res = copy(false);
        }
        match res {
            Ok(()) => seeded.push(rel),
            Err(e) => {
                log::info!("hive land: {rel} not seeded into {}: {e}", tmp.display());
                let _ = crate::hive::retire::remove_path(&dest);
            }
        }
    }
    seeded
}

/// Removes a temporary worktree, forced (it is maiTerm's own scratch), once its `.git` names a
/// git dir of this repo's that names it back; a folder git doesn't know is left and logged.
pub fn remove_temp(repo: &Path, path: &Path) {
    if fs::symlink_metadata(path).is_err() {
        return;
    }
    match crate::hive::retire::found_folder("land", repo, path) {
        Some(f) => {
            let removal = crate::hive::retire::remove_detached(repo, &f);
            if let crate::hive::retire::Removal::Kept(why) = removal {
                log::warn!("hive land: temporary worktree kept: {why}");
            }
        }
        None => log::warn!("hive land: {} isn't a worktree of {}; left in place", path.display(), repo.display()),
    }
}

/// What step 1 came to in one repo.
#[derive(Debug, Clone, PartialEq)]
pub enum Merged {
    /// The tested commit: the merge, the pinned sha when Base was already in it, or (integration)
    /// the branch's tip when the pin was already in that.
    Tested { sha: String, merged: bool },
    /// The merge conflicted in these files; it was aborted.
    Conflict(Vec<String>),
}

/// Step 1's merge (§8.1): Base's sha (never its name, which may move meanwhile) into the pinned
/// commit, in the temporary worktree.
pub fn merge_base(repo: &Path, tmp: &Temp, base_sha: &str, pinned: &str, message: &str) -> Result<Merged, String> {
    if is_ancestor(repo, base_sha, pinned) {
        return Ok(Merged::Tested { sha: pinned.to_string(), merged: false });
    }
    merge_in(repo, tmp, &[], message, base_sha)
}

/// The integration mode's merge (§8.3): exactly the pinned sha, `--no-ff`, into the temporary
/// worktree made at the integration branch's tip (`head`), so the branch gets a merge commit
/// naming it. A pin the branch already contains (a repo the worker never committed in) merges
/// nothing: git answers "Already up to date" and HEAD, an earlier land's merge commit perhaps,
/// would read as this land's.
pub fn merge_pinned(repo: &Path, tmp: &Temp, head: &str, pinned: &str, message: &str) -> Result<Merged, String> {
    if is_ancestor(repo, pinned, head) {
        return Ok(Merged::Tested { sha: head.to_string(), merged: false });
    }
    merge_in(repo, tmp, &["--no-ff"], message, pinned)
}

fn merge_in(repo: &Path, tmp: &Temp, flags: &[&str], message: &str, rev: &str) -> Result<Merged, String> {
    let f = tmp.folder("land");
    let mut cmd = lgit_wt(&f)?;
    cmd.args(identity(repo)).arg("merge").args(flags).args(["--no-edit", "--no-verify", "-m", message, rev]);
    let out = crate::util::proc::output_timeout(&mut cmd, GIT_TIMEOUT).map_err(|e| e.to_string())?;
    if !out.status.success() {
        let files = run(lgit_wt(&f)?.args(["diff", "--name-only", "--diff-filter=U"]), QUERY_TIMEOUT).unwrap_or_default();
        let files: Vec<String> = files.lines().map(str::trim).filter(|l| !l.is_empty()).map(str::to_string).collect();
        if let Err(e) = run(lgit_wt(&f)?.args(["merge", "--abort"]), QUERY_TIMEOUT) {
            log::info!("hive land: merge --abort in {}: {e}", tmp.path.display());
        }
        if files.is_empty() {
            let err = String::from_utf8_lossy(&out.stderr).trim().to_string();
            return Err(format!("git merge failed: {}", if err.is_empty() { String::from_utf8_lossy(&out.stdout).trim().to_string() } else { err }));
        }
        return Ok(Merged::Conflict(files));
    }
    let sha = run(lgit_wt(&f)?.args(["rev-parse", "--verify", "HEAD"]), QUERY_TIMEOUT)?;
    Ok(Merged::Tested { sha, merged: true })
}

/// A temporary worktree's HEAD and files put back to `to`: a batch chain drops a member's merge
/// that tripped the config rule (§6.8). Only ever maiTerm's own scratch.
pub fn reset_temp(tmp: &Temp, to: &str) -> Result<(), String> {
    run(lgit_wt(&tmp.folder("land"))?.args(["reset", "--hard", "--quiet", to]), GIT_TIMEOUT).map(|_| ())
}

/// The worktree that has `refs/heads/<branch>` checked out, if any.
pub fn checkout_of(repo: &Path, branch: &str) -> Result<Option<PathBuf>, String> {
    let mut cmd = lgit(repo);
    cmd.args(["worktree", "list", "--porcelain", "-z"]);
    let out = crate::util::proc::output_timeout(&mut cmd, QUERY_TIMEOUT).map_err(|e| e.to_string())?;
    if !out.status.success() {
        return Err(format!("git worktree list: {}", String::from_utf8_lossy(&out.stderr).trim()));
    }
    let want = format!("branch refs/heads/{branch}");
    let mut path: Option<PathBuf> = None;
    for field in out.stdout.split(|b| *b == 0) {
        let field = String::from_utf8_lossy(field);
        if field.is_empty() {
            path = None;
        } else if let Some(p) = field.strip_prefix("worktree ") {
            path = Some(PathBuf::from(p));
        } else if field == want {
            return Ok(path);
        }
    }
    Ok(None)
}

/// Why a checkout can't take a fast-forward now: a merge, cherry-pick, revert or rebase is under
/// way there.
pub fn mid_operation(checkout: &Path) -> Option<String> {
    for (name, what) in [
        ("MERGE_HEAD", "a merge"),
        ("CHERRY_PICK_HEAD", "a cherry-pick"),
        ("REVERT_HEAD", "a revert"),
        ("rebase-merge", "a rebase"),
        ("rebase-apply", "a rebase"),
    ] {
        let Ok(p) = run(lgit(checkout).args(["rev-parse", "--git-path", name]), QUERY_TIMEOUT) else { continue };
        let p = Path::new(p.trim());
        let p = if p.is_absolute() { p.to_path_buf() } else { checkout.join(p) };
        if fs::symlink_metadata(&p).is_ok() {
            return Some(format!("its checkout at {} is in the middle of {what}", checkout.display()));
        }
    }
    None
}

/// What step 2 came to in one repo.
#[derive(Debug, Clone, PartialEq)]
pub enum Step2 {
    Advanced,
    /// Waits, nothing changed: an uncommitted file in the way, a checkout mid-operation or busy.
    Held(String),
    /// Base isn't where step 1 found it: step 1 again.
    BaseMoved,
}

/// The files git names in a "would be overwritten" refusal (tab-indented, one per line).
fn named_files(stderr: &str) -> Vec<String> {
    stderr.lines().filter(|l| l.starts_with('\t')).map(|l| l.trim().to_string()).filter(|l| !l.is_empty()).collect()
}

fn list(files: &[String]) -> String {
    let shown: Vec<String> = files.iter().take(5).map(|f| format!("`{f}`")).collect();
    let more = files.len().saturating_sub(5);
    if more > 0 {
        format!("{} and {more} more", shown.join(", "))
    } else {
        shown.join(", ")
    }
}

/// Step 2 (§8.1): `refs/heads/<base>` to exactly `tested`, from `base_sha`. A checkout of Base
/// takes it by `merge --ff-only` (git refuses to overwrite uncommitted work there, and the land
/// holds); with none, `update-ref` compare-and-swaps.
pub fn advance(repo: &Path, base: &str, base_sha: &str, tested: &str) -> Result<Step2, String> {
    let Some(checkout) = checkout_of(repo, base)? else {
        let res = run(lgit(repo).args(["update-ref", "-m", "maiTerm land"]).arg(format!("refs/heads/{base}")).arg(tested).arg(base_sha), QUERY_TIMEOUT);
        return match res {
            Ok(_) => Ok(Step2::Advanced),
            Err(e) if branch_tip(repo, base).as_deref() != Some(base_sha) => {
                log::info!("hive land: {base} in {} moved: {e}", repo.display());
                Ok(Step2::BaseMoved)
            }
            Err(e) => Err(format!("git update-ref: {e}")),
        };
    };
    if let Some(why) = mid_operation(&checkout) {
        return Ok(Step2::Held(why));
    }
    if branch_tip(repo, base).as_deref() != Some(base_sha) {
        return Ok(Step2::BaseMoved);
    }
    for attempt in 0..=INDEX_LOCK_RETRIES {
        let mut cmd = lgit(&checkout);
        cmd.args(["merge", "--ff-only", "--no-autostash", "--quiet", tested]);
        let out = crate::util::proc::output_timeout(&mut cmd, GIT_TIMEOUT).map_err(|e| e.to_string())?;
        if out.status.success() {
            return Ok(Step2::Advanced);
        }
        let err = format!("{}\n{}", String::from_utf8_lossy(&out.stderr), String::from_utf8_lossy(&out.stdout));
        if err.contains("would be overwritten") {
            let files = named_files(&err);
            let untracked = err.contains("untracked working tree files");
            return Ok(Step2::Held(format!(
                "{base}'s checkout at {} has {} {}",
                checkout.display(),
                if untracked { "untracked files that would be overwritten:" } else { "uncommitted changes to" },
                if files.is_empty() { "files the land changes".to_string() } else { list(&files) }
            )));
        }
        if err.to_ascii_lowercase().contains("not possible to fast-forward") {
            return Ok(Step2::BaseMoved);
        }
        if err.contains("index.lock") {
            if attempt < INDEX_LOCK_RETRIES {
                std::thread::sleep(INDEX_LOCK_WAIT);
                continue;
            }
            return Ok(Step2::Held(format!("{base}'s checkout at {} is busy (its index is locked)", checkout.display())));
        }
        return Err(format!("git merge --ff-only in {}: {}", checkout.display(), err.trim()));
    }
    unreachable!("the loop returns on its last attempt")
}

/// A branch moved from `from` to `to` by compare-and-swap. False when it isn't at `from`.
pub fn cas_branch(repo: &Path, branch: &str, to: &str, from: &str) -> bool {
    run(lgit(repo).args(["update-ref", "-m", "maiTerm land"]).arg(format!("refs/heads/{branch}")).arg(to).arg(from), QUERY_TIMEOUT).is_ok()
}

/// A kept worktree's branch to `to` by fast-forward, through its recorded git dir.
pub fn ff_worktree(f: &WorkerFolder, to: &str) -> Result<(), String> {
    run(lgit_wt(f)?.args(["merge", "--ff-only", "--no-autostash", "--quiet", to]), GIT_TIMEOUT).map(|_| ())
}

/// The coordinator's read copy moved to `to`, unless it has changes of its own (§8.1).
pub fn move_read_copy(repo: &Path, copy: &Path, to: &str) -> Result<(), String> {
    if fs::symlink_metadata(copy).is_err() {
        return Err("it doesn't exist".to_string());
    }
    let f = crate::hive::retire::found_folder("read copy", repo, copy).ok_or("maiTerm can't find its git folder")?;
    if !crate::hive::retire::git_file_names(copy, Path::new(f.admin_dir.as_deref().unwrap_or_default())) {
        return Err("its .git file isn't the one git wrote".to_string());
    }
    let status = run(lgit_wt(&f)?.args(["status", "--porcelain", "--untracked-files=no"]), QUERY_TIMEOUT)?;
    if !status.trim().is_empty() {
        return Err("it has uncommitted changes".to_string());
    }
    run(lgit_wt(&f)?.args(["checkout", "--detach", "--quiet", to]), GIT_TIMEOUT).map(|_| ())
}

// ── Undo (§8.6) ──

/// `git merge-tree --write-tree --merge-base` arrived in git 2.40.
pub const MERGE_TREE_MIN: (u32, u32) = (2, 40);

/// `git version 2.54.0 (Apple Git-157)` → (2, 54).
pub fn parse_version(s: &str) -> Option<(u32, u32)> {
    let v = s.trim().strip_prefix("git version ")?.split_whitespace().next()?;
    let mut parts = v.split('.');
    Some((parts.next()?.parse().ok()?, parts.next()?.parse().ok()?))
}

/// Undoing a fast-forward land needs `merge-tree --merge-base`: older git is refused in words.
pub fn merge_tree_supported() -> Result<(), String> {
    let v = run(crate::util::proc::command("git").arg("--version"), QUERY_TIMEOUT)?;
    match parse_version(&v) {
        Some(have) if have >= MERGE_TREE_MIN => Ok(()),
        _ => Err(format!(
            "undoing a fast-forward land needs git {}.{} or newer, and this machine has {}: update git, or revert it by hand",
            MERGE_TREE_MIN.0,
            MERGE_TREE_MIN.1,
            v.trim()
        )),
    }
}

/// What applying a land's inverse onto the target's tip came to.
#[derive(Debug, Clone, PartialEq)]
pub enum Inverse {
    Tree(String),
    /// Later work changed the same lines: these files.
    Conflict(Vec<String>),
}

/// The inverse of `base_before..base_after` applied three-way onto `tip` (§8.6):
/// `merge-tree --write-tree --merge-base=<base_after> <tip> <base_before>`, which writes only
/// objects. Exit 1 is a conflict, its files listed after the tree.
pub fn inverse_tree(repo: &Path, base_after: &str, tip: &str, base_before: &str) -> Result<Inverse, String> {
    let mut cmd = lgit(repo);
    cmd.args(["merge-tree", "--write-tree", "--name-only", "--no-messages"]).arg(format!("--merge-base={base_after}")).args([tip, base_before]);
    let out = crate::util::proc::output_timeout(&mut cmd, GIT_TIMEOUT).map_err(|e| e.to_string())?;
    let text = String::from_utf8_lossy(&out.stdout).to_string();
    let mut lines = text.lines();
    let tree = lines.next().map(str::trim).unwrap_or_default().to_string();
    match out.status.code() {
        Some(0) if !tree.is_empty() => Ok(Inverse::Tree(tree)),
        Some(1) => {
            let mut files: Vec<String> = lines.map(str::trim).take_while(|l| !l.is_empty()).map(str::to_string).collect();
            files.dedup();
            Ok(Inverse::Conflict(files))
        }
        _ => Err(format!("git merge-tree: {}", String::from_utf8_lossy(&out.stderr).trim())),
    }
}

/// A commit of `tree` on `parent`, unsigned, in the repo's identity (plan D8). Writes no ref.
pub fn commit_tree(repo: &Path, tree: &str, parent: &str, message: &str) -> Result<String, String> {
    run(lgit(repo).args(identity(repo)).args(["commit-tree", tree, "-p", parent, "-m", message]), QUERY_TIMEOUT)
}

/// What `git revert -m 1` of a merge came to in a temporary worktree.
#[derive(Debug, Clone, PartialEq)]
pub enum Reverted {
    Commit(String),
    Conflict(Vec<String>),
    /// Reverting it changes nothing: its change is already gone.
    Empty,
}

/// `git revert -m 1 <merge>` in the temporary worktree (its HEAD the target's tip), committed
/// with maiTerm's message, hooks off. A conflict is aborted.
pub fn revert_merge(repo: &Path, tmp: &Temp, merge: &str, message: &str) -> Result<Reverted, String> {
    let f = tmp.folder("land");
    let out = crate::util::proc::output_timeout(lgit_wt(&f)?.args(identity(repo)).args(["revert", "-m", "1", "--no-commit", merge]), GIT_TIMEOUT)
        .map_err(|e| e.to_string())?;
    if !out.status.success() {
        let files = run(lgit_wt(&f)?.args(["diff", "--name-only", "--diff-filter=U"]), QUERY_TIMEOUT).unwrap_or_default();
        let files: Vec<String> = files.lines().map(str::trim).filter(|l| !l.is_empty()).map(str::to_string).collect();
        if let Err(e) = run(lgit_wt(&f)?.args(["revert", "--abort"]), QUERY_TIMEOUT) {
            log::info!("hive undo: revert --abort in {}: {e}", tmp.path.display());
        }
        if files.is_empty() {
            return Err(format!("git revert failed: {}", String::from_utf8_lossy(&out.stderr).trim()));
        }
        return Ok(Reverted::Conflict(files));
    }
    let staged = crate::util::proc::output_timeout(lgit_wt(&f)?.args(["diff", "--cached", "--quiet"]), QUERY_TIMEOUT).map_err(|e| e.to_string())?;
    if staged.status.success() {
        return Ok(Reverted::Empty);
    }
    run(lgit_wt(&f)?.args(identity(repo)).args(["commit", "--no-verify", "--quiet", "-m", message]), GIT_TIMEOUT)?;
    Ok(Reverted::Commit(run(lgit_wt(&f)?.args(["rev-parse", "--verify", "HEAD"]), QUERY_TIMEOUT)?))
}

/// One commit an undo reverts, for the human's confirmation.
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct CommitLine {
    pub sha: String,
    pub subject: String,
}

/// The commits in `range` (`a..b`), newest first, at most `limit`, and how many there are.
pub fn commits_in(repo: &Path, range: &str, limit: usize) -> Result<(Vec<CommitLine>, usize), String> {
    let total = run(lgit(repo).args(["rev-list", "--count", range]), QUERY_TIMEOUT)?.trim().parse::<usize>().unwrap_or(0);
    let text = run(lgit(repo).args(["log", "--no-color", "--format=%H%x09%s"]).arg(format!("--max-count={limit}")).arg(range).arg("--"), QUERY_TIMEOUT)?;
    let lines = text
        .lines()
        .filter_map(|l| l.split_once('\t'))
        .map(|(sha, subject)| CommitLine { sha: sha.to_string(), subject: crate::hive::deliver::one_line(subject, 200) })
        .collect();
    Ok((lines, total))
}

/// `git diff --shortstat a b`, for a card: what a commit range changes.
pub fn shortstat_between(repo: &Path, a: &str, b: &str) -> Result<String, String> {
    run(lgit(repo).args(["diff", "--shortstat", "--no-ext-diff", "--no-textconv", a, b, "--"]), GIT_TIMEOUT)
}

/// `ok`, `held: <why>`: Base pushed to its upstream (§8.5), never forced, hooks off.
pub fn push(repo: &Path, base: &str) -> String {
    let remote = run(lgit(repo).args(["config", "--get"]).arg(format!("branch.{base}.remote")), QUERY_TIMEOUT).ok().filter(|r| !r.is_empty() && r != ".");
    let merge = run(lgit(repo).args(["config", "--get"]).arg(format!("branch.{base}.merge")), QUERY_TIMEOUT).ok().filter(|m| m.starts_with("refs/heads/"));
    let (Some(remote), Some(merge)) = (remote, merge) else {
        return format!("held: {base} has no upstream to push to");
    };
    let res = run(lgit(repo).args(["push", "--quiet", "--no-verify", &remote]).arg(format!("refs/heads/{base}:{merge}")), PUSH_TIMEOUT);
    match res {
        Ok(_) => "ok".to_string(),
        Err(e) => format!("held: {}", crate::hive::deliver::one_line(&e, 300)),
    }
}
