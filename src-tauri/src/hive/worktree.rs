//! A worker's folders on disk (docs/hive.md §6.2, §6.4, §6.5): a worktree of each git folder
//! its task needs, on its own branch off the landing target's tip, the gitignored files a
//! repo's `.worktreeinclude` names, and a slim seed of a Rust target dir.
//!
//! Everything here blocks on git or the disk: callers run it under `spawn_blocking`.

use std::collections::BTreeMap;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use super::git::{clean_failed_add, git_in, roll_back, run, Added, QUERY_TIMEOUT, WORKTREE_TIMEOUT};
use super::paths::{is_component, is_reserved_folder_name, HivePaths, RESERVED_SLUGS};
use super::record::{Hive, HiveFolder, KIND_GIT, KIND_PLAIN};
use super::worker::WorkerFolder;

pub const INCLUDE_FILE: &str = ".worktreeinclude";
const INCLUDE_MAX_BYTES: u64 = 64 * 1024;
const INCLUDE_MAX_FILES: usize = 200;

/// `hive/<hive-slug>--<worker-slug>`. Not `hive/<hive-slug>/<worker-slug>` as §6.4 first had
/// it: in the approval mode `hive/<hive-slug>` is itself a branch, and git can't hold a ref
/// both as a file and as a folder of refs. A slug never holds `--`, so the split is unambiguous.
pub fn worker_branch(hive: &Hive, slug: &str) -> String {
    format!("hive/{}--{slug}", hive.slug)
}

/// One git folder's add, worked out before anything is created.
struct Plan<'a> {
    folder: &'a HiveFolder,
    repo: PathBuf,
    worktree: PathBuf,
    /// The worker's own new branch, or None for a reviewer's detached copy.
    branch: Option<String>,
    target: Option<String>,
    sha: String,
}

fn check_slug(hive: &Hive, slug: &str) -> Result<(), String> {
    if RESERVED_SLUGS.contains(&slug) || !is_component(slug) || slug.contains("--") {
        return Err(format!("\"{slug}\" can't name a worker"));
    }
    if hive.slug.is_empty() {
        return Err("This Hive has no slug to name its branches".to_string());
    }
    Ok(())
}

/// Make everything one worker has on disk but its task folder: `wt.noindex/<slug>/` with its
/// scratch and temp folders, and a worktree of each named git folder (plain folders get none).
/// Refuses a slug whose folder or any of whose branches already exists (§11.3): a restart reuses
/// them, never this. If anything fails, what this call made is taken back and the human's
/// checkouts are untouched.
pub fn create_worker_worktrees(hive: &Hive, paths: &HivePaths, slug: &str, folders: &[String]) -> Result<Vec<WorkerFolder>, String> {
    check_slug(hive, slug)?;
    let mut chosen: Vec<&HiveFolder> = Vec::new();
    for name in folders {
        let f = hive.folders.iter().find(|f| f.name == *name).ok_or_else(|| format!("This Hive has no folder called \"{name}\""))?;
        if chosen.iter().any(|c| c.name == f.name) {
            return Err(format!("\"{name}\" is named twice"));
        }
        // Create checks this too, but a record saved before it ignored case wasn't held to it.
        if is_reserved_folder_name(&f.name) {
            return Err(format!("\"{name}\" can't be a folder name: a worker's own folder has it"));
        }
        chosen.push(f);
    }
    let dir = paths.worker_dir(slug);
    if fs::symlink_metadata(&dir).is_ok() {
        return Err(format!("A worker called \"{slug}\" already has folders at {}", dir.display()));
    }

    // Every refusal before anything exists, so a refusal leaves nothing to take back.
    let mut plans: Vec<Plan> = Vec::new();
    for f in chosen.iter().copied().filter(|f| f.is_git()) {
        let repo = PathBuf::from(&f.path);
        let branch = worker_branch(hive, slug);
        let exists = run(git_in(&repo).args(["rev-parse", "--verify", "--quiet"]).arg(format!("refs/heads/{branch}")), QUERY_TIMEOUT);
        if exists.is_ok() {
            return Err(format!("{}: a branch named \"{branch}\" already exists", f.name));
        }
        // Off the landing target's current tip, so a later wave starts with what has landed.
        let (target, rev) = if hive.is_integration() {
            let b = hive.integration_branch();
            (Some(b.clone()), format!("refs/heads/{b}"))
        } else {
            (f.base.clone(), f.base.clone().unwrap_or_else(|| "HEAD".to_string()))
        };
        let sha = run(git_in(&repo).args(["rev-parse", "--verify", "--quiet"]).arg(format!("{rev}^{{commit}}")), QUERY_TIMEOUT)
            .map_err(|_| format!("{}: no branch or commit \"{}\"", f.name, target.as_deref().unwrap_or("HEAD")))?;
        plans.push(Plan { folder: f, repo, worktree: paths.worker_worktree(slug, &f.name), branch: Some(branch), target, sha });
    }
    make_worktrees(hive, paths, slug, &chosen, plans)
}

/// A reviewer's folders (§7.7): for each of the builder's git folders, a detached worktree at
/// the builder's branch tip as it is now, pinned there, so a later builder commit never moves
/// what is being reviewed. No branch is made: the reviewer's sandbox gets no ref entries, and
/// nothing it commits in its copy can reach the builder. Its plain folders are the builder's.
/// Taken back whole on a failure, as a worker's. Blocks on git.
pub fn create_review_worktrees(hive: &Hive, paths: &HivePaths, slug: &str, builder: &super::worker::WorkerRecord) -> Result<Vec<WorkerFolder>, String> {
    check_slug(hive, slug)?;
    let mut chosen: Vec<&HiveFolder> = Vec::new();
    for bf in &builder.folders {
        let f = hive.folders.iter().find(|f| f.name == bf.name).ok_or_else(|| format!("This Hive has no folder called \"{}\" now", bf.name))?;
        if is_reserved_folder_name(&f.name) {
            return Err(format!("\"{}\" can't be a folder name: a worker's own folder has it", f.name));
        }
        if f.is_git() != bf.worktree.is_some() {
            return Err(format!("{}: the builder's folder and the Hive's no longer agree on what it is", f.name));
        }
        chosen.push(f);
    }
    let dir = paths.worker_dir(slug);
    if fs::symlink_metadata(&dir).is_ok() {
        return Err(format!("A worker called \"{slug}\" already has folders at {}", dir.display()));
    }
    let mut plans: Vec<Plan> = Vec::new();
    for f in chosen.iter().copied().filter(|f| f.is_git()) {
        let bf = builder.folders.iter().find(|b| b.name == f.name).expect("chosen from the builder's folders");
        let sha = branch_tip(f, bf)?;
        plans.push(Plan { folder: f, repo: PathBuf::from(&f.path), worktree: paths.worker_worktree(slug, &f.name), branch: None, target: bf.target.clone(), sha });
    }
    make_worktrees(hive, paths, slug, &chosen, plans)
}

