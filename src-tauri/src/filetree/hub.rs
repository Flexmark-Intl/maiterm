//! One root on screen (docs/file-tree.md §5, §6.6).
//!
//! A hub holds a snapshot of every folder some subscriber has open, each with a generation, and
//! the subscriptions themselves: one webview's Channel each. It is shared by every window
//! showing the root and lives on 30 s after the last one leaves.
//!
//! **Convergence.** Every snapshot change mints a new generation from one process-wide counter
//! (so a generation from a hub that died never matches a new hub's), and is sent while the
//! snapshot lock is held, so what a client sees for a folder is always in the order the
//! snapshot changed. A `Delta` names the generation it applies to; a client holding any other
//! asks for the folder again. A missed message, a reordered chunk or a resubscribe race heals
//! instead of drifting.
//!
//! **Backpressure (§5.4).** A subscription that is paused (`ft_pause`) or stalled (more than
//! 256 messages or 8 MB unacked: an occluded webview stops acking on its own) is sent nothing;
//! the folders that changed meanwhile are remembered by name, and resent whole on resume.
//!
//! **Listing jobs.** One listing per folder at a time: a second request joins the first, so a
//! read hung on a dead mount is never re-issued onto another thread. Each hub runs at most 4
//! listings (2 on a network mount), inside a global limit of 32.
//!
//! Lock order, always: jobs → dirs → subs → watch. No lock is held across another hub's.

use std::collections::{BTreeSet, HashMap, HashSet, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Weak};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use parking_lot::Mutex;

use super::ignore_cache::IgnoreStack;
use super::list::{self, ListError, Listing, CHUNK};
use super::paths::{self, join_rel, name_key, rel_to_native, split_rel};
use super::types::{Caps, DirReq, Entry, Envelope, OpenAck, RepoInfo, StatInfo, TreeEvent};
use super::watch::{self, HubWatch, RawSender, WatchContext, WatchError};
use super::{fsclass, Focus, Slots};

/// Where a subscription's events go. A Channel in the app; a recorder in tests.
pub trait Sink: Send + Sync {
    /// False when the message could not be delivered.
    fn send(&self, env: Envelope) -> bool;
}

impl Sink for tauri::ipc::Channel<Envelope> {
    fn send(&self, env: Envelope) -> bool {
        tauri::ipc::Channel::send(self, env).is_ok()
    }
}

/// One counter for every generation in the process.
static GEN: AtomicU64 = AtomicU64::new(1);

pub fn next_gen() -> u64 {
    GEN.fetch_add(1, Ordering::SeqCst)
}

/// Unacked messages past which a subscription stalls.
pub const STALL_MESSAGES: u64 = 256;
/// Unacked bytes past which a subscription stalls.
pub const STALL_BYTES: usize = 8 << 20;
/// A listing on a network mount that hasn't answered by now is reported as not responding; it
/// keeps running and its result is still sent.
pub const NETWORK_SOFT_DEADLINE: Duration = Duration::from_secs(3);

pub struct Sub {
    pub id: u64,
    pub window: String,
    sink: Arc<dyn Sink>,
    paused: bool,
    stalled: bool,
    seq: u64,
    acked: u64,
    unacked: VecDeque<(u64, usize)>,
    unacked_bytes: usize,
    /// Folders that changed while nothing could be sent.
    dirty: BTreeSet<String>,
    root_gone: bool,
    health_dirty: bool,
    /// The folders this subscriber has open.
    interest: HashSet<String>,
    dead: bool,
}

impl Sub {
    fn new(id: u64, window: &str, sink: Arc<dyn Sink>) -> Self {
        Sub {
            id,
            window: window.to_string(),
            sink,
            paused: false,
            stalled: false,
            seq: 0,
            acked: 0,
            unacked: VecDeque::new(),
            unacked_bytes: 0,
            dirty: BTreeSet::new(),
            root_gone: false,
            health_dirty: false,
            interest: HashSet::new(),
            dead: false,
        }
    }

    fn deliver(&mut self, ev: TreeEvent) {
        if self.dead {
            return;
        }
        if self.paused || self.stalled {
            match &ev {
                TreeEvent::RootGone => self.root_gone = true,
                TreeEvent::Health { .. } => self.health_dirty = true,
                other => {
                    if let Some(rel) = other.rel() {
                        self.dirty.insert(rel.to_string());
                    }
                }
            }
            return;
        }
        self.seq += 1;
        let bytes = ev.approx_bytes();
        if !self.sink.send(Envelope { seq: self.seq, ev }) {
            self.dead = true;
            return;
        }
        self.unacked.push_back((self.seq, bytes));
        self.unacked_bytes += bytes;
        if self.seq - self.acked > STALL_MESSAGES || self.unacked_bytes > STALL_BYTES {
            self.stalled = true;
        }
    }

    /// Record an ack. True when it ended a stall (the first ack after one resumes).
    fn ack(&mut self, seq: u64) -> bool {
        let seq = seq.min(self.seq);
        if seq > self.acked {
            self.acked = seq;
        }
        while let Some(&(s, b)) = self.unacked.front() {
            if s > self.acked {
                break;
            }
            self.unacked.pop_front();
            self.unacked_bytes -= b;
        }
        let resumed = self.stalled;
        self.stalled = false;
        resumed
    }

    fn send_dir(&mut self, rel: &str, snap: &DirSnap) {
        self.send_dir_from(rel, snap, 0);
    }

