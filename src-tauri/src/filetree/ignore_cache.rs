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

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use ignore::gitignore::{Gitignore, GitignoreBuilder};
use ignore::Match;

use super::paths::{join_rel, split_parent};

/// What one folder contributes to the stack.
struct Folder {
    /// Its `.gitignore`, when it has one.
    gi: Option<Gitignore>,
    /// It holds `.git`: a repository starts here.
    repo_root: bool,
    /// That repository's `info/exclude`, when `repo_root`.
    info_exclude: Option<Gitignore>,
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

pub struct IgnoreStack {
    root: PathBuf,
    /// The root is inside a repository (its own top level or deeper).
    root_in_repo: bool,
    /// `info/exclude` of the repository the root is in, rooted at the root.
    root_exclude: Option<Gitignore>,
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
        IgnoreStack {
            root: root.to_path_buf(),
            root_in_repo: git_dir.is_some(),
            root_exclude: git_dir.and_then(|g| build_file(root, &g.join("info").join("exclude"))),
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
            let info_exclude = if repo_root && dot_git.is_dir() { build_file(&abs, &dot_git.join("info").join("exclude")) } else { None };
            let gi = build_file(&abs, &abs.join(".gitignore"));
            self.folders.insert(rel.to_string(), Folder { gi, repo_root, info_exclude });
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

    /// An `info/exclude` changed, or the user's excludes did: start over.
    pub fn invalidate_all(&mut self, git_dir: Option<&Path>, user_excludes: Option<&[String]>) {
        self.folders.clear();
        self.dir_ignored.clear();
        self.root_exclude = git_dir.and_then(|g| build_file(&self.root, &g.join("info").join("exclude")));
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
}
