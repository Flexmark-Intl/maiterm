//! The Hive's folder on disk (docs/hive.md §6.2): its fixed folders, the memory index seeded
//! with the goal (§9), and the coordinator's read copy of each git folder (§3 step 1, §6.4).
//!
//! Everything here blocks on git: callers run it under `spawn_blocking`.

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use serde::Serialize;

use super::git::{clean_failed_add, git_in, roll_back, run, Added, QUERY_TIMEOUT, WORKTREE_TIMEOUT};
use super::paths::{is_component, is_reserved_folder_name, HivePaths, COORDINATOR_SLUG};
use super::record::{Hive, KIND_GIT, KIND_PLAIN, LANDING_BASE, LANDING_INTEGRATION};

/// What the Create sheet shows for a folder the human picked (§3, §6.1, §6.6, §7.3).
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct FolderInfo {
    /// Canonical, symlinks resolved: what `HiveFolder.path` stores.
    pub path: String,
    /// `"git"` | `"plain"`. Git only at a repo's top level: a read copy is a checkout of the
    /// whole repo, so a folder below it would get one rooted somewhere else.
    pub kind: String,
    /// Set when the folder is inside a repo but isn't its top level: the sheet offers the whole
    /// repo instead. Plain until the human picks that (a dotfiles home must not become a repo).
    pub repo_top: Option<String>,
    /// The checked-out branch, the default Base. None when HEAD is detached or it's plain.
    pub base: Option<String>,
    /// Claude would skip the trust dialog here under every root asked about. A warning when
    /// false, never a refusal: trusting a repo is the human's act (§6.6).
    pub trusted: bool,
    pub local_remotes: Vec<String>,
}

/// `claude_json` is the roots the Hive's agents will launch under (the active account's, or
/// `~/.claude.json` without managed accounts).
pub fn detect_folder(path: &Path, claude_json: &[PathBuf]) -> FolderInfo {
    let real = crate::share::canon(path);
    let top = run(git_in(&real).args(["rev-parse", "--show-toplevel"]), QUERY_TIMEOUT).ok().filter(|t| !t.is_empty());
    let top = top.map(|t| crate::share::canon(Path::new(&t)));
    let (kind, base, local_remotes, repo_top) = match top {
        Some(top) if top == real => {
            let base = run(git_in(&real).args(["rev-parse", "--abbrev-ref", "HEAD"]), QUERY_TIMEOUT)
                .ok()
                .filter(|b| !b.is_empty() && b != "HEAD");
            let remotes = run(git_in(&real).args(["remote", "-v"]), QUERY_TIMEOUT).unwrap_or_default();
            (KIND_GIT, base, local_remote_paths(&remotes, &top), None)
        }
        Some(top) => (KIND_PLAIN, None, Vec::new(), Some(top.to_string_lossy().to_string())),
        None => (KIND_PLAIN, None, Vec::new(), None),
    };
    FolderInfo {
        path: real.to_string_lossy().to_string(),
        kind: kind.to_string(),
        repo_top,
        base,
        trusted: super::trust::trusted_in_all(claude_json, &real),
        local_remotes,
    }
}

/// The remotes in `git remote -v` output (fetch and push URLs both) that are paths on this
/// disk, resolved against the repo top level: a push there needs no network, so the sandbox's
/// network allowlist can't stop it and they go on `denyWrite` instead (§7.3).
pub fn local_remote_paths(remote_v: &str, top: &Path) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for line in remote_v.lines() {
        // `name<TAB>url (fetch)`, and the url may hold spaces: only the tab and the suffix delimit it.
        let Some((_, rest)) = line.split_once('\t') else { continue };
        let url = rest.rsplit_once(" (").map_or(rest, |(u, _)| u);
        let Some(p) = local_path(url, top) else { continue };
        let p = canon_or_parent(&p).to_string_lossy().to_string();
        if !out.contains(&p) {
            out.push(p);
        }
    }
    out.sort();
    out
}

