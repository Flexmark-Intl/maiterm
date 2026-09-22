//! The git questions Workspace Share asks (docs/workspace-share.md §2, §4).
//!
//! Every function here shells out and blocks. Callers run them under `spawn_blocking`:
//! a subprocess on the command thread is the pinwheel this app has shipped before.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

/// A git invocation that can never stop to ask a question. A prompt nobody can see is a
/// hang, and everything here runs where nobody can answer one.
fn git() -> Command {
    let mut cmd = Command::new("git");
    cmd.env("GIT_TERMINAL_PROMPT", "0")
        .env("GIT_SSH_COMMAND", "ssh -o BatchMode=yes -o ConnectTimeout=10")
        .stdin(Stdio::null());
    cmd
}

/// stdout of a local git command in `dir`, trimmed; None on any failure.
fn run_in(dir: &Path, args: &[&str]) -> Option<String> {
    let out = git().arg("-C").arg(dir).args(args).output().ok()?;
    if !out.status.success() {
        return None;
    }
    Some(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

/// The repository top level containing `dir`, symlinks resolved.
pub fn toplevel(dir: &Path) -> Option<PathBuf> {
    let top = run_in(dir, &["rev-parse", "--show-toplevel"])?;
    if top.is_empty() {
        return None;
    }
    std::fs::canonicalize(&top).ok().or(Some(PathBuf::from(top)))
}

/// name → fetch URL for every remote.
pub fn remotes(top: &Path) -> BTreeMap<String, String> {
    let mut map = BTreeMap::new();
    let Some(out) = run_in(top, &["remote", "-v"]) else { return map };
    for line in out.lines() {
        // "origin\tgit@github.com:org/x.git (fetch)"
        let mut parts = line.split_whitespace();
        let (Some(name), Some(url), Some(kind)) = (parts.next(), parts.next(), parts.next()) else { continue };
        if kind == "(fetch)" {
            map.insert(name.to_string(), url.to_string());
        }
    }
    map
}

/// The checked-out branch; None when HEAD is detached.
pub fn branch(top: &Path) -> Option<String> {
    run_in(top, &["symbolic-ref", "--quiet", "--short", "HEAD"]).filter(|b| !b.is_empty())
}

/// Why the receiver might not get the sender's code (§3). None = nothing to warn about.
pub fn push_warning(top: &Path, branch: Option<&str>) -> Option<String> {
    let Some(branch) = branch else {
        return Some("HEAD is detached — the receiver gets the remote's default branch".to_string());
    };
    if run_in(top, &["rev-parse", "--abbrev-ref", "--symbolic-full-name", "@{u}"]).is_none() {
        return Some(format!("branch '{branch}' has no upstream — the receiver may not be able to check it out"));
    }
    let ahead: u32 = run_in(top, &["rev-list", "--count", "@{u}..HEAD"])
        .and_then(|n| n.parse().ok())
        .unwrap_or(0);
    if ahead > 0 {
        let s = if ahead == 1 { "" } else { "s" };
        return Some(format!("branch '{branch}' has {ahead} unpushed commit{s}"));
    }
    None
}

/// One spelling for every way of writing a remote, so `git@github.com:org/x.git` and
/// `https://github.com/org/x` compare equal (§4.1).
pub fn normalize_url(url: &str) -> String {
    let mut s = url.trim().to_string();
    if let Some(i) = s.find("://") {
        s = s[i + 3..].to_string();
    }
    // user@ (and user:password@) — only before the path starts.
    let first_slash = s.find('/').unwrap_or(s.len());
    if let Some(at) = s[..first_slash].rfind('@') {
        s = s[at + 1..].to_string();
    }
    // scp form host:path → host/path. A `:` followed by digits and `/` is a port in URL form.
    if let Some(colon) = s.find(':') {
        let slash = s.find('/').unwrap_or(s.len());
        if colon < slash {
            let rest = &s[colon + 1..];
            let port_len = rest.chars().take_while(|c| c.is_ascii_digit()).count();
            let is_port = port_len > 0 && rest[port_len..].starts_with('/');
            s = if is_port {
                format!("{}{}", &s[..colon], &rest[port_len..])
            } else {
                format!("{}/{}", &s[..colon], rest)
            };
        }
    }
    let s = s.trim_end_matches('/');
    let s = s.strip_suffix(".git").unwrap_or(s);
    let (host, path) = s.split_once('/').unwrap_or((s, ""));
    format!("{}/{}", host.to_lowercase(), path)
}

/// The directory name a clone of `url` gets: its last path segment, `.git` stripped.
pub fn repo_name(url: &str) -> String {
    let n = normalize_url(url);
    n.rsplit('/').next().filter(|s| !s.is_empty()).unwrap_or("repo").to_string()
}

/// Does any of `a` name the same repository as any of `b`? Any-against-any is what keeps a
/// fork valid: its `origin` is the fork, its `upstream` is ours.
pub fn remotes_match<'a>(a: impl IntoIterator<Item = &'a String>, b: &BTreeMap<String, String>) -> bool {
    let theirs: Vec<String> = b.values().map(|u| normalize_url(u)).collect();
    a.into_iter().any(|u| theirs.contains(&normalize_url(u)))
}

/// The one rule for every candidate directory (§4): absent or empty → clone into it; a
/// top-level checkout of the same repository → use it; anything else → rejected.
#[derive(Debug, Clone, serde::Serialize, PartialEq)]
#[serde(tag = "verdict", rename_all = "snake_case")]
pub enum DirVerdict {
    Clone,
    Use,
    Reject { reason: String },
}

/// Files a directory may hold and still count as empty.
const IGNORABLE: &[&str] = &[".DS_Store", "Thumbs.db", "desktop.ini"];

fn is_empty_dir(dir: &Path) -> std::io::Result<bool> {
    for entry in std::fs::read_dir(dir)? {
        let name = entry?.file_name();
        if !IGNORABLE.iter().any(|i| name == *i) {
            return Ok(false);
        }
    }
    Ok(true)
}

pub fn classify_git_dir(dir: &Path, root_remotes: &BTreeMap<String, String>) -> DirVerdict {
    let meta = match std::fs::metadata(dir) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return DirVerdict::Clone,
        Err(e) => return DirVerdict::Reject { reason: format!("can't read it: {e}") },
        Ok(m) => m,
    };
    if !meta.is_dir() {
        return DirVerdict::Reject { reason: "it is a file, not a directory".to_string() };
    }
    match is_empty_dir(dir) {
        Ok(true) => return DirVerdict::Clone,
        Ok(false) => {}
        Err(e) => return DirVerdict::Reject { reason: format!("can't read it: {e}") },
    }
    let Some(top) = toplevel(dir) else {
        return DirVerdict::Reject { reason: "it isn't empty and isn't a git repository".to_string() };
    };
    let canon = std::fs::canonicalize(dir).unwrap_or_else(|_| dir.to_path_buf());
    if top != canon {
        return DirVerdict::Reject {
            reason: format!("it is inside the repository at {}, not its top level", top.display()),
        };
    }
    let here = remotes(&top);
    if here.is_empty() {
        return DirVerdict::Reject { reason: "it is a git repository with no remotes".to_string() };
    }
    if remotes_match(here.values(), root_remotes) {
        return DirVerdict::Use;
    }
    let shown = here.get("origin").or_else(|| here.values().next()).cloned().unwrap_or_default();
    DirVerdict::Reject { reason: format!("it is a different repository ({shown})") }
}

