//! Git decorations for a hub (docs/file-tree.md §7).
//!
//! One `git status --porcelain=v2 -z` per refresh, run through `util/proc.rs` (stdin null, no
//! prompts, no optional locks, a deadline), parsed and rolled up here, and sent as only the keys
//! that changed. Why a subprocess and not libgit2: it honours the user's config, fsmonitor, LFS
//! and `safe.directory`, and builds nothing in C on three OSes.
//!
//! **Scheduling (§7.2).** One worker thread per repository hub, blocked on a condvar with no
//! timeout while nothing is due, so idle costs nothing. Any number of signals during a run
//! collapse into one rerun after it. Runs start at least 300 ms apart, and at most two git
//! processes run app-wide (`git_slots`). Triggers and their trailing delays:
//! - `.git/index`, HEAD, refs: 120 ms;
//! - a worktree change that isn't ignored: 300 ms;
//! - a command finishing in a terminal under the root (OSC 133 D), the window regaining focus,
//!   a new view, a manual refresh: at once.
//!
//! **Slow repos.** Two runs in a row over 2 s (or a timeout) switch the repo to refreshing on
//! index/HEAD changes, focus and manual refresh only, and Health says so; a run under 1 s ends
//! it. A run nobody can see (every subscription paused or stalled) is deferred until one
//! resumes.
//!
//! **Paths.** git reports paths relative to the repository's top level even with `-C root`;
//! when the root is a folder inside a larger repo, the prefix is stripped and anything outside
//! the root dropped (the `-- .` pathspec already keeps the run to the root's subtree).

use std::collections::HashMap;
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Weak};
use std::time::{Duration, Instant};

use parking_lot::{Condvar, Mutex};

use super::hub::RootHub;
use super::paths::split_rel;
use super::types::{BranchInfo, GitUpdate};
use super::Slots;
use crate::util::proc::{self, RunError};

/// Past this many changed files only folder roll-ups and the count are sent (§7.3).
pub const MAX_FILES: usize = 20_000;
/// `ft_diff_base` refuses a base bigger than this: the diff tab would choke on it anyway.
const MAX_BASE_BYTES: usize = 10 << 20;
/// Hover numstats kept per hub before the cache starts over.
const NUMSTAT_CACHE: usize = 512;

/// The worker's clock. Fields so the slow-mode tests don't take ten seconds.
#[derive(Debug, Clone, Copy)]
pub struct GitTiming {
    /// A run is killed after this.
    pub timeout: Duration,
    /// Two runs in a row at least this long: slow mode.
    pub slow_run: Duration,
    /// A run shorter than this ends slow mode.
    pub fast_run: Duration,
    pub index_delay: Duration,
    pub worktree_delay: Duration,
    /// Run starts are at least this far apart.
    pub min_gap: Duration,
    /// A trailing delay never pushes a run further than this past the first signal: a build
    /// writing files without pause must still decorate.
    pub max_defer: Duration,
}

impl Default for GitTiming {
    fn default() -> Self {
        GitTiming {
            timeout: Duration::from_secs(10),
            slow_run: Duration::from_secs(2),
            fast_run: Duration::from_secs(1),
            index_delay: Duration::from_millis(120),
            worktree_delay: Duration::from_millis(300),
            min_gap: Duration::from_millis(300),
            max_defer: Duration::from_secs(1),
        }
    }
}

/// What the hub's git worker runs, and whether at all (`file_tree_git`).
#[derive(Debug, Clone)]
pub struct GitConfig {
    pub program: OsString,
    pub enabled: bool,
    pub timing: GitTiming,
}

impl Default for GitConfig {
    fn default() -> Self {
        GitConfig { program: OsString::from("git"), enabled: true, timing: GitTiming::default() }
    }
}

/// Why a status run is wanted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Trigger {
    /// `.git/index`, HEAD, a ref, packed-refs, MERGE_HEAD.
    Index,
    /// A file in the worktree that isn't ignored.
    Worktree,
    /// A command finished (OSC 133 D) in a terminal whose folder is in this repo.
    Command,
    /// The window regained focus.
    Focus,
    /// A view subscribed, or one that was paused came back.
    Open,
    /// The human asked (Refresh).
    Manual,
}

// ── Parsing ──────────────────────────────────────────────────────────────────────────────────

/// One changed path from a status run, relative to the repository's top level.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StatusEntry {
    pub path: String,
    pub letter: &'static str,
    /// A rename's or copy's source.
    pub orig: Option<String>,
    /// An untracked folder (`? dir/`): every file in it is untracked too.
    pub dir: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Status {
    pub branch: Option<BranchInfo>,
    pub entries: Vec<StatusEntry>,
}

