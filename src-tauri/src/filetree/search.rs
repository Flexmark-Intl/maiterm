//! Find: a fuzzy path index per hub (docs/file-tree.md §8).
//!
//! - **Built lazily**, on the first query, in the background: inside a repository from
//!   `git ls-files -co --exclude-standard -z` (tracked plus untracked, ignored left out, about
//!   100 ms for 100k files); elsewhere from a gitignore-aware parallel walk, 20 levels deep,
//!   `.git` skipped. A home or volume root is never walked unless the human asks (§6.5).
//! - **Stored as one `String` arena plus spans**, about 6 MB per 100k paths. A removed path
//!   leaves a dead span; the arena is compacted once a quarter of it is dead.
//! - **Kept fresh** from the watcher's events (a file appearing or going), and from the files
//!   each git status run names, looked up on disk, which is what reconciles Linux, whose
//!   watcher sees only open folders. A folder appearing or going marks the index dirty: the next query
//!   rebuilds it in the background while the old one answers.
//! - **Matched with nucleo** (`match_paths`, smart case, smart normalization); the top 500 come
//!   back with their matched character positions.
//! - **Stale queries stop early**: each carries a `seq`, the newest is kept in an atomic, and a
//!   scan that sees a newer one gives up.
//! - **Evicted** after 5 minutes with no query, by the thread that built it.
//! - **A root on an ssh host** (§15) is listed by its helper there (`ls-files`, or its own
//!   walk), and kept fresh by the helper's watch events; a status run's named files mark it
//!   dirty rather than being looked up, since each lookup would be a round trip.

use std::cmp::Reverse;
use std::collections::hash_map::DefaultHasher;
use std::collections::{BinaryHeap, HashMap};
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Weak};
use std::time::{Duration, Instant};

use nucleo_matcher::pattern::{CaseMatching, Normalization, Pattern};
use nucleo_matcher::{Config, Matcher, Utf32Str};
use parking_lot::{Condvar, Mutex};

use super::hub::RootHub;
use super::paths::norm;
use super::remote::RemoteRoot;
use super::types::{SearchHit, SearchResult};
use crate::util::proc;

/// Results returned at most.
pub const MAX_HITS: usize = 500;
/// An index nobody has queried for this long is dropped.
pub const EVICT_AFTER: Duration = Duration::from_secs(300);
/// Paths indexed at most: past this a walk stops (a pinned `/` would otherwise never end).
const MAX_PATHS: usize = 1_000_000;
const WALK_DEPTH: usize = 20;
/// A scan checks for a newer query this often.
const CANCEL_EVERY: usize = 2048;
/// Past this many files named by one status run, the index is rebuilt instead of each being
/// looked up on disk.
const MAX_RECONCILE: usize = 20_000;

fn hash_of(s: &str) -> u64 {
    let mut h = DefaultHasher::new();
    s.hash(&mut h);
    h.finish()
}

/// Every indexed path, root-relative POSIX, in one arena.
#[derive(Default)]
pub struct Index {
    arena: String,
    /// (start, len) into `arena`; a removed path keeps its slot with `u32::MAX` as start.
    spans: Vec<(u32, u32)>,
    /// hash(path) → slot. A collision (two paths, one hash) marks the index for a rebuild
    /// instead of risking the wrong slot.
    map: HashMap<u64, u32>,
    dead: usize,
    dead_bytes: usize,
    /// Something changed that a per-path update can't express (a folder came or went).
    pub dirty: bool,
}

impl Index {
    pub fn from_paths<I: IntoIterator<Item = String>>(paths: I) -> Index {
        let mut ix = Index::default();
        for p in paths {
            ix.insert(&p);
        }
        ix
    }

    pub fn len(&self) -> usize {
        self.spans.len() - self.dead
    }

    fn get(&self, slot: u32) -> Option<&str> {
        let (s, l) = self.spans[slot as usize];
        (s != u32::MAX).then(|| &self.arena[s as usize..(s + l) as usize])
    }

    #[cfg(test)]
    pub fn contains(&self, path: &str) -> bool {
        let path = norm(path);
        self.map.get(&hash_of(&path)).and_then(|s| self.get(*s)) == Some(path.as_ref())
    }

    /// Paths are kept in one spelling (NFC on macOS): `ls-files` gives git's NFC, a walk and
    /// the watcher give names as stored, often NFD, and one file must not be listed twice.
    pub fn insert(&mut self, path: &str) {
        let path = norm(path);
        let path = path.as_ref();
        let h = hash_of(path);
        if let Some(&slot) = self.map.get(&h) {
            if self.get(slot) != Some(path) {
                self.dirty = true;
            }
            return;
        }
        if self.arena.len() + path.len() > u32::MAX as usize {
            self.dirty = true;
            return;
        }
        let start = self.arena.len() as u32;
        self.arena.push_str(path);
        self.map.insert(h, self.spans.len() as u32);
        self.spans.push((start, path.len() as u32));
    }