/// What `git ls-remote` could tell us before anything is created (§4 access probe).
#[derive(Debug, Clone, serde::Serialize, PartialEq)]
#[serde(tag = "outcome", rename_all = "snake_case")]
pub enum Probe {
    /// The remote answered. `branch_exists` is None when no branch was asked about.
    Reachable { branch_exists: Option<bool> },
    /// The remote answered no.
    Denied { message: String },
    /// Timed out, or needed interactive auth the probe can't give — the clone may still work.
    Unverified { message: String },
}

const PROBE_TIMEOUT: Duration = Duration::from_secs(20);

pub fn probe(url: &str, branch: Option<&str>) -> Probe {
    let child = git()
        .args(["ls-remote", "--heads", url])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn();
    let mut child = match child {
        Ok(c) => c,
        Err(e) => return Probe::Unverified { message: format!("couldn't run git: {e}") },
    };
    let start = Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) if start.elapsed() > PROBE_TIMEOUT => {
                let _ = child.kill();
                let _ = child.wait();
                return Probe::Unverified { message: "timed out".to_string() };
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(100)),
            Err(e) => return Probe::Unverified { message: e.to_string() },
        }
    }
    let out = match child.wait_with_output() {
        Ok(o) => o,
        Err(e) => return Probe::Unverified { message: e.to_string() },
    };
    if out.status.success() {
        let stdout = String::from_utf8_lossy(&out.stdout);
        let branch_exists = branch.map(|b| {
            let want = format!("refs/heads/{b}");
            stdout.lines().any(|l| l.split_whitespace().nth(1) == Some(want.as_str()))
        });
        return Probe::Reachable { branch_exists };
    }
    classify_probe_failure(&String::from_utf8_lossy(&out.stderr))
}

