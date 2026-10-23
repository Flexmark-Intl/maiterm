//! Disk changes into deltas (docs/file-tree.md §6).
//!
//! One thread per hub, blocked on the raw channel: with nothing changing on disk it never
//! wakes, which is what makes idle free. A batch closes after 75 ms of quiet or 250 ms in
//! all. Each path is classified before anything costly happens to it:
//! 1. a child of an open folder: with ≤64 names changed there, only those names are stat'd and
//!    the snapshot patched; otherwise the folder is re-listed and diffed. Either way one Delta.
//! 2. an open folder itself (its own change, a poll, its removal): re-listed.
//! 3. git internals: the index, HEAD and refs schedule a status run (also from a git dir
//!    outside the root); `.git/objects`, `.git/logs` and lock files are noise;
//!    `.git/info/exclude` changes what is ignored.
//! 4. `.gitignore` / a nested `.git`: the ignore matchers for that folder are dropped and every
//!    open folder under it re-listed, since what is ignored there may have changed.
//! 5. anything else (a folder nobody has open, `.DS_Store`): no listing IO. A worktree path
//!    that isn't ignored still schedules git, and updates the search index when one exists.
//!
//! A full raw channel or a backend's "events were lost" re-lists every open folder under the
//! affected path. Nothing is dropped silently.

use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, RecvTimeoutError};
use std::sync::{Arc, Weak};
use std::time::{Duration, Instant};

use super::git::Trigger;
use super::hub::RootHub;
use super::list;
use super::paths::{self, split_parent};
use super::search::Change;
use super::types::FLAG_IGNORED;
use super::watch::RawEvent;

/// Changed paths in one batch past which the search index is rebuilt rather than updated
/// path by path.
const SEARCH_STAT_LIMIT: usize = 4096;

/// A batch closes after this much quiet…
pub const QUIET: Duration = Duration::from_millis(75);
/// …or this long after its first event, whichever comes first.
pub const TOTAL: Duration = Duration::from_millis(250);
/// Past this many changed names in one folder, re-list it instead of stat'ing each.
pub const SMALL_LIMIT: usize = 64;

pub(super) fn spawn(hub: Weak<RootHub>, rx: Receiver<RawEvent>, wakeups: Arc<AtomicU64>) {
    let spawned = std::thread::Builder::new().name("ft-coalesce".into()).spawn(move || loop {
        // Blocks with no timeout: no event, no wakeup.
        let Ok(first) = rx.recv() else { return };
        wakeups.fetch_add(1, Ordering::Relaxed);
        let mut batch = vec![first];
        let start = Instant::now();
        let mut disconnected = false;
        loop {
            let elapsed = start.elapsed();
            if elapsed >= TOTAL {
                break;
            }
            match rx.recv_timeout(QUIET.min(TOTAL - elapsed)) {
                Ok(ev) => batch.push(ev),
                Err(RecvTimeoutError::Timeout) => break,
                Err(RecvTimeoutError::Disconnected) => {
                    disconnected = true;
                    break;
                }
            }
        }
        let Some(h) = hub.upgrade() else { return };
        h.process_batch(batch);
        drop(h);
        if disconnected {
            return;
        }
    });
    if let Err(e) = spawned {
        log::error!("filetree: couldn't start the coalescer: {e}");
    }
}

/// Is a proper ancestor of `rel` (never the root) among the touched paths?
fn under_touched(rel: &str, touched: &HashSet<String>) -> bool {
    let mut cur = rel;
    loop {
        cur = split_parent(cur).0;
        if cur.is_empty() {
            return false;
        }
        if touched.contains(cur) {
            return true;
        }
    }
}

#[derive(Debug, PartialEq)]
enum Class {
    /// An ordinary file or folder.
    Worktree,
    /// A `.gitignore`.
    GitIgnore,
    /// A `.git` entry itself: a repository appeared or went away in its folder.
    RepoMarker,
    /// `.git/info/exclude` of the root's repository.
    InfoExclude,
    /// The repository's index, HEAD or refs: git decorations are due.
    GitSignal,
    Noise,
}