    /// True when the path was there.
    pub fn remove(&mut self, path: &str) -> bool {
        let path = norm(path);
        let path = path.as_ref();
        let h = hash_of(path);
        let Some(&slot) = self.map.get(&h) else { return false };
        if self.get(slot) != Some(path) {
            return false;
        }
        self.map.remove(&h);
        self.dead_bytes += self.spans[slot as usize].1 as usize;
        self.spans[slot as usize] = (u32::MAX, 0);
        self.dead += 1;
        if self.dead * 4 > self.spans.len().max(64) || self.dead_bytes * 4 > self.arena.len().max(4096) {
            self.compact();
        }
        true
    }

    fn compact(&mut self) {
        let live: Vec<String> = self.iter().map(|(_, p)| p.to_string()).collect();
        let dirty = self.dirty;
        *self = Index::from_paths(live);
        self.dirty |= dirty;
    }

    fn iter(&self) -> impl Iterator<Item = (u32, &str)> {
        self.spans.iter().enumerate().filter(|(_, (s, _))| *s != u32::MAX).map(|(i, (s, l))| (i as u32, &self.arena[*s as usize..(*s + *l) as usize]))
    }

    /// The best `limit` matches for `query`, or None when `cancelled` said stop.
    pub fn search(&self, query: &str, limit: usize, cancelled: &dyn Fn() -> bool) -> Option<Vec<SearchHit>> {
        let query = query.trim();
        if query.is_empty() || limit == 0 {
            return Some(Vec::new());
        }
        let pattern = Pattern::parse(query, CaseMatching::Smart, Normalization::Smart);
        let mut matcher = Matcher::new(Config::DEFAULT.match_paths());
        let mut buf = Vec::new();
        // A min-heap of the best so far: (score, shorter path first, then earlier path).
        let mut heap: BinaryHeap<Reverse<(u32, Reverse<u32>, Reverse<String>, u32)>> = BinaryHeap::with_capacity(limit + 1);
        for (n, (slot, path)) in self.iter().enumerate() {
            if n % CANCEL_EVERY == 0 && n > 0 && cancelled() {
                return None;
            }
            let Some(score) = pattern.score(code_points(path, &mut buf), &mut matcher) else { continue };
            let key = (score, Reverse(path.len() as u32));
            if heap.len() == limit {
                let Some(Reverse(worst)) = heap.peek() else { continue };
                // Only a strictly better hit displaces the worst; equal ones keep the earlier.
                if (key.0, key.1) <= (worst.0, worst.1) {
                    continue;
                }
                heap.pop();
            }
            heap.push(Reverse((score, key.1, Reverse(path.to_string()), slot)));
        }
        let mut best: Vec<(u32, Reverse<u32>, Reverse<String>, u32)> = heap.into_iter().map(|Reverse(x)| x).collect();
        best.sort_by(|a, b| b.cmp(a));
        let mut idx = Vec::new();
        Some(
            best.into_iter()
                .map(|(score, _, Reverse(rel), _)| {
                    idx.clear();
                    pattern.indices(code_points(&rel, &mut buf), &mut matcher, &mut idx);
                    idx.sort_unstable();
                    idx.dedup();
                    SearchHit { rel, score, idx: idx.clone() }
                })
                .collect(),
        )
    }
}

/// `path` as nucleo matches it, one slot per code point, so the positions it returns are code
/// points: what the frontend counts (`matchHighlights`). `Utf32Str::new` keeps one slot per
/// grapheme instead, and falls back to the raw bytes when every grapheme starts with an ASCII
/// character (an NFD `café`), so its positions drifted off the string at any combining mark or
/// multi-code-point emoji.
fn code_points<'a>(path: &'a str, buf: &'a mut Vec<char>) -> Utf32Str<'a> {
    if path.is_ascii() {
        Utf32Str::Ascii(path.as_bytes())
    } else {
        buf.clear();
        buf.extend(path.chars());
        Utf32Str::Unicode(buf)
    }
}

/// One change the watcher saw, already classified.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Change {
    /// A file exists here now.
    File(String),
    /// Nothing exists here now (a file, or a whole folder, went).
    Gone(String),
    /// A file git reports deleted. Unlike `Gone` it can't be a folder, so one the index
    /// doesn't hold was already removed (the watcher saw it first) and needs no rebuild.
    FileGone(String),
    /// A folder appeared or changed shape: only a rebuild knows what's in it.
    Folder,
}

#[derive(Default)]
struct Inner {
    index: Option<Index>,
    building: bool,
    /// Bumped by every build: only the newest build's thread stays to evict, so a rebuild
    /// can't leave the previous thread parked beside it.
    build_gen: u64,
    /// Changes that arrived while a build ran: replayed onto the new index.
    pending: Vec<Change>,
    /// The human asked to index a home or volume root.
    shallow_ok: bool,
    last_query: Option<Instant>,
}

pub struct SearchState {
    hub: Weak<RootHub>,
    root: PathBuf,
    /// The root is inside a repository: list with git, not a walk.
    in_repo: bool,
    shallow: bool,
    /// The root's volume folds case (`relocate::case_insensitive`).
    fold: bool,
    /// A root on an ssh host: its helper lists the paths.
    remote: Option<Arc<RemoteRoot>>,
    /// The newest query each subscription asked; an older one of its own still scanning gives
    /// up. Per subscription because `seq` is: each window counts its own from zero, and two
    /// windows on one root share this hub, so one shared counter let the window that had
    /// searched more cancel every scan of the other.
    latest: Mutex<HashMap<u64, Arc<AtomicU64>>>,
    inner: Mutex<Inner>,
    cv: Condvar,
    evict_after: Duration,
    /// Index threads alive, for the test that a rebuild leaves none behind.
    #[cfg(test)]
    pub(crate) threads: std::sync::atomic::AtomicUsize,
}