/// The builder's branch tip in one folder. Read from the repo's refs, never through the
/// builder's worktree, whose `.git` it writes, as a verifying read: a reviewer's pins and the
/// verdict's staleness check are both this. Blocks on git.
fn branch_tip(f: &HiveFolder, bf: &WorkerFolder) -> Result<String, String> {
    let branch = bf.branch.as_deref().ok_or_else(|| format!("{}: the builder has no branch here to review", f.name))?;
    run(super::git::verify_in(Path::new(&f.path)).args(["rev-parse", "--verify", "--quiet"]).arg(format!("refs/heads/{branch}^{{commit}}")), QUERY_TIMEOUT)
        .map_err(|_| format!("{}: the builder's branch \"{branch}\" is gone", f.name))
}

/// The builder's branch tips now, folder → sha: what a reviewer's `reviewed` must still equal
/// for its verdict to be about the builder's work. Blocks on git.
pub fn builder_tips(hive: &Hive, builder: &super::worker::WorkerRecord) -> Result<BTreeMap<String, String>, String> {
    let mut out = BTreeMap::new();
    for bf in builder.folders.iter().filter(|b| b.worktree.is_some()) {
        let f = hive.folders.iter().find(|f| f.name == bf.name && f.is_git()).ok_or_else(|| format!("This Hive has no repo called \"{}\" now", bf.name))?;
        out.insert(bf.name.clone(), branch_tip(f, bf)?);
    }
    Ok(out)
}

/// Claims `wt.noindex/<slug>/`, makes its scratch and temp folders and adds each planned
/// worktree, in `chosen`'s order (a plain folder gets none). Takes everything back on a failure.
fn make_worktrees(hive: &Hive, paths: &HivePaths, slug: &str, chosen: &[&HiveFolder], plans: Vec<Plan>) -> Result<Vec<WorkerFolder>, String> {
    let dir = paths.worker_dir(slug);
    fs::create_dir_all(paths.worktrees()).map_err(|e| format!("{}: {e}", paths.worktrees().display()))?;
    // `create_dir`, not `_all`: it's the claim on the slug, and a second spawn racing this one
    // for it gets AlreadyExists here.
    fs::create_dir(&dir).map_err(|e| format!("A worker called \"{slug}\" can't have folders at {}: {e}", dir.display()))?;
    let mut added: Vec<Added> = Vec::new();
    let fail = |added: &[Added], msg: String| -> Result<Vec<WorkerFolder>, String> {
        if roll_back(added) {
            if let Err(e) = fs::remove_dir_all(&dir) {
                log::warn!("hive {}: worker folder {} left in place: {e}", hive.id, dir.display());
            }
        } else {
            log::warn!("hive {}: worker folder {} kept: a worktree in it couldn't be removed", hive.id, dir.display());
        }
        Err(msg)
    };
    for d in [paths.worker_scratch(slug), paths.worker_tmp(slug)] {
        if let Err(e) = fs::create_dir(&d) {
            return fail(&added, format!("{}: {e}", d.display()));
        }
    }

    let mut out: Vec<WorkerFolder> = Vec::new();
    let mut plans = plans.into_iter();
    for f in chosen.iter().copied() {
        if !f.is_git() {
            out.push(WorkerFolder { name: f.name.clone(), kind: KIND_PLAIN.to_string(), ..WorkerFolder::default() });
            continue;
        }
        let p = plans.next().expect("a plan per git folder, in order");
        // git checks out into an existing empty folder, so a name that is another folder's on a
        // case-insensitive volume would put the worktree inside it.
        if fs::symlink_metadata(&p.worktree).is_ok() {
            return fail(&added, format!("{}: {} already exists", p.folder.name, p.worktree.display()));
        }
        let mut cmd = git_in(&p.repo);
        cmd.args(["worktree", "add", "--quiet"]);
        match &p.branch {
            Some(b) => cmd.args(["-b", b]),
            None => cmd.arg("--detach"),
        };
        cmd.arg(&p.worktree).arg(&p.sha);
        if let Err(e) = run(&mut cmd, WORKTREE_TIMEOUT) {
            // git makes the `-b` branch before the checkout, and a killed add leaves its folder.
            clean_failed_add(&p.repo, &p.worktree, p.branch.as_deref().map(|b| (b, p.sha.as_str())));
            return fail(&added, format!("{}: {e}", p.folder.name));
        }
        added.push(Added { repo: p.repo.clone(), worktree: p.worktree.clone(), branch: p.branch.clone().map(|b| (b, p.sha.clone())) });
        let dirs = run(
            git_in(&p.worktree).args(["rev-parse", "--path-format=absolute", "--git-common-dir", "--git-dir"]),
            QUERY_TIMEOUT,
        );
        let (common, admin) = match dirs.as_deref().map(|d| d.lines().map(|l| crate::share::canon(Path::new(l.trim()))).collect::<Vec<_>>()) {
            Ok(v) if v.len() == 2 => (v[0].to_string_lossy().to_string(), v[1].to_string_lossy().to_string()),
            Ok(_) => return fail(&added, format!("{}: git didn't name the worktree's git folders", p.folder.name)),
            Err(e) => return fail(&added, format!("{}: {e}", p.folder.name)),
        };
        if let Err(e) = copy_worktree_include(&p.repo, &p.worktree) {
            return fail(&added, format!("{}: {INCLUDE_FILE}: {e}", p.folder.name));
        }
        log::info!(
            "hive {}: worker {slug} worktree of {} at {} on {}",
            hive.id,
            p.repo.display(),
            p.worktree.display(),
            p.branch.as_deref().map_or_else(|| format!("{} (detached)", p.sha), str::to_string)
        );
        out.push(WorkerFolder {
            name: p.folder.name.clone(),
            kind: KIND_GIT.to_string(),
            worktree: Some(p.worktree.to_string_lossy().to_string()),
            branch: p.branch,
            target: p.target,
            base_sha: Some(p.sha),
            common_dir: Some(common),
            admin_dir: Some(admin),
            ..WorkerFolder::default()
        });
    }
    Ok(out)
}

