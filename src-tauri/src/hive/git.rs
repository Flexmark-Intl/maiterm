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

/// A graft file that can't exist: `/dev/null` is no folder.
const NO_GRAFTS: &str = if cfg!(windows) { "NUL\\maiterm-no-grafts" } else { "/dev/null/maiterm-no-grafts" };

/// maiTerm's git when it VERIFIES something (Done's receipts and unlanded counts, the land's
/// ancestry and merges, a reviewer's pins): nothing a worker can plant in the shared object store
/// redirects what it reads. A worker writes `<common>/objects`, and a commit-graph there listing
/// a commit as a parent of Base's tip made `merge-base --is-ancestor` say it landed; so the
/// commit-graph and multi-pack index are off, and replace refs, grafts and env alternates are
/// ignored. The repo's own `objects/info/alternates` still counts: it's the human's, denied to
/// workers. maiTerm's own git may pass `-c` (the ban is on agents', §7.1).
pub(crate) fn verifying(cmd: &mut std::process::Command) -> &mut std::process::Command {
    cmd.args(["-c", "core.commitGraph=false", "-c", "core.multiPackIndex=false"])
        .env("GIT_NO_REPLACE_OBJECTS", "1")
        .env("GIT_GRAFT_FILE", NO_GRAFTS)
        .env_remove("GIT_ALTERNATE_OBJECT_DIRECTORIES")
        .env_remove("GIT_OBJECT_DIRECTORY")
}

/// `git_in` for a verifying read (`verifying`).
pub(crate) fn verify_in(repo: &Path) -> std::process::Command {
    let mut cmd = git_in(repo);
    verifying(&mut cmd);
    cmd
}

/// What a verified walk from one commit down to another found (`verified_ancestry`).
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Ancestry {
    /// Reached, every edge on the way read from a commit that hashes to its id.
    Reached,
    /// Every commit under the start read, and none was the one sought.
    NotReached,
    /// This commit doesn't hash to its id or isn't a commit (or, the start, is missing).
    Unverified(String),
    /// Gave up after `WALK_CAP` commits.
    TooLong,
}

/// Commits one verified walk reads before it gives up.
pub(crate) const WALK_CAP: usize = 200_000;

/// Whether `to` is, or is an ancestor of, `from`, decided in maiTerm over objects it hashed
/// itself: each commit is read through `cat-file --batch` and hashed (in the repo's own object
/// format) BEFORE the parents it names are followed. Git's own walks read commits unchecked, so a
/// loose object a worker overwrote decides them; worse, `rev-list A..B` marks everything under A
/// uninteresting from objects it never checks, so a forged edge there (a cycle back to A) hides
/// from any range git chooses. Newest first by committer date, so a recent `to` is found quickly.
/// A missing parent is a dead end (a shallow boundary), never proof. Killed at `timeout`. Blocks
/// on git.
pub(crate) fn verified_ancestry(repo: &Path, from: &str, to: &str, timeout: Duration) -> Result<Ancestry, String> {
    use std::collections::{BinaryHeap, HashSet};
    let (from, to) = (from.to_ascii_lowercase(), to.to_ascii_lowercase());
    let mut cat = CatFile::spawn(repo, timeout)?;
    let start = match cat.verified(&from) {
        Ok(Some(c)) => c,
        Ok(None) | Err(Unread::Forged) => return Ok(Ancestry::Unverified(from)),
        Err(Unread::Failed(e)) => return Err(e),
    };
    if from == to {
        return Ok(Ancestry::Reached);
    }
    let mut seen = HashSet::from([from]);
    let mut queue = BinaryHeap::from([(start.date, start.parents)]);
    while let Some((_, parents)) = queue.pop() {
        for p in parents {
            if !seen.insert(p.clone()) {
                continue;
            }
            if seen.len() > WALK_CAP {
                return Ok(Ancestry::TooLong);
            }
            match cat.verified(&p) {
                Ok(Some(c)) => {
                    if p == to {
                        return Ok(Ancestry::Reached);
                    }
                    queue.push((c.date, c.parents));
                }
                Ok(None) => continue,
                Err(Unread::Forged) => return Ok(Ancestry::Unverified(p)),
                Err(Unread::Failed(e)) => return Err(e),
            }
        }
    }
    Ok(Ancestry::NotReached)
}