impl SearchState {
    #[cfg(test)]
    pub fn new(hub: Weak<RootHub>, root: &Path, in_repo: bool, shallow: bool, fold: bool) -> Arc<SearchState> {
        Self::new_with(hub, root, in_repo, shallow, fold, None)
    }

    pub fn new_with(hub: Weak<RootHub>, root: &Path, in_repo: bool, shallow: bool, fold: bool, remote: Option<Arc<RemoteRoot>>) -> Arc<SearchState> {
        Arc::new(SearchState {
            hub,
            root: root.to_path_buf(),
            in_repo,
            shallow,
            fold,
            remote,
            latest: Mutex::new(HashMap::new()),
            inner: Mutex::new(Inner::default()),
            cv: Condvar::new(),
            evict_after: EVICT_AFTER,
            #[cfg(test)]
            threads: std::sync::atomic::AtomicUsize::new(0),
        })
    }

    /// An index exists (or is being built): only then are watcher events worth classifying.
    pub fn wanted(&self) -> bool {
        let inner = self.inner.lock();
        inner.index.is_some() || inner.building
    }

    /// `ft_search`. Answers from the index there is; starts a build when there is none or it
    /// is dirty, and says so (`indexing`), with `search_ready` sent when it is done.
    pub fn query(self: &Arc<Self>, sub: u64, query: &str, limit: usize, seq: u64, index_shallow: bool) -> SearchResult {
        let latest = self.latest.lock().entry(sub).or_default().clone();
        latest.fetch_max(seq, Ordering::SeqCst);
        let limit = limit.clamp(1, MAX_HITS);
        let mut inner = self.inner.lock();
        inner.last_query = Some(Instant::now());
        if index_shallow {
            inner.shallow_ok = true;
        }
        if self.shallow && !self.in_repo && !inner.shallow_ok && inner.index.is_none() {
            return SearchResult {
                seq,
                indexing: false,
                items: Vec::new(),
                unavailable: Some("A home or volume folder isn't indexed by itself.".to_string()),
            };
        }
        let need_build = inner.index.as_ref().is_none_or(|ix| ix.dirty);
        if need_build && !inner.building {
            inner.building = true;
            inner.build_gen += 1;
            let (me, gen) = (self.clone(), inner.build_gen);
            match std::thread::Builder::new().name("ft-index".into()).spawn(move || me.build_and_keep(gen)) {
                // The previous build's thread, parked for eviction, wakes to see it is not
                // the newest and exits.
                Ok(_) => {
                    self.cv.notify_all();
                }
                Err(e) => {
                    inner.building = false;
                    inner.build_gen -= 1;
                    log::warn!("filetree: couldn't start the index thread: {e}");
                }
            }
        }
        let indexing = inner.building;
        let Some(ix) = inner.index.as_ref() else {
            return SearchResult { seq, indexing, items: Vec::new(), unavailable: None };
        };
        let items = ix.search(query, limit, &|| latest.load(Ordering::SeqCst) != seq).unwrap_or_default();
        SearchResult { seq, indexing, items, unavailable: None }
    }

    /// The index thread: build, publish, then stay to evict it once queries stop, unless a
    /// newer build (`gen`) has taken that over.
    fn build_and_keep(self: Arc<Self>, gen: u64) {
        #[cfg(test)]
        let _alive = {
            struct Alive(Arc<SearchState>);
            impl Drop for Alive {
                fn drop(&mut self) {
                    self.0.threads.fetch_sub(1, Ordering::SeqCst);
                }
            }
            self.threads.fetch_add(1, Ordering::SeqCst);
            Alive(self.clone())
        };
        let started = Instant::now();
        let paths = self.list_paths();
        let mut ix = Index::from_paths(paths);
        log::debug!("filetree: indexed {} paths under {} in {:?}", ix.len(), self.root.display(), started.elapsed());
        {
            let mut inner = self.inner.lock();
            for c in std::mem::take(&mut inner.pending) {
                apply_change(&mut ix, c);
            }
            inner.index = Some(ix);
            inner.building = false;
        }
        if let Some(h) = self.hub.upgrade() {
            h.search_ready();
        }
        // Eviction: wake once per idle period; a query since the last look keeps the index.
        let mut inner = self.inner.lock();
        loop {
            if inner.build_gen != gen {
                // A rebuild started: its thread takes over the eviction.
                return;
            }
            let since = inner.last_query.map(|t| t.elapsed()).unwrap_or(self.evict_after);
            if since >= self.evict_after || self.hub.strong_count() == 0 {
                inner.index = None;
                inner.pending.clear();
                return;
            }
            self.cv.wait_for(&mut inner, self.evict_after - since);
        }
    }

