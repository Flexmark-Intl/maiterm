//! Which entries git ignores, from a cached stack of matchers (docs/file-tree.md §5.5).
//!
//! One `Gitignore` per folder's `.gitignore`, built once and kept until the watcher sees that
//! file change, stacked root → folder, then the repo's `info/exclude`, then the user's global
//! `core.excludesFile`. Never `WalkBuilder::parents(true)` per expand: that re-reads every
//! ancestor `.gitignore` on every call.
//!
//! Ignored entries are FLAGGED, not dropped, so "show ignored" is a client filter with no IO.
//! The user's `file_tree_exclude` lines are applied too, as gitignore patterns that no repo
//! file can un-ignore.
//!
//! Gitignore files apply only inside a repository, as git applies them: from the nearest folder
//! holding `.git` at or above the listed folder (bounded by the hub root), or, when the root
//! itself is inside a repo, from the root. A plain folder's stray `.gitignore` hides nothing.
//!
//! Every path handed to a matcher is relative to that matcher's own folder, built from rels, so
//! no matcher ever has to guess a prefix.
//!
//! Each matcher remembers the file it was built from as it was then (`Stamp`), so an edge can
//! find a `.gitignore` or `info/exclude` whose watcher event was lost (§6.1, `IgnoreStack::stamps`).

use std::collections::{BTreeSet, HashMap};
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use ignore::gitignore::{Gitignore, GitignoreBuilder};
use ignore::Match;

use super::paths::{join_rel, split_parent};

/// A rules file as it was when a matcher was read from it, taken just before the read.
#[derive(Debug, Clone, Copy, PartialEq)]
enum Stamp {
    Absent,
    At(SystemTime, u64),
    /// Its mtime can't tell a later edit apart (under 2 s old, so within a tick that may still
    /// hold it, or in the future: a file unzipped from a later timezone is hours ahead), so its
    /// bytes are kept too,
    /// as a hash: (mtime, length, content hash).
    Unsure(Option<SystemTime>, u64, u64),
}

fn content_hash(path: &Path) -> u64 {
    let mut h = std::collections::hash_map::DefaultHasher::new();
    std::fs::read(path).ok().hash(&mut h);
    h.finish()
}

fn stamp(path: &Path, now: SystemTime) -> Stamp {
    match std::fs::metadata(path) {
        Ok(m) if m.is_file() => match m.modified() {
            Ok(t) if super::hub::trusted_mtime(t, now) => Stamp::At(t, m.len()),
            t => Stamp::Unsure(t.ok(), m.len(), content_hash(path)),
        },
        _ => Stamp::Absent,
    }
}

/// Is the file at `path` no longer what `was` describes? An `Unsure` stamp is decided by the
/// bytes when the mtime and length still match: re-read, never taken as changed for being unsure.
fn changed(path: &Path, was: Stamp, now: SystemTime) -> bool {
    match was {
        Stamp::Unsure(t, len, hash) => match std::fs::metadata(path) {
            Ok(m) if m.is_file() => m.modified().ok() != t || m.len() != len || content_hash(path) != hash,
            _ => true,
        },
        _ => stamp(path, now) != was,
    }
}

/// What one folder contributes to the stack.
struct Folder {
    /// Its `.gitignore`, when it has one.
    gi: Option<Gitignore>,
    gi_stamp: Stamp,
    /// It holds `.git`: a repository starts here.
    repo_root: bool,
    /// That repository's `info/exclude`, when `repo_root`.
    info_exclude: Option<Gitignore>,
    /// The `info/exclude` file read and its stamp, when one was looked for.
    info_exclude_file: Option<(PathBuf, Stamp)>,
}

/// The rules files that decide what the open folders show, with their stamps, to compare
/// against the disk with no lock held (`IgnoreStack::stamps`, then `moved`).
pub struct Stamps {
    root: PathBuf,
    /// (folder rel, its `.gitignore`'s stamp, whether it held `.git`, its repository's
    /// `info/exclude` and stamp when one was looked for).
    folders: Vec<(String, Stamp, bool, Option<(PathBuf, Stamp)>)>,
    root_exclude: Option<(PathBuf, Stamp)>,
}