/// A commit read and hashed: what it names as parents, and its committer date (for order only).
struct Verified {
    parents: Vec<String>,
    date: i64,
}

enum Unread {
    /// Doesn't hash to its id, or isn't a commit.
    Forged,
    Failed(String),
}

impl From<String> for Unread {
    fn from(e: String) -> Self {
        Unread::Failed(e)
    }
}

/// One `cat-file --batch`, asked a commit at a time; a watchdog kills it at the deadline.
struct CatFile {
    child: std::sync::Arc<parking_lot::Mutex<std::process::Child>>,
    stdin: Option<std::process::ChildStdin>,
    stdout: std::io::BufReader<std::process::ChildStdout>,
    timed_out: std::sync::Arc<std::sync::atomic::AtomicBool>,
    done: Option<std::sync::mpsc::Sender<()>>,
}

impl CatFile {
    fn spawn(repo: &Path, timeout: Duration) -> Result<Self, String> {
        use std::process::Stdio;
        use std::sync::{atomic::AtomicBool, Arc};
        let mut cmd = verify_in(repo);
        cmd.args(["cat-file", "--batch"]).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::null());
        let mut child = cmd.spawn().map_err(|e| format!("couldn't run git: {e}"))?;
        let stdin = child.stdin.take();
        let stdout = std::io::BufReader::new(child.stdout.take().ok_or("git cat-file has no output")?);
        let child = Arc::new(parking_lot::Mutex::new(child));
        let timed_out = Arc::new(AtomicBool::new(false));
        let (done, rx) = std::sync::mpsc::channel::<()>();
        let (c, t) = (child.clone(), timed_out.clone());
        std::thread::spawn(move || {
            if let Err(std::sync::mpsc::RecvTimeoutError::Timeout) = rx.recv_timeout(timeout) {
                t.store(true, std::sync::atomic::Ordering::SeqCst);
                let _ = c.lock().kill();
            }
        });
        Ok(Self { child, stdin, stdout, timed_out, done: Some(done) })
    }

    fn failed(&self, e: impl std::fmt::Display) -> String {
        if self.timed_out.load(std::sync::atomic::Ordering::SeqCst) {
            "git cat-file timed out".to_string()
        } else {
            format!("git cat-file failed: {e}")
        }
    }

    /// `id`, hashed before it is parsed. None when the object is missing.
    fn verified(&mut self, id: &str) -> Result<Option<Verified>, Unread> {
        use sha2::Digest;
        use std::io::{BufRead, Read, Write};
        // Only a full id: anything else would be resolved by git (a ref, an abbreviation).
        if !(id.len() == 40 || id.len() == 64) || !id.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Err(Unread::Forged);
        }
        let stdin = self.stdin.as_mut().ok_or_else(|| "git cat-file closed".to_string())?;
        if let Err(e) = stdin.write_all(format!("{id}\n").as_bytes()).and_then(|_| stdin.flush()) {
            return Err(self.failed(e).into());
        }
        let mut header = Vec::new();
        match self.stdout.read_until(b'\n', &mut header) {
            Ok(0) => return Err(self.failed("its answer ended early").into()),
            Err(e) => return Err(self.failed(e).into()),
            Ok(_) => {}
        }
        let header = String::from_utf8_lossy(&header).trim_end().to_string();
        let parts: Vec<&str> = header.split(' ').collect();
        // `<id> missing`, or `<oid> <type> <size>`.
        let [_, kind, size] = parts[..] else { return Ok(None) };
        let size: usize = size.parse().map_err(|_| format!("git cat-file's answer has no size: {header}"))?;
        let mut body = vec![0u8; size + 1];
        if let Err(e) = self.stdout.read_exact(&mut body) {
            return Err(self.failed(e).into());
        }
        body.truncate(size);
        let mut framed = format!("{kind} {size}\0").into_bytes();
        framed.extend_from_slice(&body);
        let hashed = if id.len() == 40 { hex(&sha1::Sha1::digest(&framed)) } else { hex(&sha2::Sha256::digest(&framed)) };
        if kind != "commit" || !hashed.eq_ignore_ascii_case(id) {
            return Err(Unread::Forged);
        }
        // The header runs to the first empty line; parents follow `tree`.
        let text = String::from_utf8_lossy(&body);
        let mut parents = Vec::new();
        let mut date = 0;
        for line in text.split('\n').take_while(|l| !l.is_empty()) {
            if let Some(p) = line.strip_prefix("parent ") {
                parents.push(p.trim().to_ascii_lowercase());
            } else if let Some(c) = line.strip_prefix("committer ") {
                date = c.rsplit(' ').nth(1).and_then(|d| d.parse().ok()).unwrap_or(0);
            }
        }
        Ok(Some(Verified { parents, date }))
    }
}