/// The letter for a tracked change from its XY (index, worktree) code.
fn letter_of(xy: &[u8]) -> &'static str {
    let (x, y) = (xy.first().copied().unwrap_or(b'.'), xy.get(1).copied().unwrap_or(b'.'));
    if x == b'D' || y == b'D' {
        "D"
    } else if x == b'R' {
        "R"
    } else if x == b'A' || x == b'C' {
        "A"
    } else if x == b'T' || y == b'T' {
        "T"
    } else {
        "M"
    }
}

/// `git status --porcelain=v2 -z --branch` output. Records are NUL-terminated and a rename's
/// source is the next NUL field, so names with spaces or newlines come through as they are.
/// Unknown records are skipped rather than failing the run: a newer git may add some.
pub fn parse_porcelain_v2(out: &[u8]) -> Status {
    let mut st = Status::default();
    let mut branch = BranchInfo::default();
    let mut has_branch = false;
    let mut fields = out.split(|b| *b == 0).filter(|f| !f.is_empty());
    while let Some(raw) = fields.next() {
        let rec = String::from_utf8_lossy(raw);
        let Some(kind) = rec.as_bytes().first().copied() else { continue };
        match kind {
            b'#' => {
                let mut it = rec.splitn(3, ' ');
                let (_, key, val) = (it.next(), it.next().unwrap_or(""), it.next().unwrap_or(""));
                has_branch = true;
                match key {
                    "branch.oid" => branch.oid = val.to_string(),
                    "branch.head" => branch.head = val.to_string(),
                    "branch.upstream" => branch.upstream = Some(val.to_string()),
                    "branch.ab" => {
                        for part in val.split(' ') {
                            if let Some(n) = part.strip_prefix('+') {
                                branch.ahead = n.parse().unwrap_or(0);
                            } else if let Some(n) = part.strip_prefix('-') {
                                branch.behind = n.parse().unwrap_or(0);
                            }
                        }
                    }
                    _ => {}
                }
            }
            b'1' => {
                let parts: Vec<&str> = rec.splitn(9, ' ').collect();
                if parts.len() == 9 {
                    st.entries.push(StatusEntry { path: parts[8].to_string(), letter: letter_of(parts[1].as_bytes()), orig: None, dir: false });
                }
            }
            b'2' => {
                let parts: Vec<&str> = rec.splitn(10, ' ').collect();
                // The source path is the next field, whatever happens to this record.
                let orig = fields.next().map(|o| String::from_utf8_lossy(o).into_owned());
                if parts.len() == 10 {
                    st.entries.push(StatusEntry { path: parts[9].to_string(), letter: letter_of(parts[1].as_bytes()), orig, dir: false });
                }
            }
            b'u' => {
                let parts: Vec<&str> = rec.splitn(11, ' ').collect();
                if parts.len() == 11 {
                    st.entries.push(StatusEntry { path: parts[10].to_string(), letter: "C", orig: None, dir: false });
                }
            }
            b'?' => {
                if let Some(p) = rec.strip_prefix("? ") {
                    let dir = p.ends_with('/');
                    let path = p.trim_end_matches('/').to_string();
                    if !path.is_empty() {
                        st.entries.push(StatusEntry { path, letter: "?", orig: None, dir });
                    }
                }
            }
            _ => {}
        }
    }
    if has_branch {
        st.branch = Some(branch);
    }
    st
}

// ── Roll-up and diffing ─────────────────────────────────────────────────────────────────────

/// conflict > deleted > modified > added > untracked (§7.3). Renames and type changes count
/// as modifications.
fn severity(letter: &str) -> u8 {
    match letter {
        "C" => 5,
        "D" => 4,
        "M" | "R" | "T" => 3,
        "A" => 2,
        "?" => 1,
        _ => 0,
    }
}

fn letter_for(sev: u8) -> &'static str {
    match sev {
        5 => "C",
        4 => "D",
        3 => "M",
        2 => "A",
        _ => "?",
    }
}

/// Each folder above a changed path takes the most severe state below it. The root itself
/// ("") gets none: it has no row. A walk stops at the first folder already that severe,
/// since everything above it already is too.
pub fn roll_up(files: &HashMap<String, String>) -> HashMap<String, String> {
    let mut sev: HashMap<String, u8> = HashMap::new();
    for (rel, letter) in files {
        let s = severity(letter);
        // An untracked folder's key ends in '/': its roll-up starts at its parent.
        let mut cur = rel.trim_end_matches('/');
        while let Some(i) = cur.rfind('/') {
            cur = &cur[..i];
            match sev.get_mut(cur) {
                Some(have) if *have >= s => break,
                Some(have) => *have = s,
                None => {
                    sev.insert(cur.to_string(), s);
                }
            }
        }
    }
    sev.into_iter().map(|(k, s)| (k, letter_for(s).to_string())).collect()
}