/// Take back what `create_worker_worktrees` made, for a spawn that didn't happen: each worktree,
/// each branch while it still points where it was made (a branch with work on it stays), then
/// the worker's folder. Errs, keeping the folder, if anything stayed.
pub fn remove_worker_worktrees(hive: &Hive, paths: &HivePaths, slug: &str, folders: &[WorkerFolder]) -> Result<(), String> {
    if RESERVED_SLUGS.contains(&slug) || !is_component(slug) {
        return Err(format!("\"{slug}\" can't name a worker"));
    }
    let mut added: Vec<Added> = Vec::new();
    for f in folders {
        let Some(wt) = &f.worktree else { continue };
        let repo = hive.folders.iter().find(|h| h.name == f.name).map(|h| PathBuf::from(&h.path));
        let Some(repo) = repo else { return Err(format!("This Hive has no folder called \"{}\"", f.name)) };
        let branch = f.branch.clone().zip(f.base_sha.clone());
        added.push(Added { repo, worktree: PathBuf::from(wt), branch });
    }
    if !roll_back(&added) {
        return Err(format!("Some of worker {slug}'s worktrees or branches stayed; the log names them"));
    }
    let dir = paths.worker_dir(slug);
    match fs::remove_dir_all(&dir) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(format!("{}: {e}", dir.display())),
    }
}

/// Copy the gitignored files a repo's `.worktreeinclude` names from the human's checkout into a
/// fresh worktree (§6.4), with Claude's rule that only ignored files are copied, never tracked
/// ones. One entry per line, a path from the repo's top (a leading `/` is allowed); `*` and `?`
/// may appear in the last part only. Unlike gitignore, a name without a slash is the top-level
/// file only; when nothing is there the line is logged. Fuller syntax (`**`, `!`, folders) isn't
/// read: those lines are skipped, logged. Nothing is followed through a symlink on either side,
/// nothing outside the repo is read, and nothing in the worktree is overwritten.
/// Errs only when copying a file that qualified fails.
pub fn copy_worktree_include(repo: &Path, wt: &Path) -> Result<Vec<String>, String> {
    let list = repo.join(INCLUDE_FILE);
    let meta = match fs::symlink_metadata(&list) {
        Ok(m) => m,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(e.to_string()),
    };
    if !meta.is_file() || meta.len() > INCLUDE_MAX_BYTES {
        log::warn!("hive: {} isn't a plain file under 64 KB; not read", list.display());
        return Ok(Vec::new());
    }
    let text = fs::read_to_string(&list).map_err(|e| e.to_string())?;
    let mut candidates: Vec<String> = Vec::new();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        match expand_entry(repo, line) {
            // Gitignore would look for a slashless name at any depth; this reads the top only.
            Ok(found) if found.is_empty() && !line.contains('/') => log::warn!(
                "hive: {}: \"{line}\" matched nothing at the repo's top; deeper folders aren't searched, so write the path from the top",
                list.display()
            ),
            Ok(found) => {
                for rel in found {
                    if !candidates.contains(&rel) {
                        candidates.push(rel);
                    }
                }
            }
            Err(why) => log::warn!("hive: {}: \"{line}\" skipped: {why}", list.display()),
        }
    }
    if candidates.len() > INCLUDE_MAX_FILES {
        log::warn!("hive: {} names {} files; the first {INCLUDE_MAX_FILES} are copied", list.display(), candidates.len());
        candidates.truncate(INCLUDE_MAX_FILES);
    }
    let mut copied = Vec::new();
    for rel in ignored(repo, &candidates)? {
        match copy_one(repo, wt, &rel) {
            Ok(true) => copied.push(rel),
            Ok(false) => {}
            Err(e) => return Err(format!("{rel}: {e}")),
        }
    }
    if !copied.is_empty() {
        log::info!("hive: copied {} gitignored file(s) into {}", copied.len(), wt.display());
    }
    Ok(copied)
}

fn has_glob(s: &str) -> bool {
    s.contains(['*', '?'])
}

/// The repo-relative files one `.worktreeinclude` line names that exist as plain files.
fn expand_entry(repo: &Path, line: &str) -> Result<Vec<String>, &'static str> {
    if line.starts_with('!') || line.contains("**") || line.contains(['[', '\\', '\0']) {
        return Err("only paths, with * and ? in the last part, are read");
    }
    let rel = line.strip_prefix('/').unwrap_or(line);
    if rel.ends_with('/') {
        return Err("folders aren't copied; name the files");
    }
    let parts: Vec<&str> = rel.split('/').collect();
    if parts.iter().any(|p| p.is_empty() || *p == "." || *p == "..") {
        return Err("a path must stay inside the repo");
    }
    let (last, dirs) = parts.split_last().ok_or("empty")?;
    if dirs.iter().any(|d| has_glob(d)) {
        return Err("* and ? may appear in the last part only");
    }
    if !has_glob(last) {
        return Ok(real_file(repo, rel).map(|_| vec![rel.to_string()]).unwrap_or_default());
    }
    let parent = dirs.join("/");
    let Some(dir) = real_dir(repo, &parent) else { return Ok(Vec::new()) };
    let mut out: Vec<String> = fs::read_dir(dir)
        .map_err(|_| "unreadable folder")?
        .flatten()
        .filter(|e| e.file_type().is_ok_and(|t| t.is_file()))
        .filter_map(|e| e.file_name().to_str().map(str::to_string))
        .filter(|name| glob_match(last.as_bytes(), name.as_bytes()))
        .map(|name| if parent.is_empty() { name } else { format!("{parent}/{name}") })
        .collect();
    out.sort();
    Ok(out)
}

/// `*` and `?` over one path part.
fn glob_match(pat: &[u8], s: &[u8]) -> bool {
    match (pat.first(), s.first()) {
        (None, None) => true,
        (Some(b'*'), _) => glob_match(&pat[1..], s) || (!s.is_empty() && glob_match(pat, &s[1..])),
        (Some(b'?'), Some(_)) => glob_match(&pat[1..], &s[1..]),
        (Some(p), Some(c)) if p == c => glob_match(&pat[1..], &s[1..]),
        _ => false,
    }
}