    /// The chunks of a listing from `first` on (chunk 0 may have gone out early).
    fn send_dir_from(&mut self, rel: &str, snap: &DirSnap, first: usize) {
        let n = snap.entries.len().div_ceil(CHUNK).max(1);
        for i in first..n {
            let chunk = if snap.entries.is_empty() { Vec::new() } else { snap.entries[i * CHUNK..((i + 1) * CHUNK).min(snap.entries.len())].to_vec() };
            self.deliver(TreeEvent::Dir {
                rel: rel.to_string(),
                gen: snap.gen,
                chunk: i as u32,
                last: i + 1 == n,
                total: snap.total,
                truncated: snap.truncated,
                entries: chunk,
            });
            if self.paused || self.stalled || self.dead {
                // The rest would only be marked dirty one chunk at a time; the folder is.
                self.dirty.insert(rel.to_string());
                return;
            }
        }
    }
}

#[derive(Debug, Clone)]
pub(super) struct DirSnap {
    pub gen: u64,
    pub entries: Vec<Entry>,
    pub total: u32,
    pub truncated: bool,
}

impl DirSnap {
    fn from(l: Listing, gen: u64) -> Self {
        DirSnap { gen, entries: l.entries, total: l.total, truncated: l.truncated }
    }
}

/// Who is waiting on a listing.
#[derive(Debug, Clone)]
pub(super) enum Waiter {
    /// `ft_expand` / `ft_open` restore: NotModified when its generation is still current.
    Expand { sub: u64, known_gen: Option<u64> },
    /// `ft_refresh`: always a full Dir.
    Refresh { sub: u64 },
}

impl Waiter {
    fn sub(&self) -> u64 {
        match self {
            Waiter::Expand { sub, .. } | Waiter::Refresh { sub } => *sub,
        }
    }
}

struct Inflight {
    id: u64,
    waiters: Vec<Waiter>,
    /// The disk changed after this listing started reading: list again when it's done.
    rerun: bool,
    started: bool,
    timed_out: bool,
}

#[derive(Default)]
struct Jobs {
    queue: VecDeque<String>,
    inflight: HashMap<String, Inflight>,
    workers: usize,
    next_id: u64,
}

struct WatchState {
    hw: HubWatch,
    /// The watcher is still being started (in the background).
    pending: bool,
    /// Folders with a watch of their own, oldest expansion first (PerDir only).
    order: VecDeque<String>,
    /// Folders polled because no watch could be had.
    polled: BTreeSet<String>,
    /// Each polled folder's mtime when polling started, then at its last tick. Taken when
    /// polling starts, not on the first tick: a change in between would read as the baseline.
    poll_mtimes: HashMap<String, Option<SystemTime>>,
    poller_running: bool,
    /// Why there is no watcher, when there isn't.
    error: Option<String>,
}

pub struct HubInit {
    pub place: String,
    pub root: PathBuf,
    pub fs_class: &'static str,
    pub shallow: bool,
    pub user_excludes: Vec<String>,
    pub raw_capacity: usize,
}

pub struct RootHub {
    me: Weak<RootHub>,
    pub place: String,
    /// Canonical.
    pub root: PathBuf,
    /// The volume folds case.
    pub fold: bool,
    pub fs_class: &'static str,
    pub repo: Option<RepoInfo>,
    git_dir: Option<PathBuf>,
    pub(super) dirs: Mutex<HashMap<String, DirSnap>>,
    subs: Mutex<HashMap<u64, Sub>>,
    pub(super) ignore: Mutex<IgnoreStack>,
    jobs: Mutex<Jobs>,
    max_workers: usize,
    slots: Arc<Slots>,
    focus: Arc<Focus>,
    watch: Mutex<WatchState>,
    raw: RawSender,
    pub(super) overflow: Arc<std::sync::atomic::AtomicBool>,
    gone: AtomicBool,
    /// Coalescer batches processed: the idle test's measure.
    pub wakeups: Arc<AtomicU64>,
    /// Bumped by every subscribe and every reap scheduled; a reap only happens if unchanged.
    pub grace_token: AtomicU64,
}

/// The repository the root is in, found the way git finds it: the nearest `.git` at or above
/// the root — a folder, or a file saying `gitdir: …` (a linked worktree or submodule), whose
/// `commondir` names the shared repository. Read off the disk rather than asked of `git
/// rev-parse`, which costs a process spawn (~20 ms) on the path to the first rows.
pub(super) fn repo_info(root: &Path) -> Option<(RepoInfo, PathBuf)> {
    let (top, git_dir) = root.ancestors().find_map(|dir| {
        let dot = dir.join(".git");
        let meta = std::fs::symlink_metadata(&dot).ok()?;
        if meta.is_dir() || (meta.file_type().is_symlink() && dot.is_dir()) {
            return Some((dir.to_path_buf(), crate::share::canon(&dot)));
        }
        let text = std::fs::read_to_string(&dot).ok()?;
        let target = text.lines().find_map(|l| l.strip_prefix("gitdir:"))?.trim();
        let target = Path::new(target);
        let abs = if target.is_absolute() { target.to_path_buf() } else { dir.join(target) };
        abs.is_dir().then(|| (dir.to_path_buf(), crate::share::canon(&abs)))
    })?;
    let common = std::fs::read_to_string(git_dir.join("commondir"))
        .ok()
        .map(|c| {
            let c = PathBuf::from(c.trim());
            crate::share::canon(&if c.is_absolute() { c } else { git_dir.join(c) })
        })
        .unwrap_or_else(|| git_dir.clone());
    let top = crate::share::canon(&top);
    let toplevel_rel = if top == root {
        String::new()
    } else {
        match root.strip_prefix(&top) {
            Ok(below) => vec![".."; below.components().count()].join("/"),
            Err(_) => return None,
        }
    };
    Some((
        RepoInfo { toplevel_rel, git_dir: git_dir.to_string_lossy().into_owned(), common_dir: common.to_string_lossy().into_owned() },
        git_dir,
    ))
}