/// The leaf may not exist yet; its parent's symlinks are still resolved.
fn canon_or_parent(p: &Path) -> PathBuf {
    if let Ok(c) = std::fs::canonicalize(p) {
        return crate::share::canon(&c);
    }
    match (p.parent(), p.file_name()) {
        (Some(dir), Some(leaf)) if std::fs::canonicalize(dir).is_ok() => crate::share::canon(dir).join(leaf),
        _ => p.to_path_buf(),
    }
}

/// Git URL-decodes a `file://` path (`my%20remote` pushes into `my remote`), so the path
/// denied must be the decoded one.
fn percent_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        let hex = |c: u8| (c as char).to_digit(16);
        if b[i] == b'%' && i + 2 < b.len() {
            if let (Some(h), Some(l)) = (hex(b[i + 1]), hex(b[i + 2])) {
                out.push((h * 16 + l) as u8);
                i += 3;
                continue;
            }
        }
        out.push(b[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn local_path(url: &str, top: &Path) -> Option<PathBuf> {
    if let Some(rest) = url.strip_prefix("file://") {
        // file:///abs, or file://localhost/abs
        let rest = rest.strip_prefix("localhost").unwrap_or(rest);
        return rest.starts_with('/').then(|| PathBuf::from(percent_decode(rest)));
    }
    if url.contains("://") {
        return None;
    }
    let drive = url.len() >= 3 && url.as_bytes()[1] == b':' && url.as_bytes()[0].is_ascii_alphabetic() && matches!(url.as_bytes()[2], b'\\' | b'/');
    if !drive {
        // scp-like `host:path`: a colon before the first slash.
        if let Some(colon) = url.find(':') {
            if !url[..colon].contains('/') {
                return None;
            }
        }
    }
    if let Some(rest) = url.strip_prefix("~/") {
        return dirs::home_dir().map(|h| h.join(rest));
    }
    let p = Path::new(url);
    Some(if p.is_absolute() || drive { p.to_path_buf() } else { top.join(p) })
}

/// The memory index as maiTerm creates it (§9, spike 5: two agents that both created it with
/// Write lost one's lines, so it exists before any agent does). The goal is the kept block's
/// first line; fence markers inside it are defused so the goal can never close a fence.
pub fn memory_seed(goal: &str) -> String {
    use super::memory::index::{kept_block, INDEX_BEGIN_LINE, INDEX_END_LINE};
    format!("{}{INDEX_BEGIN_LINE}\n{INDEX_END_LINE}\n", kept_block(&seed_kept(goal)))
}

/// The kept block maiTerm seeds: the goal, one line, fence markers defused.
pub fn seed_kept(goal: &str) -> Vec<String> {
    let goal = super::memory::index::defuse(&goal.split_whitespace().collect::<Vec<_>>().join(" "));
    vec![format!("- Goal: {goal}")]
}

/// What `create_layout` needs to be true before it touches the disk.
pub fn validate(hive: &Hive) -> Result<(), String> {
    if ![LANDING_BASE, LANDING_INTEGRATION].contains(&hive.landing_mode.as_str()) {
        return Err(format!("Unknown landing mode \"{}\"", hive.landing_mode));
    }
    let slug_ok = !hive.slug.is_empty()
        && hive.slug.len() <= 60
        && hive.slug.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
        && !hive.slug.starts_with('-')
        && !hive.slug.ends_with('-');
    if !slug_ok {
        return Err(format!("\"{}\" can't name a branch: use lowercase letters, digits and dashes", hive.slug));
    }
    let mut seen: Vec<&str> = Vec::new();
    for f in &hive.folders {
        // A worker's worktree of a folder sits beside its scratch and temp folders (§6.2).
        if !is_component(&f.name) || is_reserved_folder_name(&f.name) {
            return Err(format!("\"{}\" can't be a folder name", f.name));
        }
        // `Web` and `web` are one folder on a case-insensitive volume.
        if seen.iter().any(|s| s.eq_ignore_ascii_case(&f.name)) {
            return Err(format!("Two folders are called \"{}\"", f.name));
        }
        seen.push(&f.name);
        if ![KIND_GIT, KIND_PLAIN].contains(&f.kind.as_str()) {
            return Err(format!("{}: unknown folder kind \"{}\"", f.name, f.kind));
        }
        if f.base.as_deref().is_some_and(|b| b.is_empty() || b.starts_with('-')) {
            return Err(format!("{}: \"{}\" isn't a branch", f.name, f.base.as_deref().unwrap_or("")));
        }
    }
    Ok(())
}

/// Lay out a new Hive under `root`. Idempotent for the folders; never overwrites `MEMORY.md`;
/// refuses a read copy whose folder already exists. If a read copy fails, the ones this call
/// added are removed again (and the integration branches it created, while they still point
/// where it put them), and nothing else is touched.
pub fn create_layout(root: &Path, hive: &Hive) -> Result<(), String> {
    validate(hive)?;
    let p = HivePaths::new(root);
    for d in [
        p.memory(),
        p.gallery_files(),
        p.gallery_thumbs(),
        p.gallery_trash(),
        p.coordinator_task(),
        p.precedents(),
        // Shared by every Hive, on every agent's denyWrite: made here so the deny names a folder.
        p.global_precedents(),
        p.logs(),
        p.coordinator_home(),
        p.coordinator_tmp(),
        p.worktrees().join(COORDINATOR_SLUG),
    ] {
        fs::create_dir_all(&d).map_err(|e| format!("{}: {e}", d.display()))?;
    }
    match fs::OpenOptions::new().write(true).create_new(true).open(p.memory_index()) {
        Ok(mut f) => f.write_all(memory_seed(&hive.goal).as_bytes()).map_err(|e| format!("MEMORY.md: {e}"))?,
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(e) => return Err(format!("MEMORY.md: {e}")),
    }
    // Rust's copy of the human's kept block (plan D4): what a changed block is measured against.
    if super::memory::record::load(&p).is_none() {
        let rec = super::memory::record::MemoryRecord { v: super::memory::record::RECORD_VERSION, kept: seed_kept(&hive.goal), ..Default::default() };
        super::memory::record::save(&p, &rec)?;
    }

    let mut added: Vec<Added> = Vec::new();
    for f in hive.git_folders() {
        match add_read_copy(&p, hive, &f.name, Path::new(&f.path), f.base.as_deref()) {
            Ok(a) => added.push(a),
            Err(e) => {
                roll_back(&added);
                return Err(format!("{}: {e}", f.name));
            }
        }
    }
    Ok(())
}

fn add_read_copy(p: &HivePaths, hive: &Hive, name: &str, repo: &Path, base: Option<&str>) -> Result<Added, String> {
    let worktree = p.coordinator_worktree(name);
    if fs::symlink_metadata(&worktree).is_ok() {
        return Err(format!("{} already exists", worktree.display()));
    }
    // A read copy checks out the whole repo, so a folder below the top level would map wrong.
    let top = run(git_in(repo).args(["rev-parse", "--show-toplevel"]), QUERY_TIMEOUT)?;
    if crate::share::canon(Path::new(&top)) != crate::share::canon(repo) {
        return Err(format!("{} is inside the repo at {top}, not its top level", repo.display()));
    }
    let base = base.unwrap_or("HEAD");
    let sha = run(git_in(repo).args(["rev-parse", "--verify", "--quiet"]).arg(format!("{base}^{{commit}}")), QUERY_TIMEOUT)
        .map_err(|_| format!("no branch or commit \"{base}\""))?;
    let branch = hive.is_integration().then(|| hive.integration_branch());
    if let Some(b) = &branch {
        // Refused up front, so a branch there after a failed add is one this call made.
        if run(git_in(repo).args(["rev-parse", "--verify", "--quiet"]).arg(format!("refs/heads/{b}")), QUERY_TIMEOUT).is_ok() {
            return Err(format!("a branch named \"{b}\" already exists"));
        }
    }
    let mut cmd = git_in(repo);
    cmd.args(["worktree", "add", "--quiet"]);
    match &branch {
        Some(b) => cmd.args(["-b", b]),
        None => cmd.arg("--detach"),
    };
    if let Err(e) = run(cmd.arg(&worktree).arg(&sha), WORKTREE_TIMEOUT) {
        // git makes the `-b` branch before the checkout, and a killed add leaves its folder.
        clean_failed_add(repo, &worktree, branch.as_deref().map(|b| (b, sha.as_str())));
        return Err(e);
    }
    log::info!("hive {}: read copy of {} at {}", hive.id, repo.display(), worktree.display());
    Ok(Added { repo: repo.to_path_buf(), worktree, branch: branch.map(|b| (b, sha)) })
}

/// Take back a whole `create_layout` whose Hive was never saved (the workspace went away, or the
/// state write failed): its read copies, the integration branches while they still point where
/// the read copy stands, then the folder, which nothing else has seen yet.
pub fn undo_layout(root: &Path, hive: &Hive) {
    let p = HivePaths::new(root);
    let added: Vec<Added> = hive
        .git_folders()
        .filter_map(|f| {
            let worktree = p.coordinator_worktree(&f.name);
            fs::symlink_metadata(&worktree).ok()?;
            let branch = hive.is_integration().then(|| {
                let sha = run(git_in(&worktree).args(["rev-parse", "HEAD"]), QUERY_TIMEOUT).ok()?;
                Some((hive.integration_branch(), sha))
            });
            Some(Added { repo: PathBuf::from(&f.path), worktree, branch: branch.flatten() })
        })
        .collect();
    roll_back(&added);
    if let Err(e) = fs::remove_dir_all(root) {
        log::warn!("hive {}: unsaved Hive folder {} left in place: {e}", hive.id, root.display());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hive::record::HiveFolder;
    use std::process::Command;

    fn scratch(tag: &str) -> PathBuf {
        let t = std::env::temp_dir().join(format!("hive-layout-{tag}-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&t).unwrap();
        crate::share::canon(&t)
    }

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

    fn repo(base: &Path, name: &str) -> PathBuf {
        let r = base.join(name);
        fs::create_dir_all(&r).unwrap();
        git(&r, &["init", "-q", "-b", "main"]);
        fs::write(r.join("README.md"), "r\n").unwrap();
        git(&r, &["add", "-A"]);
        git(&r, &["commit", "-q", "--no-verify", "-m", "init"]);
        r
    }

    fn folder(name: &str, path: &Path, base: &str) -> HiveFolder {
        HiveFolder {
            name: name.into(),
            path: path.to_string_lossy().into(),
            kind: KIND_GIT.into(),
            base: Some(base.into()),
            primary: name == "web",
            ..HiveFolder::default()
        }
    }

    fn hive(mode: &str, folders: Vec<HiveFolder>) -> Hive {
        Hive { id: "h1".into(), slug: "ship-it".into(), goal: "Ship\nthe  <!-- thing -->".into(), landing_mode: mode.into(), folders, ..Hive::default() }
    }

    #[test]
    fn base_mode_lays_out_the_folders_and_a_detached_read_copy() {
        let base = scratch("base");
        let web = repo(&base, "web");
        let plain = base.join("brand");
        fs::create_dir_all(&plain).unwrap();
        let mut brand = folder("brand", &plain, "main");
        brand.kind = KIND_PLAIN.into();
        brand.base = None;
        let root = base.join("hive/h1");
        create_layout(&root, &hive(LANDING_BASE, vec![folder("web", &web, "main"), brand])).unwrap();

        let p = HivePaths::new(&root);
        for d in [p.memory(), p.gallery_files(), p.gallery_thumbs(), p.gallery_trash(), p.coordinator_task(), p.precedents(), p.logs(), p.coordinator_home(), p.coordinator_tmp()] {
            assert!(d.is_dir(), "{}", d.display());
        }
        let wt = p.coordinator_worktree("web");
        assert!(wt.join("README.md").is_file());
        assert_eq!(git(&wt, &["rev-parse", "--abbrev-ref", "HEAD"]), "HEAD", "detached");
        assert_eq!(git(&wt, &["rev-parse", "HEAD"]), git(&web, &["rev-parse", "main"]));
        assert!(!p.coordinator_worktree("brand").exists(), "a plain folder gets no read copy");
        assert!(git(&web, &["branch", "--list", "hive/*"]).is_empty(), "base mode makes no branch");

        let memory = fs::read_to_string(p.memory_index()).unwrap();
        assert_eq!(memory, memory_seed("Ship the <!-- thing -->"));
        assert!(memory.starts_with("<!-- maiterm:kept:begin"));
        assert!(memory.contains("- Goal: Ship the <! -- thing -- >\n<!-- maiterm:kept:end -->\n"));
        assert!(memory.contains("<!-- maiterm:index:begin — generated from the files; edit the files, not these lines -->\n<!-- maiterm:index:end -->"));
        let rec = crate::hive::memory::record::load(&p).expect("the memory record");
        assert_eq!(rec.kept, vec!["- Goal: Ship the <! -- thing -- >".to_string()], "the seed's kept block is the human's");
        assert_eq!(crate::hive::memory::index::split(&memory).unwrap().kept, rec.kept);

        // A second run keeps MEMORY.md and refuses to reuse the read copy.
        fs::write(p.memory_index(), "edited").unwrap();
        let again = create_layout(&root, &hive(LANDING_BASE, vec![folder("web", &web, "main")]));
        assert!(again.unwrap_err().contains("already exists"));
        assert_eq!(fs::read_to_string(p.memory_index()).unwrap(), "edited");
        let _ = fs::remove_dir_all(&base);
    }

    #[test]
    fn integration_mode_puts_the_read_copy_on_the_hive_branch() {
        let base = scratch("integration");
        let web = repo(&base, "web");
        let root = base.join("hive/h1");
        create_layout(&root, &hive(LANDING_INTEGRATION, vec![folder("web", &web, "main")])).unwrap();
        let wt = HivePaths::new(&root).coordinator_worktree("web");
        assert_eq!(git(&wt, &["rev-parse", "--abbrev-ref", "HEAD"]), "hive/ship-it");
        assert_eq!(git(&web, &["rev-parse", "hive/ship-it"]), git(&web, &["rev-parse", "main"]));
        let _ = fs::remove_dir_all(&base);
    }

    #[test]
    fn a_failure_takes_back_only_what_this_call_added() {
        let base = scratch("rollback");
        let web = repo(&base, "web");
        let api = repo(&base, "api");
        let root = base.join("hive/h1");
        let h = hive(LANDING_INTEGRATION, vec![folder("web", &web, "main"), folder("api", &api, "no-such-branch")]);
        let err = create_layout(&root, &h).unwrap_err();
        assert!(err.starts_with("api: "), "{err}");

        let p = HivePaths::new(&root);
        assert!(!p.coordinator_worktree("web").exists(), "web's read copy was removed");
        assert!(!git(&web, &["worktree", "list"]).contains("wt.noindex"));
        assert!(git(&web, &["branch", "--list", "hive/*"]).is_empty(), "the branch it created is gone");
        assert!(p.memory_index().is_file() && p.logs().is_dir(), "folders stay: nothing else is touched");
        assert_eq!(git(&web, &["rev-parse", "--abbrev-ref", "HEAD"]), "main", "the human's checkout is untouched");

        // With the cause fixed, the same call succeeds.
        let h = hive(LANDING_INTEGRATION, vec![folder("web", &web, "main"), folder("api", &api, "main")]);
        create_layout(&root, &h).unwrap();
        let _ = fs::remove_dir_all(&base);
    }

    #[test]
    fn an_unsaved_hive_is_taken_back_whole() {
        let base = scratch("undo");
        let web = repo(&base, "web");
        let root = base.join("hive/h1");
        let h = hive(LANDING_INTEGRATION, vec![folder("web", &web, "main")]);
        create_layout(&root, &h).unwrap();
        undo_layout(&root, &h);
        assert!(!root.exists());
        assert!(!git(&web, &["worktree", "list"]).contains("wt.noindex"));
        assert!(git(&web, &["branch", "--list", "hive/*"]).is_empty());
        let _ = fs::remove_dir_all(&base);
    }

    #[test]
    fn an_existing_hive_branch_is_refused_not_reused() {
        let base = scratch("branch");
        let web = repo(&base, "web");
        git(&web, &["branch", "hive/ship-it"]);
        let err = create_layout(&base.join("hive/h1"), &hive(LANDING_INTEGRATION, vec![folder("web", &web, "main")])).unwrap_err();
        assert!(err.starts_with("web: "), "{err}");
        assert!(!git(&web, &["branch", "--list", "hive/ship-it"]).is_empty(), "the human's branch stays");
        let _ = fs::remove_dir_all(&base);
    }

    #[cfg(unix)]
    #[test]
    fn a_failed_add_leaves_no_branch_or_folder_so_a_retry_works() {
        use std::os::unix::fs::PermissionsExt;
        let base = scratch("failed-add");
        let web = repo(&base, "web");
        // git creates the `-b` branch, then fails to make its admin dir.
        let admin = web.join(".git/worktrees");
        fs::create_dir_all(&admin).unwrap();
        fs::set_permissions(&admin, fs::Permissions::from_mode(0o555)).unwrap();
        let root = base.join("hive/h1");
        let h = hive(LANDING_INTEGRATION, vec![folder("web", &web, "main")]);
        let err = create_layout(&root, &h).unwrap_err();
        fs::set_permissions(&admin, fs::Permissions::from_mode(0o755)).unwrap();
        assert!(err.starts_with("web: "), "{err}");
        assert!(git(&web, &["branch", "--list", "hive/*"]).is_empty(), "the branch the failed add made is gone");
        assert!(!HivePaths::new(&root).coordinator_worktree("web").exists());

        create_layout(&root, &h).unwrap();
        let _ = fs::remove_dir_all(&base);
    }

    #[test]
    fn a_git_folder_below_the_repo_top_is_refused() {
        let base = scratch("subdir");
        let web = repo(&base, "web");
        let sub = web.join("packages/app");
        fs::create_dir_all(&sub).unwrap();
        let err = create_layout(&base.join("hive/h1"), &hive(LANDING_INTEGRATION, vec![folder("app", &sub, "main")])).unwrap_err();
        assert!(err.contains("not its top level"), "{err}");
        assert!(git(&web, &["branch", "--list", "hive/*"]).is_empty());
        let _ = fs::remove_dir_all(&base);
    }

    #[test]
    fn unsafe_names_and_slugs_are_refused_before_anything_is_written() {
        let base = scratch("validate");
        let root = base.join("hive/h1");
        let mut h = hive(LANDING_BASE, vec![folder("../escape", &base, "main")]);
        assert!(create_layout(&root, &h).is_err());
        for reserved in ["coordinator", "scratch", "claude-tmp", "cargo-target", "Scratch", "Claude-Tmp", "CARGO-TARGET", "Coordinator"] {
            h.folders = vec![folder(reserved, &base, "main")];
            assert!(create_layout(&root, &h).is_err(), "{reserved}");
        }
        h.folders = vec![folder("web", &base, "main"), folder("web", &base, "main")];
        assert!(create_layout(&root, &h).is_err());
        h.folders = vec![folder("web", &base, "main"), folder("Web", &base, "main")];
        assert!(create_layout(&root, &h).is_err(), "one folder on a case-insensitive volume");
        h.folders = vec![folder("web", &base, "--upload-pack=x")];
        assert!(create_layout(&root, &h).is_err());
        h.folders = vec![];
        h.slug = "Has Spaces".into();
        assert!(create_layout(&root, &h).is_err());
        h.slug = "ok".into();
        h.landing_mode = "someday".into();
        assert!(create_layout(&root, &h).is_err());
        assert!(!root.exists(), "nothing was laid out");
        let _ = fs::remove_dir_all(&base);
    }

    // Git for Windows names the top level with forward slashes; no Hive runs there (§14.16).
    #[cfg(unix)]
    #[test]
    fn detect_reads_kind_base_and_local_remotes() {
        let base = scratch("detect");
        let web = repo(&base, "web");
        let bare = base.join("bare.git");
        git(&base, &["init", "-q", "--bare", bare.to_str().unwrap()]);
        git(&web, &["remote", "add", "origin", "git@github.com:org/web.git"]);
        git(&web, &["remote", "add", "local", bare.to_str().unwrap()]);
        git(&web, &["remote", "add", "rel", "../bare.git"]);

        let info = detect_folder(&web, &[]);
        assert_eq!(info.kind, KIND_GIT);
        assert_eq!(info.base.as_deref(), Some("main"));
        assert_eq!(info.local_remotes, vec![crate::share::canon(&bare).to_string_lossy().to_string()], "network remotes aren't local; one path, once");
        assert!(!info.trusted, "no root asked: no evidence of trust");

        let plain = base.join("plain");
        fs::create_dir_all(&plain).unwrap();
        let info = detect_folder(&plain, &[]);
        assert_eq!((info.kind.as_str(), info.base, info.local_remotes.len(), info.repo_top), (KIND_PLAIN, None, 0, None));

        // Inside a repo but below its top: plain, with the repo offered.
        let sub = web.join("packages/app");
        fs::create_dir_all(&sub).unwrap();
        let info = detect_folder(&sub, &[]);
        assert_eq!(info.path, sub.to_string_lossy());
        assert_eq!((info.kind.as_str(), info.base, info.local_remotes.len()), (KIND_PLAIN, None, 0));
        assert_eq!(info.repo_top.as_deref(), Some(&*web.to_string_lossy()));
        let _ = fs::remove_dir_all(&base);
    }

    // POSIX paths.
    #[cfg(unix)]
    #[test]
    fn local_remote_forms() {
        let top = Path::new("/r/web");
        let v = "a\t/srv/x.git (fetch)\na\t/srv/x.git (push)\nb\tfile:///srv/y.git (fetch)\nc\thttps://h/z.git (fetch)\n\
                 d\tgit@h:z.git (fetch)\ne\th:z (fetch)\nf\t../sib.git (push)\ng\tssh://h/z (fetch)\n\
                 h\t/srv/my remote.git (fetch)\ni\tfile:///srv/b%20c.git (push)\nj\tfile://localhost/srv/d%2fe (fetch)\n";
        // A path that doesn't exist can't be canonicalized, so it stays as joined.
        assert_eq!(
            local_remote_paths(v, top),
            vec!["/r/web/../sib.git", "/srv/b c.git", "/srv/d/e", "/srv/my remote.git", "/srv/x.git", "/srv/y.git"]
        );
    }

    #[test]
    fn a_local_remote_with_a_space_or_percent_escape_resolves_to_the_real_repo() {
        let base = scratch("remote-space");
        let web = repo(&base, "web");
        let bare = base.join("my remote.git");
        git(&base, &["init", "-q", "--bare", bare.to_str().unwrap()]);
        git(&web, &["remote", "add", "sp", bare.to_str().unwrap()]);
        git(&web, &["remote", "add", "enc", &format!("file://{}/../my%20remote.git", web.display())]);
        let info = detect_folder(&web, &[]);
        assert_eq!(info.local_remotes, vec![bare.to_string_lossy().to_string()], "both name the same repo");
        let _ = fs::remove_dir_all(&base);
    }
}