/// `root/rel` when every part of it is a real folder (no symlink anywhere).
fn real_dir(root: &Path, rel: &str) -> Option<PathBuf> {
    let mut cur = root.to_path_buf();
    for part in rel.split('/').filter(|p| !p.is_empty()) {
        cur.push(part);
        let m = fs::symlink_metadata(&cur).ok()?;
        if !m.is_dir() {
            return None;
        }
    }
    Some(cur)
}

/// `root/rel` when it's a plain file reached through real folders only.
fn real_file(root: &Path, rel: &str) -> Option<PathBuf> {
    let (parent, name) = rel.rsplit_once('/').unwrap_or(("", rel));
    let p = real_dir(root, parent)?.join(name);
    fs::symlink_metadata(&p).ok().filter(|m| m.is_file()).map(|_| p)
}

/// The candidates git says the repo ignores. A tracked file is never reported ignored, so the
/// human's committed files never qualify.
/// One call per path, read by exit code alone: its output quotes unusual names, and a list is
/// a handful of files. Errs only when git can't be run; a path git won't answer for (one inside
/// a submodule exits 128) just doesn't qualify.
fn ignored(repo: &Path, candidates: &[String]) -> Result<Vec<String>, String> {
    let mut out = Vec::new();
    for c in candidates {
        let mut cmd = git_in(repo);
        cmd.args(["check-ignore", "-q", "--"]).arg(c);
        let res = crate::util::proc::output_timeout(&mut cmd, QUERY_TIMEOUT).map_err(|e| e.to_string())?;
        // 0: ignored; 1: not.
        match res.status.code() {
            Some(0) => out.push(c.clone()),
            Some(1) => {}
            _ => log::warn!(
                "hive: {}: \"{c}\" not copied: {}",
                repo.join(INCLUDE_FILE).display(),
                String::from_utf8_lossy(&res.stderr).trim()
            ),
        }
    }
    Ok(out)
}

/// One file into the worktree, created new (`O_EXCL`), through real folders it makes itself.
/// False when the worktree already has something there, or the source stopped qualifying.
fn copy_one(repo: &Path, wt: &Path, rel: &str) -> io::Result<bool> {
    let Some(src) = real_file(repo, rel) else { return Ok(false) };
    let (parent, name) = rel.rsplit_once('/').unwrap_or(("", rel));
    let mut dst_dir = wt.to_path_buf();
    for part in parent.split('/').filter(|p| !p.is_empty()) {
        dst_dir.push(part);
        match fs::symlink_metadata(&dst_dir) {
            Ok(m) if m.is_dir() => {}
            Ok(_) => return Ok(false),
            Err(e) if e.kind() == io::ErrorKind::NotFound => fs::create_dir(&dst_dir)?,
            Err(e) => return Err(e),
        }
    }
    let mut src_opts = fs::OpenOptions::new();
    src_opts.read(true);
    let mut dst_opts = fs::OpenOptions::new();
    dst_opts.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
        src_opts.custom_flags(libc::O_NOFOLLOW);
        // A `.env` is often 0600; the copy keeps that.
        dst_opts.mode(fs::symlink_metadata(&src)?.permissions().mode() & 0o777);
    }
    let mut from = src_opts.open(&src)?;
    let mut to = match dst_opts.open(dst_dir.join(name)) {
        Ok(f) => f,
        Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {
            log::warn!("hive: {rel} already exists in {}; not overwritten", wt.display());
            return Ok(false);
        }
        Err(e) => return Err(e),
    };
    io::copy(&mut from, &mut to)?;
    Ok(true)
}

/// The target dir to seed a worker's from: `<folder>/target/debug`, or one level down
/// (`<folder>/src-tauri/target/debug`), beside a `Cargo.toml`, holding `.fingerprint`.
pub fn seed_source(folder: &Path) -> Option<PathBuf> {
    cargo_dirs(folder).into_iter().map(|d| d.join("target").join("debug")).find(|debug| debug.join(".fingerprint").is_dir())
}

/// The folders holding a `Cargo.toml`: `folder` itself, then one level down, by name.
fn cargo_dirs(folder: &Path) -> Vec<PathBuf> {
    let mut out: Vec<PathBuf> = Vec::new();
    if folder.join("Cargo.toml").is_file() {
        out.push(folder.to_path_buf());
    }
    let Ok(rd) = fs::read_dir(folder) else { return out };
    let mut children: Vec<PathBuf> = rd
        .flatten()
        .filter(|e| e.file_type().is_ok_and(|t| t.is_dir()))
        .filter(|e| !e.file_name().to_string_lossy().starts_with('.') && e.file_name() != "node_modules" && e.file_name() != "target")
        .map(|e| e.path())
        .filter(|c| c.join("Cargo.toml").is_file())
        .collect();
    children.sort();
    out.extend(children);
    out
}

fn worker_human_folders<'a>(hive: &'a Hive, w: &'a super::worker::WorkerRecord) -> impl Iterator<Item = &'a Path> + 'a {
    w.git_folders().filter_map(|f| hive.folders.iter().find(|h| h.name == f.name && h.is_git())).map(|h| Path::new(&h.path))
}

/// The worker's seed: the human's built target of its first git folder that has one. One
/// `CARGO_TARGET_DIR` serves all its Rust repos, and the seed matters only for the first build.
pub fn worker_seed_source(hive: &Hive, w: &super::worker::WorkerRecord) -> Option<PathBuf> {
    worker_human_folders(hive, w).find_map(seed_source)
}

/// Whether the worker builds in its own `CARGO_TARGET_DIR`: as recorded at its first launch,
/// or, before that, whether any of its git folders is a Rust project. Never whether the human
/// has a built target right now, which comes and goes (`cargo clean`).
pub fn worker_uses_own_cargo_target(hive: &Hive, w: &super::worker::WorkerRecord) -> bool {
    match w.cargo_target.as_str() {
        super::worker::CARGO_TARGET_OWN => true,
        super::worker::CARGO_TARGET_NONE => false,
        _ => worker_human_folders(hive, w).any(|h| !cargo_dirs(h).is_empty()),
    }
}