/// Tests: called as `Stamps::moved` starts, for a root whose path ends with the key, so a test
/// can see what is held while it reads the disk (the hub's ignore lock must not be).
#[cfg(test)]
#[allow(clippy::type_complexity)]
pub static MOVED_HOOKS: parking_lot::Mutex<Vec<(PathBuf, std::sync::Arc<dyn Fn() + Send + Sync>)>> = parking_lot::Mutex::new(Vec::new());

impl Stamps {
    /// Reads the disk. The folders whose rules changed since they were read, and whether the
    /// root repository's `info/exclude` did.
    pub fn moved(&self) -> (Vec<String>, bool) {
        #[cfg(test)]
        {
            let hooks: Vec<_> = MOVED_HOOKS.lock().iter().filter(|(r, _)| self.root.ends_with(r)).map(|h| h.1.clone()).collect();
            for h in hooks {
                h();
            }
        }
        let now = SystemTime::now();
        let folders = self
            .folders
            .iter()
            .filter(|(rel, gs, repo_root, ex)| {
                let Ok(abs) = super::paths::rel_to_native(&self.root, rel) else { return false };
                // The root's own `.git` is the hub's to watch (`repo_current`), not the stack's.
                let git_moved = !rel.is_empty() && std::fs::symlink_metadata(abs.join(".git")).is_ok() != *repo_root;
                changed(&abs.join(".gitignore"), *gs, now) || git_moved || ex.as_ref().is_some_and(|(p, s)| changed(p, *s, now))
            })
            .map(|f| f.0.clone())
            .collect();
        let root = self.root_exclude.as_ref().is_some_and(|(p, s)| changed(p, *s, now));
        (folders, root)
    }

    /// Folders whose rules files are checked.
    #[cfg(test)]
    pub fn len(&self) -> usize {
        self.folders.len()
    }
}

/// One folder's matchers, borrowed from the stack.
pub struct DirMatcher<'a> {
    dir: String,
    /// The folder itself is ignored: so is everything in it.
    ignored_dir: bool,
    user: Option<&'a Gitignore>,
    /// (folder rel, its .gitignore), deepest first.
    stack: Vec<(String, &'a Gitignore)>,
    /// (repo root rel, info/exclude).
    exclude: Option<(String, &'a Gitignore)>,
    /// (repo root rel, core.excludesFile).
    global: Option<(String, &'a Gitignore)>,
}

impl DirMatcher<'_> {
    pub fn is_ignored(&self, name: &str, is_dir: bool) -> bool {
        if self.ignored_dir {
            return true;
        }
        if self.user.is_none() && self.stack.is_empty() && self.exclude.is_none() && self.global.is_none() {
            return false;
        }
        let path = join_rel(&self.dir, name);
        if let Some(u) = self.user {
            if decide(u.matched(&path, is_dir)) == Some(true) {
                return true;
            }
        }
        for (base, gi) in self.stack.iter().chain(self.exclude.iter()).chain(self.global.iter()) {
            if let Some(v) = decide(gi.matched(rel_from(base, &path), is_dir)) {
                return v;
            }
        }
        false
    }
}

/// The hub's lock on its `IgnoreStack`. A plain mutex; in tests its guard also counts, per
/// thread, how many it holds (`held_here`), so a test can ask whether the thread it runs on
/// holds the lock, which `is_locked` (any thread) can't tell.
pub struct IgnoreLock(parking_lot::Mutex<IgnoreStack>);

#[cfg(not(test))]
pub type IgnoreGuard<'a> = parking_lot::MutexGuard<'a, IgnoreStack>;

impl IgnoreLock {
    pub fn new(stack: IgnoreStack) -> Self {
        IgnoreLock(parking_lot::Mutex::new(stack))
    }

    #[cfg(not(test))]
    pub fn lock(&self) -> IgnoreGuard<'_> {
        self.0.lock()
    }

    #[cfg(test)]
    pub fn lock(&self) -> IgnoreGuard<'_> {
        let g = self.0.lock();
        HELD.with(|h| h.set(h.get() + 1));
        IgnoreGuard(g)
    }
}

#[cfg(test)]
thread_local! {
    static HELD: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// Tests: whether the calling thread holds an `IgnoreLock`.
#[cfg(test)]
pub fn held_here() -> bool {
    HELD.with(|h| h.get() != 0)
}

#[cfg(test)]
pub struct IgnoreGuard<'a>(parking_lot::MutexGuard<'a, IgnoreStack>);