/// The keys of `new` that are new or changed, and the keys of `old` that are gone.
pub fn diff_maps(old: &HashMap<String, String>, new: &HashMap<String, String>) -> (Vec<(String, String)>, Vec<String>) {
    let mut set: Vec<(String, String)> = new.iter().filter(|(k, v)| old.get(*k) != Some(*v)).map(|(k, v)| (k.clone(), v.clone())).collect();
    let mut del: Vec<String> = old.keys().filter(|k| !new.contains_key(*k)).cloned().collect();
    set.sort();
    del.sort();
    (set, del)
}

// ── The worker ───────────────────────────────────────────────────────────────────────────────

/// Why a run failed. Some failures won't fix themselves: those stop the automatic runs until
/// the human refreshes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Failure {
    Missing,
    Untrusted,
    TimedOut,
    Other(String),
}

impl Failure {
    pub fn note(&self) -> String {
        match self {
            Failure::Missing => "git: not installed, so no decorations".to_string(),
            Failure::Untrusted => "git: this repository isn't trusted (safe.directory), so no decorations".to_string(),
            Failure::TimedOut => "git: status timed out".to_string(),
            Failure::Other(m) => format!("git: {m}"),
        }
    }

    fn permanent(&self) -> bool {
        matches!(self, Failure::Missing | Failure::Untrusted)
    }
}

fn failure_from_stderr(stderr: &[u8]) -> Failure {
    let text = String::from_utf8_lossy(stderr);
    if text.contains("dubious ownership") || text.contains("safe.directory") {
        return Failure::Untrusted;
    }
    let line = text.lines().map(str::trim).find(|l| !l.is_empty()).unwrap_or("status failed");
    Failure::Other(line.trim_start_matches("fatal: ").to_string())
}

#[derive(Default)]
struct Sched {
    due: Option<Instant>,
    first: Option<Instant>,
    last_start: Option<Instant>,
    thread: bool,
    stop: bool,
    /// A run came due while nobody could see it.
    deferred: bool,
}

/// The decorations as last computed, and the worker's own bookkeeping.
#[derive(Default)]
pub struct GitData {
    /// Bumped by every change sent; 0 before the first run.
    pub gen: u64,
    pub ran: bool,
    /// Every changed path, root-relative (kept whole even when the wire is truncated: hover
    /// numstats and the index reconciliation read it).
    pub files: HashMap<String, String>,
    pub dirs: HashMap<String, String>,
    pub truncated: bool,
    pub branch: Option<BranchInfo>,
    pub slow: bool,
    slow_streak: u32,
    pub error: Option<Failure>,
    numstat: HashMap<String, (u64, Option<(u32, u32)>)>,
}

impl GitData {
    /// The files map as the wire carries it: empty past the cap.
    fn wire_files(&self) -> HashMap<String, String> {
        if self.truncated {
            HashMap::new()
        } else {
            self.files.clone()
        }
    }

    pub fn full_update(&self) -> GitUpdate {
        let mut files: Vec<(String, String)> = if self.truncated { Vec::new() } else { self.files.iter().map(|(k, v)| (k.clone(), v.clone())).collect() };
        let mut dirs: Vec<(String, String)> = self.dirs.iter().map(|(k, v)| (k.clone(), v.clone())).collect();
        files.sort();
        dirs.sort();
        GitUpdate {
            gen: self.gen,
            full: true,
            files,
            dirs,
            del: Vec::new(),
            branch: self.branch.clone(),
            truncated: self.truncated,
            count: self.files.len() as u32,
        }
    }
}

/// What a finished run changed, for the hub to send.
pub struct Published {
    pub from: u64,
    pub diff: GitUpdate,
    /// Untracked files and deleted paths, for the search index.
    pub untracked: Vec<String>,
    pub deleted: Vec<String>,
}

pub struct GitCore {
    hub: Weak<RootHub>,
    program: OsString,
    pub timing: GitTiming,
    /// Every command runs here, `-C root`.
    root: PathBuf,
    /// The root relative to the repository's top level ("" when it is the top level).
    prefix: String,
    /// A network mount has no watcher: git refreshes on edges only.
    network: bool,
    enabled: AtomicBool,
    sched: Mutex<Sched>,
    cv: Condvar,
    pub data: Mutex<GitData>,
    slots: Arc<Slots>,
}