/// Seed a worker's `CARGO_TARGET_DIR` (`dst`) from `src_debug` with APFS clones of only
/// `.fingerprint/`, `build/` and `deps/*.{rmeta,rlib,dylib,d}` (§6.5, spike 4: a clone of the
/// whole target was slower than a cold build). macOS only; elsewhere, and when `dst` already
/// has a `debug/`, it does nothing and answers false. A failed seed is removed again, so the
/// worker builds cold rather than from half a seed.
pub fn seed_cargo_target(src_debug: &Path, dst: &Path) -> Result<bool, String> {
    #[cfg(target_os = "macos")]
    {
        let dst_debug = dst.join("debug");
        if fs::symlink_metadata(&dst_debug).is_ok() || !src_debug.is_dir() {
            return Ok(false);
        }
        let res = seed_macos(src_debug, &dst_debug);
        if res.is_err() {
            let _ = fs::remove_dir_all(&dst_debug);
        }
        res.map(|()| true)
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = (src_debug, dst);
        Ok(false)
    }
}

#[cfg(target_os = "macos")]
fn seed_macos(src: &Path, dst_debug: &Path) -> Result<(), String> {
    use std::time::Duration;
    const CP_TIMEOUT: Duration = Duration::from_secs(600);
    let cp = |args: &mut dyn Iterator<Item = &std::ffi::OsStr>, into: &Path| -> Result<(), String> {
        let mut cmd = crate::util::proc::command("cp");
        cmd.args(["-c", "-p", "-R"]).args(args).arg(into);
        let out = crate::util::proc::output_timeout(&mut cmd, CP_TIMEOUT).map_err(|e| e.to_string())?;
        if !out.status.success() {
            return Err(format!("cp: {}", String::from_utf8_lossy(&out.stderr).trim()));
        }
        Ok(())
    };
    let deps = dst_debug.join("deps");
    fs::create_dir_all(&deps).map_err(|e| format!("{}: {e}", deps.display()))?;
    for d in [".fingerprint", "build"] {
        let s = src.join(d);
        if fs::symlink_metadata(&s).is_ok_and(|m| m.is_dir()) {
            cp(&mut std::iter::once(s.as_os_str()), dst_debug)?;
        }
    }
    let Ok(entries) = fs::read_dir(src.join("deps")) else { return Ok(()) };
    let files: Vec<PathBuf> = entries
        .flatten()
        .filter(|e| e.file_type().is_ok_and(|t| t.is_file()))
        .map(|e| e.path())
        .filter(|p| p.extension().and_then(|x| x.to_str()).is_some_and(|x| matches!(x, "rmeta" | "rlib" | "dylib" | "d")))
        .collect();
    for chunk in files.chunks(256) {
        cp(&mut chunk.iter().map(|p| p.as_os_str()), &deps)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hive::layout::create_layout;
    use crate::hive::record::{LANDING_BASE, LANDING_INTEGRATION};
    use std::process::Command;

    fn scratch(tag: &str) -> PathBuf {
        let t = std::env::temp_dir().join(format!("hive-worktree-{tag}-{}", uuid::Uuid::new_v4()));
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

    fn folder(name: &str, path: &Path, kind: &str) -> HiveFolder {
        HiveFolder {
            name: name.into(),
            path: path.to_string_lossy().into(),
            kind: kind.into(),
            base: (kind == KIND_GIT).then(|| "main".to_string()),
            primary: name == "web",
            ..HiveFolder::default()
        }
    }

    fn hive(mode: &str, folders: Vec<HiveFolder>) -> Hive {
        Hive { id: "h1".into(), slug: "ship-it".into(), landing_mode: mode.into(), folders, ..Hive::default() }
    }

    fn names(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn two_repos_land_on_the_worker_branch_at_base() {
        let base = scratch("base");
        let (web, api) = (repo(&base, "web"), repo(&base, "api"));
        let brand = base.join("brand");
        fs::create_dir_all(&brand).unwrap();
        let h = hive(LANDING_BASE, vec![folder("web", &web, KIND_GIT), folder("api", &api, KIND_GIT), folder("brand", &brand, KIND_PLAIN)]);
        let p = HivePaths::new(base.join("hive/h1"));
        let got = create_worker_worktrees(&h, &p, "auth", &names(&["web", "api", "brand"])).unwrap();

        assert_eq!(got.iter().map(|f| f.name.as_str()).collect::<Vec<_>>(), vec!["web", "api", "brand"]);
        for (f, r) in got.iter().zip([&web, &api]) {
            let wt = PathBuf::from(f.worktree.as_ref().unwrap());
            assert_eq!(wt, p.worker_worktree("auth", &f.name));
            assert_eq!(git(&wt, &["rev-parse", "--abbrev-ref", "HEAD"]), "hive/ship-it--auth");
            assert_eq!(git(&wt, &["rev-parse", "HEAD"]), git(r, &["rev-parse", "main"]));
            assert_eq!(f.branch.as_deref(), Some("hive/ship-it--auth"));
            assert_eq!(f.target.as_deref(), Some("main"));
            assert_eq!(f.base_sha.as_deref(), Some(git(r, &["rev-parse", "main"]).as_str()));
            assert_eq!(f.common_dir.as_deref(), Some(&*r.join(".git").to_string_lossy()));
            let admin = PathBuf::from(f.admin_dir.as_ref().unwrap());
            assert!(admin.starts_with(r.join(".git/worktrees")) && admin.join("gitdir").is_file(), "{}", admin.display());
        }
        assert_eq!((got[2].kind.as_str(), got[2].worktree.as_ref()), (KIND_PLAIN, None), "a plain folder gets no worktree");
        assert!(!p.worker_worktree("auth", "brand").exists());
        assert!(p.worker_scratch("auth").is_dir() && p.worker_tmp("auth").is_dir());
        assert_eq!(git(&web, &["rev-parse", "--abbrev-ref", "HEAD"]), "main", "the human's checkout stays on its branch");

        remove_worker_worktrees(&h, &p, "auth", &got).unwrap();
        assert!(!p.worker_dir("auth").exists());
        for r in [&web, &api] {
            assert!(git(r, &["branch", "--list", "hive/*"]).is_empty());
            assert!(!git(r, &["worktree", "list"]).contains("wt.noindex"));
        }
        let _ = fs::remove_dir_all(&base);
    }

    #[test]
    fn a_reviewers_worktree_is_detached_at_the_builders_tip_and_stays_there() {
        let base = scratch("review");
        let web = repo(&base, "web");
        let brand = base.join("brand");
        fs::create_dir_all(&brand).unwrap();
        let h = hive(LANDING_BASE, vec![folder("web", &web, KIND_GIT), folder("brand", &brand, KIND_PLAIN)]);
        let p = HivePaths::new(base.join("hive/h1"));
        let folders = create_worker_worktrees(&h, &p, "auth", &names(&["web", "brand"])).unwrap();
        let bwt = p.worker_worktree("auth", "web");
        fs::write(bwt.join("login.rs"), "v1").unwrap();
        git(&bwt, &["add", "-A"]);
        git(&bwt, &["commit", "-q", "--no-verify", "-m", "v1"]);
        let pinned = git(&bwt, &["rev-parse", "HEAD"]);
        let builder = super::super::worker::WorkerRecord { slug: "auth".into(), folders, ..Default::default() };

        let got = create_review_worktrees(&h, &p, "auth-review", &builder).unwrap();
        assert_eq!(got.iter().map(|f| (f.name.as_str(), f.kind.as_str())).collect::<Vec<_>>(), vec![("web", KIND_GIT), ("brand", KIND_PLAIN)]);
        let rwt = p.worker_worktree("auth-review", "web");
        assert_eq!((got[0].branch.as_ref(), got[0].base_sha.as_deref()), (None, Some(pinned.as_str())), "no ref of its own");
        assert_eq!(got[0].target.as_deref(), Some("main"));
        assert!(got[0].admin_dir.is_some() && got[0].common_dir.is_some());
        assert_eq!(git(&rwt, &["rev-parse", "HEAD"]), pinned);
        assert_eq!(git(&rwt, &["rev-parse", "--abbrev-ref", "HEAD"]), "HEAD", "detached");
        assert_eq!(fs::read_to_string(rwt.join("login.rs")).unwrap(), "v1");

        // The builder commits on: the review stays on what it was given.
        fs::write(bwt.join("login.rs"), "v2").unwrap();
        git(&bwt, &["commit", "-q", "-a", "--no-verify", "-m", "v2"]);
        assert_eq!(git(&rwt, &["rev-parse", "HEAD"]), pinned);
        // And nothing the reviewer commits in its copy reaches the builder's branch.
        git(&rwt, &["commit", "-q", "--allow-empty", "--no-verify", "-m", "reviewer"]);
        assert_ne!(git(&web, &["rev-parse", "hive/ship-it--auth"]), git(&rwt, &["rev-parse", "HEAD"]));
        assert!(git(&web, &["branch", "--list"]).lines().count() == 2, "main and the builder's branch only");

        remove_worker_worktrees(&h, &p, "auth-review", &got).unwrap();
        assert!(!p.worker_dir("auth-review").exists());
        assert!(!git(&web, &["branch", "--list", "hive/ship-it--auth"]).is_empty(), "the builder's branch is never the reviewer's to take back");

        let mut gone = builder.clone();
        gone.folders[0].branch = Some("hive/ship-it--nobody".into());
        assert!(create_review_worktrees(&h, &p, "r2", &gone).unwrap_err().contains("is gone"));
        assert!(!p.worker_dir("r2").exists(), "refused before anything was made");
        let _ = fs::remove_dir_all(&base);
    }

    #[test]
    fn integration_mode_branches_off_the_hive_branch_tip() {
        let base = scratch("integration");
        let web = repo(&base, "web");
        let h = hive(LANDING_INTEGRATION, vec![folder("web", &web, KIND_GIT)]);
        let root = base.join("hive/h1");
        create_layout(&root, &h).unwrap();
        // Something has landed on the Hive branch that Base doesn't have.
        let read_copy = HivePaths::new(&root).coordinator_worktree("web");
        fs::write(read_copy.join("landed.txt"), "x").unwrap();
        git(&read_copy, &["add", "-A"]);
        git(&read_copy, &["commit", "-q", "--no-verify", "-m", "landed"]);

        let p = HivePaths::new(&root);
        let got = create_worker_worktrees(&h, &p, "ui", &names(&["web"])).unwrap();
        let wt = p.worker_worktree("ui", "web");
        assert_eq!(git(&wt, &["rev-parse", "HEAD"]), git(&web, &["rev-parse", "hive/ship-it"]));
        assert_ne!(git(&wt, &["rev-parse", "HEAD"]), git(&web, &["rev-parse", "main"]));
        assert!(wt.join("landed.txt").is_file());
        assert_eq!(got[0].target.as_deref(), Some("hive/ship-it"));
        assert_eq!(git(&wt, &["rev-parse", "--abbrev-ref", "HEAD"]), "hive/ship-it--ui", "beside the Hive branch, not under it");
        let _ = fs::remove_dir_all(&base);
    }

    #[cfg(unix)]
    #[test]
    fn a_failure_in_the_second_repo_takes_back_the_first() {
        use std::os::unix::fs::PermissionsExt;
        let base = scratch("rollback");
        let (web, api) = (repo(&base, "web"), repo(&base, "api"));
        fs::write(web.join("draft.txt"), "the human's uncommitted work").unwrap();
        // api's add makes its branch, then can't make its admin dir.
        let admin = api.join(".git/worktrees");
        fs::create_dir_all(&admin).unwrap();
        fs::set_permissions(&admin, fs::Permissions::from_mode(0o555)).unwrap();
        let h = hive(LANDING_BASE, vec![folder("web", &web, KIND_GIT), folder("api", &api, KIND_GIT)]);
        let p = HivePaths::new(base.join("hive/h1"));
        let err = create_worker_worktrees(&h, &p, "auth", &names(&["web", "api"])).unwrap_err();
        fs::set_permissions(&admin, fs::Permissions::from_mode(0o755)).unwrap();

        assert!(err.starts_with("api: "), "{err}");
        assert!(!p.worker_dir("auth").exists(), "the worker's folder is gone");
        for r in [&web, &api] {
            assert!(git(r, &["branch", "--list", "hive/*"]).is_empty(), "no branch left in {}", r.display());
            assert!(!git(r, &["worktree", "list"]).contains("wt.noindex"));
            assert_eq!(git(r, &["rev-parse", "--abbrev-ref", "HEAD"]), "main");
        }
        assert_eq!(git(&web, &["status", "--porcelain"]), "?? draft.txt", "the human's checkout is untouched");

        // With the cause gone, the same spawn works.
        create_worker_worktrees(&h, &p, "auth", &names(&["web", "api"])).unwrap();
        let _ = fs::remove_dir_all(&base);
    }

    #[test]
    fn an_existing_slug_or_branch_is_refused_and_nothing_is_made() {
        let base = scratch("refuse");
        let (web, api) = (repo(&base, "web"), repo(&base, "api"));
        let h = hive(LANDING_BASE, vec![folder("web", &web, KIND_GIT), folder("api", &api, KIND_GIT)]);
        let p = HivePaths::new(base.join("hive/h1"));
        create_worker_worktrees(&h, &p, "auth", &names(&["web"])).unwrap();
        let err = create_worker_worktrees(&h, &p, "auth", &names(&["web"])).unwrap_err();
        assert!(err.contains("already has folders"), "{err}");

        // A branch left behind (the human's, or a removed worker's with work on it) is never reused.
        git(&api, &["branch", "hive/ship-it--billing"]);
        let err = create_worker_worktrees(&h, &p, "billing", &names(&["web", "api"])).unwrap_err();
        assert!(err.starts_with("api: a branch named"), "{err}");
        assert!(!p.worker_dir("billing").exists(), "refused before anything was made");
        assert!(git(&web, &["branch", "--list", "hive/ship-it--billing"]).is_empty());
        assert!(!git(&api, &["branch", "--list", "hive/ship-it--billing"]).is_empty(), "the existing branch stays");

        for (slug, folders) in [("coordinator", vec!["web"]), ("land", vec!["web"]), ("a--b", vec!["web"]), ("x", vec!["nope"]), ("y", vec!["web", "web"])] {
            assert!(create_worker_worktrees(&h, &p, slug, &names(&folders)).is_err(), "{slug} {folders:?}");
        }
        let missing = hive(LANDING_BASE, vec![HiveFolder { base: Some("gone".into()), ..folder("web", &web, KIND_GIT) }]);
        assert!(create_worker_worktrees(&missing, &p, "z", &names(&["web"])).unwrap_err().contains("no branch or commit"));
        assert!(!p.worker_dir("z").exists());
        let _ = fs::remove_dir_all(&base);
    }

    #[test]
    fn a_folder_name_that_is_a_worker_folder_in_another_case_is_refused() {
        let base = scratch("case");
        let (web, api) = (repo(&base, "web"), repo(&base, "api"));
        let p = HivePaths::new(base.join("hive/h1"));
        // Records older than Create's case-blind check: the spawn holds them to it itself.
        for name in ["Scratch", "Claude-Tmp", "cargo-target"] {
            let h = hive(LANDING_BASE, vec![folder(name, &web, KIND_GIT)]);
            let err = create_worker_worktrees(&h, &p, "auth", &names(&[name])).unwrap_err();
            assert!(err.contains("can't be a folder name"), "{err}");
            assert!(!p.worker_dir("auth").exists());
        }
        // Two folders one case apart are one folder on macOS's default volume: the second add
        // would land in the first's checkout.
        fs::write(base.join("probe"), "").unwrap();
        if base.join("PROBE").exists() {
            let h = hive(LANDING_BASE, vec![folder("web", &web, KIND_GIT), folder("Web", &api, KIND_GIT)]);
            let err = create_worker_worktrees(&h, &p, "auth", &names(&["web", "Web"])).unwrap_err();
            assert!(err.starts_with("Web: ") && err.contains("already exists"), "{err}");
            assert!(!p.worker_dir("auth").exists(), "the first worktree was taken back");
            assert!(git(&web, &["branch", "--list", "hive/*"]).is_empty());
        }
        let _ = fs::remove_dir_all(&base);
    }

    #[test]
    fn a_worktreeinclude_line_inside_a_submodule_doesnt_fail_the_spawn() {
        let base = scratch("submodule");
        let lib = repo(&base, "lib");
        let web = repo(&base, "web");
        git(&web, &["-c", "protocol.file.allow=always", "submodule", "add", "-q", &lib.to_string_lossy(), "vendor"]);
        fs::write(web.join(".gitignore"), ".env\n").unwrap();
        git(&web, &["add", "-A"]);
        git(&web, &["commit", "-q", "--no-verify", "-m", "sub"]);
        fs::write(web.join("vendor/.env"), "sub").unwrap();
        fs::write(web.join(".env"), "KEY=1\n").unwrap();
        // git check-ignore exits 128 for the first line ("is in submodule").
        fs::write(web.join(INCLUDE_FILE), "vendor/.env\n.env\n").unwrap();

        let h = hive(LANDING_BASE, vec![folder("web", &web, KIND_GIT)]);
        let p = HivePaths::new(base.join("hive/h1"));
        create_worker_worktrees(&h, &p, "auth", &names(&["web"])).unwrap();
        let wt = p.worker_worktree("auth", "web");
        assert_eq!(fs::read_to_string(wt.join(".env")).unwrap(), "KEY=1\n");
        assert!(!wt.join("vendor/.env").exists());
        let _ = fs::remove_dir_all(&base);
    }

    #[test]
    fn a_slashless_include_line_names_the_top_level_file_only() {
        let base = scratch("unanchored");
        fs::create_dir_all(base.join("apps/web")).unwrap();
        fs::write(base.join("apps/web/.env"), "deep").unwrap();
        assert!(expand_entry(&base, ".env").unwrap().is_empty(), "not gitignore's any-depth match");
        fs::write(base.join(".env"), "top").unwrap();
        assert_eq!(expand_entry(&base, ".env").unwrap(), vec![".env".to_string()]);
        assert_eq!(expand_entry(&base, "apps/web/.env").unwrap(), vec!["apps/web/.env".to_string()]);
        let _ = fs::remove_dir_all(&base);
    }

    #[test]
    fn a_scratch_only_worker_gets_just_its_own_folders() {
        let base = scratch("scratch-only");
        let brand = base.join("brand");
        fs::create_dir_all(&brand).unwrap();
        let h = hive(LANDING_BASE, vec![folder("brand", &brand, KIND_PLAIN)]);
        let p = HivePaths::new(base.join("hive/h1"));
        let got = create_worker_worktrees(&h, &p, "hero", &[]).unwrap();
        assert!(got.is_empty());
        assert!(p.worker_scratch("hero").is_dir());
        remove_worker_worktrees(&h, &p, "hero", &got).unwrap();
        assert!(!p.worker_dir("hero").exists());
        let _ = fs::remove_dir_all(&base);
    }

    #[cfg(unix)]
    #[test]
    fn worktreeinclude_copies_only_ignored_files_from_inside_the_repo() {
        use std::os::unix::fs::PermissionsExt;
        let base = scratch("include");
        let web = repo(&base, "web");
        fs::write(web.join(".gitignore"), ".env\n*.local\nsecrets/\nlink.env\nlinked/\n").unwrap();
        fs::write(web.join("tracked.txt"), "committed\n").unwrap();
        git(&web, &["add", "-A"]);
        git(&web, &["commit", "-q", "--no-verify", "-m", "ignore"]);
        fs::write(web.join(".env"), "KEY=1\n").unwrap();
        fs::set_permissions(web.join(".env"), fs::Permissions::from_mode(0o600)).unwrap();
        fs::write(web.join("a.local"), "a").unwrap();
        fs::write(web.join("b.local"), "b").unwrap();
        fs::create_dir_all(web.join("secrets")).unwrap();
        fs::write(web.join("secrets/key.pem"), "pem").unwrap();
        fs::write(web.join("untracked-not-ignored.txt"), "u").unwrap();
        fs::write(base.join("outside.env"), "never").unwrap();
        std::os::unix::fs::symlink(base.join("outside.env"), web.join("link.env")).unwrap();
        fs::create_dir_all(base.join("elsewhere")).unwrap();
        fs::write(base.join("elsewhere/x.env"), "never").unwrap();
        std::os::unix::fs::symlink(base.join("elsewhere"), web.join("linked")).unwrap();
        fs::write(
            web.join(INCLUDE_FILE),
            "# copied in\n.env\n/*.local\nsecrets/key.pem\ntracked.txt\nuntracked-not-ignored.txt\n../outside.env\n\
             link.env\nlinked/x.env\nsecrets/\n!keep\n**/deep\n",
        )
        .unwrap();

        let h = hive(LANDING_BASE, vec![folder("web", &web, KIND_GIT)]);
        let p = HivePaths::new(base.join("hive/h1"));
        create_worker_worktrees(&h, &p, "auth", &names(&["web"])).unwrap();
        let wt = p.worker_worktree("auth", "web");
        assert_eq!(fs::read_to_string(wt.join(".env")).unwrap(), "KEY=1\n");
        assert_eq!(fs::metadata(wt.join(".env")).unwrap().permissions().mode() & 0o777, 0o600);
        assert!(wt.join("a.local").is_file() && wt.join("b.local").is_file());
        assert_eq!(fs::read_to_string(wt.join("secrets/key.pem")).unwrap(), "pem");
        assert!(!wt.join("untracked-not-ignored.txt").exists(), "only ignored files");
        assert!(!wt.join("link.env").exists() && !wt.join("linked").exists(), "no symlink is followed");
        assert!(!base.join("hive/h1/wt.noindex/auth/outside.env").exists() && !wt.join("outside.env").exists());

        // Called again on the same worktree: nothing is overwritten, and a tracked file never qualifies.
        fs::write(wt.join(".env"), "the worker's own\n").unwrap();
        let again = copy_worktree_include(&web, &wt).unwrap();
        assert!(!again.contains(&".env".to_string()) && !again.contains(&"tracked.txt".to_string()), "{again:?}");
        assert_eq!(fs::read_to_string(wt.join(".env")).unwrap(), "the worker's own\n");
        assert_eq!(fs::read_to_string(wt.join("tracked.txt")).unwrap(), "committed\n");
        let _ = fs::remove_dir_all(&base);
    }

    #[test]
    fn include_entries_that_escape_or_use_unsupported_syntax_are_refused() {
        let root = Path::new("/nonexistent-hive-test");
        for bad in ["../x", "a/../../x", "a//b", "./x", "!x", "**/x", "a/*/b", "dir/", "[ab].env", "a\\b"] {
            assert!(expand_entry(root, bad).is_err(), "{bad}");
        }
        assert_eq!(expand_entry(root, "/.env").unwrap(), Vec::<String>::new(), "allowed, but nothing there");
        assert!(glob_match(b"*.local", b"a.local") && glob_match(b"?.env", b"x.env") && !glob_match(b"*.local", b"a.locals"));
    }

    #[test]
    fn the_seed_source_is_the_folder_or_one_level_down() {
        let base = scratch("seed-source");
        let app = base.join("app");
        fs::create_dir_all(app.join("src-tauri/target/debug/.fingerprint")).unwrap();
        fs::write(app.join("src-tauri/Cargo.toml"), "").unwrap();
        assert_eq!(seed_source(&app), Some(app.join("src-tauri/target/debug")));
        fs::create_dir_all(app.join("target/debug/.fingerprint")).unwrap();
        assert_eq!(seed_source(&app), Some(app.join("src-tauri/target/debug")), "no Cargo.toml at the top: not a crate");
        fs::write(app.join("Cargo.toml"), "").unwrap();
        assert_eq!(seed_source(&app), Some(app.join("target/debug")));
        assert_eq!(seed_source(&base.join("none")), None);
        let _ = fs::remove_dir_all(&base);
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn the_seed_copies_only_the_listed_parts() {
        let base = scratch("seed");
        let src = base.join("target/debug");
        for d in [".fingerprint/crate-1", "build/crate-1/out", "deps", "incremental/x", "examples"] {
            fs::create_dir_all(src.join(d)).unwrap();
        }
        for f in [".fingerprint/crate-1/lib", "build/crate-1/out/gen.rs", "deps/a.rlib", "deps/a.rmeta", "deps/a.d", "deps/b.dylib", "deps/a.o", "deps/app-1234", "incremental/x/q", "app"] {
            fs::write(src.join(f), f).unwrap();
        }
        let dst = base.join("hive/wt.noindex/w/cargo-target");
        assert!(seed_cargo_target(&src, &dst).unwrap());
        let d = dst.join("debug");
        for kept in [".fingerprint/crate-1/lib", "build/crate-1/out/gen.rs", "deps/a.rlib", "deps/a.rmeta", "deps/a.d", "deps/b.dylib"] {
            assert_eq!(fs::read_to_string(d.join(kept)).unwrap(), kept);
        }
        for gone in ["deps/a.o", "deps/app-1234", "incremental", "examples", "app"] {
            assert!(!d.join(gone).exists(), "{gone} isn't seeded");
        }
        assert!(!seed_cargo_target(&src, &dst).unwrap(), "seeded once: a second seed would overwrite the worker's builds");
        let _ = fs::remove_dir_all(&base);
    }
}
