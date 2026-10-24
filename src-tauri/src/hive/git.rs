//! maiTerm's own git on a Hive's repos, shared by the coordinator's read copies (`layout`) and
//! the workers' worktrees (`worktree`), and by the stack's copies on other checkouts
//! (`crate::stack::target`): the hardened invocation, the one for a worktree a worker has written
//! in, and taking back a worktree add.
//!
//! Everything here blocks on git: callers run it under `spawn_blocking`.

use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

use super::worker::WorkerFolder;
use crate::util::proc;

pub(crate) const QUERY_TIMEOUT: Duration = Duration::from_secs(15);
/// A checkout of a large repo takes a while; one that takes longer is a hung filesystem.
pub(crate) const WORKTREE_TIMEOUT: Duration = Duration::from_secs(600);

/// maiTerm's own git on a Hive repo runs no hooks and no fsmonitor daemon: a worker can write
/// a repo's hook or config, and maiTerm must not run what it wrote (§13.1).
pub(crate) fn git_in(repo: &Path) -> std::process::Command {
    let mut cmd = proc::git();
    cmd.arg("-C").arg(repo).args(["-c", "core.fsmonitor=false", "-c", "core.hooksPath=/dev/null"]);
    cmd
}

/// maiTerm's git in a worker's worktree, on the git dir maiTerm recorded when it made it. The
/// worktree's `.git` file is the worker's to rewrite, and followed, it would point git at a repo
/// whose config the worker wrote (a filter driver runs whatever that names, as maiTerm).
pub(crate) fn git_in_worktree(f: &WorkerFolder) -> Result<std::process::Command, String> {
    let wt = f.worktree.as_deref().ok_or("no worktree")?;
    let admin = f.admin_dir.as_deref().ok_or("no recorded git dir")?;
    // The worker writes its admin dir but not `commondir` (denyWrite). Without that file git
    // would take the admin dir as a whole repo, a config the worker wrote included.
    if !Path::new(admin).join("commondir").is_file() {
        return Err("its git dir isn't a worktree's".to_string());
    }
    let mut cmd = git_in(Path::new(wt));
    cmd.arg(format!("--git-dir={admin}")).arg(format!("--work-tree={wt}"));
    Ok(cmd)
}

pub(crate) fn run(cmd: &mut std::process::Command, timeout: Duration) -> Result<String, String> {
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
pub(crate) fn clean_failed_add(repo: &Path, worktree: &Path, branch: Option<(&str, &str)>) {
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;

    fn git(dir: &Path, args: &[&str]) -> String {
        let out = Command::new("git")
            .current_dir(dir)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .args(["-c", "user.email=t@example.com", "-c", "user.name=t", "-c", "commit.gpgsign=false"])
            .args(args)
            .output()
            .unwrap();
        assert!(out.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&out.stderr));
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    }

    /// A repo, a worktree of it on its own branch, and that worktree as a worker's record has it.
    fn rig(base: &Path) -> (PathBuf, WorkerFolder) {
        let repo = base.join("web");
        fs::create_dir_all(&repo).unwrap();
        git(&repo, &["init", "-q", "-b", "main"]);
        fs::write(repo.join("README.md"), "r\n").unwrap();
        git(&repo, &["add", "-A"]);
        git(&repo, &["commit", "-q", "--no-verify", "-m", "init"]);
        let wt = base.join("wt");
        git(&repo, &["worktree", "add", "-q", "-b", "hive/s--w", wt.to_str().unwrap(), "main"]);
        let dirs = git(&wt, &["rev-parse", "--path-format=absolute", "--git-common-dir", "--git-dir"]);
        let dirs: Vec<String> = dirs.lines().map(|l| crate::share::canon(Path::new(l.trim())).to_string_lossy().to_string()).collect();
        let f = WorkerFolder {
            name: "web".into(),
            kind: "git".into(),
            worktree: Some(wt.to_string_lossy().into()),
            branch: Some("hive/s--w".into()),
            common_dir: Some(dirs[0].clone()),
            admin_dir: Some(dirs[1].clone()),
            ..WorkerFolder::default()
        };
        (wt, f)
    }

    #[test]
    fn a_forged_git_file_in_a_worktree_is_not_followed() {
        let base = crate::share::canon(&{
            let t = std::env::temp_dir().join(format!("hive-git-{}", uuid::Uuid::new_v4()));
            fs::create_dir_all(&t).unwrap();
            t
        });
        let (wt, f) = rig(&base);
        // The worker points its worktree's `.git` at a repo of its own, whose config it wrote.
        git(&base, &["init", "-q", "fake"]);
        let fake = crate::share::canon(&base.join("fake/.git"));
        fs::write(wt.join(".git"), format!("gitdir: {}\n", fake.display())).unwrap();
        assert_eq!(crate::share::canon(Path::new(&git(&wt, &["rev-parse", "--absolute-git-dir"]))), fake, "plain git follows it");

        let ours = run(git_in_worktree(&f).unwrap().args(["rev-parse", "--absolute-git-dir"]), QUERY_TIMEOUT).unwrap();
        assert_eq!(crate::share::canon(Path::new(&ours)).to_string_lossy(), f.admin_dir.clone().unwrap(), "the recorded git dir");
        let head = run(git_in_worktree(&f).unwrap().args(["rev-parse", "--abbrev-ref", "HEAD"]), QUERY_TIMEOUT).unwrap();
        assert_eq!(head, "hive/s--w");

        // A recorded dir that isn't a worktree's (no `commondir`) is a whole repo: refused.
        let not_wt = WorkerFolder { admin_dir: Some(fake.to_string_lossy().into()), ..f.clone() };
        assert!(git_in_worktree(&not_wt).unwrap_err().contains("isn't a worktree's"));
        assert!(git_in_worktree(&WorkerFolder { admin_dir: None, ..f.clone() }).is_err());
        assert!(git_in_worktree(&WorkerFolder { worktree: None, ..f }).is_err());
        let _ = fs::remove_dir_all(&base);
    }
}