impl GitCore {
    pub fn new(hub: Weak<RootHub>, cfg: &GitConfig, root: &Path, top: &Path, network: bool, slots: Arc<Slots>) -> Arc<GitCore> {
        let prefix = root
            .strip_prefix(top)
            .map(|p| p.components().map(|c| c.as_os_str().to_string_lossy()).collect::<Vec<_>>().join("/"))
            .unwrap_or_default();
        Arc::new(GitCore {
            hub,
            program: cfg.program.clone(),
            timing: cfg.timing,
            root: root.to_path_buf(),
            prefix,
            network,
            enabled: AtomicBool::new(cfg.enabled),
            sched: Mutex::new(Sched::default()),
            cv: Condvar::new(),
            data: Mutex::new(GitData::default()),
            slots,
        })
    }

    pub fn enabled(&self) -> bool {
        self.enabled.load(Ordering::SeqCst)
    }

    /// `file_tree_git` changed. Turning it off forgets everything (the hub then tells its
    /// subscribers to drop their decorations); turning it on runs at once.
    pub fn set_enabled(self: &Arc<Self>, on: bool) -> bool {
        let was = self.enabled.swap(on, Ordering::SeqCst);
        if was == on {
            return false;
        }
        if on {
            self.signal(Trigger::Manual);
        } else {
            let mut d = self.data.lock();
            let gen = d.gen;
            *d = GitData { gen: gen + 1, ran: true, ..GitData::default() };
        }
        true
    }

    /// Ask for a status run. Cheap: a lock and maybe a notify.
    pub fn signal(self: &Arc<Self>, t: Trigger) {
        if !self.enabled() {
            return;
        }
        {
            let d = self.data.lock();
            if d.error.as_ref().is_some_and(Failure::permanent) && t != Trigger::Manual {
                return;
            }
            // Slow repos, and network mounts (no watcher to trust), refresh on index changes,
            // focus, a new view and the human's refresh only.
            if (d.slow || self.network) && matches!(t, Trigger::Worktree | Trigger::Command) {
                return;
            }
        }
        let delay = match t {
            Trigger::Index => self.timing.index_delay,
            Trigger::Worktree => self.timing.worktree_delay,
            _ => Duration::ZERO,
        };
        let now = Instant::now();
        let mut s = self.sched.lock();
        if s.stop {
            return;
        }
        let first = *s.first.get_or_insert(now);
        let want = now + delay;
        let mut due = match s.due {
            // Trailing: the latest signal decides, but never past `max_defer` after the first.
            Some(d) if delay > Duration::ZERO => d.max(want).min(first + self.timing.max_defer),
            Some(d) => d.min(want),
            None => want,
        };
        if let Some(last) = s.last_start {
            due = due.max(last + self.timing.min_gap);
        }
        s.due = Some(due);
        s.deferred = false;
        if !s.thread {
            s.thread = true;
            let me = self.clone();
            let spawned = std::thread::Builder::new().name("ft-git".into()).spawn(move || worker(me));
            if let Err(e) = spawned {
                s.thread = false;
                log::warn!("filetree: couldn't start the git worker: {e}");
            }
        } else {
            self.cv.notify_one();
        }
    }

    /// A subscription came back: run what was deferred while nobody could see it.
    pub fn wake_if_deferred(self: &Arc<Self>) {
        let deferred = std::mem::take(&mut self.sched.lock().deferred);
        if deferred {
            self.signal(Trigger::Open);
        }
    }

    /// The hub is going away: the worker exits.
    pub fn stop(&self) {
        let mut s = self.sched.lock();
        s.stop = true;
        self.cv.notify_all();
    }

    fn cmd(&self) -> std::process::Command {
        let mut c = proc::git_at(&self.program);
        c.arg("--no-optional-locks").arg("-C").arg(&self.root);
        c
    }

    /// One status run, blocking.
    pub fn run_status(&self) -> Result<Status, Failure> {
        let mut c = self.cmd();
        c.args(["status", "--porcelain=v2", "-z", "--branch", "--untracked-files=normal", "--", "."]);
        match proc::output_timeout(&mut c, self.timing.timeout) {
            Err(RunError::Spawn(e)) if e.kind() == std::io::ErrorKind::NotFound => Err(Failure::Missing),
            Err(RunError::TimedOut) => Err(Failure::TimedOut),
            Err(e) => Err(Failure::Other(e.to_string())),
            Ok(o) if !o.status.success() => Err(failure_from_stderr(&o.stderr)),
            Ok(o) => Ok(parse_porcelain_v2(&o.stdout)),
        }
    }

    /// A repository path as a root-relative one; None when it is outside the root (or is it).
    fn to_root_rel(&self, path: &str) -> Option<String> {
        if self.prefix.is_empty() {
            return Some(path.to_string());
        }
        let rest = path.strip_prefix(self.prefix.as_str())?.strip_prefix('/')?;
        (!rest.is_empty()).then(|| rest.to_string())
    }

