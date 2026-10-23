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
//! the folders that changed meanwhile are remembered by name, and resent whole on resume (a
//! listing the stall cut short carries on from its next chunk while its snapshot is unchanged).
//! Only an ack that makes room in the window ends a stall.
//!
//! **Listing jobs.** One listing per folder at a time: a second request joins the first, so a
//! read hung on a dead mount is never re-issued onto another thread. Each hub runs at most 4
//! listings (2 on a network mount), inside a global limit of 32.
//!
//! **Git (§7).** A hub in a repository has a git worker (`git.rs`). Each subscription records
//! the git generation it holds; a change is sent as only the keys that changed to a
//! subscription holding the previous generation, and as everything (`full`) to any other — a
//! new one, or one that was paused or stalled through a change.
//!
//! **Remote roots (§15).** A hub whose root is on an ssh host holds a `RemoteRoot`: listings,
//! stat, git and the find index go through the host's helper, its watch events arrive as
//! `remote_changed`, and nothing local (watcher, coalescer, ignore files) is read. Snapshots are
//! never served without asking the host (one round trip, answered NotModified when unchanged).
//!
//! Lock order, always: jobs → dirs → subs → git data → watch. The git data lock is never held
//! while taking subs. No lock is held across another hub's.

use std::collections::{BTreeSet, HashMap, HashSet, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Weak};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use parking_lot::Mutex;

use super::git::{Failure, GitConfig, GitCore, Status, Trigger};
use super::ignore_cache::IgnoreStack;
use super::list::{self, ListError, Listing, CHUNK};
use super::paths::{self, join_rel, name_key, rel_to_native, split_rel};
use super::remote::RemoteRoot;
use super::search::SearchState;
use super::types::{AgentMark, Caps, DirReq, Entry, Envelope, GitUpdate, OpenAck, RepoInfo, SearchResult, StatInfo, TreeEvent};
use super::watch::{self, HubWatch, RawSender, WatchContext, WatchError};
use super::{fsclass, Focus, Slots};
use crate::agent_files::AgentFiles;

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
    /// A listing a stall cut short: its generation and the first chunk not sent. A resume
    /// carries on from there while the snapshot is unchanged. Restarting from chunk 0 instead
    /// never finished a folder bigger than one ack's worth of window (the client acks every
    /// 32 messages), and resent it forever.
    partial: HashMap<String, (u64, usize)>,
    root_gone: bool,
    health_dirty: bool,
    /// The folders this subscriber has open.
    interest: HashSet<String>,
    dead: bool,
    /// The git generation this subscriber holds; None before its first update.
    git_gen: Option<u64>,
    /// A git update was held back (paused or stalled): resend everything on resume.
    git_dirty: bool,
    /// A `search_ready` was held back: a query answered `indexing` is still waiting for it.
    search_ready: bool,
    /// Agent marks were held back: the standing ones are sent on resume.
    agent_missed: bool,
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
            partial: HashMap::new(),
            root_gone: false,
            health_dirty: false,
            interest: HashSet::new(),
            dead: false,
            git_gen: None,
            git_dirty: false,
            search_ready: false,
            agent_missed: false,
        }
    }

    /// Send a git update that brings this subscriber to `gen`. Recorded only if it went out;
    /// one met by a pause or stall marks the subscriber for a full resend instead.
    fn send_git(&mut self, update: GitUpdate) {
        let gen = update.gen;
        let before = self.seq;
        self.deliver(TreeEvent::Git(update));
        if self.seq > before {
            self.git_gen = Some(gen);
            self.git_dirty = false;
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
                TreeEvent::Git(_) => self.git_dirty = true,
                TreeEvent::SearchReady => self.search_ready = true,
                TreeEvent::Agent { .. } => self.agent_missed = true,
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
        // Only an ack that made room ends a stall: a repeated one would resume into a full
        // window just to stall again on the first message.
        let room = self.seq - self.acked <= STALL_MESSAGES && self.unacked_bytes <= STALL_BYTES;
        let resumed = self.stalled && room;
        if room {
            self.stalled = false;
        }
        resumed
    }

    fn send_dir(&mut self, rel: &str, snap: &DirSnap) {
        self.send_dir_from(rel, snap, 0);
    }

    /// The chunks of a listing from `first` on (chunk 0 may have gone out early).
    fn send_dir_from(&mut self, rel: &str, snap: &DirSnap, first: usize) {
        let n = snap.entries.len().div_ceil(CHUNK).max(1);
        // A send that can go out supersedes any cut-short one; one that can't (still stalled)
        // leaves it, so a re-send of the same snapshot still resumes where the cut was.
        if !(self.paused || self.stalled || self.dead) {
            self.partial.remove(rel);
        }
        for i in first..n {
            let chunk = if snap.entries.is_empty() { Vec::new() } else { snap.entries[i * CHUNK..((i + 1) * CHUNK).min(snap.entries.len())].to_vec() };
            let before = self.seq;
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
                // `seq` moved only if this chunk actually went out (the one that filled the
                // window did; one met by an existing stall was only marked dirty).
                let next = if self.seq > before { i + 1 } else { i };
                // The chunk that filled the window was the last: the client has the whole
                // listing, so there is nothing to resume. Marking it dirty would resend it all
                // from chunk 0, and when the chunk count lines up with the ack cadence, the
                // resend ends on the last chunk again, forever. A real change made during the
                // stall still marks it dirty, through `deliver`.
                if next == n {
                    return;
                }
                if next > 0 {
                    self.partial.insert(rel.to_string(), (snap.gen, next));
                }
                self.dirty.insert(rel.to_string());
                return;
            }
        }
    }

    /// After a resume: one listing per folder that changed meanwhile, continuing one a stall
    /// cut short when its snapshot is the one already partly sent. Returns the folders with
    /// no snapshot, which must be listed again.
    fn flush_dirty(&mut self, dirs: &HashMap<String, DirSnap>) -> Vec<String> {
        let mut relist = Vec::new();
        let rels: Vec<String> = std::mem::take(&mut self.dirty).into_iter().filter(|r| self.interest.contains(r)).collect();
        for rel in rels {
            match dirs.get(&rel) {
                Some(snap) => {
                    let from = match self.partial.get(&rel) {
                        Some(&(gen, next)) if gen == snap.gen => next,
                        _ => 0,
                    };
                    self.send_dir_from(&rel, snap, from);
                }
                None => relist.push(rel),
            }
        }
        relist
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
    /// Watches on the repository's own folders (index, HEAD, refs) when the root's watcher
    /// doesn't cover them: a root inside a larger repo, a linked worktree, a per-folder
    /// watcher (Linux, shallow roots). Held only so they stay alive.
    #[allow(dead_code)]
    git_watch: Option<Box<dyn Send>>,
}

