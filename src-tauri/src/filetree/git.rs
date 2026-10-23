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
//!
//! **Remote roots (§15).** The same commands run on the ssh host through its helper
//! (`run`), which adds the same environment there. Keys are then left as git spells them: the
//! host's volume is not macOS's, so there is no NFC to fold to.

use std::borrow::Cow;
use std::collections::HashMap;
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Weak};
use std::time::{Duration, Instant};

use parking_lot::{Condvar, Mutex};

use super::hub::RootHub;
use super::paths::{norm, split_rel};
use super::remote::RemoteRoot;
use super::types::{BranchInfo, GitUpdate};
use super::Slots;
use crate::util::proc::{self, RunError};

/// Past this many changed files only folder roll-ups and the count are sent (§7.3).
pub const MAX_FILES: usize = 20_000;
/// `ft_diff_base` refuses a base bigger than this: the diff tab would choke on it anyway.
const MAX_BASE_BYTES: usize = 10 << 20;
/// Hover numstats kept per hub before the cache starts over.
const NUMSTAT_CACHE: usize = 512;
/// The two `ls-files -o` runs that list an untracked root's top level (`untracked_children`):
/// pathspecs, and whether the run passes `--directory`.
pub(super) const UNTRACKED_RUNS: [(&[&str], bool); 2] = [(&[":(glob)*", ":(exclude,glob)*/**"], false), (&[":(glob)*/**"], true)];

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
    /// An agent's write tool finished on a file under the root (§7.2): an edge, so it runs at
    /// once rather than after the watcher's trailing delay.
    Agent,
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
    /// numstats read it).
    pub files: HashMap<String, String>,
    pub dirs: HashMap<String, String>,
    pub truncated: bool,
    pub branch: Option<BranchInfo>,
    pub slow: bool,
    slow_streak: u32,
    pub error: Option<Failure>,
    /// A staged rename's source (repository-relative) by its key: numstat names both sides so
    /// git can pair them, or the new name counts as wholly added.
    renames: HashMap<String, String>,
    /// key → (gen, the file's stamp, counts). The stamp, because a file that stays `M` while
    /// it keeps being edited changes no letter and bumps no gen.
    numstat: HashMap<String, (u64, FileStamp, Option<(u32, u32)>)>,
}

/// A worktree file's size and modification time: when either moved, its counts did too.
pub type FileStamp = (u64, Option<std::time::SystemTime>);

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
    /// Every file the run named, a rename's source included, for the search index to look up
    /// on disk (`SearchState::reconcile`). Untracked folders aren't files and aren't named.
    pub named: Vec<String>,
}

/// What one git command answered.
pub struct GitOut {
    pub ok: bool,
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
}

pub struct GitCore {
    hub: Weak<RootHub>,
    program: OsString,
    /// A root on an ssh host: every command runs there, through its helper.
    remote: Option<Arc<RemoteRoot>>,
    pub timing: GitTiming,
    /// Every command runs here, `-C root`.
    root: PathBuf,
    /// The root relative to the repository's top level ("" when it is the top level).
    prefix: String,
    /// A network mount has no watcher: git refreshes on edges only.
    network: bool,
    enabled: AtomicBool,
    /// Bumped by every `file_tree_git` change. A run carries the epoch it started in, and a
    /// result from an earlier one is dropped: a run still going when decorations were turned
    /// off would otherwise publish onto the cleared data and colour the rows again.
    epoch: AtomicU64,
    sched: Mutex<Sched>,
    cv: Condvar,
    pub data: Mutex<GitData>,
    slots: Arc<Slots>,
}

impl GitCore {
    #[cfg(test)]
    pub fn new(hub: Weak<RootHub>, cfg: &GitConfig, root: &Path, top: &Path, network: bool, slots: Arc<Slots>) -> Arc<GitCore> {
        Self::new_with(hub, cfg, root, top, network, slots, None)
    }

    /// `remote`: the root is on an ssh host, and `root`/`top` are its POSIX paths there.
    pub fn new_with(hub: Weak<RootHub>, cfg: &GitConfig, root: &Path, top: &Path, network: bool, slots: Arc<Slots>, remote: Option<Arc<RemoteRoot>>) -> Arc<GitCore> {
        // In git's spelling (NFC on macOS): the root's own path may be stored NFD.
        let joined = root.strip_prefix(top).map(|p| p.components().map(|c| c.as_os_str().to_string_lossy()).collect::<Vec<_>>().join("/")).unwrap_or_default();
        let prefix = if remote.is_some() { joined } else { norm(&joined).into_owned() };
        Arc::new(GitCore {
            hub,
            program: cfg.program.clone(),
            remote,
            timing: cfg.timing,
            root: root.to_path_buf(),
            prefix,
            network,
            enabled: AtomicBool::new(cfg.enabled),
            epoch: AtomicU64::new(0),
            sched: Mutex::new(Sched::default()),
            cv: Condvar::new(),
            data: Mutex::new(GitData::default()),
            slots,
        })
    }