/// A path inside a git dir, by what follows it. Lock files are noise: git writes
/// `index.lock` and renames it onto `index`, and the rename is the signal.
fn classify_git_internal(rest: &[String]) -> Class {
    if rest.last().is_some_and(|n| n.ends_with(".lock")) {
        return Class::Noise;
    }
    match rest.first().map(String::as_str) {
        // The git dir itself changed (a per-folder watch reports its own folder).
        None => Class::GitSignal,
        Some("index" | "HEAD" | "packed-refs" | "MERGE_HEAD" | "CHERRY_PICK_HEAD" | "REVERT_HEAD" | "REBASE_HEAD") if rest.len() == 1 => Class::GitSignal,
        Some("refs") => Class::GitSignal,
        Some("info") if rest[1..] == ["exclude"] => Class::InfoExclude,
        _ => Class::Noise,
    }
}

fn classify(comps: &[String]) -> Class {
    if let Some(i) = comps.iter().position(|c| c == ".git") {
        if i + 1 == comps.len() {
            return Class::RepoMarker;
        }
        if i == 0 {
            return classify_git_internal(&comps[1..]);
        }
        return Class::Noise;
    }
    match comps.last().map(String::as_str) {
        Some(".DS_Store") => Class::Noise,
        Some(".gitignore") => Class::GitIgnore,
        _ => Class::Worktree,
    }
}

impl RootHub {
    /// `rel` is an open folder or being listed: re-list it (or have the running listing
    /// re-run).
    fn note_folder(&self, rel: &str, revalidate: &mut BTreeSet<String>) {
        if self.note_inflight_change(rel) {
            return;
        }
        if self.dirs.lock().contains_key(rel) {
            revalidate.insert(rel.to_string());
        }
    }

    /// `name` in `parent` changed: patch the snapshot if `parent` is open.
    fn note_child(&self, parent: &str, name: &str, names: &mut BTreeMap<String, BTreeSet<String>>) {
        if self.note_inflight_change(parent) {
            return;
        }
        if self.dirs.lock().contains_key(parent) {
            names.entry(parent.to_string()).or_default().insert(name.to_string());
        }
    }

    /// What a batch means for git and the search index. Worktree paths are checked against
    /// the ignore rules first (an ignored folder's verdict is cached, so a burst under
    /// node_modules costs a lookup per event and runs no git). Events were lost (`lost`):
    /// git runs and the index is rebuilt on its next query.
    fn note_for_git_and_search(&self, git_signal: bool, mut worktree_git: bool, lost: bool, worktree: Vec<String>, reshaped: &HashSet<String>) {
        if git_signal {
            self.git_signal(Trigger::Index);
        }
        let search = self.search.wanted();
        if lost {
            self.git_signal(Trigger::Worktree);
            if search {
                self.search.note(vec![Change::Folder]);
            }
            return;
        }
        if worktree.is_empty() || (!search && (worktree_git || self.git.is_none())) {
            if worktree_git {
                self.git_signal(Trigger::Worktree);
            }
            return;
        }
        let mut live: Vec<String> = Vec::new();
        {
            let mut ig = self.ignore.lock();
            for rel in worktree {
                let (parent, name) = split_parent(&rel);
                if ig.dir_is_ignored(parent) || ig.is_ignored(parent, name, false) {
                    continue;
                }
                live.push(rel);
                if !search {
                    // One is enough to schedule git.
                    break;
                }
            }
        }
        if !live.is_empty() {
            worktree_git = true;
        }
        if worktree_git {
            self.git_signal(Trigger::Worktree);
        }
        if !search || live.is_empty() {
            return;
        }
        // Past this many, a rebuild is cheaper than a stat each.
        if live.len() > SEARCH_STAT_LIMIT {
            self.search.note(vec![Change::Folder]);
            return;
        }
        let changes: Vec<Change> = live
            .into_iter()
            .filter_map(|rel| {
                let abs = paths::rel_to_native(&self.root, &rel).ok()?;
                Some(match std::fs::symlink_metadata(&abs) {
                    // A folder only "modified" gained or lost an entry, and that entry has an
                    // event of its own; rebuilding for it would rebuild on every new file on
                    // Windows. A folder that appeared or moved in: only a rebuild knows its files.
                    Ok(m) if m.is_dir() && !reshaped.contains(&rel) => return None,
                    Ok(m) if m.is_dir() => Change::Folder,
                    // `mv Foo.ts foo.ts` reports both names, and where the volume folds case
                    // both look up: the old one is gone if the disk spells it otherwise.
                    Ok(_) if !paths::spelled_on_disk(&self.root, &rel, self.fold) => Change::FileGone(rel),
                    Ok(_) => Change::File(rel),
                    Err(_) => Change::Gone(rel),
                })
            })
            .collect();
        self.search.note(changes);
    }