pub struct HubInit {
    pub place: String,
    pub root: PathBuf,
    pub fs_class: &'static str,
    pub shallow: bool,
    pub user_excludes: Vec<String>,
    pub raw_capacity: usize,
    pub git: GitConfig,
    /// The app-wide limit on git processes (§7.2).
    pub git_slots: Arc<Slots>,
    /// The agent ledger: shell writes are attributed to it and its marks primed from it (§9).
    pub agents: Arc<AgentFiles>,
    /// The root is on an ssh host (§15); `root` is then its real POSIX path there.
    pub remote: Option<Arc<RemoteRoot>>,
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
    pub(super) agents: Arc<AgentFiles>,
    /// A root on an ssh host: everything that reads or watches the disk goes through it.
    pub(crate) remote: Option<Arc<RemoteRoot>>,
    git_dir: Option<PathBuf>,
    /// The repository's git dir and (when different) common dir: events under them are git
    /// signals, wherever they are.
    git_dirs: Vec<PathBuf>,
    /// The git worker, when the root is in a repository.
    pub(super) git: Option<Arc<GitCore>>,
    pub(super) search: Arc<SearchState>,
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
        let remote = init.remote.clone();
        // A remote root's repository is the helper's answer; nothing local is read for it.
        let repo = match &remote {
            Some(r) => r.repo.clone().map(|ri| {
                let g = PathBuf::from(&ri.git_dir);
                (ri, g)
            }),
            None => repo_info(&init.root),
        };
        let git_dir = repo.as_ref().map(|(_, g)| g.clone());
        let ignore = if remote.is_some() { IgnoreStack::inert(&init.root) } else { IgnoreStack::new(&init.root, git_dir.as_deref(), &init.user_excludes) };
        let (raw, rx, overflow) = watch::channel(init.raw_capacity);
        let (root, shallow) = (init.root.clone(), init.shallow);
        // The top level, from the `..` steps: a root inside a larger repo strips its prefix
        // from git's paths.
        let top = repo.as_ref().and_then(|(r, _)| {
            let up = if r.toplevel_rel.is_empty() { 0 } else { r.toplevel_rel.split('/').count() };
            init.root.ancestors().nth(up).map(Path::to_path_buf)
        });
        let mut git_dirs: Vec<PathBuf> = Vec::new();
        if let Some((r, g)) = &repo {
            git_dirs.push(g.clone());
            let common = PathBuf::from(&r.common_dir);
            if common != *g {
                git_dirs.push(common);
            }
        }
        // A home or volume root decorates only when it is itself a repository (§6.5).
        let git_on = repo.as_ref().is_some_and(|(r, _)| !shallow || r.toplevel_rel.is_empty());
        // An ssh host's volume is assumed case-sensitive (Linux); probing it from here would
        // probe this computer's disk at the same path.
        let fold = remote.is_none() && crate::relocate::case_insensitive(&init.root);
        let hub = Arc::new_cyclic(|me: &Weak<RootHub>| RootHub {
            me: me.clone(),
            place: init.place,
            fold,
            git: match (&top, git_on) {
                (Some(top), true) => Some(GitCore::new_with(me.clone(), &init.git, &init.root, top, network, init.git_slots.clone(), remote.clone())),
                _ => None,
            },
            search: SearchState::new_with(me.clone(), &init.root, repo.is_some(), shallow, fold, remote.clone()),
            root: init.root,
            fs_class: init.fs_class,
            agents: init.agents,
            remote: remote.clone(),
            repo: repo.map(|(r, _)| r),
            git_dir,
            git_dirs,
            dirs: Mutex::new(HashMap::new()),
            subs: Mutex::new(HashMap::new()),
            ignore: Mutex::new(ignore),
            jobs: Mutex::new(Jobs::default()),
            max_workers: if network { 2 } else { 4 },
            slots,
            focus,
            watch: Mutex::new(WatchState {
                hw: HubWatch::None,
                pending: !network && remote.is_none(),
                order: VecDeque::new(),
                polled: BTreeSet::new(),
                poll_mtimes: HashMap::new(),
                poller_running: false,
                error: None,
                git_watch: None,
            }),
            raw,
            overflow,
            gone: AtomicBool::new(false),
            wakeups: Arc::new(AtomicU64::new(0)),
            grace_token: AtomicU64::new(0),
        });
        if let Some(r) = &remote {
            // The host's helper watches; there is nothing here for a coalescer to drain.
            drop(rx);
            r.attach(Arc::downgrade(&hub));
            return hub;
        }
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
                    h.watcher_started(hw, error, &ctx);
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