impl Drop for CatFile {
    fn drop(&mut self) {
        drop(self.stdin.take());
        drop(self.done.take());
        let mut child = self.child.lock();
        let _ = child.kill();
        let _ = child.wait();
    }
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
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

    /// Grafts and replace refs, the two other ways a commit's parents are rewritten without
    /// touching it, decide nothing in a verifying read or in `verified_ancestry`, which flags what
    /// isn't a commit. The commit-graph and forged loose objects are Done's tests.
    #[test]
    fn a_verifying_read_ignores_grafts_and_replace_refs() {
        let base = crate::share::canon(&{
            let t = std::env::temp_dir().join(format!("hive-verify-{}", uuid::Uuid::new_v4()));
            fs::create_dir_all(&t).unwrap();
            t
        });
        let repo = base.join("web");
        fs::create_dir_all(&repo).unwrap();
        git(&repo, &["init", "-q", "-b", "main"]);
        git(&repo, &["config", "core.autocrlf", "false"]);
        let commit = |name: &str| {
            fs::write(repo.join(name), name).unwrap();
            git(&repo, &["add", "-A"]);
            git(&repo, &["commit", "-q", "--no-verify", "-m", name]);
            git(&repo, &["rev-parse", "HEAD"])
        };
        let root = commit("root");
        git(&repo, &["checkout", "-q", "-b", "side"]);
        let c = commit("c");
        git(&repo, &["checkout", "-q", "main"]);
        let t = commit("t");
        let plain = |args: &[&str]| {
            let mut cmd = Command::new("git");
            cmd.current_dir(&repo).env("GIT_CONFIG_GLOBAL", "/dev/null").args(["-c", "advice.graftFileDeprecated=false"]).args(args);
            cmd.output().unwrap().status.success()
        };
        let ours = |a: &str, b: &str| run(verify_in(&repo).args(["merge-base", "--is-ancestor", a, b]), QUERY_TIMEOUT).is_ok();
        assert!(!plain(&["merge-base", "--is-ancestor", &c, "main"]) && !ours(&c, "main"));

        fs::create_dir_all(repo.join(".git/info")).unwrap();
        fs::write(repo.join(".git/info/grafts"), format!("{t} {c}\n")).unwrap();
        assert!(plain(&["merge-base", "--is-ancestor", &c, "main"]), "a graft fools plain git");
        assert!(!ours(&c, "main"), "but not a verifying read");
        fs::remove_file(repo.join(".git/info/grafts")).unwrap();

        git(&repo, &["replace", "--graft", &t, &c]);
        assert!(plain(&["merge-base", "--is-ancestor", &c, "main"]), "a replace ref fools plain git");
        assert!(!ours(&c, "main"), "but not a verifying read");

        // Nor maiTerm's own walk, which reads the objects alone.
        let walk = |from: &str, to: &str| verified_ancestry(&repo, from, to, QUERY_TIMEOUT).unwrap();
        assert_eq!(walk(&t, &root), Ancestry::Reached);
        assert_eq!(walk(&t, &t.to_uppercase()), Ancestry::Reached, "a commit is its own");
        assert_eq!(walk(&t, &c), Ancestry::NotReached, "the replace ref decides nothing");
        let tree = git(&repo, &["rev-parse", "main^{tree}"]);
        let missing = "0123456789abcdef0123456789abcdef01234567".to_string();
        assert_eq!(walk(&tree, &root), Ancestry::Unverified(tree.clone()), "a tree isn't a commit");
        assert_eq!(walk(&missing, &root), Ancestry::Unverified(missing.clone()));
        assert_eq!(walk("main", &root), Ancestry::Unverified("main".into()), "only a full id, never a name git resolves");
        let _ = fs::remove_dir_all(&base);
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