    /// Fold a run's outcome into the data. Some when something the wire carries changed.
    pub fn apply(&self, out: Result<Status, Failure>, took: Duration) -> Option<Published> {
        let mut d = self.data.lock();
        let slow_run = took >= self.timing.slow_run || matches!(out, Err(Failure::TimedOut));
        if slow_run {
            d.slow_streak += 1;
            if d.slow_streak >= 2 {
                d.slow = true;
            }
        } else {
            d.slow_streak = 0;
            if took < self.timing.fast_run {
                d.slow = false;
            }
        }
        let st = match out {
            Err(f) => {
                if d.error.as_ref() != Some(&f) {
                    log::info!("filetree: {} ({})", f.note(), self.root.display());
                }
                d.error = Some(f);
                return None;
            }
            Ok(st) => st,
        };
        d.error = None;
        let mut files: HashMap<String, String> = HashMap::with_capacity(st.entries.len());
        let mut untracked = Vec::new();
        let mut deleted = Vec::new();
        for e in st.entries {
            let Some(rel) = self.to_root_rel(&e.path) else { continue };
            if e.letter == "?" && !e.dir {
                untracked.push(rel.clone());
            }
            if e.letter == "D" {
                deleted.push(rel.clone());
            }
            // A wholly untracked folder keeps git's trailing slash: the client can't tell a
            // folder from a file by name, and needs to, to mark everything inside it.
            let key = if e.dir { format!("{rel}/") } else { rel };
            files.insert(key, e.letter.to_string());
        }
        let dirs = roll_up(&files);
        let truncated = files.len() > MAX_FILES;
        let old_wire = d.wire_files();
        let new_wire = if truncated { HashMap::new() } else { files.clone() };
        let (set_files, mut del) = diff_maps(&old_wire, &new_wire);
        let (set_dirs, del_dirs) = diff_maps(&d.dirs, &dirs);
        del.extend(del_dirs);
        del.sort();
        del.dedup();
        let count_changed = d.files.len() != files.len();
        let unchanged = d.ran
            && set_files.is_empty()
            && set_dirs.is_empty()
            && del.is_empty()
            && d.branch == st.branch
            && d.truncated == truncated
            && !count_changed;
        d.files = files;
        d.dirs = dirs;
        d.truncated = truncated;
        if unchanged {
            return None;
        }
        let from = d.gen;
        d.gen += 1;
        d.ran = true;
        d.branch = st.branch;
        d.numstat.clear();
        let diff = GitUpdate {
            gen: d.gen,
            full: false,
            files: set_files,
            dirs: set_dirs,
            del,
            branch: d.branch.clone(),
            truncated,
            count: d.files.len() as u32,
        };
        Some(Published { from, diff, untracked, deleted })
    }

    /// (state, note) for Health: "off" | "live" | "slow" | "error".
    pub fn health(&self) -> (&'static str, Option<String>) {
        if !self.enabled() {
            return ("off", None);
        }
        let d = self.data.lock();
        if let Some(f) = &d.error {
            return ("error", Some(f.note()));
        }
        if d.slow {
            return ("slow", Some("git: slow repo, refresh on demand".to_string()));
        }
        ("live", None)
    }

    /// `git diff --numstat HEAD` for one changed file, cached until the next status run
    /// changes anything. Clean and untracked files cost nothing: there is no diff to count.
    pub fn numstat(&self, rel: &str) -> Option<(u32, u32)> {
        if !self.enabled() || split_rel(rel).is_err() || rel.is_empty() {
            return None;
        }
        let gen = {
            let d = self.data.lock();
            let letter = d.files.get(rel)?;
            if letter == "?" {
                return None;
            }
            if let Some((g, v)) = d.numstat.get(rel) {
                if *g == d.gen {
                    return *v;
                }
            }
            d.gen
        };
        let got = {
            let _slot = self.slots.acquire();
            let mut c = self.cmd();
            c.args(["diff", "--numstat", "-z", "HEAD", "--"]).arg(format!(":(literal){rel}"));
            proc::output_timeout(&mut c, Duration::from_secs(5)).ok().filter(|o| o.status.success()).and_then(|o| parse_numstat(&o.stdout))
        };
        let mut d = self.data.lock();
        if d.gen == gen {
            if d.numstat.len() >= NUMSTAT_CACHE {
                d.numstat.clear();
            }
            d.numstat.insert(rel.to_string(), (gen, got));
        }
        got
    }