impl RootHub {
    pub fn new(init: HubInit, slots: Arc<Slots>, focus: Arc<Focus>, ctx: Arc<WatchContext>) -> Arc<RootHub> {
        let network = init.fs_class == fsclass::NETWORK;
        let repo = repo_info(&init.root);
        let git_dir = repo.as_ref().map(|(_, g)| g.clone());
        let ignore = IgnoreStack::new(&init.root, git_dir.as_deref(), &init.user_excludes);
        let (raw, rx, overflow) = watch::channel(init.raw_capacity);
        let (root, shallow) = (init.root.clone(), init.shallow);
        let hub = Arc::new_cyclic(|me| RootHub {
            me: me.clone(),
            place: init.place,
            fold: crate::relocate::case_insensitive(&init.root),
            root: init.root,
            fs_class: init.fs_class,
            repo: repo.map(|(r, _)| r),
            git_dir,
            dirs: Mutex::new(HashMap::new()),
            subs: Mutex::new(HashMap::new()),
            ignore: Mutex::new(ignore),
            jobs: Mutex::new(Jobs::default()),
            max_workers: if network { 2 } else { 4 },
            slots,
            focus,
            watch: Mutex::new(WatchState {
                hw: HubWatch::None,
                pending: !network,
                order: VecDeque::new(),
                polled: BTreeSet::new(),
                poll_mtimes: HashMap::new(),
                poller_running: false,
                error: None,
            }),
            raw,
            overflow,
            gone: AtomicBool::new(false),
            wakeups: Arc::new(AtomicU64::new(0)),
            grace_token: AtomicU64::new(0),
        });
        super::coalesce::spawn(Arc::downgrade(&hub), rx, hub.wakeups.clone());
        if !network {
            // The watcher starts off the open path: FSEvents' stream start blocks for seconds
            // while fseventsd is busy (a big checkout, an npm install), and the first rows must
            // never wait on that. Until it runs, snapshots aren't trusted and every expand
            // reads the disk; once it runs, every open folder is listed again to catch what
            // changed in between.
            let me = Arc::downgrade(&hub);
            let raw = hub.raw.clone();
            let spawned = std::thread::Builder::new().name("ft-watch-start".into()).spawn(move || {
                let (hw, error) = watch::create(&root, shallow, false, raw, &ctx);
                if let Some(h) = me.upgrade() {
                    h.watcher_started(hw, error);
                }
            });
            if let Err(e) = spawned {
                log::warn!("filetree: couldn't start the watcher thread: {e}");
                let mut ws = hub.watch.lock();
                ws.pending = false;
                ws.error = Some(format!("watch: unavailable ({e})"));
            }
        }
        hub
    }

    fn watcher_started(&self, hw: HubWatch, error: Option<String>) {
        {
            let mut ws = self.watch.lock();
            ws.hw = hw;
            ws.error = error;
            ws.pending = false;
        }
        let mut open: BTreeSet<String> = self.dirs.lock().keys().cloned().collect();
        open.extend(self.subs.lock().values().flat_map(|s| s.interest.iter().cloned()));
        for rel in &open {
            self.watch_dir(rel);
        }
        for rel in &open {
            self.request(rel, None);
        }
        self.broadcast_health();
    }

    pub fn is_gone(&self) -> bool {
        self.gone.load(Ordering::SeqCst)
    }

    pub fn is_network(&self) -> bool {
        self.fs_class == fsclass::NETWORK
    }

    pub(super) fn git_dir(&self) -> Option<&Path> {
        self.git_dir.as_deref()
    }

    pub fn raw_sender(&self) -> RawSender {
        self.raw.clone()
    }

    // ── Subscriptions ────────────────────────────────────────────────────────────────────

    pub fn add_sub(&self, id: u64, window: &str, sink: Arc<dyn Sink>) {
        let health = self.health_event();
        let mut subs = self.subs.lock();
        let mut sub = Sub::new(id, window, sink);
        sub.deliver(health);
        if self.is_gone() {
            sub.deliver(TreeEvent::RootGone);
        }
        subs.insert(id, sub);
    }

    /// Drop a subscription; returns how many remain. Its folders lose their snapshots and
    /// watches only while other subscribers remain: with none left, the hub is in its grace
    /// period and keeps everything, so a return within it costs nothing.
    pub fn remove_sub(&self, id: u64) -> usize {
        let (interest, remaining) = {
            let mut subs = self.subs.lock();
            let interest = subs.remove(&id).map(|s| s.interest).unwrap_or_default();
            let left: Vec<String> = if subs.is_empty() { Vec::new() } else { interest.into_iter().filter(|r| !subs.values().any(|s| s.interest.contains(r))).collect() };
            (left, subs.len())
        };
        for rel in interest {
            self.drop_dir(&rel);
        }
        remaining
    }

    pub fn sub_count(&self) -> usize {
        self.subs.lock().len()
    }

    pub fn sub_window(&self, id: u64) -> Option<String> {
        self.subs.lock().get(&id).map(|s| s.window.clone())
    }

    pub fn open_ack(&self, sub_id: u64) -> OpenAck {
        OpenAck {
            sub_id,
            root_native: self.root.to_string_lossy().into_owned(),
            sep: std::path::MAIN_SEPARATOR.to_string(),
            case_insensitive: self.fold,
            repo: self.repo.clone(),
            caps: Caps { watch: self.watch_kind(), git: false, trash: false, remote: false, writable: false },
        }
    }

    /// Add `rel` to `sub`'s open folders. True when nobody had it open before.
    fn add_interest(&self, sub: u64, rel: &str) -> bool {
        let mut subs = self.subs.lock();
        let before = subs.values().any(|s| s.interest.contains(rel));
        if let Some(s) = subs.get_mut(&sub) {
            s.interest.insert(rel.to_string());
        }
        !before && subs.get(&sub).is_some()
    }