    fn watcher_started(&self, hw: HubWatch, error: Option<String>, ctx: &WatchContext) {
        // The root's own watcher sees the repository's index and refs only when it is
        // recursive and the git dirs are inside the root.
        let covered = matches!(hw, HubWatch::Recursive(_)) && self.git_dirs.iter().all(|g| g.starts_with(&self.root));
        let git_watch = if self.git.is_some() && !covered { watch::create_git(&self.git_dirs, self.raw.clone(), ctx) } else { None };
        {
            let mut ws = self.watch.lock();
            ws.hw = hw;
            ws.error = error;
            ws.pending = false;
            ws.git_watch = git_watch;
        }
        let mut open: BTreeSet<String> = self.dirs.lock().keys().cloned().collect();
        open.extend(self.subs.lock().values().flat_map(|s| s.interest.iter().cloned()));
        for rel in &open {
            self.watch_dir(rel);
        }
        for rel in &open {
            self.request(rel, None);
        }
        // Whatever changed before the watcher ran was never reported: git looks again, and an
        // index built meanwhile is rebuilt on its next query.
        self.git_signal(Trigger::Open);
        if self.search.wanted() {
            self.search.note(vec![super::search::Change::Folder]);
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

    /// The repository's git dir and common dir.
    pub(super) fn git_dirs(&self) -> &[PathBuf] {
        &self.git_dirs
    }

    /// A subscription of this hub lives in window `label`.
    pub fn has_window(&self, label: &str) -> bool {
        self.subs.lock().values().any(|s| s.window == label)
    }

    /// `path` (absolute) is inside the repository this hub decorates, or inside its root.
    pub fn covers(&self, path: &Path) -> bool {
        // A path here is never inside a folder on an ssh host, whatever its spelling.
        if self.remote.is_some() {
            return false;
        }
        if paths::strip_root(path, &self.root, self.fold).is_some() {
            return true;
        }
        let Some(r) = &self.repo else { return false };
        let up = if r.toplevel_rel.is_empty() { 0 } else { r.toplevel_rel.split('/').count() };
        self.root.ancestors().nth(up).is_some_and(|top| paths::strip_root(path, top, self.fold).is_some())
    }

    pub fn raw_sender(&self) -> RawSender {
        self.raw.clone()
    }

    // ── Subscriptions ────────────────────────────────────────────────────────────────────

    pub fn add_sub(&self, id: u64, window: &str, sink: Arc<dyn Sink>) {
        let health = self.health_event();
        // Read before taking subs: placing a mark may look at the disk.
        let standing = self.standing_agent_marks();
        {
            let mut subs = self.subs.lock();
            let mut sub = Sub::new(id, window, sink);
            sub.deliver(health);
            if self.is_gone() {
                sub.deliver(TreeEvent::RootGone);
            }
            if !standing.is_empty() {
                sub.deliver(TreeEvent::Agent { marks: standing });
            }
            // What git already knows goes out at once; the run below brings it up to date.
            if let Some(full) = self.git_full() {
                sub.send_git(full);
            }
            subs.insert(id, sub);
        }
        self.git_signal(Trigger::Open);
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
        self.search.forget(id);
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
        if let Some(r) = &self.remote {
            let writable = r.writable();
            return OpenAck {
                sub_id,
                root_native: r.root.clone(),
                sep: "/".into(),
                case_insensitive: false,
                nfc_names: false,
                repo: self.repo.clone(),
                caps: Caps { watch: self.watch_kind(), git: self.git.as_ref().is_some_and(|g| g.enabled()), trash: writable, remote: true, writable },
                remote_hostname: r.conn.hello().map(|h| h.hostname).filter(|h| !h.is_empty()),
            };
        }
        OpenAck {
            sub_id,
            root_native: self.root.to_string_lossy().into_owned(),
            sep: std::path::MAIN_SEPARATOR.to_string(),
            case_insensitive: self.fold,
            nfc_names: super::paths::NORMALIZATION_INSENSITIVE,
            repo: self.repo.clone(),
            caps: Caps { watch: self.watch_kind(), git: self.git.as_ref().is_some_and(|g| g.enabled()), trash: true, remote: false, writable: true },
            remote_hostname: None,
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
                    s.partial.remove(&rel);
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
            None => {
                // Refresh everything: the decorations too, and a host's helper that gave up
                // (no key, no python3) tries again.
                if let Some(r) = &self.remote {
                    r.conn.retry();
                }
                self.git_signal(Trigger::Manual);
                self.subs.lock().get(&sub).map(|s| s.interest.iter().cloned().collect()).unwrap_or_default()
            }
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
        let missed = self.subs.lock().get(&sub).is_some_and(|s| s.agent_missed);
        let standing = if missed { Some(self.standing_agent_marks()) } else { None };
        let relist = {
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
            if s.git_dirty {
                // Taken under subs: git data comes after subs in the lock order.
                if let Some(full) = self.git_full() {
                    if s.git_gen != Some(full.gen) {
                        s.send_git(full);
                    } else {
                        s.git_dirty = false;
                    }
                }
            }
            if std::mem::take(&mut s.search_ready) {
                s.deliver(TreeEvent::SearchReady);
            }
            if let Some(marks) = standing {
                s.agent_missed = false;
                s.deliver(TreeEvent::Agent { marks });
            }
            s.flush_dirty(&dirs)
        };
        for rel in relist {
            self.request(&rel, Some(Waiter::Refresh { sub }));
        }
        // A run deferred while nobody could see it happens now.
        if let Some(g) = &self.git {
            g.wake_if_deferred();
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

    /// A file operation of ours changed `parents`: re-list the open ones now, so the result
    /// shows with the op's answer instead of after the watcher's window (and at all on a
    /// network mount, which has no watcher). Open folders at or under a path that moved away
    /// are re-listed too, which reports them gone. A no-op where the listing hasn't changed.
    pub fn touched(&self, parents: &[String], gone: &[String]) {
        let open: Vec<String> = {
            let dirs = self.dirs.lock();
            parents.iter().filter(|p| dirs.contains_key(p.as_str())).cloned().collect()
        };
        for rel in open {
            self.request(&rel, None);
        }
        for g in gone.iter().filter(|g| !g.is_empty()) {
            self.revalidate_under(g);
        }
    }

    pub fn set_user_excludes(&self, excludes: &[String]) {
        match &self.remote {
            Some(r) => r.set_excludes(excludes),
            None => self.ignore.lock().invalidate_all(self.git_dir(), Some(excludes)),
        }
        self.revalidate_under("");
    }

    /// The host's helper saw these open folders change: list each again (or have its running
    /// listing run again), which sends the difference as a Delta.
    pub(crate) fn remote_changed(&self, rels: &[String]) {
        for rel in rels {
            if self.note_inflight_change(rel) {
                continue;
            }
            if self.dirs.lock().contains_key(rel.as_str()) {
                self.request(rel, None);
            }
        }
    }

    /// Every folder some subscriber has open.
    pub(crate) fn open_dirs(&self) -> BTreeSet<String> {
        self.subs.lock().values().flat_map(|s| s.interest.iter().cloned()).collect()
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
            let before = s.seq;
            s.deliver(TreeEvent::Dir { rel: rel.to_string(), gen, chunk: 0, last: false, total, truncated, entries: head.to_vec() });
            // Only a sub chunk 0 actually reached continues from chunk 1. One that was paused or
            // stalled only had the folder marked dirty: counting it would record a resume point
            // past a chunk it never got, and every chunk after would be refused as out of order.
            if s.seq > before {
                got.insert(s.id);
            }
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
                                let at = positions(&snap.entries, &add, &chg);
                                s.deliver(TreeEvent::Delta { rel: rel.to_string(), from: o.gen, gen, add, del, chg, at });
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
        let at = positions(&snap.entries, &add, &chg);
        let mut subs = self.subs.lock();
        for s in subs.values_mut().filter(|s| s.interest.contains(rel)) {
            s.deliver(TreeEvent::Delta { rel: rel.to_string(), from, gen, add: add.clone(), del: del.clone(), chg: chg.clone(), at: at.clone() });
        }
        true
    }

    // ── Watching and polling ─────────────────────────────────────────────────────────────

    /// "live" | "partial" | "poll" | "none".
    fn watch_kind(&self) -> String {
        if let Some(r) = &self.remote {
            return r.health().0.to_string();
        }
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
        let note = if let Some(r) = &self.remote {
            r.health().1
        } else {
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
        let (git, git_note) = self.git.as_ref().map(|g| g.health()).unwrap_or(("off", None));
        let note = match (note, git_note) {
            (Some(a), Some(b)) => Some(format!("{a} · {b}")),
            (a, b) => a.or(b),
        };
        TreeEvent::Health { watch, git: git.into(), note }
    }

    pub(crate) fn broadcast_health(&self) {
        let ev = self.health_event();
        let mut subs = self.subs.lock();
        for s in subs.values_mut() {
            s.deliver(ev.clone());
        }
    }

    /// Can a snapshot of `rel` be served without reading the disk? Only when a watcher has been
    /// covering it: never on a network mount, never for a polled folder.
    fn snapshot_trusted(&self, rel: &str) -> bool {
        // A remote folder is always asked again: one round trip, NotModified when unchanged,
        // and never a listing a dropped connection let go stale.
        if self.is_network() || self.remote.is_some() {
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
        if let Some(r) = &self.remote {
            r.watch(rel);
            return;
        }
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
        if let Some(r) = &self.remote {
            r.unwatch(rel);
            return;
        }
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

    /// Whether each absolute path is on disk on this root's machine. On a host only a path
    /// under the root can be asked (one stat each); None where nothing could say.
    pub fn exists(&self, abs_paths: &[String]) -> Vec<Option<bool>> {
        abs_paths
            .iter()
            .map(|a| match &self.remote {
                Some(r) => self.locate_one(a).and_then(|rel| r.exists(&rel)),
                None => paths::on_disk(Path::new(a)),
            })
            .collect()
    }

    fn locate_one(&self, abs: &str) -> Option<String> {
        if let Some(r) = &self.remote {
            // A remote editor's path is the host's own; names are as the host spells them.
            return r.locate(abs);
        }
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
        if let Some(r) = &self.remote {
            split_rel(rel)?;
            let (mut info, is_file, stamp) = r.stat(rel)?;
            if is_file {
                info.numstat = self.git.as_ref().and_then(|g| g.numstat(rel, stamp));
            }
            return Ok(info);
        }
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
        let numstat = if m.is_file() { self.git.as_ref().and_then(|g| g.numstat(rel, (m.len(), m.modified().ok()))) } else { None };
        Ok(StatInfo { size: m.len(), mtime_ms, exec, link_target, numstat })
    }

    // ── Git ──────────────────────────────────────────────────────────────────────────────

    /// Ask for a status run (a no-op outside a repository or with decorations off).
    pub fn git_signal(&self, t: Trigger) {
        if let Some(g) = &self.git {
            g.signal(t);
        }
    }

    /// Everything a subscriber should hold, once git has run.
    fn git_full(&self) -> Option<GitUpdate> {
        let g = self.git.as_ref()?;
        let d = g.data.lock();
        d.ran.then(|| d.full_update())
    }

    /// Someone can see a status run's result: a subscription neither paused nor stalled.
    pub(super) fn git_audience(&self) -> bool {
        self.subs.lock().values().any(|s| !s.paused && !s.stalled && !s.dead)
    }

    /// A status run finished: fold it in, send what changed, feed the search index, and say
    /// so in Health when the repo turned slow or failed (or recovered).
    pub(super) fn git_finished(&self, epoch: u64, out: Result<Status, Failure>, took: Duration) {
        let Some(g) = &self.git else { return };
        let before = g.health();
        // Where HEAD was, from a run already shown: the first run's HEAD is no move.
        let old_head = {
            let d = g.data.lock();
            d.ran.then(|| d.branch.as_ref().map(|b| b.oid.clone())).flatten()
        };
        let published = g.apply(epoch, out, took);
        if let Some(p) = published {
            let new_head = p.diff.branch.as_ref().map(|b| b.oid.as_str());
            if let (Some(old), Some(new)) = (old_head.as_deref(), new_head) {
                if old != new && super::git::is_oid(old) && super::git::is_oid(new) {
                    self.credit_commit(old, new);
                }
            }
            self.search.reconcile(&p.named);
            let mut subs = self.subs.lock();
            let mut full: Option<GitUpdate> = None;
            for s in subs.values_mut() {
                if s.git_gen == Some(p.diff.gen) {
                    continue;
                }
                if s.git_gen == Some(p.from) && !s.git_dirty {
                    s.send_git(p.diff.clone());
                } else {
                    let f = full.get_or_insert_with(|| g.data.lock().full_update());
                    s.send_git(f.clone());
                }
            }
        }
        if g.health() != before {
            self.broadcast_health();
        }
    }

    /// `file_tree_git` changed. Off: every subscriber drops its decorations.
    pub fn set_git_enabled(&self, on: bool) {
        let Some(g) = &self.git else { return };
        if !g.set_enabled(on) {
            return;
        }
        if !on {
            let full = g.data.lock().full_update();
            let mut subs = self.subs.lock();
            for s in subs.values_mut() {
                s.send_git(full.clone());
            }
        }
        self.broadcast_health();
    }

    /// `ft_diff_base`: the file's content at HEAD or in the index.
    pub fn diff_base(&self, rel: &str, base: &str) -> Result<Option<String>, String> {
        match &self.git {
            Some(g) => g.diff_base(rel, base),
            None => Err("This folder isn't in a git repository".to_string()),
        }
    }

    // ── Search ───────────────────────────────────────────────────────────────────────────

    /// `ft_search`.
    pub fn search(&self, sub: u64, query: &str, limit: usize, seq: u64, index_shallow: bool) -> SearchResult {
        self.search.query(sub, query, limit, seq, index_shallow)
    }

    /// The index finished building: every subscriber may re-ask a query answered `indexing`.
    pub(super) fn search_ready(&self) {
        let mut subs = self.subs.lock();
        for s in subs.values_mut() {
            s.deliver(TreeEvent::SearchReady);
        }
    }

    // ── Agents (§9) ──────────────────────────────────────────────────────────────────────

    /// An agent's path (absolute) as a rel of this root; None outside it. An ssh tab's
    /// `user@host:/abs` is never under a local root. Each name is spelled as the listing of an
    /// open folder spells it (a hook path arrives NFC, the disk may hold NFD), and as given
    /// below a folder nobody has open: marks arrive on every Read, and `ft_locate`'s look at
    /// the disk for each name would cost a folder listing per mark there.
    pub(crate) fn locate_agent_path(&self, abs: &str) -> Option<String> {
        if let Some(r) = &self.remote {
            // An ssh tab's ledger path is `host:/abs`, keyed by the same connect args as the
            // place (§15.3): only that host's paths are under this root.
            let path = abs.strip_prefix(r.host.as_str())?.strip_prefix(':')?;
            return r.locate(path);
        }
        if !Path::new(abs).is_absolute() {
            return None;
        }
        let comps = paths::locate_components(&self.root, self.fold, abs)?;
        let dirs = self.dirs.lock();
        let mut rel = String::new();
        for c in comps {
            let key = name_key(&c, self.fold);
            let name = dirs.get(&rel).and_then(|snap| snap.entries.iter().find(|e| e.n == c || name_key(&e.n, self.fold) == key)).map(|e| e.n.clone()).unwrap_or(c);
            rel = join_rel(&rel, &name);
        }
        Some(rel)
    }

    pub(crate) fn agents_routing(&self) -> bool {
        self.agents.routing()
    }

    /// One coalesced batch of marks from the dispatcher, to every subscriber.
    pub fn agent_marks(&self, marks: Vec<AgentMark>) {
        if marks.is_empty() {
            return;
        }
        let ev = TreeEvent::Agent { marks };
        let mut subs = self.subs.lock();
        for s in subs.values_mut() {
            s.deliver(ev.clone());
        }
    }

    /// The marks that still stand under this root, for a subscriber that is new or missed some.
    fn standing_agent_marks(&self) -> Vec<AgentMark> {
        if !self.agents.routing() {
            return Vec::new();
        }
        let host = self.remote.as_ref().map(|r| r.host.as_str());
        self.agents.standing_marks_on(None, host).into_iter().filter_map(|m| self.locate_agent_path(&m.path).map(|rel| m.with_rel(rel))).collect()
    }

    /// How much of a shell call's writes this tree can see (§9.2): "full" with one recursive
    /// watcher, "expanded_only" when only opened folders are watched (Linux, a home or volume
    /// root), "none" with no watcher (a network mount).
    pub fn coverage(&self) -> &'static str {
        if let Some(r) = &self.remote {
            return r.coverage();
        }
        if self.is_network() {
            return "none";
        }
        let ws = self.watch.lock();
        match &ws.hw {
            _ if ws.pending => "full",
            HubWatch::Recursive(_) => "full",
            HubWatch::PerDir(_) => "expanded_only",
            HubWatch::None => "none",
        }
    }

    /// Changes the coalescer saw while exactly one agent tab had a shell call running here
    /// (§9.2): every path that isn't ignored, recorded as "changed while its shell ran". A
    /// folder that was only modified (an entry came or went in it) has an event of its own for
    /// that entry, so it is left out. Costs nothing while no shell call is running.
    pub(super) fn attribute_to_shell(&self, worktree: &[String], reshaped: &HashSet<String>) {
        if worktree.is_empty() {
            return;
        }
        let Some(shell) = self.agents.sole_shell_in(&self.root, self.fold) else { return };
        let mut paths: Vec<PathBuf> = Vec::new();
        for rel in worktree {
            let Ok(abs) = rel_to_native(&self.root, rel) else { continue };
            let is_dir = std::fs::symlink_metadata(&abs).is_ok_and(|m| m.is_dir());
            if is_dir && !reshaped.contains(rel) {
                continue;
            }
            let (parent, name) = paths::split_parent(rel);
            {
                let mut ig = self.ignore.lock();
                if ig.dir_is_ignored(parent) || ig.is_ignored(parent, name, is_dir) {
                    continue;
                }
            }
            if !paths.contains(&abs) {
                paths.push(abs);
            }
        }
        if !paths.is_empty() {
            self.agents.record_fs(&shell, &paths);
        }
    }

    /// HEAD moved from `old` to `new` (both commits). If exactly one agent tab has a turn open
    /// in this repository and the move went forward (commits made, not a checkout elsewhere),
    /// the files those commits changed are credited to that turn (§9.2).
    fn credit_commit(&self, old: &str, new: &str) {
        let Some(g) = &self.git else { return };
        let Some((tab, turn)) = self.agents.sole_open_turn(&|p| self.covers(p)) else { return };
        let Some(rels) = g.committed_between(old, new) else { return };
        let abs: Vec<PathBuf> = rels.iter().filter_map(|r| rel_to_native(&self.root, r).ok()).collect();
        if !abs.is_empty() {
            self.agents.record_commit(&tab, turn, &abs);
        }
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

impl Drop for RootHub {
    /// The git worker and the index's eviction wait hold no strong reference to the hub, so
    /// they are told to end here rather than finding out on their next wakeup.
    fn drop(&mut self) {
        if let Some(g) = &self.git {
            g.stop();
        }
        self.search.stop();
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

/// Where each added then changed entry sits in the folder after a delta (`Delta.at`).
fn positions(entries: &[Entry], add: &[Entry], chg: &[Entry]) -> Vec<u32> {
    if add.is_empty() && chg.is_empty() {
        return Vec::new();
    }
    let index: HashMap<&str, u32> = entries.iter().enumerate().map(|(i, e)| (e.n.as_str(), i as u32)).collect();
    add.iter().chain(chg).map(|e| index.get(e.n.as_str()).copied().unwrap_or(u32::MAX)).collect()
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
        let result = match &hub.remote {
            // A round trip, not local IO: no global IO slot held while it waits.
            Some(r) => r.list(&rel),
            None => {
                let _slot = hub.slots.acquire();
                list::list_dir_with(&hub.root, &rel, &hub.ignore, &mut |head, total, truncated| {
                    early = hub.early_head(&rel, head, total, truncated);
                })
            }
        };
        hub.complete(&rel, result, early);
    }
}

#[cfg(test)]
mod stall_tests {
    use super::*;

    #[derive(Default)]
    struct Recorder {
        msgs: Mutex<Vec<Envelope>>,
    }

    impl Sink for Recorder {
        fn send(&self, env: Envelope) -> bool {
            self.msgs.lock().push(env);
            true
        }
    }

    /// A folder bigger than one ack's worth of window, dirtied during a stall, must finish
    /// with the frontend's ack policy (every 32 messages), not be resent from chunk 0 on
    /// every ack forever.
    #[test]
    fn a_big_folder_dirtied_in_a_stall_finishes_under_partial_acks() {
        // 32 and 64 chunks line the folder's last chunk up with the window filling, which once
        // resent the whole finished folder on every ack.
        for chunks in [2, 31, 32, 33, 40, 64, 96] {
            big_folder_finishes_under_partial_acks(chunks);
        }
    }

    fn big_folder_finishes_under_partial_acks(chunks: usize) {
        const ACK_EVERY: u64 = 32;
        let rec = Arc::new(Recorder::default());
        let mut s = Sub::new(1, "w", rec.clone());
        s.interest.insert("big".to_string());
        let entries: Vec<Entry> = (0..chunks * CHUNK).map(|i| Entry { n: format!("f{i:06}"), k: "f", f: 0 }).collect();
        let mut dirs = HashMap::new();
        dirs.insert("big".to_string(), DirSnap { gen: 7, entries, total: (chunks * CHUNK) as u32, truncated: false });

        // Fill the window until it stalls, then the folder changes.
        while !s.stalled {
            s.deliver(TreeEvent::NotModified { rel: "other".into(), gen: 1 });
        }
        s.deliver(TreeEvent::NotModified { rel: "big".into(), gen: 7 });
        assert!(s.dirty.contains("big"));

        // The client: applies in order, acks every 32nd message; each resume flushes.
        let (mut read, mut since_ack, mut next_chunk, mut done) = (0usize, 0u64, None::<u32>, false);
        while read < rec.msgs.lock().len() {
            assert!(read < 2_000 + chunks, "{chunks} chunks: the folder was resent over and over");
            let env = rec.msgs.lock()[read].clone();
            read += 1;
            if let TreeEvent::Dir { rel, chunk, last, gen, .. } = &env.ev {
                assert_eq!((rel.as_str(), *gen), ("big", 7));
                assert!(!done, "{chunks} chunks: a finished folder was sent again");
                next_chunk = match (chunk, next_chunk) {
                    (0, _) => Some(1),
                    (c, Some(n)) if *c == n => Some(n + 1),
                    (c, n) => panic!("{chunks} chunks: chunk {c} out of order (expected {n:?})"),
                };
                done = *last;
            }
            since_ack += 1;
            if since_ack >= ACK_EVERY {
                since_ack = 0;
                if s.ack(env.seq) {
                    let relist = s.flush_dirty(&dirs);
                    assert!(relist.is_empty());
                }
            }
        }
        assert!(done, "{chunks} chunks: the big folder never finished");
        assert_eq!(next_chunk, Some(chunks as u32));
    }

    /// A big folder's early chunk 0 that met a paused sub was only marked dirty; the resume
    /// must start the listing from chunk 0, not carry on from chunk 1.
    #[test]
    fn an_early_head_a_paused_sub_missed_is_resent_from_chunk_zero() {
        const CHUNKS: usize = 3;
        let root = std::env::temp_dir().join(format!("ft-early-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        let ft = super::super::FileTreeState::with(Duration::from_secs(30), super::super::watch::RAW_CAPACITY, None);
        let rec = Arc::new(Recorder::default());
        let ack = ft.open("w", &root.to_string_lossy(), vec![], rec.clone(), vec![]).unwrap();
        let hub = ft.hub_of(ack.sub_id).unwrap();
        // The watcher first: once it runs it re-lists every folder a sub is interested in,
        // and "big", not on disk, would come back gone and drop the snapshot below. Windows
        // starts it fast enough to land inside this test. Its start ends with a Health
        // broadcast, the second this sub gets (the first came with the subscribe), and that is
        // after it read the interest set, which `watch_pending` turning false is not.
        let healths = || rec.msgs.lock().iter().filter(|m| matches!(m.ev, TreeEvent::Health { .. })).count();
        let deadline = std::time::Instant::now() + Duration::from_secs(60);
        while hub.watch_pending() || healths() < 2 {
            assert!(std::time::Instant::now() < deadline, "the watcher never started");
            std::thread::sleep(Duration::from_millis(10));
        }
        // "big" is never on disk: the snapshot published below is all the hub knows of it.
        hub.subs.lock().get_mut(&ack.sub_id).unwrap().interest.insert("big".to_string());
        hub.pause(ack.sub_id, true);

        let entries: Vec<Entry> = (0..CHUNKS * CHUNK).map(|i| Entry { n: format!("f{i:06}"), k: "f", f: 0 }).collect();
        let early = hub.early_head("big", &entries[..CHUNK], entries.len() as u32, false);
        assert!(early.as_ref().is_some_and(|(_, got)| got.is_empty()), "a paused sub got no chunk 0");
        hub.publish("big", Ok(Listing { entries, total: (CHUNKS * CHUNK) as u32, truncated: false }), Vec::new(), early);
        hub.pause(ack.sub_id, false);

        let chunks: Vec<u32> = rec
            .msgs
            .lock()
            .iter()
            .filter_map(|m| match &m.ev {
                TreeEvent::Dir { rel, chunk, .. } if rel == "big" => Some(*chunk),
                _ => None,
            })
            .collect();
        assert_eq!(chunks, (0..CHUNKS as u32).collect::<Vec<_>>());
        ft.close(ack.sub_id);
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn an_ack_that_makes_no_room_does_not_end_a_stall() {
        let rec = Arc::new(Recorder::default());
        let mut s = Sub::new(1, "w", rec.clone());
        while !s.stalled {
            s.deliver(TreeEvent::NotModified { rel: "x".into(), gen: 1 });
        }
        assert!(!s.ack(0));
        assert!(s.stalled);
        assert!(s.ack(1));
        assert!(!s.stalled);
    }
}