    fn list_paths(&self) -> Vec<String> {
        if let Some(r) = &self.remote {
            return r.files().unwrap_or_else(|e| {
                log::info!("filetree: indexing {} on {}: {e}", r.root, r.host);
                Vec::new()
            });
        }
        if self.in_repo {
            if let Some(p) = git_ls_files(&self.root) {
                return p;
            }
        }
        walk(&self.root)
    }

    /// Watcher changes, already classified. Cheap when there is no index.
    pub fn note(&self, changes: Vec<Change>) {
        if changes.is_empty() {
            return;
        }
        note_locked(&mut self.inner.lock(), changes);
    }

    /// The files a status run named (each changed file, and a rename's source): the Linux
    /// reconciliation, whose watcher sees only open folders. Each is looked up on disk rather
    /// than taken from git's letter, because git's view is as old as its scan and its letters
    /// don't say what is on disk: a file made and removed during the run is gone, an
    /// index-only deletion (`git rm --cached`, of one file or of a folder git then lists as
    /// `? dir/`) is still there, and a staged rename's two sides are neither untracked nor
    /// deleted. The lookup happens under the lock the watcher's own changes take, so a change
    /// it reports meanwhile lands after this and is never undone by it.
    pub fn reconcile(&self, named: &[String]) {
        if named.is_empty() {
            return;
        }
        let mut inner = self.inner.lock();
        if inner.index.is_none() && !inner.building {
            return;
        }
        if named.len() > MAX_RECONCILE || self.remote.is_some() {
            // One `ls-files` costs less than this many lookups (and on an ssh host, than one
            // round trip of them).
            note_locked(&mut inner, vec![Change::Folder]);
            return;
        }
        // A volume that folds case finds both sides of a case-only rename (`git mv Foo.ts
        // foo.ts` names both), so those, and only those, are asked how they are spelled on
        // disk: asking for every name would cost a canonicalization each, on every run.
        let mut folded: HashMap<String, usize> = HashMap::new();
        if self.fold {
            for p in named {
                *folded.entry(super::paths::name_key(p, true)).or_default() += 1;
            }
        }
        let changes = named
            .iter()
            .filter_map(|p| {
                let abs = super::paths::rel_to_native(&self.root, p).ok()?;
                let file = std::fs::symlink_metadata(&abs).is_ok_and(|m| !m.is_dir())
                    && (!self.fold || folded.get(&super::paths::name_key(p, true)).is_none_or(|n| *n < 2) || super::paths::spelled_on_disk(&self.root, p, true));
                Some(if file { Change::File(p.clone()) } else { Change::FileGone(p.clone()) })
            })
            .collect();
        note_locked(&mut inner, changes);
    }

    /// A subscription closed: its query counter goes with it.
    pub fn forget(&self, sub: u64) {
        self.latest.lock().remove(&sub);
    }

    /// The hub is gone: let the eviction wait end now.
    pub fn stop(&self) {
        self.inner.lock().last_query = None;
        self.cv.notify_all();
    }

    #[cfg(test)]
    pub(crate) fn len(&self) -> Option<usize> {
        self.inner.lock().index.as_ref().map(Index::len)
    }
}

/// Changes into the index there is, and into the queue a running build replays.
fn note_locked(inner: &mut Inner, changes: Vec<Change>) {
    if inner.building {
        inner.pending.extend(changes.iter().cloned());
    }
    if let Some(ix) = inner.index.as_mut() {
        for c in changes {
            apply_change(ix, c);
        }
    }
}

fn apply_change(ix: &mut Index, c: Change) {
    match c {
        Change::File(p) => ix.insert(&p),
        Change::Gone(p) => {
            // Not an indexed file: it may have been a folder, whose files only a rebuild drops.
            if !ix.remove(&p) {
                ix.dirty = true;
            }
        }
        Change::FileGone(p) => {
            ix.remove(&p);
        }
        Change::Folder => ix.dirty = true,
    }
}

/// Tracked and untracked files, ignored ones left out, relative to the root.
fn git_ls_files(root: &Path) -> Option<Vec<String>> {
    let mut c = proc::git();
    // `-c` lists the index, which still holds a file deleted from the worktree but not yet
    // staged: every rebuild would bring an `rm`'d file (or a whole `rm -rf`'d folder) back
    // as a hit that opens nothing. `-d -t` lists those again tagged `R`, to leave out.
    c.arg("--no-optional-locks").arg("-C").arg(root).args(["ls-files", "-cdo", "-t", "--exclude-standard", "-z"]);
    let out = proc::output_timeout(&mut c, Duration::from_secs(30)).ok()?;
    if !out.status.success() {
        return None;
    }
    Some(parse_ls_files_tagged(&out.stdout, &mut OnDisk::new(root)))
}

/// Whether a root-relative path is on disk, remembering each folder's answer: a sparse
/// checkout leaves whole folders out, so one lookup of the folder settles every file in it.
struct OnDisk<'a> {
    root: &'a Path,
    dirs: HashMap<String, bool>,
}

impl<'a> OnDisk<'a> {
    fn new(root: &'a Path) -> Self {
        OnDisk { root, dirs: HashMap::new() }
    }