    fn deliver_to(&self, sub: u64, ev: TreeEvent) {
        if let Some(s) = self.subs.lock().get_mut(&sub) {
            s.deliver(ev);
        }
    }

    /// `ft_expand` (and `ft_open`'s restore): open each folder, answering from the snapshot when
    /// a watcher has kept it current, listing it otherwise.
    pub fn expand(&self, sub: u64, reqs: Vec<DirReq>) {
        for req in reqs {
            if let Err(msg) = split_rel(&req.rel) {
                self.deliver_to(sub, TreeEvent::DirError { rel: req.rel, code: "denied".into(), msg });
                continue;
            }
            let rel = req.rel;
            if self.add_interest(sub, &rel) {
                self.watch_dir(&rel);
            }
            let trusted = self.snapshot_trusted(&rel);
            let answered = {
                let dirs = self.dirs.lock();
                match dirs.get(&rel) {
                    Some(snap) if trusted => {
                        let mut subs = self.subs.lock();
                        if let Some(s) = subs.get_mut(&sub) {
                            if req.known_gen == Some(snap.gen) {
                                s.deliver(TreeEvent::NotModified { rel: rel.clone(), gen: snap.gen });
                            } else {
                                s.send_dir(&rel, snap);
                            }
                        }
                        true
                    }
                    _ => false,
                }
            };
            if !answered {
                self.request(&rel, Some(Waiter::Expand { sub, known_gen: req.known_gen }));
            }
        }
    }

    /// `ft_collapse`: the subscriber closed these folders.
    pub fn collapse(&self, sub: u64, rels: Vec<String>) {
        for rel in rels {
            let nobody = {
                let mut subs = self.subs.lock();
                if let Some(s) = subs.get_mut(&sub) {
                    s.interest.remove(&rel);
                }
                !subs.values().any(|s| s.interest.contains(&rel))
            };
            if nobody {
                self.drop_dir(&rel);
            }
        }
    }

    fn drop_dir(&self, rel: &str) {
        self.dirs.lock().remove(rel);
        self.unwatch_dir(rel);
    }

    /// `ft_refresh`: a full Dir for `rel`, or for every folder the subscriber has open.
    pub fn refresh(&self, sub: u64, rel: Option<String>) {
        let rels: Vec<String> = match rel {
            Some(r) => {
                if let Err(msg) = split_rel(&r) {
                    self.deliver_to(sub, TreeEvent::DirError { rel: r, code: "denied".into(), msg });
                    return;
                }
                if self.add_interest(sub, &r) {
                    self.watch_dir(&r);
                }
                vec![r]
            }
            None => self.subs.lock().get(&sub).map(|s| s.interest.iter().cloned().collect()).unwrap_or_default(),
        };
        for r in rels {
            self.request(&r, Some(Waiter::Refresh { sub }));
        }
    }

    /// `ft_pause`: the webview was hidden (or shown again).
    pub fn pause(&self, sub: u64, paused: bool) {
        let resume = {
            let mut subs = self.subs.lock();
            match subs.get_mut(&sub) {
                Some(s) => {
                    s.paused = paused;
                    !paused && !s.stalled
                }
                None => false,
            }
        };
        if resume {
            self.flush(sub);
        }
    }

    /// `ft_ack`: the client has applied everything up to `seq`.
    pub fn ack(&self, sub: u64, seq: u64) {
        let resume = {
            let mut subs = self.subs.lock();
            match subs.get_mut(&sub) {
                Some(s) => s.ack(seq) && !s.paused,
                None => false,
            }
        };
        if resume {
            self.flush(sub);
        }
    }

    /// Catch a resumed subscriber up: one whole Dir per folder that changed while it was
    /// paused or stalled, then whatever else it missed.
    fn flush(&self, sub: u64) {
        let health = self.health_event();
        let mut relist = Vec::new();
        {
            let dirs = self.dirs.lock();
            let mut subs = self.subs.lock();
            let Some(s) = subs.get_mut(&sub) else { return };
            if s.paused || s.stalled {
                return;
            }
            if std::mem::take(&mut s.root_gone) {
                s.deliver(TreeEvent::RootGone);
            }
            if std::mem::take(&mut s.health_dirty) {
                s.deliver(health);
            }
            let rels: Vec<String> = std::mem::take(&mut s.dirty).into_iter().filter(|r| s.interest.contains(r)).collect();
            for rel in rels {
                match dirs.get(&rel) {
                    Some(snap) => s.send_dir(&rel, snap),
                    None => relist.push(rel),
                }
            }
        }
        for rel in relist {
            self.request(&rel, Some(Waiter::Refresh { sub }));
        }
    }

    /// Re-list every folder `label`'s subscribers have open: the window came back after a
    /// long absence, or the root is a network mount with no watcher.
    pub fn revalidate_window(&self, label: &str) {
        let rels: BTreeSet<String> = self.subs.lock().values().filter(|s| s.window == label).flat_map(|s| s.interest.iter().cloned()).collect();
        for rel in rels {
            self.request(&rel, None);
        }
    }

    /// Re-list every folder there is a snapshot of (overflow, a rescan, new ignore rules).
    pub(super) fn revalidate_under(&self, base: &str) {
        let rels: Vec<String> = self.dirs.lock().keys().filter(|r| paths::is_at_or_under(r, base)).cloned().collect();
        for rel in rels {
            self.request(&rel, None);
        }
    }

    pub fn set_user_excludes(&self, excludes: &[String]) {
        self.ignore.lock().invalidate_all(self.git_dir(), Some(excludes));
        self.revalidate_under("");
    }