#[cfg(test)]
impl Drop for IgnoreGuard<'_> {
    fn drop(&mut self) {
        HELD.with(|h| h.set(h.get() - 1));
    }
}

#[cfg(test)]
impl std::ops::Deref for IgnoreGuard<'_> {
    type Target = IgnoreStack;
    fn deref(&self) -> &IgnoreStack {
        &self.0
    }
}

#[cfg(test)]
impl std::ops::DerefMut for IgnoreGuard<'_> {
    fn deref_mut(&mut self) -> &mut IgnoreStack {
        &mut self.0
    }
}

pub struct IgnoreStack {
    root: PathBuf,
    /// The root is inside a repository (its own top level or deeper).
    root_in_repo: bool,
    /// `info/exclude` of the repository the root is in, rooted at the root.
    root_exclude: Option<Gitignore>,
    /// That file and its stamp, when the root is in a repository.
    root_exclude_file: Option<(PathBuf, Stamp)>,
    /// `core.excludesFile`, read once per hub, rooted at the root.
    global: Option<Gitignore>,
    /// The user's `file_tree_exclude`.
    user: Option<Gitignore>,
    folders: HashMap<String, Folder>,
    /// Memo of "this folder is itself ignored", so its children are without a match each.
    dir_ignored: HashMap<String, bool>,
}

fn build_file(root: &Path, file: &Path) -> Option<Gitignore> {
    if !file.is_file() {
        return None;
    }
    let mut b = GitignoreBuilder::new(root);
    if let Some(e) = b.add(file) {
        log::debug!("filetree: {} partly unreadable: {e}", file.display());
    }
    b.build().ok().filter(|g| !g.is_empty())
}

fn build_lines(root: &Path, lines: &[String]) -> Option<Gitignore> {
    let mut b = GitignoreBuilder::new(root);
    for l in lines.iter().map(|l| l.trim()).filter(|l| !l.is_empty()) {
        if let Err(e) = b.add_line(None, l) {
            log::warn!("filetree: exclude pattern {l:?} ignored: {e}");
        }
    }
    b.build().ok().filter(|g| !g.is_empty())
}

/// The root repository's `info/exclude`: the file and its stamp, and its rules.
fn read_root_exclude(root: &Path, git_dir: Option<&Path>) -> (Option<(PathBuf, Stamp)>, Option<Gitignore>) {
    let Some(g) = git_dir else { return (None, None) };
    let file = g.join("info").join("exclude");
    let st = stamp(&file, SystemTime::now());
    let rules = build_file(root, &file);
    (Some((file, st)), rules)
}

/// A match as a verdict: Ignore = true, Whitelist (`!pattern`) = false, None = undecided.
fn decide<T>(m: Match<T>) -> Option<bool> {
    match m {
        Match::None => None,
        Match::Ignore(_) => Some(true),
        Match::Whitelist(_) => Some(false),
    }
}

/// `rel` relative to its ancestor folder `base` ("" = the root).
fn rel_from(base: &str, rel: &str) -> String {
    if base.is_empty() {
        rel.to_string()
    } else {
        rel.strip_prefix(base).and_then(|r| r.strip_prefix('/')).unwrap_or(rel).to_string()
    }
}

impl IgnoreStack {
    /// The stack for a hub. `git_dir` is the root's repository git dir when it is in one;
    /// `global` is `core.excludesFile` (None = read it the way git does).
    pub fn new(root: &Path, git_dir: Option<&Path>, user_excludes: &[String]) -> Self {
        let global = ignore::gitignore::gitconfig_excludes_path().and_then(|p| build_file(root, &p));
        Self::with(root, git_dir, global, user_excludes)
    }

    fn with(root: &Path, git_dir: Option<&Path>, global: Option<Gitignore>, user_excludes: &[String]) -> Self {
        let (root_exclude_file, root_exclude) = read_root_exclude(root, git_dir);
        IgnoreStack {
            root: root.to_path_buf(),
            root_in_repo: git_dir.is_some(),
            root_exclude,
            root_exclude_file,
            global,
            user: build_lines(root, user_excludes),
            folders: HashMap::new(),
            dir_ignored: HashMap::new(),
        }
    }