    fn file(&mut self, rel: &str) -> bool {
        if let Some((parent, _)) = rel.rsplit_once('/') {
            if !self.dir(parent) {
                return false;
            }
        }
        super::paths::rel_to_native(self.root, rel).is_ok_and(|abs| std::fs::symlink_metadata(abs).is_ok_and(|m| !m.is_dir()))
    }

    fn dir(&mut self, rel: &str) -> bool {
        if let Some(&known) = self.dirs.get(rel) {
            return known;
        }
        let up = match rel.rsplit_once('/') {
            Some((parent, _)) => self.dir(parent),
            None => true,
        };
        let here = up && super::paths::rel_to_native(self.root, rel).is_ok_and(|abs| abs.is_dir());
        self.dirs.insert(rel.to_string(), here);
        here
    }
}

/// `ls-files -t -z` records: a one-letter tag, a space, the path. `R` (deleted from the
/// worktree) removes a path the `H` record listed. `S` (skip-worktree) is listed only when it
/// is on disk (`on_disk`): outside a sparse checkout's cone it is in the index but not on
/// disk, and git never reports it deleted; but `git update-index --skip-worktree`, the usual
/// way to hide local edits to a config file, sets the same bit on a file that stays, and git
/// status never names that one, so nothing else would bring it back. Every other tag lists one.
fn parse_ls_files_tagged(out: &[u8], on_disk: &mut OnDisk<'_>) -> Vec<String> {
    let mut removed = std::collections::HashSet::new();
    let mut listed = Vec::new();
    for rec in out.split(|b| *b == 0) {
        if rec.len() < 3 || rec[1] != b' ' {
            continue;
        }
        let path = String::from_utf8_lossy(&rec[2..]).into_owned();
        match rec[0] {
            b'R' => {
                removed.insert(path);
            }
            b'S' => {
                if on_disk.file(&path) {
                    listed.push(path);
                }
            }
            _ => listed.push(path),
        }
    }
    let mut seen = std::collections::HashSet::new();
    listed
        .into_iter()
        .filter(|p| !removed.contains(p))
        // A path with a conflict is listed once per stage.
        .filter(|p| seen.insert(hash_of(p)))
        .take(MAX_PATHS)
        .collect()
}