    // ── Listing jobs ─────────────────────────────────────────────────────────────────────

    /// List `rel`, joining a listing already running for it. `waiter` None: no one asked, the
    /// disk changed — interested subscribers get a Delta if it differs.
    pub(super) fn request(&self, rel: &str, waiter: Option<Waiter>) {
        let mut jobs = self.jobs.lock();
        if let Some(f) = jobs.inflight.get_mut(rel) {
            match waiter {
                Some(w) => f.waiters.push(w),
                // A listing that already started may have read the folder before this change.
                None if f.started => f.rerun = true,
                None => {}
            }
            return;
        }
        jobs.next_id += 1;
        let id = jobs.next_id;
        jobs.inflight.insert(rel.to_string(), Inflight { id, waiters: waiter.into_iter().collect(), rerun: false, started: false, timed_out: false });
        jobs.queue.push_back(rel.to_string());
        if jobs.workers < self.max_workers {
            jobs.workers += 1;
            let me = self.me.clone();
            let spawned = std::thread::Builder::new().name("ft-list".into()).spawn(move || worker(me));
            if spawned.is_err() {
                jobs.workers -= 1;
                log::warn!("filetree: couldn't start a listing thread");
            }
        }
    }

    /// The coalescer saw `rel` change: if a listing of it is running, it must run again.
    /// True when there was one.
    pub(super) fn note_inflight_change(&self, rel: &str) -> bool {
        let mut jobs = self.jobs.lock();
        match jobs.inflight.get_mut(rel) {
            Some(f) => {
                if f.started {
                    f.rerun = true;
                }
                true
            }
            None => false,
        }
    }

    /// Every running listing whose folder matches: it may have read the disk too early.
    pub(super) fn rerun_inflight_where(&self, f: impl Fn(&str) -> bool) {
        let mut jobs = self.jobs.lock();
        for (rel, job) in jobs.inflight.iter_mut() {
            if job.started && f(rel) {
                job.rerun = true;
            }
        }
    }

    fn deadline_passed(&self, rel: &str, id: u64) {
        let subs: Vec<u64> = {
            let mut jobs = self.jobs.lock();
            match jobs.inflight.get_mut(rel) {
                Some(f) if f.id == id && f.started && !f.timed_out => {
                    f.timed_out = true;
                    f.waiters.iter().map(Waiter::sub).collect()
                }
                _ => return,
            }
        };
        let mut all = self.subs.lock();
        for id in subs {
            if let Some(s) = all.get_mut(&id) {
                s.deliver(TreeEvent::DirError { rel: rel.to_string(), code: "timeout".into(), msg: "Not responding…".into() });
            }
        }
    }

    /// The first chunk of a big folder nobody has a snapshot of, sent before the rest is
    /// sorted: the first rows of a 50k folder paint without waiting on the whole sort. Returns
    /// the generation it went out under and who got it; the rest follows in `publish`. Not
    /// done when a snapshot exists: then the answer may be NotModified or a Delta, which only
    /// the whole listing can tell.
    fn early_head(&self, rel: &str, head: &[Entry], total: u32, truncated: bool) -> Option<(u64, HashSet<u64>)> {
        let dirs = self.dirs.lock();
        if dirs.contains_key(rel) {
            return None;
        }
        let gen = next_gen();
        let mut got = HashSet::new();
        let mut subs = self.subs.lock();
        for s in subs.values_mut().filter(|s| s.interest.contains(rel)) {
            s.deliver(TreeEvent::Dir { rel: rel.to_string(), gen, chunk: 0, last: false, total, truncated, entries: head.to_vec() });
            got.insert(s.id);
        }
        Some((gen, got))
    }

    /// A listing finished: publish it, then list again if the disk moved meanwhile or someone
    /// asked while it was being published.
    fn complete(&self, rel: &str, result: Result<Listing, ListError>, early: Option<(u64, HashSet<u64>)>) {
        let waiters = self.jobs.lock().inflight.get_mut(rel).map(|f| std::mem::take(&mut f.waiters)).unwrap_or_default();
        self.publish(rel, result, waiters, early);
        let mut jobs = self.jobs.lock();
        let again = match jobs.inflight.get_mut(rel) {
            Some(f) if f.rerun || !f.waiters.is_empty() => {
                f.rerun = false;
                f.started = false;
                f.timed_out = false;
                true
            }
            _ => false,
        };
        if again {
            jobs.next_id += 1;
            let id = jobs.next_id;
            if let Some(f) = jobs.inflight.get_mut(rel) {
                f.id = id;
            }
            jobs.queue.push_back(rel.to_string());
        } else {
            jobs.inflight.remove(rel);
        }
    }

