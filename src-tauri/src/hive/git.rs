//! maiTerm's own git on a Hive's repos, shared by the coordinator's read copies (`layout`) and
//! the workers' worktrees (`worktree`): the hardened invocation, and taking back a worktree add.
//!
//! Everything here blocks on git: callers run it under `spawn_blocking`.

use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::util::proc;

pub(super) const QUERY_TIMEOUT: Duration = Duration::from_secs(15);
/// A checkout of a large repo takes a while; one that takes longer is a hung filesystem.
pub(super) const WORKTREE_TIMEOUT: Duration = Duration::from_secs(600);

/// maiTerm's own git on a Hive repo runs no hooks and no fsmonitor daemon: a worker can write
/// a repo's hook or config, and maiTerm must not run what it wrote (§13.1).
pub(super) fn git_in(repo: &Path) -> std::process::Command {
    let mut cmd = proc::git();
    cmd.arg("-C").arg(repo).args(["-c", "core.fsmonitor=false", "-c", "core.hooksPath=/dev/null"]);
    cmd
}

pub(super) fn run(cmd: &mut std::process::Command, timeout: Duration) -> Result<String, String> {
    let out = proc::output_timeout(cmd, timeout).map_err(|e| e.to_string())?;
    if !out.status.success() {
        let err = String::from_utf8_lossy(&out.stderr).trim().to_string();
        return Err(if err.is_empty() { format!("git exited with {}", out.status) } else { err });
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

/// A worktree one call added, so a failure later in the same call can take it back.
pub(super) struct Added {
    pub repo: PathBuf,
    pub worktree: PathBuf,
    /// The branch it created, and the commit it created it at.
    pub branch: Option<(String, String)>,
}

/// Undo a `worktree add` that failed partway, so a retry isn't refused by its leftovers. The
/// folder didn't exist before the add and sits under the Hive's own root, so removing it is safe.
pub(super) fn clean_failed_add(repo: &Path, worktree: &Path, branch: Option<(&str, &str)>) {
    if fs::symlink_metadata(worktree).is_ok() {
        if let Err(e) = fs::remove_dir_all(worktree) {
            log::warn!("hive: half-made worktree {} left in place: {e}", worktree.display());
        }
    }
    // With the folder gone this drops just its `.git/worktrees` entry; never `prune`, which would
    // also drop the human's own entries for a worktree on an unmounted disk. Fails harmlessly
    // when the add never registered it.
    if let Err(e) = run(git_in(repo).args(["worktree", "remove", "--force"]).arg(worktree), QUERY_TIMEOUT) {
        log::debug!("hive: no worktree entry for {} to drop: {e}", worktree.display());
    }
    if let Some((branch, sha)) = branch {
        let reference = format!("refs/heads/{branch}");
        if run(git_in(repo).args(["rev-parse", "--verify", "--quiet"]).arg(&reference), QUERY_TIMEOUT).is_ok() {
            if let Err(e) = run(git_in(repo).args(["update-ref", "-d"]).arg(&reference).arg(sha), QUERY_TIMEOUT) {
                log::warn!("hive: branch {branch} in {} left in place: {e}", repo.display());
            }
        }
    }
}

/// Remove the worktrees, newest first, then each branch while it still points where it was
/// created (compare-and-delete: a branch with work on it stays). False if anything stayed.
pub(super) fn roll_back(added: &[Added]) -> bool {
    let mut all = true;
    for a in added.iter().rev() {
        let res = run(git_in(&a.repo).args(["worktree", "remove", "--force"]).arg(&a.worktree), WORKTREE_TIMEOUT);
        if let Err(e) = res {
            log::warn!("hive: worktree {} left in place: {e}", a.worktree.display());
            all = false;
            continue;
        }
        if let Some((branch, sha)) = &a.branch {
            let res = run(git_in(&a.repo).args(["update-ref", "-d"]).arg(format!("refs/heads/{branch}")).arg(sha), QUERY_TIMEOUT);
            if let Err(e) = res {
                log::warn!("hive: branch {branch} in {} left in place: {e}", a.repo.display());
                all = false;
            }
        }
    }
    all
}
