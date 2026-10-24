//! Claude's folder trust for the Hive's own folders (docs/hive.md §6.6, spike 2).
//!
//! What Claude does, measured on 2.1.309: trust is read only from the launching root's
//! `.claude.json`, keyed by realpath, and a folder is trusted when it or an ancestor up to its
//! git root (to `/` outside a repo) is — with a linked worktree mapped to its main checkout.
//! So a worktree of a repo the human trusts needs nothing, and maiTerm writes trust only for
//! folders it made itself (the coordinator's `home/`). Nothing automated may answer the dialog
//! instead (`trust_dialog_open`), so a missing entry is written before launch, in every root.

use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use super::paths::HivePaths;
use crate::relocate::agents::Homes;
use crate::util::json_file::update_json;
use crate::util::proc;

const GIT_TIMEOUT: Duration = Duration::from_secs(15);

/// The keys whose `hasTrustDialogAccepted` would cover `path`, most specific first.
pub fn trust_candidates(path: &Path) -> Vec<String> {
    let real = crate::share::canon(path);
    let mut out = Vec::new();
    let mut push = |p: &Path| {
        let k = p.to_string_lossy().to_string();
        if !out.contains(&k) {
            out.push(k);
        }
    };
    match git_dirs(&real) {
        Some(g) => {
            // The walk is clamped at the git root: an ancestor's trust stops there.
            for a in real.ancestors().take_while(|a| a.starts_with(&g.top)) {
                push(a);
            }
            if let Some(main) = g.main_checkout {
                push(&main);
            }
        }
        None => real.ancestors().for_each(&mut push),
    }
    out
}

struct GitDirs {
    top: PathBuf,
    /// Set for a linked worktree: the checkout whose `.git` is the common dir.
    main_checkout: Option<PathBuf>,
}

fn git_dirs(dir: &Path) -> Option<GitDirs> {
    let out = proc::output_timeout(
        proc::git()
            .arg("-C")
            .arg(dir)
            .args(["rev-parse", "--path-format=absolute", "--show-toplevel", "--git-common-dir", "--git-dir"]),
        GIT_TIMEOUT,
    )
    .ok()?;
    if !out.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&out.stdout);
    let mut lines = text.lines().map(|l| crate::share::canon(Path::new(l.trim())));
    let (top, common, git_dir) = (lines.next()?, lines.next()?, lines.next()?);
    let main_checkout = (common != git_dir && common.file_name().is_some_and(|n| n == ".git"))
        .then(|| common.parent().map(Path::to_path_buf))
        .flatten();
    Some(GitDirs { top, main_checkout })
}

pub fn has_trust_in(json: &Value, candidates: &[String]) -> bool {
    candidates.iter().any(|k| json["projects"][k.as_str()]["hasTrustDialogAccepted"] == Value::Bool(true))
}

/// Would a Claude launched in `path` under the root whose `.claude.json` is `json` skip the
/// dialog?
pub fn has_trust(json: &Value, path: &Path) -> bool {
    has_trust_in(json, &trust_candidates(path))
}

/// `has_trust` against every file in `claude_json`. False for an empty list: no root, no
/// evidence of trust.
pub fn trusted_in_all(claude_json: &[PathBuf], path: &Path) -> bool {
    keys_trusted_in_all(claude_json, &trust_candidates(path))
}

pub fn keys_trusted_in_all(claude_json: &[PathBuf], candidates: &[String]) -> bool {
    !claude_json.is_empty() && claude_json.iter().all(|f| read_json(f).is_some_and(|v| has_trust_in(&v, candidates)))
}

/// `trust_candidates` for a worker's worktree, from the common dir maiTerm recorded when it made
/// it rather than from git run there: the worktree's `.git` file is the worker's to rewrite. A
/// worktree is its own git root, so it counts, and its main checkout.
pub fn worktree_candidates(worktree: &Path, common_dir: Option<&Path>) -> Vec<String> {
    let mut out = vec![crate::share::canon(worktree).to_string_lossy().to_string()];
    let main = common_dir.filter(|c| c.file_name().is_some_and(|n| n == ".git")).and_then(Path::parent);
    if let Some(k) = main.map(|m| crate::share::canon(m).to_string_lossy().to_string()) {
        if !out.contains(&k) {
            out.push(k);
        }
    }
    out
}