    fn publish(&self, rel: &str, result: Result<Listing, ListError>, waiters: Vec<Waiter>, early: Option<(u64, HashSet<u64>)>) {
        let asked: HashMap<u64, Waiter> = waiters.into_iter().map(|w| (w.sub(), w)).collect();
        match result {
            Err(ListError::Gone) if rel.is_empty() => self.root_gone(),
            Err(e) => {
                let mut dirs = self.dirs.lock();
                dirs.remove(rel);
                let mut subs = self.subs.lock();
                for s in subs.values_mut() {
                    if asked.contains_key(&s.id) || s.interest.contains(rel) {
                        s.deliver(TreeEvent::DirError { rel: rel.to_string(), code: e.code().into(), msg: e.msg() });
                    }
                }
            }
            Ok(listing) => {
                let mut dirs = self.dirs.lock();
                let old = dirs.get(rel);
                let unchanged = early.is_none() && old.is_some_and(|o| o.entries == listing.entries && o.total == listing.total && o.truncated == listing.truncated);
                let gen = match (&early, old) {
                    // Chunk 0 already went out under this generation.
                    (Some((g, _)), _) => *g,
                    (None, Some(o)) if unchanged => o.gen,
                    _ => next_gen(),
                };
                let early_subs = early.map(|(_, s)| s).unwrap_or_default();
                let mut subs = self.subs.lock();
                let wanted = subs.values().any(|s| s.interest.contains(rel));
                // Nobody has it open (closed while it was being listed): keep a snapshot only in
                // the grace period, when there are no subscribers at all and it already had one.
                if !wanted && !(subs.is_empty() && old.is_some()) {
                    dirs.remove(rel);
                    return;
                }
                let snap = DirSnap::from(listing, gen);
                for s in subs.values_mut() {
                    if !s.interest.contains(rel) {
                        continue;
                    }
                    if early_subs.contains(&s.id) {
                        s.send_dir_from(rel, &snap, 1);
                        continue;
                    }
                    match asked.get(&s.id) {
                        Some(Waiter::Expand { known_gen: Some(k), .. }) if *k == gen => {
                            s.deliver(TreeEvent::NotModified { rel: rel.to_string(), gen });
                        }
                        Some(_) => s.send_dir(rel, &snap),
                        None if unchanged => {}
                        None => match old {
                            Some(o) if !o.truncated && !snap.truncated => {
                                let (add, del, chg) = diff(&o.entries, &snap.entries);
                                s.deliver(TreeEvent::Delta { rel: rel.to_string(), from: o.gen, gen, add, del, chg });
                            }
                            _ => s.send_dir(rel, &snap),
                        },
                    }
                }
                dirs.insert(rel.to_string(), snap);
            }
        }
    }

    /// The root was deleted or renamed away. Every subscriber is told; nothing re-roots.
    pub(super) fn root_gone(&self) {
        self.gone.store(true, Ordering::SeqCst);
        let mut dirs = self.dirs.lock();
        dirs.clear();
        let mut subs = self.subs.lock();
        for s in subs.values_mut() {
            s.deliver(TreeEvent::RootGone);
        }
    }

    /// Apply a change the coalescer worked out for one folder and send it as a Delta.
    /// `updates`: name → its entry now (None = gone). Returns false when the folder has no
    /// snapshot or is truncated, and must be re-listed instead.
    pub(super) fn apply_small(&self, rel: &str, updates: Vec<(String, Option<Entry>)>) -> bool {
        let mut dirs = self.dirs.lock();
        let Some(snap) = dirs.get_mut(rel) else { return false };
        if snap.truncated {
            return false;
        }
        let (mut add, mut del, mut chg) = (Vec::new(), Vec::new(), Vec::new());
        for (name, now) in updates {
            let at = snap.entries.iter().position(|e| e.n == name);
            match (at, now) {
                (None, None) => {}
                (Some(i), None) => {
                    snap.entries.remove(i);
                    del.push(name);
                }
                (None, Some(e)) => {
                    let pos = snap.entries.binary_search_by(|x| list::entry_cmp(x, &e)).unwrap_or_else(|p| p);
                    snap.entries.insert(pos, e.clone());
                    add.push(e);
                }
                (Some(i), Some(e)) => {
                    if snap.entries[i] != e {
                        snap.entries.remove(i);
                        let pos = snap.entries.binary_search_by(|x| list::entry_cmp(x, &e)).unwrap_or_else(|p| p);
                        snap.entries.insert(pos, e.clone());
                        chg.push(e);
                    }
                }
            }
        }
        if add.is_empty() && del.is_empty() && chg.is_empty() {
            return true;
        }
        if snap.entries.len() > list::MAX_ENTRIES {
            // Grew past the cap: the truncated listing comes from a full re-list.
            return false;
        }
        snap.total = snap.entries.len() as u32;
        let from = snap.gen;
        snap.gen = next_gen();
        let gen = snap.gen;
        let mut subs = self.subs.lock();
        for s in subs.values_mut().filter(|s| s.interest.contains(rel)) {
            s.deliver(TreeEvent::Delta { rel: rel.to_string(), from, gen, add: add.clone(), del: del.clone(), chg: chg.clone() });
        }
        true
    }

    // ── Watching and polling ─────────────────────────────────────────────────────────────

    /// "live" | "partial" | "poll" | "none".
    fn watch_kind(&self) -> String {
        let ws = self.watch.lock();
        let kind = match &ws.hw {
            _ if self.is_network() => "poll",
            // Being started: reported as what it will be, not as a failure.
            _ if ws.pending => "live",
            HubWatch::None => "none",
            HubWatch::PerDir(_) if !ws.polled.is_empty() => "partial",
            _ => "live",
        };
        kind.to_string()
    }

    pub fn health_event(&self) -> TreeEvent {
        let watch = self.watch_kind();
        let note = {
            let ws = self.watch.lock();
            if self.is_network() {
                Some("network mount: not watched".to_string())
            } else if !ws.polled.is_empty() {
                let why = if cfg!(target_os = "linux") { " (inotify limit)" } else { "" };
                Some(format!("watch: polling {} folder{}{why}", ws.polled.len(), if ws.polled.len() == 1 { "" } else { "s" }))
            } else {
                ws.error.clone()
            }
        };
        // Git decorations arrive in phase 2.
        TreeEvent::Health { watch, git: "off".into(), note }
    }

    fn broadcast_health(&self) {
        let ev = self.health_event();
        let mut subs = self.subs.lock();
        for s in subs.values_mut() {
            s.deliver(ev.clone());
        }
    }