/// Every file below `root` that git would not ignore, 20 levels deep, `.git` skipped.
fn walk(root: &Path) -> Vec<String> {
    let out = Arc::new(Mutex::new(Vec::new()));
    let walker = ignore::WalkBuilder::new(root)
        .hidden(false)
        .git_ignore(true)
        .git_exclude(true)
        .git_global(true)
        .parents(true)
        .follow_links(false)
        .max_depth(Some(WALK_DEPTH))
        .filter_entry(|e| e.file_name() != ".git")
        .build_parallel();
    walker.run(|| {
        let out = out.clone();
        let root = root.to_path_buf();
        Box::new(move |res| {
            let Ok(entry) = res else { return ignore::WalkState::Continue };
            if !entry.file_type().is_some_and(|t| t.is_file() || t.is_symlink()) {
                return ignore::WalkState::Continue;
            }
            let Ok(rel) = entry.path().strip_prefix(&root) else { return ignore::WalkState::Continue };
            let rel = rel.components().map(|c| c.as_os_str().to_string_lossy()).collect::<Vec<_>>().join("/");
            let mut v = out.lock();
            if v.len() >= MAX_PATHS {
                return ignore::WalkState::Quit;
            }
            v.push(rel);
            ignore::WalkState::Continue
        })
    });
    let mut v = std::mem::take(&mut *out.lock());
    v.sort();
    v
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ix(paths: &[&str]) -> Index {
        Index::from_paths(paths.iter().map(|s| s.to_string()))
    }

    fn rels(hits: &[SearchHit]) -> Vec<&str> {
        hits.iter().map(|h| h.rel.as_str()).collect()
    }

    #[test]
    fn a_hit_in_the_file_name_beats_one_spread_over_folders() {
        let i = ix(&["src/lib/filetree/model.ts", "src/models/index.ts", "mod/el/x.ts", "docs/model-notes.md", "src/lib/stores/fileTree.svelte.ts"]);
        let hits = i.search("model", 10, &|| false).unwrap();
        let r = rels(&hits);
        assert!(r.iter().position(|p| *p == "src/lib/filetree/model.ts").unwrap() < r.iter().position(|p| *p == "mod/el/x.ts").unwrap());
        assert!(!r.contains(&"src/lib/stores/fileTree.svelte.ts"));
        // Scores never increase down the list.
        assert!(hits.windows(2).all(|w| w[0].score >= w[1].score));
    }

    #[test]
    fn smart_case_and_indices_point_at_matched_characters() {
        let i = ix(&["src/FileTree.ts", "src/filetree.ts"]);
        // An upper-case letter makes the query case-sensitive.
        assert_eq!(rels(&i.search("FT", 10, &|| false).unwrap()), vec!["src/FileTree.ts"]);
        let hits = i.search("ftree", 10, &|| false).unwrap();
        assert_eq!(hits.len(), 2);
        for h in &hits {
            let chars: Vec<char> = h.rel.chars().collect();
            let picked: String = h.idx.iter().map(|&i| chars[i as usize].to_ascii_lowercase()).collect();
            assert_eq!(picked, "ftree", "{}", h.rel);
        }
    }

    #[test]
    fn indices_are_characters_not_bytes() {
        let i = ix(&["caf\u{e9}/\u{1F600}/x.ts"]);
        let h = &i.search("x.ts", 10, &|| false).unwrap()[0];
        let chars: Vec<char> = h.rel.chars().collect();
        assert_eq!(h.idx.iter().map(|&i| chars[i as usize]).collect::<String>(), "x.ts");
    }

    /// The frontend reads positions as code points. nucleo's own `Utf32Str::new` counts
    /// graphemes (or bytes, when every grapheme starts with ASCII), which drifted at a
    /// combining mark or a skin-toned emoji.
    #[test]
    fn indices_are_code_points_with_combining_marks_and_emoji() {
        // q + combining dot (no precomposed form, so NFC keeps it two code points), and a
        // thumbs-up with a skin tone (two code points, one grapheme).
        for rel in ["q\u{307}/x.ts", "\u{1F44D}\u{1F3FD}/x.ts", "a\u{301}b\u{301}c/x.ts"] {
            let i = ix(&[rel]);
            let h = &i.search("x.ts", 10, &|| false).unwrap()[0];
            let chars: Vec<char> = h.rel.chars().collect();
            assert!(h.idx.iter().all(|&n| (n as usize) < chars.len()), "{rel}: {:?}", h.idx);
            assert_eq!(h.idx.iter().map(|&n| chars[n as usize]).collect::<String>(), "x.ts", "{rel}");
        }
    }

    #[test]
    fn a_tracked_file_deleted_from_the_worktree_is_not_listed() {
        // `-c` lists the index; the `R` records (deleted) take those paths back out.
        let out = b"? new.ts\0H kept.ts\0H gone.ts\0H old/a.ts\0R gone.ts\0R old/a.ts\0H c.ts\0H c.ts\0";
        let none = Path::new("/nonexistent");
        assert_eq!(parse_ls_files_tagged(out, &mut OnDisk::new(none)), vec!["new.ts".to_string(), "kept.ts".to_string(), "c.ts".to_string()]);
    }

    /// Skip-worktree is one bit for two things: a file outside a sparse checkout's cone (not
    /// on disk) and a file hidden with `git update-index --skip-worktree` (on disk, and edited).
    #[test]
    fn a_skip_worktree_file_is_listed_only_when_it_is_on_disk() {
        let root = std::env::temp_dir().join(format!("ft-skip-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(root.join("in")).unwrap();
        std::fs::create_dir_all(root.join("cfg")).unwrap();
        for f in ["in/a", "top", "cfg/database.yml", ".env"] {
            std::fs::write(root.join(f), "").unwrap();
        }
        // `git sparse-checkout set in` left out/ off disk; out/b and out/deep/c are tagged `S`.
        // `cfg/database.yml` and `.env` were hidden with `--skip-worktree` and stayed.
        let out = b"H in/a\0S out/b\0S out/deep/c\0S cfg/database.yml\0S .env\0S cfg/gone\0H top\0";
        let mut on_disk = OnDisk::new(&root);
        assert_eq!(parse_ls_files_tagged(out, &mut on_disk), ["in/a", "cfg/database.yml", ".env", "top"].map(String::from).to_vec());
        // A folder off disk settles everything below it.
        assert_eq!(on_disk.dirs.get("out"), Some(&false));
        assert_eq!(on_disk.dirs.get("out/deep"), Some(&false));
        let _ = std::fs::remove_dir_all(&root);
    }

    /// The same through real git: `--skip-worktree` on a file that stays, and a sparse
    /// checkout that takes a folder off disk.
    #[test]
    fn find_lists_a_hidden_config_file_but_not_one_outside_the_sparse_cone() {
        let root = std::env::temp_dir().join(format!("ft-skip-git-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        let root = crate::share::canon(&root);
        let git = |args: &[&str]| {
            let o = std::process::Command::new("git")
                .current_dir(&root)
                .env("GIT_CONFIG_GLOBAL", "/dev/null")
                .env("GIT_CONFIG_NOSYSTEM", "1")
                .args(["-c", "user.email=t@example.com", "-c", "user.name=t", "-c", "commit.gpgsign=false"])
                .args(args)
                .output()
                .unwrap();
            assert!(o.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&o.stderr));
        };
        git(&["init", "-q", "-b", "main"]);
        std::fs::create_dir_all(root.join("in")).unwrap();
        std::fs::create_dir_all(root.join("out")).unwrap();
        for f in ["in/a", "out/b", ".env"] {
            std::fs::write(root.join(f), "x").unwrap();
        }
        git(&["add", "-A"]);
        git(&["commit", "-q", "--no-verify", "-m", "init"]);
        git(&["update-index", "--skip-worktree", ".env"]);
        std::fs::write(root.join(".env"), "local edit").unwrap();
        git(&["sparse-checkout", "set", "in"]);
        assert!(!root.join("out/b").exists() && root.join(".env").exists());
        let mut got = git_ls_files(&root).unwrap();
        got.sort();
        assert_eq!(got, [".env", "in/a"].map(String::from).to_vec());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn git_reporting_a_file_deleted_that_is_already_gone_costs_no_rebuild() {
        let mut i = ix(&["a.ts", "b.ts"]);
        // The watcher removed it first; the status run reports it deleted afterwards.
        apply_change(&mut i, Change::Gone("a.ts".into()));
        apply_change(&mut i, Change::FileGone("a.ts".into()));
        assert!(!i.dirty && !i.contains("a.ts") && i.contains("b.ts"));
    }

    /// Git's letters say nothing reliable about the disk: what a status run names is checked
    /// there before the index follows it.
    #[test]
    fn a_status_run_reconciles_the_files_it_names_against_the_disk() {
        let root = std::env::temp_dir().join(format!("ft-reconcile-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(root.join("cfg")).unwrap();
        std::fs::create_dir_all(root.join("src/deep")).unwrap();
        for f in ["top", "cfg/a", "cfg/b", "src/deep/b.ts"] {
            std::fs::write(root.join(f), "").unwrap();
        }
        let state = SearchState::new(Weak::new(), &root, true, false, false);
        state.inner.lock().index = Some(ix(&["top", "cfg/a", "cfg/b", "gone", "src/deep/a.ts", "other"]));
        // `git rm --cached top` and `git rm --cached -r cfg` (reported `D` plus `? cfg/`): still
        // on disk. `gone` really is. `git mv src/deep/a.ts src/deep/b.ts`: the source went,
        // the new name came. `ghost` was untracked when git scanned and removed before now.
        let named: Vec<String> = ["top", "cfg/a", "cfg/b", "gone", "src/deep/b.ts", "src/deep/a.ts", "ghost"].iter().map(|s| s.to_string()).collect();
        state.reconcile(&named);
        {
            let inner = state.inner.lock();
            let i = inner.index.as_ref().unwrap();
            for p in ["top", "cfg/a", "cfg/b", "src/deep/b.ts", "other"] {
                assert!(i.contains(p), "{p} is on disk and stays findable");
            }
            for p in ["gone", "src/deep/a.ts", "ghost"] {
                assert!(!i.contains(p), "{p} is not on disk");
            }
            assert!(!i.dirty, "per-file changes cost no rebuild");
        }
        // A run naming more than lookups are worth rebuilds instead.
        let many: Vec<String> = (0..=MAX_RECONCILE).map(|n| format!("f{n}")).collect();
        state.reconcile(&many);
        assert!(state.inner.lock().index.as_ref().unwrap().dirty);
        let _ = std::fs::remove_dir_all(&root);
    }

    /// `git mv Foo.ts foo.ts` names both sides, and where the volume folds case a lookup of
    /// `Foo.ts` still finds the file: only its stored spelling says the old name is gone.
    /// (On a volume that keeps case the old name simply isn't there; the test asks the same.)
    #[test]
    fn a_case_only_rename_leaves_one_name_in_the_index() {
        let tmp = std::env::temp_dir().join(format!("ft-case-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(tmp.join("src")).unwrap();
        let root = crate::share::canon(&tmp);
        std::fs::write(root.join("src/foo.ts"), "").unwrap();
        std::fs::write(root.join("Other.ts"), "").unwrap();
        let fold = crate::relocate::case_insensitive(&root);
        let state = SearchState::new(Weak::new(), &root, true, false, fold);
        // Built before the rename; `Other.ts` is a file a run names alone, in its own case.
        state.inner.lock().index = Some(ix(&["src/Foo.ts", "Other.ts"]));
        state.reconcile(&["src/foo.ts", "src/Foo.ts", "Other.ts"].map(String::from));
        let inner = state.inner.lock();
        let i = inner.index.as_ref().unwrap();
        assert!(i.contains("src/foo.ts") && i.contains("Other.ts"));
        assert!(!i.contains("src/Foo.ts"), "the rename's source is gone, however the volume looks it up");
        assert_eq!(i.len(), 2);
        drop(inner);
        let _ = std::fs::remove_dir_all(&root);
    }

    /// One file is one path, however the source spelled it: `ls-files` gives git's NFC, the
    /// watcher the name as stored (NFD on macOS).
    #[cfg(target_os = "macos")]
    #[test]
    fn nfc_and_nfd_spellings_are_one_path_on_macos() {
        let mut i = ix(&["caf\u{e9}/x.ts"]);
        i.insert("cafe\u{301}/x.ts");
        assert_eq!(i.len(), 1);
        assert!(i.remove("cafe\u{301}/x.ts"));
        assert_eq!(i.len(), 0);
    }

    #[test]
    fn ties_go_to_the_shorter_path_and_the_limit_holds() {
        let i = ix(&["a/b/c/readme.md", "readme.md", "x/readme.md"]);
        let hits = i.search("readme.md", 2, &|| false).unwrap();
        assert_eq!(hits.len(), 2);
        assert_eq!(hits[0].rel, "readme.md");
    }

    #[test]
    fn an_empty_query_finds_nothing() {
        assert!(ix(&["a"]).search("  ", 10, &|| false).unwrap().is_empty());
    }

    #[test]
    fn a_cancelled_scan_gives_up() {
        let i = Index::from_paths((0..10_000).map(|n| format!("d/f{n}.ts")));
        assert!(i.search("f", 10, &|| true).is_none());
    }

    #[test]
    fn insert_remove_and_compaction_keep_the_index_exact() {
        let mut i = ix(&["a.ts", "b.ts"]);
        i.insert("a.ts");
        assert_eq!(i.len(), 2);
        assert!(i.remove("a.ts"));
        assert!(!i.remove("a.ts"));
        assert!(!i.contains("a.ts") && i.contains("b.ts"));
        for n in 0..500 {
            i.insert(&format!("x/{n}"));
        }
        for n in 0..400 {
            assert!(i.remove(&format!("x/{n}")));
        }
        assert_eq!(i.len(), 101);
        assert!(i.dead * 4 <= i.spans.len().max(64), "compacted");
        assert!(i.contains("x/450") && i.contains("b.ts"));
        assert!(!i.dirty);
    }

    #[test]
    fn a_vanished_folder_marks_the_index_dirty() {
        let mut i = ix(&["src/a.ts"]);
        apply_change(&mut i, Change::Gone("src".into()));
        assert!(i.dirty);
        let mut i = ix(&["src/a.ts"]);
        apply_change(&mut i, Change::Gone("src/a.ts".into()));
        assert!(!i.dirty && i.len() == 0);
    }

    #[test]
    fn a_stale_query_stops_and_the_newest_answers() {
        let state = SearchState::new(Weak::new(), Path::new("/nonexistent"), false, false, false);
        state.inner.lock().index = Some(Index::from_paths((0..50_000).map(|n| format!("dir{}/file{n}.rs", n % 50))));
        // Seq 2 was asked before seq 1's scan began: seq 1 is stale from its first check.
        state.latest.lock().entry(7).or_default().store(2, Ordering::SeqCst);
        let stale = state.query(7, "file", 500, 1, false);
        assert_eq!(stale.seq, 1);
        assert!(stale.items.is_empty(), "a stale query returns nothing");
        let fresh = state.query(7, "file", 500, 2, false);
        assert_eq!(fresh.items.len(), 500);
    }

    #[test]
    fn another_window_on_the_same_root_never_cancels_a_query() {
        let state = SearchState::new(Weak::new(), Path::new("/nonexistent"), false, false, false);
        state.inner.lock().index = Some(Index::from_paths((0..50_000).map(|n| format!("dir{}/file{n}.rs", n % 50))));
        // Window A has searched a hundred times; window B, counting from zero, is not stale.
        assert_eq!(state.query(1, "file", 500, 100, false).items.len(), 500);
        assert_eq!(state.query(2, "file", 500, 1, false).items.len(), 500);
        state.forget(1);
        assert!(!state.latest.lock().contains_key(&1), "a closed view's counter goes");
    }

    #[test]
    fn a_shallow_root_outside_a_repo_waits_for_the_human() {
        let state = SearchState::new(Weak::new(), Path::new("/"), false, true, false);
        let r = state.query(1, "x", 10, 1, false);
        assert!(r.unavailable.is_some() && !r.indexing);
    }

    /// The keystroke budget: 100k paths answer in ≤30 ms (release; a debug build only checks
    /// it finishes). `cargo test --release --lib filetree::search -- --ignored`.
    #[test]
    #[ignore]
    fn a_keystroke_over_100k_paths_is_within_budget() {
        let words = ["src", "lib", "components", "filetree", "stores", "utils", "tests", "docs", "assets", "server"];
        let paths: Vec<String> = (0..100_000)
            .map(|n| format!("{}/{}/{}/file_{n}.{}", words[n % 10], words[(n / 10) % 10], words[(n / 100) % 10], ["ts", "rs", "svelte", "md"][n % 4]))
            .collect();
        let i = Index::from_paths(paths);
        let _ = i.search("warm", 500, &|| false);
        let mut worst = Duration::ZERO;
        for q in ["fts", "comp/file_99", "stores svelte", "x"] {
            let t = Instant::now();
            let hits = i.search(q, 500, &|| false).unwrap();
            worst = worst.max(t.elapsed());
            assert!(hits.len() <= 500);
        }
        eprintln!("worst keystroke over 100k paths: {worst:?}");
        if !cfg!(debug_assertions) {
            assert!(worst <= Duration::from_millis(30), "{worst:?}");
        }
    }

    #[test]
    fn walk_skips_dot_git_and_ignored_files() {
        let root = std::env::temp_dir().join(format!("ft-walk-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(root.join(".git")).unwrap();
        std::fs::create_dir_all(root.join("src/deep")).unwrap();
        std::fs::create_dir_all(root.join("node_modules/x")).unwrap();
        std::fs::write(root.join(".gitignore"), "node_modules/\n").unwrap();
        std::fs::write(root.join(".git/HEAD"), "ref: x\n").unwrap();
        std::fs::write(root.join("src/deep/a.ts"), "").unwrap();
        std::fs::write(root.join("node_modules/x/i.js"), "").unwrap();
        let got = walk(&crate::share::canon(&root));
        assert_eq!(got, vec![".gitignore".to_string(), "src/deep/a.ts".to_string()]);
        let _ = std::fs::remove_dir_all(&root);
    }
}