    pub(super) fn process_batch(&self, batch: Vec<RawEvent>) {
        if self.is_gone() {
            return;
        }
        let mut revalidate = BTreeSet::new();
        let mut rescan: Vec<String> = Vec::new();
        let mut names: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
        let mut ignore_dirs: BTreeSet<String> = BTreeSet::new();
        let mut exclude_changed = false;
        let mut repo_changed = false;
        let mut root_check = false;
        // Every path that changed. A folder renamed or deleted reports only its own path, never
        // its descendants', so an open folder below one of these must be re-listed too.
        let mut touched: HashSet<String> = HashSet::new();
        // Git: the index or refs moved; a worktree path (checked against the ignore rules
        // below, so `npm install` runs no git) changed.
        let mut git_signal = false;
        let mut worktree_git = false;
        let mut worktree: Vec<String> = Vec::new();
        // Worktree paths some event said appeared, went or moved (not only "modified").
        let mut reshaped: HashSet<String> = HashSet::new();
        if self.overflow.swap(false, Ordering::SeqCst) {
            log::debug!("filetree: event overflow under {}, re-listing", self.root.display());
            rescan.push(String::new());
        }
        for ev in batch {
            if ev.paths.is_empty() {
                if ev.rescan {
                    rescan.push(String::new());
                }
                continue;
            }
            for p in &ev.paths {
                let Some(comps) = paths::strip_root(p, &self.root, self.fold) else {
                    // A git dir outside the root (a root inside a larger repo, a linked
                    // worktree): only its index, HEAD and refs matter.
                    if let Some(rest) = self.git_dirs().iter().find_map(|g| paths::strip_root(p, g, self.fold)) {
                        match classify_git_internal(&rest) {
                            Class::GitSignal => git_signal = true,
                            Class::InfoExclude => exclude_changed = true,
                            _ => {}
                        }
                        continue;
                    }
                    // Above the root: the root itself may have been moved or deleted.
                    if self.root.starts_with(p) {
                        root_check = true;
                        if ev.rescan {
                            rescan.push(String::new());
                        }
                    }
                    continue;
                };
                let rel = comps.join("/");
                if ev.rescan {
                    rescan.push(rel.clone());
                }
                match classify(&comps) {
                    Class::Noise => continue,
                    Class::GitSignal => {
                        git_signal = true;
                        continue;
                    }
                    Class::InfoExclude => {
                        exclude_changed = true;
                        continue;
                    }
                    Class::RepoMarker => {
                        repo_changed = true;
                        worktree_git = true;
                        ignore_dirs.insert(split_parent(&rel).0.to_string());
                    }
                    Class::GitIgnore => {
                        // What git reports as untracked may have changed.
                        worktree_git = true;
                        ignore_dirs.insert(split_parent(&rel).0.to_string());
                    }
                    Class::Worktree => {
                        if !rel.is_empty() {
                            if !ev.content_only {
                                reshaped.insert(rel.clone());
                            }
                            worktree.push(rel.clone());
                        }
                    }
                }
                if rel.is_empty() {
                    root_check = true;
                }
                self.note_folder(&rel, &mut revalidate);
                touched.insert(rel.clone());
                if !rel.is_empty() {
                    let (parent, name) = split_parent(&rel);
                    self.note_child(parent, name, &mut names);
                }
            }
        }
        if root_check && !self.root.is_dir() {
            self.root_gone();
            return;
        }
        if exclude_changed {
            self.ignore.lock().invalidate_all(self.git_dir(), None);
            rescan.push(String::new());
        }
        for d in ignore_dirs {
            self.ignore.lock().invalidate_folder(&d);
            rescan.push(d);
        }
        if repo_changed {
            super::place::invalidate_toplevel_under(&self.root);
        }
        if exclude_changed {
            worktree_git = true;
        }
        // While one agent tab's shell call runs here, what changed is credited to it (§9.2).
        self.attribute_to_shell(&worktree, &reshaped);
        self.note_for_git_and_search(git_signal, worktree_git, !rescan.is_empty(), worktree, &reshaped);
        {
            let dirs = self.dirs.lock();
            for base in &rescan {
                revalidate.extend(dirs.keys().filter(|r| paths::is_at_or_under(r, base)).cloned());
            }
            // Open folders under a touched path: one hash lookup per ancestor, so the cost is
            // open folders × depth, never events × open folders.
            if !touched.is_empty() {
                revalidate.extend(dirs.keys().filter(|k| under_touched(k, &touched)).cloned());
            }
        }
        if !touched.is_empty() {
            // A listing already reading a folder under a renamed one may have read it before
            // the rename: it runs again.
            self.rerun_inflight_where(|rel| under_touched(rel, &touched));
        }
        for (parent, set) in names {
            if revalidate.contains(&parent) {
                continue;
            }
            if set.len() > SMALL_LIMIT {
                revalidate.insert(parent);
                continue;
            }
            let Ok(abs) = paths::rel_to_native(&self.root, &parent) else { continue };
            // Stat'd with no lock held: a slow disk holds up nobody else.
            let mut updates: Vec<(String, Option<super::types::Entry>)> = set.into_iter().map(|n| {
                let e = list::stat_entry(&abs, &n);
                (n, e)
            }).collect();
            {
                let mut ig = self.ignore.lock();
                for (n, e) in updates.iter_mut() {
                    if let Some(e) = e {
                        if ig.is_ignored(&parent, n, e.is_dirish()) {
                            e.f |= FLAG_IGNORED;
                        }
                    }
                }
            }
            if !self.apply_small(&parent, updates) {
                revalidate.insert(parent);
            }
        }
        for rel in revalidate {
            self.request(&rel, None);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn c(p: &str) -> Vec<String> {
        p.split('/').map(String::from).collect()
    }

    #[test]
    fn classification() {
        assert_eq!(classify(&c("src/a.ts")), Class::Worktree);
        assert_eq!(classify(&c(".git/objects/ab/cdef")), Class::Noise);
        assert_eq!(classify(&c(".git/index")), Class::GitSignal);
        assert_eq!(classify(&c(".git/index.lock")), Class::Noise);
        assert_eq!(classify(&c(".git/HEAD")), Class::GitSignal);
        assert_eq!(classify(&c(".git/refs/heads/main")), Class::GitSignal);
        assert_eq!(classify(&c(".git/refs/heads/main.lock")), Class::Noise);
        assert_eq!(classify(&c(".git/logs/HEAD")), Class::Noise);
        assert_eq!(classify(&c(".git/packed-refs")), Class::GitSignal);
        assert_eq!(classify(&c("vendor/x/.git/index")), Class::Noise);
        assert_eq!(classify_git_internal(&c("refs/remotes/origin/main")), Class::GitSignal);
        assert_eq!(classify_git_internal(&[]), Class::GitSignal);
        assert_eq!(classify(&c(".git/info/exclude")), Class::InfoExclude);
        assert_eq!(classify(&c(".git")), Class::RepoMarker);
        assert_eq!(classify(&c("vendor/x/.git")), Class::RepoMarker);
        assert_eq!(classify(&c("vendor/x/.git/info/exclude")), Class::Noise);
        assert_eq!(classify(&c("src/.gitignore")), Class::GitIgnore);
        assert_eq!(classify(&c("a/.DS_Store")), Class::Noise);
        assert_eq!(classify(&c(".github/workflows/ci.yml")), Class::Worktree);
    }
}