    /// Can a snapshot of `rel` be served without reading the disk? Only when a watcher has been
    /// covering it: never on a network mount, never for a polled folder.
    fn snapshot_trusted(&self, rel: &str) -> bool {
        if self.is_network() {
            return false;
        }
        let ws = self.watch.lock();
        match &ws.hw {
            HubWatch::Recursive(_) => true,
            HubWatch::PerDir(_) => !ws.polled.contains(rel),
            HubWatch::None => false,
        }
    }

    fn watch_dir(&self, rel: &str) {
        let Ok(abs) = rel_to_native(&self.root, rel) else { return };
        // The mtime the poller compares against, should this folder end up polled. Read before
        // the listing starts and outside the lock (a network stat can be slow), so a change
        // made while the folder is being listed still shows up on the first tick.
        let baseline = if self.watch_is_recursive() { None } else { dir_mtime(&abs) };
        let mut health_changed = false;
        {
            let mut ws = self.watch.lock();
            let ws = &mut *ws;
            // Kept through a grace period: a returning subscriber finds it still watched.
            if ws.order.iter().any(|r| r == rel) || ws.polled.contains(rel) {
                return;
            }
            match &mut ws.hw {
                HubWatch::PerDir(w) => match w.add(&abs) {
                    Ok(()) => ws.order.push_back(rel.to_string()),
                    Err(WatchError::Limit) => {
                        // Out of watches: the least recently opened folder (never the root)
                        // gives its watch up to this one, which was just opened and is on screen.
                        let victim = ws.order.iter().position(|r| !r.is_empty()).and_then(|i| ws.order.remove(i));
                        let placed = match victim {
                            Some(v) => {
                                if let Ok(vabs) = rel_to_native(&self.root, &v) {
                                    w.remove(&vabs);
                                    // Its watch saw everything up to now: now is its baseline.
                                    ws.poll_mtimes.insert(v.clone(), dir_mtime(&vabs));
                                }
                                ws.polled.insert(v);
                                w.add(&abs).is_ok()
                            }
                            None => false,
                        };
                        if placed {
                            ws.order.push_back(rel.to_string());
                        } else {
                            ws.polled.insert(rel.to_string());
                            ws.poll_mtimes.insert(rel.to_string(), baseline);
                        }
                        health_changed = true;
                    }
                    Err(WatchError::Other(e)) => {
                        log::debug!("filetree: polling {}: {e}", abs.display());
                        ws.polled.insert(rel.to_string());
                        ws.poll_mtimes.insert(rel.to_string(), baseline);
                        health_changed = true;
                    }
                },
                HubWatch::None if self.is_network() => {
                    ws.poll_mtimes.insert(rel.to_string(), baseline);
                    if !ws.poller_running {
                        ws.poller_running = true;
                        watch::poll::spawn(self.me.clone(), watch::poll::NETWORK_INTERVAL);
                    }
                }
                _ => {}
            }
            if health_changed && !ws.polled.is_empty() && !ws.poller_running {
                ws.poller_running = true;
                watch::poll::spawn(self.me.clone(), watch::poll::LIMIT_INTERVAL);
            }
        }
        if health_changed {
            self.broadcast_health();
        }
    }

    fn unwatch_dir(&self, rel: &str) {
        let health_changed = {
            let mut ws = self.watch.lock();
            ws.poll_mtimes.remove(rel);
            if ws.polled.remove(rel) {
                true
            } else {
                if let (HubWatch::PerDir(w), Ok(abs)) = (&mut ws.hw, rel_to_native(&self.root, rel)) {
                    w.remove(&abs);
                }
                ws.order.retain(|r| r != rel);
                false
            }
        };
        if health_changed {
            self.broadcast_health();
        }
    }

    fn watch_is_recursive(&self) -> bool {
        matches!(self.watch.lock().hw, HubWatch::Recursive(_))
    }

    /// One poller tick: the polled folders whose mtime moved. None: nothing is left to poll,
    /// and the poller is recorded as gone (under the same lock that starts one, so a folder
    /// added meanwhile starts a new one).
    pub fn poll_tick(&self) -> Option<Vec<PathBuf>> {
        let open: BTreeSet<String> = if self.is_network() { self.subs.lock().values().flat_map(|s| s.interest.iter().cloned()).collect() } else { BTreeSet::new() };
        let targets: Vec<String> = {
            let mut ws = self.watch.lock();
            let rels = if self.is_network() { open } else { ws.polled.clone() };
            if rels.is_empty() || self.is_gone() {
                ws.poller_running = false;
                return None;
            }
            rels.into_iter().collect()
        };
        if !self.poll_allowed() {
            return Some(Vec::new());
        }
        // Stat'd with no lock held: a network folder can take its time.
        let now: Vec<(String, PathBuf, Option<SystemTime>)> = targets
            .into_iter()
            .filter_map(|r| rel_to_native(&self.root, &r).ok().map(|a| (r, a)))
            .map(|(r, a)| {
                let m = dir_mtime(&a);
                (r, a, m)
            })
            .collect();
        let mut ws = self.watch.lock();
        let mut changed = Vec::new();
        for (r, a, m) in now {
            // Only folders still polled: one closed during the stat has no baseline to keep.
            let still = if self.is_network() { true } else { ws.polled.contains(&r) };
            if !still {
                continue;
            }
            if let Some(prev) = ws.poll_mtimes.insert(r, m) {
                if prev != m {
                    changed.push(a);
                }
            }
        }
        Some(changed)
    }

    /// Poll only for a subscriber that can show the result: unpaused, in a focused window.
    pub fn poll_allowed(&self) -> bool {
        self.subs.lock().values().any(|s| !s.paused && !s.stalled && self.focus.is_focused(&s.window))
    }

    // ── Hover and reveal ─────────────────────────────────────────────────────────────────