/// A refusal is a remote that answered and said no. Everything else — including ssh's
/// "Permission denied (publickey)", which under BatchMode also means "your key needs a
/// passphrase I wasn't allowed to ask for" — is only unverified.
fn classify_probe_failure(stderr: &str) -> Probe {
    let message = stderr
        .lines()
        .map(|l| l.trim().trim_start_matches("fatal:").trim().trim_start_matches("remote:").trim())
        .find(|l| !l.is_empty())
        .unwrap_or("git ls-remote failed")
        .to_string();
    let lower = stderr.to_lowercase();
    let denied = ["repository not found", "does not exist", "access denied", "not authorized", "returned error: 403", "returned error: 404"]
        .iter()
        .any(|p| lower.contains(p));
    if denied { Probe::Denied { message } } else { Probe::Unverified { message } }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn urls_normalize_across_spellings() {
        let want = "github.com/org/x";
        for u in [
            "git@github.com:org/x.git",
            "https://github.com/org/x",
            "https://github.com/org/x.git/",
            "ssh://git@github.com/org/x.git",
            "https://user:tok@GitHub.com/org/x",
            "git@github.com:org/x",
        ] {
            assert_eq!(normalize_url(u), want, "{u}");
        }
        assert_eq!(normalize_url("ssh://git@host:2222/org/x.git"), "host/org/x");
        assert_ne!(normalize_url("git@github.com:org/y.git"), want);
    }

    #[test]
    fn repo_name_is_the_last_segment() {
        assert_eq!(repo_name("git@github.com:org/maiterm.git"), "maiterm");
        assert_eq!(repo_name("https://gitlab.com/a/b/c/"), "c");
    }

    #[test]
    fn a_fork_matches_through_upstream() {
        let mut ours = BTreeMap::new();
        ours.insert("origin".to_string(), "git@github.com:org/x.git".to_string());
        let theirs = ["https://github.com/me/x".to_string(), "https://github.com/org/x".to_string()];
        assert!(remotes_match(theirs.iter(), &ours));
        assert!(!remotes_match(["https://github.com/me/x".to_string()].iter(), &ours));
    }

    #[test]
    fn probe_failures_split_refusal_from_unknown() {
        assert!(matches!(classify_probe_failure("ERROR: Repository not found.\nfatal: Could not read from remote repository."), Probe::Denied { .. }));
        assert!(matches!(classify_probe_failure("git@github.com: Permission denied (publickey).\nfatal: Could not read from remote repository."), Probe::Unverified { .. }));
        assert!(matches!(classify_probe_failure("fatal: could not read Username for 'https://github.com': terminal prompts disabled"), Probe::Unverified { .. }));
    }

    #[test]
    fn empty_and_absent_dirs_clone() {
        let base = std::env::temp_dir().join(format!("share-git-{}", uuid::Uuid::new_v4()));
        let remotes = BTreeMap::new();
        assert_eq!(classify_git_dir(&base.join("absent"), &remotes), DirVerdict::Clone);
        std::fs::create_dir_all(&base).unwrap();
        std::fs::write(base.join(".DS_Store"), b"").unwrap();
        assert_eq!(classify_git_dir(&base, &remotes), DirVerdict::Clone);
        std::fs::write(base.join("file"), b"x").unwrap();
        assert!(matches!(classify_git_dir(&base, &remotes), DirVerdict::Reject { .. }));
        assert!(matches!(classify_git_dir(&base.join("file"), &remotes), DirVerdict::Reject { .. }));
        let _ = std::fs::remove_dir_all(&base);
    }
}