    /// A file's content at `base` ("HEAD" or "index") for Compare with HEAD. Ok(None): the
    /// file isn't there (new since that base).
    pub fn diff_base(&self, rel: &str, base: &str) -> Result<Option<String>, String> {
        let parts = split_rel(rel)?;
        if parts.is_empty() {
            return Err("The root folder has no content to compare".to_string());
        }
        // `./` makes the path relative to `-C root`, so a root inside a larger repo works.
        let spec = match base {
            "HEAD" => format!("HEAD:./{rel}"),
            "index" => format!(":./{rel}"),
            other => return Err(format!("Unknown base: {other}")),
        };
        let out = {
            let _slot = self.slots.acquire();
            let mut c = self.cmd();
            c.args(["cat-file", "blob"]).arg(spec);
            proc::output_timeout(&mut c, Duration::from_secs(15)).map_err(|e| format!("git {e}"))?
        };
        if !out.status.success() {
            let err = String::from_utf8_lossy(&out.stderr);
            let absent = ["does not exist", "exists on disk, but not in", "not in the index", "invalid object name", "Not a valid object name"];
            if absent.iter().any(|a| err.contains(a)) {
                return Ok(None);
            }
            return Err(failure_from_stderr(&out.stderr).note());
        }
        if out.stdout.len() > MAX_BASE_BYTES {
            return Err("The file is too large to compare".to_string());
        }
        if out.stdout[..out.stdout.len().min(8000)].contains(&0) {
            return Err("Binary file: nothing to compare as text".to_string());
        }
        Ok(Some(String::from_utf8_lossy(&out.stdout).into_owned()))
    }
}

/// `added\tdeleted\tpath\0` for one file; a binary file's `-\t-` counts nothing.
fn parse_numstat(out: &[u8]) -> Option<(u32, u32)> {
    let rec = out.split(|b| *b == 0).next()?;
    let text = std::str::from_utf8(rec).ok()?;
    let mut it = text.splitn(3, '\t');
    let a = it.next()?.parse().ok()?;
    let d = it.next()?.parse().ok()?;
    Some((a, d))
}