    pub fn enabled(&self) -> bool {
        self.enabled.load(Ordering::SeqCst)
    }

    /// Which `file_tree_git` setting a run starts under (see `epoch`).
    pub fn epoch(&self) -> u64 {
        self.epoch.load(Ordering::SeqCst)
    }

    /// `file_tree_git` changed. Turning it off forgets everything (the hub then tells its
    /// subscribers to drop their decorations); turning it on runs at once.
    pub fn set_enabled(self: &Arc<Self>, on: bool) -> bool {
        let was = self.enabled.swap(on, Ordering::SeqCst);
        if was == on {
            return false;
        }
        self.epoch.fetch_add(1, Ordering::SeqCst);
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
            if (d.slow || self.network) && matches!(t, Trigger::Worktree | Trigger::Command | Trigger::Agent) {
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
        untranslated(&mut c);
        c
    }

    /// One git command in the root, here or on the ssh host, killed at `timeout`.
    fn run(&self, args: &[String], timeout: Duration) -> Result<GitOut, RunError> {
        if let Some(r) = &self.remote {
            return r.git(args, timeout);
        }
        let mut c = self.cmd();
        c.args(args);
        proc::output_timeout(&mut c, timeout).map(|o| GitOut { ok: o.status.success(), stdout: o.stdout, stderr: o.stderr })
    }

    /// A path in git's spelling as this root keys it (see `new_with`).
    fn norm<'a>(&self, s: &'a str) -> Cow<'a, str> {
        if self.remote.is_some() {
            Cow::Borrowed(s)
        } else {
            norm(s)
        }
    }

    /// One status run, blocking.
    pub fn run_status(&self) -> Result<Status, Failure> {
        let args = ["status", "--porcelain=v2", "-z", "--branch", "--untracked-files=normal", "--", "."].map(String::from);
        match self.run(&args, self.timing.timeout) {
            Err(RunError::Spawn(e)) if e.kind() == std::io::ErrorKind::NotFound => Err(Failure::Missing),
            Err(RunError::TimedOut) => Err(Failure::TimedOut),
            Err(e) => Err(Failure::Other(e.to_string())),
            Ok(o) if !o.ok => Err(failure_from_stderr(&o.stderr)),
            Ok(o) => {
                let mut st = parse_porcelain_v2(&o.stdout);
                self.expand_untracked_root(&mut st);
                Ok(st)
            }
        }
    }

    /// A root inside a wholly untracked folder: git collapses the folder, so the only entry is
    /// the root itself (`? new/sub/`) or a folder above it (`? new/` for the root `new`), and
    /// neither names anything below the root. Swap it for the root's own untracked contents,
    /// collapsed the same way, so they decorate and the Changed lens lists them.
    fn expand_untracked_root(&self, st: &mut Status) {
        if self.prefix.is_empty() {
            return;
        }
        let covers_root = |e: &StatusEntry| e.dir && e.letter == "?" && super::paths::is_at_or_under(&self.prefix, &self.norm(&e.path));
        let Some(i) = st.entries.iter().position(covers_root) else { return };
        let Some(children) = self.untracked_children() else { return };
        st.entries.remove(i);
        for (name, dir) in children {
            st.entries.push(StatusEntry { path: format!("{}/{name}", self.prefix), letter: "?", orig: None, dir });
        }
    }

    /// The root's top-level entries git counts as untracked, as `-unormal` would report them
    /// from the repository's top: a folder holding only ignored files (or nothing) is left
    /// out. Two runs with fixed arguments, since `--directory` from inside an untracked
    /// folder collapses everything to `./`: `*` (which never crosses a `/` under `glob`)
    /// lists the top-level files, and `*/**` with `--directory` lists each top-level folder
    /// holding something untracked, collapsed. `*` alone still descends into every folder
    /// (git matches a folder's name against it and walks in), which made the first run cost
    /// every file under the root, a whole unignored `node_modules` on each status run: the
    /// `exclude` pathspec `*/**` is what stops that, since git prunes a folder an exclude
    /// covers. Each run then costs the top level only: a pathspec per name made each run
    /// match every entry against every name, seconds for a folder of 20k, and a batch of long
    /// names overran Windows' command line. None (and the run keeps git's own entry) when
    /// either fails.
    fn untracked_children(&self) -> Option<Vec<(String, bool)>> {
        let mut out = Vec::new();
        for (specs, directory) in UNTRACKED_RUNS {
            let mut args: Vec<String> = ["ls-files", "-o", "--exclude-standard", "-z"].map(String::from).to_vec();
            if directory {
                args.extend(["--directory", "--no-empty-directory"].map(String::from));
            }
            args.push("--".into());
            args.extend(specs.iter().map(|s| s.to_string()));
            let o = match self.run(&args, self.timing.timeout) {
                Ok(o) if o.ok => o,
                Ok(o) => {
                    log::debug!("filetree: listing the untracked root failed: {}", failure_from_stderr(&o.stderr).note());
                    return None;
                }
                Err(e) => {
                    log::debug!("filetree: listing the untracked root failed: {e}");
                    return None;
                }
            };
            for raw in o.stdout.split(|b| *b == 0).filter(|f| !f.is_empty()) {
                let p = String::from_utf8_lossy(raw);
                let name = p.trim_end_matches('/');
                // Only the top level: the `*/**` run lists a folder as `name/`.
                if !name.is_empty() && !name.contains('/') {
                    out.push((name.to_string(), directory));
                }
            }
        }
        Some(out)
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
    /// `epoch` is the one the run started in: a run that outlived a `file_tree_git` change
    /// is dropped whole (checked under the data lock, which `set_enabled` resets under).
    pub fn apply(&self, epoch: u64, out: Result<Status, Failure>, took: Duration) -> Option<Published> {
        let mut d = self.data.lock();
        if epoch != self.epoch() || !self.enabled() {
            return None;
        }
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
        let mut named = Vec::new();
        let mut renames = HashMap::new();
        for e in st.entries {
            // A rename's source counts even when only it is under the root (moved out).
            if let Some(o) = e.orig.as_deref().and_then(|o| self.to_root_rel(&self.norm(o))) {
                named.push(o);
            }
            // Keys are NFC on macOS whatever `core.precomposeunicode` says, so the client can
            // look a row up by normalizing its on-disk name once (`OpenAck.nfc_names`).
            let Some(rel) = self.to_root_rel(&self.norm(&e.path)) else { continue };
            if !e.dir {
                named.push(rel.clone());
            }
            if let (Some(o), "R") = (&e.orig, e.letter) {
                renames.insert(rel.clone(), o.clone());
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
        d.renames = renames;
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
        Some(Published { from, diff, named })
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

    /// `git diff --numstat HEAD` for one changed file, cached until a status run changes
    /// anything or the file itself changes (`stamp`, from the hover's own stat). Clean and
    /// untracked files cost nothing: there is no diff to count.
    pub fn numstat(&self, rel: &str, stamp: FileStamp) -> Option<(u32, u32)> {
        if !self.enabled() || split_rel(rel).is_err() || rel.is_empty() {
            return None;
        }
        // The row's rel is the on-disk spelling; the keys are git's (NFC on macOS).
        let key = self.norm(rel);
        let (gen, source) = {
            let d = self.data.lock();
            let letter = d.files.get(key.as_ref())?;
            if letter == "?" {
                return None;
            }
            if let Some((g, s, v)) = d.numstat.get(key.as_ref()) {
                if *g == d.gen && *s == stamp {
                    return *v;
                }
            }
            (d.gen, d.renames.get(key.as_ref()).cloned())
        };
        // git prints repository-relative paths: the record for this file is found by that.
        let target = if self.prefix.is_empty() { key.to_string() } else { format!("{}/{key}", self.prefix) };
        let got = {
            let _slot = self.slots.acquire();
            // `-M` whatever `diff.renames` says, and the source named too (from the top: it
            // may be outside the root), so a staged rename counts its edits, not its size.
            let mut args: Vec<String> = ["diff", "--numstat", "-z", "-M", "HEAD", "--"].map(String::from).to_vec();
            args.push(format!(":(literal){rel}"));
            if let Some(src) = &source {
                args.push(format!(":(top,literal){src}"));
            }
            let nfc = self.remote.is_none();
            self.run(&args, Duration::from_secs(5)).ok().filter(|o| o.ok).and_then(|o| parse_numstat(&o.stdout, &target, nfc))
        };
        let mut d = self.data.lock();
        if d.gen == gen {
            if d.numstat.len() >= NUMSTAT_CACHE {
                d.numstat.clear();
            }
            d.numstat.insert(key.into_owned(), (gen, stamp, got));
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
            self.run(&["cat-file".to_string(), "blob".to_string(), spec], Duration::from_secs(15)).map_err(|e| format!("git {e}"))?
        };
        if !out.ok {
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

/// A commit id as `# branch.oid` gives it: not "(initial)" before the first commit.
pub fn is_oid(s: &str) -> bool {
    s.len() >= 7 && s.bytes().all(|b| b.is_ascii_hexdigit())
}

/// Files a commit may name before the rest are left out: a commit made during a turn is a
/// handful of files, and a merge pulling in a branch is not "what the agent committed".
const MAX_COMMITTED: usize = 2000;

impl GitCore {
    /// The files changed from `old` to `new`, as rels of the root, when `new` descends from
    /// `old` (commits were made: a checkout of another branch is no commit). None when it
    /// doesn't, or git fails.
    pub fn committed_between(&self, old: &str, new: &str) -> Option<Vec<String>> {
        if !(is_oid(old) && is_oid(new)) {
            return None;
        }
        let _slot = self.slots.acquire();
        let o = self.run(&["merge-base", "--is-ancestor", old, new].map(String::from), Duration::from_secs(5)).ok()?;
        if !o.ok {
            return None;
        }
        let args = ["diff-tree", "--no-commit-id", "--name-only", "-r", "-z", old, new].map(String::from);
        let o = self.run(&args, Duration::from_secs(10)).ok().filter(|o| o.ok)?;
        Some(
            o.stdout
                .split(|b| *b == 0)
                .filter(|f| !f.is_empty())
                .filter_map(|f| self.to_root_rel(&self.norm(&String::from_utf8_lossy(f))))
                .take(MAX_COMMITTED)
                .collect(),
        )
    }
}

/// git's messages in English: `diff_base` tells "not at that base" from a real failure, and
/// `failure_from_stderr` spots `safe.directory`, by reading stderr. A Linux git with
/// translations under a German desktop would otherwise answer in German, and a new file's
/// Compare with HEAD would fail instead of diffing against nothing. LC_ALL, because it beats
/// a LC_MESSAGES or LANGUAGE the user set; the -z output this module parses is bytes either way.
fn untranslated(c: &mut std::process::Command) {
    c.env("LC_ALL", "C");
}

/// The counts for `target` (repository-relative) from `--numstat -z`: `added\tdeleted\tpath\0`,
/// or `added\tdeleted\t\0source\0dest\0` for a rename. A binary file's `-\t-` counts nothing.
/// When no record names the target (a spelling git and the disk disagree on), a lone record
/// is still this file's: the pathspec named nothing else. `nfc`: compare in this platform's
/// one spelling (a remote root's names are compared as git gives them).
fn parse_numstat(out: &[u8], target: &str, nfc: bool) -> Option<(u32, u32)> {
    let mut fields = out.split(|b| *b == 0).filter(|f| !f.is_empty());
    let (mut records, mut lone) = (0, None);
    while let Some(rec) = fields.next() {
        let text = String::from_utf8_lossy(rec);
        let mut it = text.splitn(3, '\t');
        let (Some(a), Some(d)) = (it.next(), it.next()) else { continue };
        let path = match it.next() {
            Some(p) if !p.is_empty() => p.to_string(),
            _ => {
                let _source = fields.next();
                fields.next().map(|p| String::from_utf8_lossy(p).into_owned()).unwrap_or_default()
            }
        };
        let counts = a.parse().ok().zip(d.parse().ok());
        if (if nfc { norm(&path) } else { Cow::Borrowed(path.as_str()) }) == target {
            return counts;
        }
        records += 1;
        lone = counts;
    }
    if records == 1 {
        lone
    } else {
        None
    }
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
        let epoch = core.epoch();
        if !core.enabled() {
            continue;
        }
        if !hub.git_audience() {
            core.sched.lock().deferred = true;
            continue;
        }
        // Timed from the slot, not from before waiting for it: a fast repo queued behind two
        // slow ones (or a numstat, or a Compare with HEAD) is not a slow repo.
        let (out, took) = {
            let _slot = core.slots.acquire();
            let started = Instant::now();
            (core.run_status(), started.elapsed())
        };
        hub.git_finished(epoch, out, took);
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

    /// A run that started under the current setting.
    fn now(c: &Arc<GitCore>, out: Result<Status, Failure>, took: Duration) -> Option<Published> {
        c.apply(c.epoch(), out, took)
    }

    #[test]
    fn a_run_that_outlived_turning_decorations_off_is_dropped() {
        let c = core(Path::new("/r"), Path::new("/r"));
        now(&c, st(&[("a.ts", "M")]), Duration::ZERO).unwrap();
        // The run starts, then the human turns decorations off (and maybe on again) before
        // it finishes: neither may publish what it found onto the cleared data.
        let started = c.epoch();
        assert!(c.set_enabled(false));
        assert!(c.apply(started, st(&[("a.ts", "M")]), Duration::ZERO).is_none());
        assert!(c.data.lock().files.is_empty(), "the cleared data stays cleared");
        let off = c.epoch();
        c.epoch.fetch_add(1, Ordering::SeqCst); // what set_enabled(true) does, without spawning a run
        c.enabled.store(true, Ordering::SeqCst);
        assert!(c.apply(off, st(&[("a.ts", "M")]), Duration::ZERO).is_none(), "nor after turning them back on");
        assert!(now(&c, st(&[("a.ts", "M")]), Duration::ZERO).is_some(), "a run started after the change publishes");
    }

    #[test]
    fn apply_sends_a_change_only_when_something_changed() {
        let c = core(Path::new("/r"), Path::new("/r"));
        let p = now(&c,st(&[("a/x.ts", "M")]), Duration::ZERO).expect("first run publishes");
        assert_eq!(p.from, 0);
        assert_eq!(p.diff.files, vec![("a/x.ts".to_string(), "M".to_string())]);
        assert_eq!(p.diff.dirs, vec![("a".to_string(), "M".to_string())]);
        assert!(now(&c,st(&[("a/x.ts", "M")]), Duration::ZERO).is_none(), "an unchanged run sends nothing");
        let p = now(&c,st(&[("b.ts", "?")]), Duration::ZERO).unwrap();
        assert_eq!(p.diff.files, vec![("b.ts".to_string(), "?".to_string())]);
        assert_eq!(p.diff.del, vec!["a".to_string(), "a/x.ts".to_string()]);
        assert_eq!(p.named, vec!["b.ts".to_string()]);
        assert_eq!(p.diff.count, 1);
    }

    #[test]
    fn the_search_index_hears_every_file_a_run_names_and_no_folder() {
        // `git rm --cached -r cfg` (`1 D. cfg/a` plus `? cfg/`) and `git mv lib/a.ts lib/b.ts`
        // (`2 R. … lib/b.ts`, source `lib/a.ts`): which are on disk is the index's lookup.
        let c = core(Path::new("/r"), Path::new("/r"));
        let entries = vec![
            StatusEntry { path: "cfg/a".into(), letter: "D", orig: None, dir: false },
            StatusEntry { path: "cfg".into(), letter: "?", orig: None, dir: true },
            StatusEntry { path: "lib/b.ts".into(), letter: "R", orig: Some("lib/a.ts".into()), dir: false },
        ];
        let p = now(&c, Ok(Status { branch: None, entries }), Duration::ZERO).unwrap();
        assert_eq!(p.named, vec!["cfg/a".to_string(), "lib/a.ts".to_string(), "lib/b.ts".to_string()]);
        // A root inside the repo: a source outside it is not the root's to name.
        let c = core(Path::new("/r/lib"), Path::new("/r"));
        let entries = vec![StatusEntry { path: "lib/b.ts".into(), letter: "R", orig: Some("elsewhere/a.ts".into()), dir: false }];
        assert_eq!(now(&c, Ok(Status { branch: None, entries }), Duration::ZERO).unwrap().named, vec!["b.ts".to_string()]);
    }

    #[test]
    fn a_root_inside_a_larger_repo_keeps_only_its_own_paths() {
        let c = core(Path::new("/r/a/b"), Path::new("/r"));
        let p = now(&c,st(&[("a/b/x.ts", "M"), ("a/bb/y.ts", "M"), ("a/b", "?"), ("top.ts", "A")]), Duration::ZERO).unwrap();
        assert_eq!(p.diff.files, vec![("x.ts".to_string(), "M".to_string())]);
    }

    /// macOS git spells paths NFC (`core.precomposeunicode`) while the root's own path may be
    /// stored NFD: the prefix must still strip, and keys come out NFC either way.
    #[cfg(target_os = "macos")]
    #[test]
    fn keys_are_nfc_and_an_nfd_root_still_strips() {
        let nfd_root = format!("/r/Re\u{301}sume\u{301}");
        let c = core(Path::new(&nfd_root), Path::new("/r"));
        let nfd_file = "R\u{e9}sum\u{e9}/cafe\u{301}.txt".to_string(); // precompose turned off
        let p = now(&c, st(&[("R\u{e9}sum\u{e9}/caf\u{e9}.txt", "M"), (&nfd_file, "?")]), Duration::ZERO).expect("the root's paths publish");
        assert_eq!(p.diff.files, vec![("caf\u{e9}.txt".to_string(), "?".to_string())], "one NFC key for one name");
    }

    #[test]
    fn an_untracked_folder_keeps_its_slash() {
        let c = core(Path::new("/r"), Path::new("/r"));
        let out = Ok(Status { branch: None, entries: vec![StatusEntry { path: "build".into(), letter: "?", orig: None, dir: true }] });
        let p = now(&c,out, Duration::ZERO).unwrap();
        assert_eq!(p.diff.files, vec![("build/".to_string(), "?".to_string())]);
        assert!(p.diff.dirs.is_empty());
        assert!(p.named.is_empty(), "a folder is not a file for the search index");
    }

    #[test]
    fn past_the_cap_only_folders_and_the_count_go_out() {
        let c = core(Path::new("/r"), Path::new("/r"));
        let many: Vec<(String, &'static str)> = (0..MAX_FILES + 5).map(|i| (format!("d{}/f{i}", i % 3), "M")).collect();
        let entries: Vec<(&str, &'static str)> = many.iter().map(|(p, l)| (p.as_str(), *l)).collect();
        let p = now(&c,st(&entries), Duration::ZERO).unwrap();
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
        now(&c,st(&[]), slow);
        assert!(!c.data.lock().slow, "one slow run is not a slow repo");
        now(&c,st(&[]), slow);
        assert!(c.data.lock().slow);
        assert_eq!(c.health().0, "slow");
        // Between 1 s and 2 s: neither slow nor fast, the mode stays.
        now(&c,st(&[]), c.timing.fast_run + Duration::from_millis(1));
        assert!(c.data.lock().slow);
        now(&c,st(&[]), Duration::from_millis(5));
        assert!(!c.data.lock().slow);
        // A timeout counts as slow.
        now(&c,Err(Failure::TimedOut), Duration::ZERO);
        now(&c,Err(Failure::TimedOut), Duration::ZERO);
        assert!(c.data.lock().slow);
    }

    #[test]
    fn numstat_parses_counts_and_skips_binary() {
        assert_eq!(parse_numstat(b"12\t3\tsrc/a.ts\0", "src/a.ts", true), Some((12, 3)));
        assert_eq!(parse_numstat(b"-\t-\timg.png\0", "img.png", true), None);
        assert_eq!(parse_numstat(b"", "a", true), None);
        // A rename's record names the destination after the source.
        assert_eq!(parse_numstat(b"1\t0\t\0a.txt\0b.txt\0", "b.txt", true), Some((1, 0)));
        // Several records: only the target's counts; a lone record is the file's whatever its spelling.
        assert_eq!(parse_numstat(b"5\t5\tsrc.txt\0201\t0\tb.txt\0", "b.txt", true), Some((201, 0)));
        assert_eq!(parse_numstat(b"5\t5\tsrc.txt\0201\t0\tb.txt\0", "c.txt", true), None);
        assert_eq!(parse_numstat(b"7\t1\tcafe\xcc\x81.txt\0", "caf\u{e9}.txt", true), Some((7, 1)));
    }

    #[test]
    fn git_answers_in_english_whatever_the_desktop_language() {
        let c = core(Path::new("/r"), Path::new("/r")).cmd();
        assert!(c.get_envs().any(|(k, v)| k == "LC_ALL" && v == Some(std::ffi::OsStr::new("C"))));
    }

    #[test]
    fn stderr_classification() {
        assert_eq!(failure_from_stderr(b"fatal: detected dubious ownership in repository at '/x'\n"), Failure::Untrusted);
        assert_eq!(failure_from_stderr(b"fatal: not a git repository\n"), Failure::Other("not a git repository".into()));
    }
}