    /// A stack that reads nothing: a root on an ssh host, whose ignore rules the helper applies
    /// there (§15.2). A local file at the same path as a remote one must never decide a flag.
    pub fn inert(root: &Path) -> Self {
        Self::with(root, None, None, &[])
    }

    /// Test seam: a stack with an explicit global excludes file.
    #[cfg(test)]
    pub fn for_test(root: &Path, git_dir: Option<&Path>, global_file: Option<&Path>, user_excludes: &[String]) -> Self {
        let global = global_file.and_then(|f| build_file(root, f));
        Self::with(root, git_dir, global, user_excludes)
    }

    fn folder(&mut self, rel: &str) -> &Folder {
        if !self.folders.contains_key(rel) {
            let abs = super::paths::rel_to_native(&self.root, rel).unwrap_or_else(|_| self.root.clone());
            let dot_git = abs.join(".git");
            // A nested repository (`.git` here, below the root). The root's own repo is covered
            // by `root_exclude`; `.git` as a FILE is a linked worktree or submodule, whose
            // info/exclude lives elsewhere and is skipped.
            let repo_root = !rel.is_empty() && std::fs::symlink_metadata(&dot_git).is_ok();
            // Stamped before each read: an edit during the read leaves a stamp that differs.
            let now = SystemTime::now();
            let (info_exclude, info_exclude_file) = if repo_root && dot_git.is_dir() {
                let file = dot_git.join("info").join("exclude");
                let st = stamp(&file, now);
                (build_file(&abs, &file), Some((file, st)))
            } else {
                (None, None)
            };
            let gi_file = abs.join(".gitignore");
            let gi_stamp = stamp(&gi_file, now);
            let gi = build_file(&abs, &gi_file);
            self.folders.insert(rel.to_string(), Folder { gi, gi_stamp, repo_root, info_exclude, info_exclude_file });
        }
        &self.folders[rel]
    }

    /// Is `name` in folder `dir` ignored?
    pub fn is_ignored(&mut self, dir: &str, name: &str, is_dir: bool) -> bool {
        self.for_dir(dir).is_ignored(name, is_dir)
    }