    /// `ft_locate`: absolute paths to rels, spelled as the disk spells them; None = outside.
    pub fn locate(&self, abs_paths: &[String]) -> Vec<Option<String>> {
        abs_paths.iter().map(|a| self.locate_one(a)).collect()
    }

    fn locate_one(&self, abs: &str) -> Option<String> {
        let comps = paths::locate_components(&self.root, self.fold, abs)?;
        let mut rel = String::new();
        for c in comps {
            let name = self.disk_name(&rel, &c);
            rel = join_rel(&rel, &name);
        }
        Some(rel)
    }

    /// `name` in folder `parent` as the disk spells it: a hook path arrives NFC and maybe in
    /// another case, while the tree keys rows by the listed name.
    fn disk_name(&self, parent: &str, name: &str) -> String {
        let key = name_key(name, self.fold);
        if let Some(snap) = self.dirs.lock().get(parent) {
            if snap.entries.iter().any(|e| e.n == name) {
                return name.to_string();
            }
            if let Some(e) = snap.entries.iter().find(|e| name_key(&e.n, self.fold) == key) {
                return e.n.clone();
            }
        }
        if !self.fold && name.is_ascii() {
            return name.to_string();
        }
        let Ok(dir) = rel_to_native(&self.root, parent) else { return name.to_string() };
        if dir.join(name).symlink_metadata().is_err() {
            return name.to_string();
        }
        std::fs::read_dir(dir)
            .into_iter()
            .flatten()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .find(|n| n == name || name_key(n, self.fold) == key)
            .unwrap_or_else(|| name.to_string())
    }

    /// `ft_stat`: what the hover tooltip shows.
    pub fn stat(&self, rel: &str) -> Result<StatInfo, String> {
        let abs = rel_to_native(&self.root, rel)?;
        let lm = std::fs::symlink_metadata(&abs).map_err(|e| e.to_string())?;
        let is_link = lm.file_type().is_symlink();
        let link_target = if is_link { std::fs::read_link(&abs).ok().map(|t| t.to_string_lossy().into_owned()) } else { None };
        let m = if is_link { std::fs::metadata(&abs).unwrap_or(lm) } else { lm };
        let mtime_ms = m
            .modified()
            .ok()
            .map(|t| match t.duration_since(UNIX_EPOCH) {
                Ok(d) => d.as_millis() as i64,
                Err(e) => -(e.duration().as_millis() as i64),
            })
            .unwrap_or(0);
        #[cfg(unix)]
        let exec = {
            use std::os::unix::fs::PermissionsExt;
            m.is_file() && m.permissions().mode() & 0o111 != 0
        };
        #[cfg(not(unix))]
        let exec = false;
        Ok(StatInfo { size: m.len(), mtime_ms, exec, link_target, numstat: None })
    }

    #[cfg(all(test, target_os = "linux"))]
    pub(crate) fn watched_and_polled(&self) -> (usize, usize) {
        let ws = self.watch.lock();
        (ws.order.len(), ws.polled.len())
    }

    /// No listing queued or running, and the watcher is running.
    #[cfg(test)]
    pub(crate) fn test_idle(&self) -> bool {
        let jobs = self.jobs.lock();
        jobs.inflight.is_empty() && jobs.queue.is_empty() && !self.watch.lock().pending
    }

    #[cfg(test)]
    pub(crate) fn watch_pending(&self) -> bool {
        self.watch.lock().pending
    }
}

fn dir_mtime(abs: &Path) -> Option<SystemTime> {
    std::fs::metadata(abs).and_then(|m| m.modified()).ok()
}

/// (added, deleted names, changed) from `old` to `new`, both sorted.
fn diff(old: &[Entry], new: &[Entry]) -> (Vec<Entry>, Vec<String>, Vec<Entry>) {
    let before: HashMap<&str, &Entry> = old.iter().map(|e| (e.n.as_str(), e)).collect();
    let after: HashSet<&str> = new.iter().map(|e| e.n.as_str()).collect();
    let mut add = Vec::new();
    let mut chg = Vec::new();
    for e in new {
        match before.get(e.n.as_str()) {
            None => add.push(e.clone()),
            Some(o) if *o != e => chg.push(e.clone()),
            _ => {}
        }
    }
    let del = old.iter().filter(|e| !after.contains(e.n.as_str())).map(|e| e.n.clone()).collect();
    (add, del, chg)
}

/// One listing thread: takes folders off the hub's queue until it is empty. It holds the hub
/// only while working, so an idle hub can still be dropped.
fn worker(me: Weak<RootHub>) {
    loop {
        let Some(hub) = me.upgrade() else { return };
        let next = {
            let mut jobs = hub.jobs.lock();
            match jobs.queue.pop_front() {
                Some(rel) => {
                    let id = jobs.inflight.get_mut(&rel).map(|f| {
                        f.started = true;
                        f.id
                    });
                    id.map(|id| (rel, id))
                }
                None => {
                    jobs.workers -= 1;
                    return;
                }
            }
        };
        let Some((rel, id)) = next else { continue };
        if hub.fs_class != fsclass::LOCAL {
            let weak = me.clone();
            let r = rel.clone();
            let _ = std::thread::Builder::new().name("ft-deadline".into()).spawn(move || {
                std::thread::sleep(NETWORK_SOFT_DEADLINE);
                if let Some(h) = weak.upgrade() {
                    h.deadline_passed(&r, id);
                }
            });
        }
        let mut early = None;
        let result = {
            let _slot = hub.slots.acquire();
            list::list_dir_with(&hub.root, &rel, &hub.ignore, &mut |head, total, truncated| {
                early = hub.early_head(&rel, head, total, truncated);
            })
        };
        hub.complete(&rel, result, early);
    }
}