fn read_json(p: &Path) -> Option<Value> {
    serde_json::from_slice(&fs::read(p).ok()?).ok()
}

/// One key maiTerm set, in one root.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
struct Written {
    file: String,
    key: String,
    /// maiTerm created the whole project entry (removal drops it) rather than flipping the flag
    /// on an entry Claude had made (removal flips it back).
    created: bool,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct Record {
    #[serde(default)]
    written: Vec<Written>,
}

fn load_record(hive: &HivePaths) -> Record {
    read_json(&hive.trust_record()).and_then(|v| serde_json::from_value(v).ok()).unwrap_or_default()
}

fn save_record(hive: &HivePaths, r: &Record) -> Result<(), String> {
    let path = hive.trust_record();
    let tmp = path.with_extension(format!("tmp-{}", std::process::id()));
    let bytes = serde_json::to_vec_pretty(r).map_err(|e| e.to_string())?;
    fs::write(&tmp, bytes).and_then(|_| fs::rename(&tmp, &path)).map_err(|e| {
        let _ = fs::remove_file(&tmp);
        format!("couldn't record the trust write: {e}")
    })
}

#[derive(Debug, Default)]
pub struct TrustWrite {
    /// Roots the entry was written to.
    pub written: Vec<PathBuf>,
    /// Roots that already trusted it (their own entry, or an ancestor's).
    pub already: Vec<PathBuf>,
    /// One sentence per root that couldn't be written; the launch must not go ahead under it.
    pub failed: Vec<String>,
}

/// Trust `path` in every root's `.claude.json` that doesn't already. A root whose file doesn't
/// exist is skipped: no Claude has run under it, and it would make its own on first run — a
/// launch re-verifies first (§6.6). A file that exists but can't be read is `failed`, never
/// skipped. Err only when `path` doesn't exist.
pub fn write_trust(homes: &Homes, hive: &HivePaths, path: &Path) -> Result<TrustWrite, String> {
    // `~/.claude.json` is the other maiTerms' too (crate::instance); the Hive is refused there
    // first, and this is the backstop for any path that reaches a trust write anyway.
    if let Some(why) = crate::instance::global_write_refusal() {
        return Err(why);
    }
    let real = fs::canonicalize(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let real = crate::share::canon(&real);
    let key = real.to_string_lossy().to_string();
    let candidates = trust_candidates(&real);
    let mut report = TrustWrite::default();
    let mut record = load_record(hive);
    for file in &homes.claude_json {
        let (mut reached, mut already, mut created, mut malformed) = (false, false, false, false);
        let res = update_json(file, |v| {
            (reached, already, created, malformed) = (true, false, false, false);
            if has_trust_in(v, &candidates) {
                already = true;
                return false;
            }
            let Some(root) = v.as_object_mut() else {
                malformed = true;
                return false;
            };
            let projects = root.entry("projects").or_insert_with(|| Value::Object(Map::new()));
            let Some(projects) = projects.as_object_mut() else {
                malformed = true;
                return false;
            };
            created = !projects.contains_key(&key);
            let entry = projects.entry(key.clone()).or_insert_with(|| Value::Object(Map::new()));
            match entry.as_object_mut() {
                Some(e) => {
                    e.insert("hasTrustDialogAccepted".into(), Value::Bool(true));
                    true
                }
                None => {
                    malformed = true;
                    false
                }
            }
        });
        match res {
            Ok(true) => {
                log::info!("hive: trusted {} in {}", key, file.display());
                report.written.push(file.clone());
                let w = Written { file: file.to_string_lossy().to_string(), key: key.clone(), created };
                if !record.written.iter().any(|r| r.file == w.file && r.key == w.key) {
                    record.written.push(w);
                }
            }
            Ok(false) if already => report.already.push(file.clone()),
            Ok(false) if malformed => report.failed.push(format!("{} isn't a config Claude could read", file.display())),
            // The edit never ran: no such file, or one that isn't JSON (empty, half-written).
            Ok(false) if !reached && fs::metadata(file).is_ok() => report.failed.push(format!("{} isn't valid JSON", file.display())),
            Ok(false) => {}
            Err(e) => report.failed.push(format!("{}: {e}", file.display())),
        }
    }
    if !report.written.is_empty() {
        save_record(hive, &record)?;
    }
    for f in &report.failed {
        log::warn!("hive: trust for {key} not written: {f}");
    }
    Ok(report)
}

/// Undo what `write_trust` recorded for `path`, and nothing else: Claude's own entries (a
/// worktree session creates its main repo's) and the human's are never touched. The folder may
/// already be gone, so the key is matched as recorded. Returns how many roots changed.
pub fn remove_trust(hive: &HivePaths, path: &Path) -> Result<usize, String> {
    let key = crate::share::canon(path).to_string_lossy().to_string();
    let mut record = load_record(hive);
    let (mine, rest): (Vec<Written>, Vec<Written>) = record.written.into_iter().partition(|w| w.key == key);
    let mut changed = 0;
    let mut kept = Vec::new();
    for w in mine {
        let res = update_json(Path::new(&w.file), |v| {
            let Some(projects) = v.get_mut("projects").and_then(Value::as_object_mut) else { return false };
            if w.created {
                projects.remove(&w.key).is_some()
            } else if let Some(e) = projects.get_mut(&w.key).and_then(Value::as_object_mut) {
                e.insert("hasTrustDialogAccepted".into(), Value::Bool(false));
                true
            } else {
                false
            }
        });
        match res {
            Ok(c) => changed += c as usize,
            Err(e) => {
                log::warn!("hive: trust for {} not removed from {}: {e}", w.key, w.file);
                kept.push(w);
            }
        }
    }
    record.written = rest.into_iter().chain(kept).collect();
    save_record(hive, &record)?;
    Ok(changed)
}

/// Every recorded write still standing, as `(key, file)`.
pub fn written_entries(hive: &HivePaths) -> Vec<(String, String)> {
    load_record(hive).written.into_iter().map(|w| (w.key, w.file)).collect()
}

/// Every key maiTerm recorded writing for this Hive, once each.
pub fn written_keys(hive: &HivePaths) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for w in load_record(hive).written {
        if !out.contains(&w.key) {
            out.push(w.key);
        }
    }
    out
}

/// `remove_trust` for every key in the record (a deleted Hive, §11.2). Returns how many root
/// entries changed; a key that couldn't be undone stays recorded and is logged.
pub fn remove_all_trust(hive: &HivePaths) -> usize {
    written_keys(hive)
        .iter()
        .map(|k| match remove_trust(hive, Path::new(k)) {
            Ok(n) => n,
            Err(e) => {
                log::warn!("hive: trust for {k} not removed: {e}");
                0
            }
        })
        .sum()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;

    fn scratch(tag: &str) -> PathBuf {
        let t = std::env::temp_dir().join(format!("hive-trust-{tag}-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&t).unwrap();
        crate::share::canon(&t)
    }

    /// Two roots, as `Homes::real()` would list `~/.claude.json` and one managed account's.
    fn homes(base: &Path) -> Homes {
        let a = base.join("home/.claude.json");
        let b = base.join("accounts/claude/acct/.claude.json");
        for f in [&a, &b] {
            fs::create_dir_all(f.parent().unwrap()).unwrap();
        }
        fs::write(&a, r#"{"projects":{"/foreign":{"hasTrustDialogAccepted":true,"allowedTools":[]}},"numStartups":3}"#).unwrap();
        fs::write(&b, r#"{"oauthAccount":{"x":1}}"#).unwrap();
        Homes {
            claude_projects: base.join("home/.claude/projects"),
            claude_json: vec![a, b],
            claude_history: vec![],
            codex: base.join("codex"),
            gemini: base.join("gemini"),
        }
    }

    fn git(dir: &Path, args: &[&str]) {
        let ok = Command::new("git")
            .current_dir(dir)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .args(["-c", "user.email=t@example.com", "-c", "user.name=t", "-c", "commit.gpgsign=false"])
            .args(args)
            .status()
            .unwrap()
            .success();
        assert!(ok, "git {args:?}");
    }

    // The key is the realpath through a symlink, in POSIX form; no Hive runs on Windows (§14.16).
    #[cfg(unix)]
    #[test]
    fn written_to_every_root_under_the_realpath_and_removed_without_touching_foreign_keys() {
        let base = scratch("write");
        let h = homes(&base);
        let hive = HivePaths::new(base.join("hive"));
        let real = base.join("hive/coordinator/home");
        fs::create_dir_all(&real).unwrap();
        let link = base.join("link-to-hive");
        std::os::unix::fs::symlink(base.join("hive"), &link).unwrap();

        let r = write_trust(&h, &hive, &link.join("coordinator/home")).unwrap();
        assert_eq!(r.written.len(), 2, "every root: {r:?}");
        let key = real.to_string_lossy().to_string();
        for f in &h.claude_json {
            let v = read_json(f).unwrap();
            assert_eq!(v["projects"][key.as_str()]["hasTrustDialogAccepted"], true, "realpath key in {}", f.display());
            assert!(has_trust(&v, &real));
        }
        assert!(trusted_in_all(&h.claude_json, &real));
        assert_eq!(read_json(&h.claude_json[0]).unwrap()["numStartups"], 3, "the rest of the file is kept");

        // Already trusted: nothing written, nothing recorded twice.
        let again = write_trust(&h, &hive, &real).unwrap();
        assert!(again.written.is_empty());
        assert_eq!(again.already.len(), 2);

        assert_eq!(remove_trust(&hive, &real).unwrap(), 2);
        let a = read_json(&h.claude_json[0]).unwrap();
        assert!(a["projects"].get(key.as_str()).is_none());
        assert_eq!(a["projects"]["/foreign"]["hasTrustDialogAccepted"], true, "a key maiTerm didn't write stays");
        assert!(read_json(&h.claude_json[1]).unwrap()["projects"].get(key.as_str()).is_none());
        assert!(!trusted_in_all(&h.claude_json, &real));
        let _ = fs::remove_dir_all(&base);
    }

    #[test]
    fn removal_flips_back_an_entry_claude_made_instead_of_deleting_it() {
        let base = scratch("flip");
        let h = homes(&base);
        let hive = HivePaths::new(base.join("hive"));
        let dir = base.join("hive/coordinator/home");
        fs::create_dir_all(&dir).unwrap();
        let key = dir.to_string_lossy().to_string();
        let claude_made = serde_json::json!({"projects": {key.clone(): {"hasTrustDialogAccepted": false, "history": [1]}}});
        fs::write(&h.claude_json[1], serde_json::to_vec(&claude_made).unwrap()).unwrap();

        write_trust(&h, &hive, &dir).unwrap();
        remove_trust(&hive, &dir).unwrap();
        let v = read_json(&h.claude_json[1]).unwrap();
        assert_eq!(v["projects"][key.as_str()]["hasTrustDialogAccepted"], false);
        assert_eq!(v["projects"][key.as_str()]["history"], serde_json::json!([1]), "Claude's data stays");
        let _ = fs::remove_dir_all(&base);
    }

    #[test]
    fn a_missing_root_file_is_skipped_not_created() {
        let base = scratch("missing");
        let mut h = homes(&base);
        h.claude_json.push(base.join("accounts/claude/never-ran/.claude.json"));
        let hive = HivePaths::new(base.join("hive"));
        fs::create_dir_all(&hive.root).unwrap();
        let r = write_trust(&h, &hive, &hive.root).unwrap();
        assert_eq!(r.written.len(), 2);
        assert!(r.failed.is_empty());
        assert!(!base.join("accounts/claude/never-ran/.claude.json").exists());
        let _ = fs::remove_dir_all(&base);
    }

    #[test]
    fn an_unparseable_root_file_is_a_failure_not_a_missing_file() {
        let base = scratch("unparseable");
        let h = homes(&base);
        fs::write(&h.claude_json[1], b"").unwrap();
        let hive = HivePaths::new(base.join("hive"));
        fs::create_dir_all(&hive.root).unwrap();
        let r = write_trust(&h, &hive, &hive.root).unwrap();
        assert_eq!(r.written, vec![h.claude_json[0].clone()]);
        assert_eq!(r.failed.len(), 1, "{r:?}");
        assert!(r.failed[0].contains("isn't valid JSON"));
        assert_eq!(fs::read(&h.claude_json[1]).unwrap(), b"", "left as found");
        let _ = fs::remove_dir_all(&base);
    }

    #[test]
    fn a_trusted_main_repo_covers_its_worktree_and_the_walk_stops_at_a_git_root() {
        let base = scratch("wt");
        let repo = base.join("repo");
        fs::create_dir_all(&repo).unwrap();
        git(&repo, &["init", "-q", "-b", "main"]);
        fs::write(repo.join("a"), "a").unwrap();
        git(&repo, &["add", "-A"]);
        git(&repo, &["commit", "-q", "--no-verify", "-m", "init"]);
        let wt = base.join("wt.noindex/coordinator/repo");
        git(&repo, &["worktree", "add", "-q", "--detach", wt.to_str().unwrap(), "main"]);
        let wt = crate::share::canon(&wt);

        let k = |p: &Path| p.to_string_lossy().to_string();
        let json = serde_json::json!({"projects": {k(&repo): {"hasTrustDialogAccepted": true}}});
        assert!(has_trust(&json, &wt), "the main checkout's trust covers the worktree");
        let only_wt = serde_json::json!({"projects": {k(&wt): {"hasTrustDialogAccepted": true}}});
        assert!(!has_trust(&only_wt, &repo), "the reverse doesn't hold");

        // An ancestor above the git root doesn't count; outside a repo it does.
        let parent = serde_json::json!({"projects": {k(&base): {"hasTrustDialogAccepted": true}}});
        assert!(!has_trust(&parent, &repo));
        let plain = base.join("plain/deep");
        fs::create_dir_all(&plain).unwrap();
        assert!(has_trust(&parent, &plain));
        let _ = fs::remove_dir_all(&base);
    }

    /// A worker's worktree is judged by the common dir maiTerm recorded: a `.git` file the worker
    /// rewrote to name a repo of its own changes what git answers, not what maiTerm reads.
    #[test]
    fn a_workers_worktree_is_judged_by_its_recorded_common_dir() {
        let base = scratch("recorded");
        let repo = base.join("repo");
        fs::create_dir_all(&repo).unwrap();
        git(&repo, &["init", "-q", "-b", "main"]);
        fs::write(repo.join("a"), "a").unwrap();
        git(&repo, &["add", "-A"]);
        git(&repo, &["commit", "-q", "--no-verify", "-m", "init"]);
        let wt = base.join("wt.noindex/auth/repo");
        git(&repo, &["worktree", "add", "-q", "-b", "hive/s--auth", wt.to_str().unwrap(), "main"]);
        let wt = crate::share::canon(&wt);
        let common = crate::share::canon(&repo.join(".git"));
        let k = |p: &Path| p.to_string_lossy().to_string();

        assert_eq!(worktree_candidates(&wt, Some(&common)), vec![k(&wt), k(&repo)]);
        assert_eq!(worktree_candidates(&wt, Some(&common)), trust_candidates(&wt), "the keys git finds for an honest worktree");
        git(&base, &["init", "-q", "fake"]);
        fs::write(wt.join(".git"), format!("gitdir: {}\n", base.join("fake/.git").display())).unwrap();
        assert_eq!(trust_candidates(&wt), vec![k(&wt)], "git follows the forged file");
        assert_eq!(worktree_candidates(&wt, Some(&common)), vec![k(&wt), k(&repo)], "maiTerm doesn't");
        assert_eq!(worktree_candidates(&wt, None), vec![k(&wt)]);
        let _ = fs::remove_dir_all(&base);
    }
}