/// The hub's git thread: waits until a run is due, runs it inside the global slots, hands the
/// result to the hub. Holds the hub only while running, so an idle hub can still be dropped
/// (its Drop stops this thread).
fn worker(core: Arc<GitCore>) {
    loop {
        {
            let mut s = core.sched.lock();
            loop {
                if s.stop {
                    s.thread = false;
                    return;
                }
                match s.due {
                    None => core.cv.wait(&mut s),
                    Some(d) => {
                        if d <= Instant::now() {
                            break;
                        }
                        core.cv.wait_until(&mut s, d);
                    }
                }
            }
            s.due = None;
            s.first = None;
            s.last_start = Some(Instant::now());
        }
        let Some(hub) = core.hub.upgrade() else {
            core.sched.lock().thread = false;
            return;
        };
        if !core.enabled() {
            continue;
        }
        if !hub.git_audience() {
            core.sched.lock().deferred = true;
            continue;
        }
        let started = Instant::now();
        let out = {
            let _slot = core.slots.acquire();
            core.run_status()
        };
        hub.git_finished(out, started.elapsed());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn z(records: &[&str]) -> Vec<u8> {
        let mut v = Vec::new();
        for r in records {
            v.extend_from_slice(r.as_bytes());
            v.push(0);
        }
        v
    }

    const H: &str = "N... 100644 100644 100644 587be6b4c3f93f93c489c0111bba5596147a26cb 587be6b4c3f93f93c489c0111bba5596147a26cb";

    #[test]
    fn parses_branch_headers() {
        let st = parse_porcelain_v2(&z(&["# branch.oid abc123", "# branch.head main", "# branch.upstream origin/main", "# branch.ab +2 -1", "# stash 3"]));
        assert_eq!(
            st.branch,
            Some(BranchInfo { head: "main".into(), oid: "abc123".into(), upstream: Some("origin/main".into()), ahead: 2, behind: 1 })
        );
        let st = parse_porcelain_v2(&z(&["# branch.oid (initial)", "# branch.head (detached)"]));
        let b = st.branch.unwrap();
        assert_eq!((b.head.as_str(), b.oid.as_str(), b.upstream, b.ahead), ("(detached)", "(initial)", None, 0));
        assert_eq!(parse_porcelain_v2(b"").branch, None);
    }

    #[test]
    fn parses_ordinary_renamed_unmerged_and_untracked_records() {
        let out = z(&[
            "# branch.head main",
            &format!("1 .M {H} src/a.ts"),
            &format!("1 A. {H} new file.txt"),
            &format!("1 D. {H} gone.rs"),
            &format!("1 .D {H} deleted in worktree.rs"),
            &format!("1 .T {H} link"),
            &format!("2 R. {H} R100 dir/new name.ts"),
            "dir/old name.ts",
            &format!("2 C. {H} C75 copy.ts"),
            "orig.ts",
            "u UU N... 100644 100644 100644 100644 aaaa bbbb cccc conflict.txt",
            "? untracked.txt",
            "? build/",
            "! ignored.log",
        ]);
        let st = parse_porcelain_v2(&out);
        let got: Vec<(&str, &str, Option<&str>, bool)> = st.entries.iter().map(|e| (e.path.as_str(), e.letter, e.orig.as_deref(), e.dir)).collect();
        assert_eq!(
            got,
            vec![
                ("src/a.ts", "M", None, false),
                ("new file.txt", "A", None, false),
                ("gone.rs", "D", None, false),
                ("deleted in worktree.rs", "D", None, false),
                ("link", "T", None, false),
                ("dir/new name.ts", "R", Some("dir/old name.ts"), false),
                ("copy.ts", "A", Some("orig.ts"), false),
                ("conflict.txt", "C", None, false),
                ("untracked.txt", "?", None, false),
                ("build", "?", None, true),
            ]
        );
    }

    #[test]
    fn names_with_newlines_spaces_and_unicode_survive() {
        let out = z(&[
            &format!("1 .M {H} a\nb.txt"),
            &format!("1 .M {H}   leading spaces.md"),
            &format!("2 R. {H} R90 caf\u{e9}/\u{1F600} x.ts"),
            "caf\u{e9}/old\nname.ts",
            "? na\u{ef}ve/",
        ]);
        let st = parse_porcelain_v2(&out);
        let paths: Vec<&str> = st.entries.iter().map(|e| e.path.as_str()).collect();
        assert_eq!(paths, vec!["a\nb.txt", "  leading spaces.md", "caf\u{e9}/\u{1F600} x.ts", "na\u{ef}ve"]);
        assert_eq!(st.entries[2].orig.as_deref(), Some("caf\u{e9}/old\nname.ts"));
    }

    #[test]
    fn a_rename_source_is_consumed_even_when_it_looks_like_a_record() {
        // The source of a rename is a raw path; one named "? x" must not become an entry.
        let st = parse_porcelain_v2(&z(&[&format!("2 R. {H} R100 new"), "? x", "? real"]));
        let paths: Vec<&str> = st.entries.iter().map(|e| e.path.as_str()).collect();
        assert_eq!(paths, vec!["new", "real"]);
    }

    fn m(pairs: &[(&str, &str)]) -> HashMap<String, String> {
        pairs.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect()
    }

    #[test]
    fn roll_up_takes_the_most_severe_state_below() {
        let files = m(&[
            ("a/b/c.ts", "?"),
            ("a/b/d.ts", "A"),
            ("u/new/", "?"),
            ("a/e.ts", "M"),
            ("a/x/r.ts", "R"),
            ("z/gone.ts", "D"),
            ("z/y/conflict.ts", "C"),
            ("w/t", "T"),
            ("top.txt", "M"),
        ]);
        let dirs = roll_up(&files);
        // An untracked folder ("u/new/") rolls up into its parent, not into itself.
        assert_eq!(dirs, m(&[("a", "M"), ("a/b", "A"), ("a/x", "M"), ("z", "C"), ("z/y", "C"), ("w", "M"), ("u", "?")]));
        assert!(!dirs.contains_key(""), "the root has no row and gets no roll-up");
        // Order independence: the early stop must not hide a more severe state.
        let mut sorted: Vec<_> = files.into_iter().collect();
        sorted.reverse();
        assert_eq!(roll_up(&sorted.into_iter().collect()), dirs);
    }

    #[test]
    fn diffing_sends_only_changed_keys() {
        let old = m(&[("a", "M"), ("b", "?"), ("c", "A")]);
        let new = m(&[("a", "M"), ("b", "A"), ("d", "D")]);
        let (set, del) = diff_maps(&old, &new);
        assert_eq!(set, vec![("b".to_string(), "A".to_string()), ("d".to_string(), "D".to_string())]);
        assert_eq!(del, vec!["c".to_string()]);
        let (set, del) = diff_maps(&new, &new);
        assert!(set.is_empty() && del.is_empty());
    }

    fn core(prefix_root: &Path, top: &Path) -> Arc<GitCore> {
        GitCore::new(Weak::new(), &GitConfig::default(), prefix_root, top, false, Arc::new(Slots::new(2)))
    }

    fn st(entries: &[(&str, &'static str)]) -> Result<Status, Failure> {
        Ok(Status { branch: None, entries: entries.iter().map(|(p, l)| StatusEntry { path: p.to_string(), letter: l, orig: None, dir: false }).collect() })
    }

    #[test]
    fn apply_sends_a_change_only_when_something_changed() {
        let c = core(Path::new("/r"), Path::new("/r"));
        let p = c.apply(st(&[("a/x.ts", "M")]), Duration::ZERO).expect("first run publishes");
        assert_eq!(p.from, 0);
        assert_eq!(p.diff.files, vec![("a/x.ts".to_string(), "M".to_string())]);
        assert_eq!(p.diff.dirs, vec![("a".to_string(), "M".to_string())]);
        assert!(c.apply(st(&[("a/x.ts", "M")]), Duration::ZERO).is_none(), "an unchanged run sends nothing");
        let p = c.apply(st(&[("b.ts", "?")]), Duration::ZERO).unwrap();
        assert_eq!(p.diff.files, vec![("b.ts".to_string(), "?".to_string())]);
        assert_eq!(p.diff.del, vec!["a".to_string(), "a/x.ts".to_string()]);
        assert_eq!(p.untracked, vec!["b.ts".to_string()]);
        assert_eq!(p.diff.count, 1);
    }

    #[test]
    fn a_root_inside_a_larger_repo_keeps_only_its_own_paths() {
        let c = core(Path::new("/r/a/b"), Path::new("/r"));
        let p = c.apply(st(&[("a/b/x.ts", "M"), ("a/bb/y.ts", "M"), ("a/b", "?"), ("top.ts", "A")]), Duration::ZERO).unwrap();
        assert_eq!(p.diff.files, vec![("x.ts".to_string(), "M".to_string())]);
    }

    #[test]
    fn an_untracked_folder_keeps_its_slash() {
        let c = core(Path::new("/r"), Path::new("/r"));
        let out = Ok(Status { branch: None, entries: vec![StatusEntry { path: "build".into(), letter: "?", orig: None, dir: true }] });
        let p = c.apply(out, Duration::ZERO).unwrap();
        assert_eq!(p.diff.files, vec![("build/".to_string(), "?".to_string())]);
        assert!(p.diff.dirs.is_empty());
        assert!(p.untracked.is_empty(), "a folder is not a file for the search index");
    }

    #[test]
    fn past_the_cap_only_folders_and_the_count_go_out() {
        let c = core(Path::new("/r"), Path::new("/r"));
        let many: Vec<(String, &'static str)> = (0..MAX_FILES + 5).map(|i| (format!("d{}/f{i}", i % 3), "M")).collect();
        let entries: Vec<(&str, &'static str)> = many.iter().map(|(p, l)| (p.as_str(), *l)).collect();
        let p = c.apply(st(&entries), Duration::ZERO).unwrap();
        assert!(p.diff.truncated);
        assert!(p.diff.files.is_empty());
        assert_eq!(p.diff.dirs.len(), 3);
        assert_eq!(p.diff.count as usize, MAX_FILES + 5);
        let full = c.data.lock().full_update();
        assert!(full.files.is_empty() && full.truncated);
    }

    #[test]
    fn slow_mode_trips_after_two_slow_runs_and_ends_on_a_fast_one() {
        let c = core(Path::new("/r"), Path::new("/r"));
        let slow = c.timing.slow_run + Duration::from_millis(1);
        c.apply(st(&[]), slow);
        assert!(!c.data.lock().slow, "one slow run is not a slow repo");
        c.apply(st(&[]), slow);
        assert!(c.data.lock().slow);
        assert_eq!(c.health().0, "slow");
        // Between 1 s and 2 s: neither slow nor fast, the mode stays.
        c.apply(st(&[]), c.timing.fast_run + Duration::from_millis(1));
        assert!(c.data.lock().slow);
        c.apply(st(&[]), Duration::from_millis(5));
        assert!(!c.data.lock().slow);
        // A timeout counts as slow.
        c.apply(Err(Failure::TimedOut), Duration::ZERO);
        c.apply(Err(Failure::TimedOut), Duration::ZERO);
        assert!(c.data.lock().slow);
    }

    #[test]
    fn numstat_parses_counts_and_skips_binary() {
        assert_eq!(parse_numstat(b"12\t3\tsrc/a.ts\0"), Some((12, 3)));
        assert_eq!(parse_numstat(b"-\t-\timg.png\0"), None);
        assert_eq!(parse_numstat(b""), None);
    }

    #[test]
    fn stderr_classification() {
        assert_eq!(failure_from_stderr(b"fatal: detected dubious ownership in repository at '/x'\n"), Failure::Untrusted);
        assert_eq!(failure_from_stderr(b"fatal: not a git repository\n"), Failure::Other("not a git repository".into()));
    }
}