    /// Everything that decides for one folder's entries, resolved once: a 50k-entry folder
    /// then costs one match per matcher per entry, not a walk up the stack per entry.
    pub fn for_dir(&mut self, dir: &str) -> DirMatcher<'_> {
        let ignored_dir = self.dir_is_ignored(dir);
        self.matcher(dir, ignored_dir)
    }

    fn matcher(&mut self, dir: &str, ignored_dir: bool) -> DirMatcher<'_> {
        // The folders from `dir` up to where its repository starts, loaded first (mutably).
        let mut chain = Vec::new();
        let mut cur = dir.to_string();
        let mut repo_base: Option<String> = None;
        loop {
            let repo_root = self.folder(&cur).repo_root;
            chain.push(cur.clone());
            if repo_root {
                repo_base = Some(cur.clone());
                break;
            }
            if cur.is_empty() {
                break;
            }
            cur = split_parent(&cur).0.to_string();
        }
        let this = &*self;
        let in_repo = repo_base.is_some() || this.root_in_repo;
        // Deepest first: a nested .gitignore overrides its parents, the first decision wins.
        let stack = if in_repo { chain.iter().filter_map(|f| this.folders[f].gi.as_ref().map(|g| (f.clone(), g))).collect() } else { Vec::new() };
        let base = repo_base.clone().unwrap_or_default();
        let exclude = match (&repo_base, in_repo) {
            (_, false) => None,
            (Some(b), true) => this.folders[b].info_exclude.as_ref(),
            (None, true) => this.root_exclude.as_ref(),
        };
        DirMatcher {
            dir: dir.to_string(),
            ignored_dir,
            user: this.user.as_ref(),
            stack,
            exclude: exclude.map(|g| (base.clone(), g)),
            global: if in_repo { this.global.as_ref().map(|g| (base, g)) } else { None },
        }
    }

    /// Is the folder `rel` itself ignored (so everything under it is)?
    pub fn dir_is_ignored(&mut self, rel: &str) -> bool {
        if rel.is_empty() {
            return false;
        }
        if let Some(&v) = self.dir_ignored.get(rel) {
            return v;
        }
        let (parent, name) = split_parent(rel);
        let (parent, name) = (parent.to_string(), name.to_string());
        let v = self.dir_is_ignored(&parent) || self.matcher(&parent, false).is_ignored(&name, true);
        self.dir_ignored.insert(rel.to_string(), v);
        v
    }

    /// The `.gitignore` (or `.git`) in folder `rel` changed: forget that folder's matcher and
    /// every memoised folder verdict. The caller re-lists the folders under it.
    pub fn invalidate_folder(&mut self, rel: &str) {
        self.folders.remove(rel);
        self.dir_ignored.clear();
    }

    /// The rules files to check on an edge, with their stamps, for `Stamps::moved` (no lock
    /// held while it reads the disk).
    ///
    /// Only the rules that decide what `open` folders show: each open folder's and each of its
    /// ancestors' (a folder's rows are matched from it up to its repository, and whether it is
    /// itself ignored from its parents), plus the root's `info/exclude`. A folder the stack read
    /// for another reason (a worktree event checked for git or Find) is left out, so an edge
    /// costs open folders × depth, whatever else the stack holds. Nothing is built from paths
    /// here, under the lock: `moved` does that.
    pub fn stamps<'a>(&self, open: impl IntoIterator<Item = &'a String>) -> Stamps {
        let mut chain: BTreeSet<&str> = BTreeSet::new();
        for rel in open {
            let mut cur = rel.as_str();
            while chain.insert(cur) && !cur.is_empty() {
                cur = split_parent(cur).0;
            }
        }
        let folders = chain
            .into_iter()
            .filter_map(|rel| self.folders.get(rel).map(|f| (rel.to_string(), f.gi_stamp, f.repo_root, f.info_exclude_file.clone())))
            .collect();
        Stamps { root: self.root.clone(), folders, root_exclude: self.root_exclude_file.clone() }
    }

    /// An `info/exclude` changed, or the user's excludes did: start over.
    pub fn invalidate_all(&mut self, git_dir: Option<&Path>, user_excludes: Option<&[String]>) {
        self.folders.clear();
        self.dir_ignored.clear();
        let (file, rules) = read_root_exclude(&self.root, git_dir);
        self.root_exclude_file = file;
        self.root_exclude = rules;
        if let Some(u) = user_excludes {
            self.user = build_lines(&self.root, u);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn tree() -> PathBuf {
        let t = std::env::temp_dir().join(format!("ft-ignore-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(t.join(".git/info")).unwrap();
        fs::create_dir_all(t.join("src/gen")).unwrap();
        fs::create_dir_all(t.join("node_modules/x")).unwrap();
        fs::create_dir_all(t.join("vendor/inner/.git")).unwrap();
        fs::write(t.join(".gitignore"), "node_modules/\n*.log\nbuild\n").unwrap();
        fs::write(t.join("src/.gitignore"), "gen/\n!keep.log\n").unwrap();
        fs::write(t.join(".git/info/exclude"), "secret.txt\n").unwrap();
        fs::write(t.join("vendor/inner/.gitignore"), "local.out\n").unwrap();
        t
    }

    #[test]
    fn nested_gitignores_negation_and_info_exclude() {
        let t = tree();
        let mut s = IgnoreStack::for_test(&t, Some(&t.join(".git")), None, &[]);
        assert!(s.is_ignored("", "node_modules", true));
        assert!(s.is_ignored("node_modules", "x", true), "under an ignored folder");
        assert!(s.is_ignored("node_modules/x", "index.js", false));
        assert!(s.is_ignored("", "debug.log", false));
        assert!(s.is_ignored("src", "debug.log", false), "a root pattern applies below");
        assert!(!s.is_ignored("src", "keep.log", false), "negated by the nearer .gitignore");
        assert!(s.is_ignored("src", "gen", true));
        assert!(!s.is_ignored("", "gen", true), "src/.gitignore applies under src only");
        assert!(s.is_ignored("", "secret.txt", false), "info/exclude");
        assert!(!s.is_ignored("", "README.md", false));
        // A directory-only pattern doesn't hide a file of the same name.
        assert!(!s.is_ignored("", "node_modules", false));
        fs::remove_dir_all(&t).unwrap();
    }

    #[test]
    fn a_nested_repo_starts_its_own_stack() {
        let t = tree();
        let mut s = IgnoreStack::for_test(&t, Some(&t.join(".git")), None, &[]);
        assert!(s.is_ignored("vendor/inner", "local.out", false));
        // The outer repo's `*.log` doesn't reach into a repository of its own.
        assert!(!s.is_ignored("vendor/inner", "x.log", false));
        fs::remove_dir_all(&t).unwrap();
    }

    #[test]
    fn outside_a_repo_gitignore_hides_nothing_but_user_excludes_do() {
        let t = std::env::temp_dir().join(format!("ft-ignore-plain-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&t).unwrap();
        fs::write(t.join(".gitignore"), "*.log\n").unwrap();
        let mut s = IgnoreStack::for_test(&t, None, None, &["*.tmp".to_string()]);
        assert!(!s.is_ignored("", "a.log", false));
        assert!(s.is_ignored("", "a.tmp", false));
        fs::remove_dir_all(&t).unwrap();
    }

    #[test]
    fn global_excludes_and_invalidation() {
        let t = tree();
        let global = t.join("global-ignore");
        fs::write(&global, "*.swp\n").unwrap();
        let mut s = IgnoreStack::for_test(&t, Some(&t.join(".git")), Some(&global), &[]);
        assert!(s.is_ignored("src", "a.swp", false));
        assert!(!s.is_ignored("", "fresh.txt", false));
        fs::write(t.join(".gitignore"), "fresh.txt\n").unwrap();
        assert!(!s.is_ignored("", "fresh.txt", false), "cached until told");
        s.invalidate_folder("");
        assert!(s.is_ignored("", "fresh.txt", false));
        fs::remove_dir_all(&t).unwrap();
    }

    /// An edge checks only the rules that decide what is open: a folder the stack read for
    /// another reason (a worktree event checked for git or Find) costs it nothing.
    #[test]
    fn an_edge_stamps_only_the_open_chain() {
        let t = tree();
        let mut s = IgnoreStack::for_test(&t, Some(&t.join(".git")), None, &[]);
        for i in 0..300 {
            let d = format!("many/d{i}");
            fs::create_dir_all(t.join(&d)).unwrap();
            s.is_ignored(&d, "x.txt", false);
        }
        assert!(s.folders.len() > 300, "the stack read every folder an event named");
        s.is_ignored("src/gen", "x.txt", false);
        let open = ["src/gen".to_string()];
        let stamps = s.stamps(&open);
        assert_eq!(stamps.len(), 3, "\"\", src and src/gen, and nothing else");
        // A rules change outside the open chain is not looked at; one inside it is.
        fs::write(t.join("many/d7/.gitignore"), "x.txt\n").unwrap();
        assert_eq!(stamps.moved(), (vec![], false));
        fs::write(t.join("src/.gitignore"), "other/\n").unwrap();
        assert_eq!(s.stamps(&open).moved(), (vec!["src".to_string()], false));
        fs::remove_dir_all(&t).unwrap();
    }

    /// A whole-second mtime in the future (a zip made in a later timezone) can't tell a later
    /// edit apart by itself. Re-read on every pass, it is taken as changed only when its bytes
    /// are.
    #[test]
    fn a_future_whole_second_rules_file_is_decided_by_its_bytes() {
        let t = tree();
        let future = SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(1_900_000_000);
        let set = |text: &str| {
            fs::write(t.join(".gitignore"), text).unwrap();
            fs::File::options().write(true).open(t.join(".gitignore")).unwrap().set_modified(future).unwrap();
        };
        set("node_modules/\n");
        let mut s = IgnoreStack::for_test(&t, Some(&t.join(".git")), None, &[]);
        assert!(s.is_ignored("", "node_modules", true));
        let open = [String::new()];
        for _ in 0..3 {
            assert_eq!(s.stamps(&open).moved(), (vec![], false), "unchanged: nothing to do");
        }
        // Same length, same mtime, other rules: only the bytes tell.
        set("node_modulez/\n");
        assert_eq!(s.stamps(&open).moved(), (vec![String::new()], false));
        fs::remove_dir_all(&t).unwrap();
    }
}
