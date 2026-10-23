//! Files: the project tree (docs/file-tree.md). Phase 1 is the local, read-only explorer.
//!
//! Rust owns the truth — listings, generations, watching — and the frontend owns the view
//! (§12.1). One `RootHub` per distinct root on screen, shared by every window showing it and
//! kept 30 s after its last subscriber leaves, so cost scales with roots, never with tabs. Each
//! subscription is one webview's `Channel<Envelope>`: `emit` would broadcast to every webview.
//!
//! - `place.rs`: which folder a tab's tree shows (`ft_resolve_place`).
//! - `hub.rs`: a root's snapshots, generations, subscriptions, backpressure and listing jobs.
//! - `list.rs` + `ignore_cache.rs`: one folder's listing, flagged and sorted.
//! - `coalesce.rs` + `watch/`: disk changes into deltas.
//! - `paths.rs`: rel ↔ native, containment, NFC, case folding.
//! - `fsclass.rs`: network mounts, which get no watcher.
//! - `git.rs`: decorations from `git status`, scheduled per repository (phase 2).
//! - `search.rs`: the find index, built on the first query (phase 2).
//! - `ops.rs`: file operations, their guards, the trash thread and delete tickets (phase 3).
//! - `remote/`: roots on an ssh host, served by one Python helper per host (phase 6).

pub mod commands;
mod coalesce;
pub mod fsclass;
pub mod git;
pub mod hub;
mod ignore_cache;
pub mod list;
pub mod ops;
pub mod paths;
pub mod place;
pub mod remote;
pub mod search;
pub mod types;
pub mod watch;

#[cfg(test)]
mod tests;
#[cfg(test)]
mod git_tests;

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, OnceLock, Weak};
use std::time::{Duration, Instant};

use parking_lot::{Condvar, Mutex};

use crate::agent_files::AgentFiles;
use git::{GitConfig, Trigger};
use hub::{HubInit, RootHub, Sink};
use types::{DeleteTicket, DirReq, FileOp, OpResult, OpenAck};

/// How long a hub outlives its last subscriber: switching back to a root within this is free.
pub const HUB_GRACE: Duration = Duration::from_secs(30);
/// Listings running at once across the app. A hung `stat` on a dead mount pins at most its
/// hub's own share of these, never the blocking pool or the main thread.
pub const GLOBAL_IO_SLOTS: usize = 32;
/// After this long unfocused, regaining focus revalidates the window's folders: FSEvents can
/// coalesce changes away across a sleep.
pub const FOCUS_REVALIDATE_AFTER: Duration = Duration::from_secs(60);
/// git processes running at once across the app (§7.2): seventy tabs on ten repos can't pile
/// up status runs.
pub const GIT_SLOTS: usize = 2;

/// A counting semaphore for blocking IO threads.
pub struct Slots {
    max: usize,
    used: Mutex<usize>,
    cv: Condvar,
}

pub struct SlotGuard<'a>(&'a Slots);

impl Slots {
    pub fn new(max: usize) -> Self {
        Slots { max, used: Mutex::new(0), cv: Condvar::new() }
    }

    pub fn acquire(&self) -> SlotGuard<'_> {
        let mut used = self.used.lock();
        while *used >= self.max {
            self.cv.wait(&mut used);
        }
        *used += 1;
        SlotGuard(self)
    }
}

impl Drop for SlotGuard<'_> {
    fn drop(&mut self) {
        *self.0.used.lock() -= 1;
        self.0.cv.notify_one();
    }
}

/// Which windows are focused, as Rust sees `WindowEvent::Focused`. Polling runs only for a
/// focused window, and a long absence triggers a revalidation on return.
#[derive(Default)]
pub struct Focus {
    map: Mutex<HashMap<String, (bool, Instant)>>,
}

impl Focus {
    /// A window never reported is treated as focused: the first Focused event can arrive after
    /// the tree has already opened.
    pub fn is_focused(&self, label: &str) -> bool {
        self.map.lock().get(label).map(|f| f.0).unwrap_or(true)
    }

    /// Record a change. On regaining focus, how long the window had been away.
    fn set(&self, label: &str, focused: bool) -> Option<Duration> {
        let mut map = self.map.lock();
        let prev = map.insert(label.to_string(), (focused, Instant::now()));
        match prev {
            Some((false, since)) if focused => Some(since.elapsed()),
            _ => None,
        }
    }

    fn forget(&self, label: &str) {
        self.map.lock().remove(label);
    }
}

pub struct FileTreeState {
    me: Weak<FileTreeState>,
    /// place (canonical root) → hub.
    hubs: Mutex<HashMap<String, Arc<RootHub>>>,
    /// sub id → its hub.
    subs: Mutex<HashMap<u64, Arc<RootHub>>>,
    next_sub: AtomicU64,
    slots: Arc<Slots>,
    focus: Arc<Focus>,
    watch_ctx: OnceLock<Arc<watch::WatchContext>>,
    grace: Duration,
    raw_capacity: usize,
    /// How new hubs run git (`file_tree_git`, and the program: a stub in tests).
    git: Mutex<GitConfig>,
    git_slots: Arc<Slots>,
    /// Shell pids whose command just finished (OSC 133 D), for the thread that turns each
    /// into a git signal. Started on first use.
    commands: OnceLock<std::sync::mpsc::Sender<u32>>,
    /// The one thread every trash call runs on, started by the first trash.
    trash: OnceLock<ops::TrashWorker>,
    /// Permanent-delete tickets (`ft_prepare_delete`).
    tickets: ops::Tickets,
    /// What each agent tab is reading and changing (§9): fed by the hooks, routed to the hubs.
    pub agents: Arc<AgentFiles>,
    /// One helper per ssh host, for remote roots (§15).
    pub(crate) remotes: remote::Remotes,
}

/// Agent sessions working on one ssh host, for a remote root's live-cwd guard: (tab id, tab
/// name, cwd there). Asked with the host's connect args.
pub type RemoteHolders<'a> = &'a dyn Fn(&str) -> Vec<(String, String, String)>;

/// How long the resolver waits on a host's helper before keeping its first candidate.
const PROBE_TIMEOUT: Duration = Duration::from_secs(4);

impl FileTreeState {
    pub fn new() -> Arc<Self> {
        Self::with(HUB_GRACE, watch::RAW_CAPACITY, None)
    }

    /// Test seam: a shorter grace, a smaller raw channel (to force overflow), an injected
    /// watch context (a low inotify budget).
    pub(crate) fn with(grace: Duration, raw_capacity: usize, ctx: Option<watch::WatchContext>) -> Arc<Self> {
        let watch_ctx = OnceLock::new();
        if let Some(c) = ctx {
            let _ = watch_ctx.set(Arc::new(c));
        }
        Arc::new_cyclic(|me| FileTreeState {
            me: me.clone(),
            hubs: Mutex::new(HashMap::new()),
            subs: Mutex::new(HashMap::new()),
            next_sub: AtomicU64::new(1),
            slots: Arc::new(Slots::new(GLOBAL_IO_SLOTS)),
            focus: Arc::new(Focus::default()),
            watch_ctx,
            grace,
            raw_capacity,
            git: Mutex::new(GitConfig::default()),
            git_slots: Arc::new(Slots::new(GIT_SLOTS)),
            commands: OnceLock::new(),
            trash: OnceLock::new(),
            tickets: ops::Tickets::default(),
            agents: AgentFiles::new(me.clone()),
            remotes: remote::Remotes::default(),
        })
    }

    /// Every hub still serving its root, for the agent dispatcher to route marks to.
    pub(crate) fn live_hubs(&self) -> Vec<Arc<RootHub>> {
        self.hubs.lock().values().filter(|h| !h.is_gone()).cloned().collect()
    }

    /// The open hubs `path` (absolute, local) is under, with how much of a shell call's writes
    /// each can see (`getAgentFileActivity`'s `coverage`).
    pub fn coverage_for(&self, path: &Path) -> Vec<(String, &'static str)> {
        self.live_hubs()
            .into_iter()
            // A remote root's path is the host's, never this computer's.
            .filter(|h| h.remote.is_none())
            .filter(|h| h.covers(path) || paths::strip_root(&h.root, path, h.fold).is_some())
            .map(|h| (h.place.clone(), h.coverage()))
            .collect()
    }

    /// Test seam: a stand-in trash, so tests never fill the real one. Only before the first
    /// trash.
    #[cfg(test)]
    pub(crate) fn set_trash_backend(&self, backend: Box<dyn ops::TrashBackend>) {
        let _ = self.trash.set(ops::TrashWorker::start(backend));
    }

    fn trash_worker(&self) -> &ops::TrashWorker {
        self.trash.get_or_init(|| ops::TrashWorker::start(Box::new(ops::SystemTrash::new())))
    }

    /// `ft_op`: run a file operation on the subscription's root, then re-list the folders it
    /// changed that are open, so the view doesn't wait on the watcher (a network mount has
    /// none). `holders` lists the tabs working in some folder, for the live-cwd guard; it is
    /// asked only when a folder is moved or removed.
    #[cfg(test)]
    pub fn op(&self, sub_id: u64, op: &FileOp, holders: &dyn Fn() -> Vec<ops::CwdHolder>) -> Result<OpResult, String> {
        self.op_with(sub_id, op, holders, &|_| Vec::new())
    }

    /// `op`, with the agent sessions on an ssh host for a remote root's live-cwd guard.
    pub fn op_with(&self, sub_id: u64, op: &FileOp, holders: &dyn Fn() -> Vec<ops::CwdHolder>, remote_holders: RemoteHolders) -> Result<OpResult, String> {
        let hub = self.hub_of(sub_id)?;
        let result = match &hub.remote {
            Some(r) => {
                let on_host = || remote_holders(&r.host);
                let env = remote::RemoteEnv { place: &hub.place, remote: r, tickets: &self.tickets, holders: &on_host };
                remote::run_op(&env, op)?
            }
            None => {
                let agents = |items: &[std::path::PathBuf]| self.agents.active_tabs(items, hub.fold);
                let env = ops::Env {
                    place: &hub.place,
                    root: &hub.root,
                    fold: hub.fold,
                    trash: self.trash_worker(),
                    tickets: &self.tickets,
                    holders,
                    agents: &agents,
                    rename: ops::rename_noreplace,
                };
                ops::run(&env, op)?
            }
        };
        // An import's `from` is a native path elsewhere, not a rel of this root.
        let from_is_rel = op.kind != "import";
        let mut parents: Vec<String> = Vec::new();
        let mut gone: Vec<String> = Vec::new();
        for item in &result.done {
            if from_is_rel {
                parents.push(paths::split_parent(&item.from).0.to_string());
                if matches!(op.kind.as_str(), "rename" | "move" | "trash" | "delete") {
                    gone.push(item.from.clone());
                }
            }
            if let Some(to) = &item.to {
                parents.push(paths::split_parent(to).0.to_string());
            }
        }
        // A create's folders on the way: the first one made is in its parent's listing.
        if let Some(first) = result.undo.as_ref().filter(|_| op.kind.starts_with("create_")).and_then(|u| u.src.first()) {
            parents.push(paths::split_parent(first).0.to_string());
        }
        parents.sort();
        parents.dedup();
        hub.touched(&parents, &gone);
        Ok(result)
    }

    /// `ft_prepare_delete`: a ticket for permanently deleting exactly `rels`, with the counts.
    pub fn prepare_delete(&self, sub_id: u64, rels: &[String]) -> Result<DeleteTicket, String> {
        let hub = self.hub_of(sub_id)?;
        if let Some(r) = &hub.remote {
            let nobody = Vec::new;
            let env = remote::RemoteEnv { place: &hub.place, remote: r, tickets: &self.tickets, holders: &nobody };
            return remote::prepare_delete(&env, rels);
        }
        let none = || Vec::new();
        let no_agents = |_: &[std::path::PathBuf]| Vec::new();
        let env = ops::Env {
            place: &hub.place,
            root: &hub.root,
            fold: hub.fold,
            trash: self.trash_worker(),
            tickets: &self.tickets,
            holders: &none,
            agents: &no_agents,
            rename: ops::rename_noreplace,
        };
        ops::prepare_delete(&env, rels)
    }

    /// Test seam: how hubs opened from now on run git.
    #[cfg(test)]
    pub(crate) fn set_git_config(&self, cfg: GitConfig) {
        *self.git.lock() = cfg;
    }

    /// Linux's one inotify instance is made on first use, not at launch.
    fn watch_ctx(&self) -> Arc<watch::WatchContext> {
        self.watch_ctx
            .get_or_init(|| {
                #[allow(unused_mut)]
                let mut ctx = watch::WatchContext::default();
                #[cfg(target_os = "linux")]
                {
                    ctx.inotify = Some(watch::inotify::SharedInotify::new());
                }
                Arc::new(ctx)
            })
            .clone()
    }

    /// Subscribe a webview to a local root. `restore` re-requests the folders it had open, with
    /// the generations it holds, so a returning view costs one round trip. Events may reach the
    /// Channel before this returns.
    #[cfg(test)]
    pub fn open(&self, window: &str, place: &str, restore: Vec<DirReq>, sink: Arc<dyn Sink>, excludes: Vec<String>) -> Result<OpenAck, String> {
        self.open_with(window, place, restore, sink, excludes, None)
    }

    /// `open`, with how to find an ssh host's connection (the tab's bridge tunnel) for a remote
    /// place, `user@host:/abs`. Blocking: the first remote root on a host starts its helper.
    pub fn open_with(&self, window: &str, place: &str, restore: Vec<DirReq>, sink: Arc<dyn Sink>, excludes: Vec<String>, lookup: Option<remote::Lookup>) -> Result<OpenAck, String> {
        let given = Path::new(place.trim());
        if given.is_absolute() {
            let root = crate::share::canon(given);
            if !root.is_dir() {
                return Err(format!("Folder missing: {place}"));
            }
            let key = root.to_string_lossy().into_owned();
            let hub = self.hub_for(&key, &root, excludes);
            return Ok(self.subscribe(hub, window, restore, sink));
        }
        let Some((host, root)) = place::split_remote(place.trim()) else {
            return Err(format!("Files can't open {place}: not a folder here or on an ssh host"));
        };
        let hub = self.remote_hub_for(place.trim(), &host, &root, excludes, lookup)?;
        Ok(self.subscribe(hub, window, restore, sink))
    }

    /// The hub for a remote place, made (and its root opened on the host) if there is none.
    fn remote_hub_for(&self, key: &str, host: &str, root: &str, excludes: Vec<String>, lookup: Option<remote::Lookup>) -> Result<Arc<RootHub>, String> {
        let lease = self.remotes.lease(host, lookup);
        let kept = self.hubs.lock().get(key).filter(|h| h.reusable()).cloned();
        if let Some(h) = kept {
            // A hub in its grace period had nobody watching for a `.git` there that a poll
            // would have caught (no inotify), so the host is asked once before it is handed out.
            match h.remote.as_ref().filter(|_| h.sub_count() == 0).map(|r| r.repo_now()) {
                Some(Some(now)) if now != h.repo => h.repo_moved(),
                _ => return Ok(h),
            }
        }
        // Outside the map's lock: it waits on the host.
        let r = remote::RemoteRoot::open(lease, root, excludes.clone())?;
        let real = PathBuf::from(&r.root);
        let init = HubInit {
            place: key.to_string(),
            shallow: place::is_shallow_remote(&r.root, r.conn.hello().map(|h| h.home).as_deref()),
            root: real,
            fs_class: fsclass::UNKNOWN,
            user_excludes: excludes,
            raw_capacity: self.raw_capacity,
            git: self.git.lock().clone(),
            git_slots: self.git_slots.clone(),
            agents: self.agents.clone(),
            remote: Some(r),
        };
        let fresh = RootHub::new(init, self.slots.clone(), self.focus.clone(), self.watch_ctx());
        let mut hubs = self.hubs.lock();
        Ok(match hubs.get(key) {
            Some(h) if h.reusable() => h.clone(),
            _ => {
                hubs.insert(key.to_string(), fresh.clone());
                fresh
            }
        })
    }

    /// The resolver's question for an ssh tab's candidate folders (§4.1): each one's real path
    /// when it is a folder on the host, and its repository's top level. None when the host
    /// can't be asked in time. Starts the host's helper if it isn't running (kept for the grace
    /// period, so the tree that opens next finds it ready).
    pub fn probe_remote(&self, host: &str, lookup: Option<remote::Lookup>, paths: &[String]) -> Option<Vec<remote::proto::Probed>> {
        if paths.is_empty() {
            return Some(Vec::new());
        }
        // Held for the probe; dropping it after starts the grace period.
        let lease = self.remotes.lease(host, lookup);
        lease.conn().probe(paths, PROBE_TIMEOUT)
    }

    fn subscribe(&self, hub: Arc<RootHub>, window: &str, mut restore: Vec<DirReq>, sink: Arc<dyn Sink>) -> OpenAck {
        let sub_id = self.next_sub.fetch_add(1, Ordering::Relaxed);
        self.subs.lock().insert(sub_id, hub.clone());
        // A new subscriber cancels any pending reap.
        hub.grace_token.fetch_add(1, Ordering::SeqCst);
        hub.add_sub(sub_id, window, sink);
        if !restore.iter().any(|r| r.rel.is_empty()) {
            restore.insert(0, DirReq { rel: String::new(), known_gen: None });
        }
        hub.expand(sub_id, restore);
        hub.open_ack(sub_id)
    }

    fn hub_for(&self, key: &str, root: &Path, excludes: Vec<String>) -> Arc<RootHub> {
        let kept = self.hubs.lock().get(key).filter(|h| h.reusable()).cloned();
        if let Some(h) = kept {
            // Re-read off the disk (a few stats): a `git init` while the hub sat in its grace
            // period, or on a mount nothing watches, was never seen.
            if h.repo_current() {
                return h;
            }
            h.repo_moved();
        }
        // Built outside the map's lock: it runs `git rev-parse` and starts a watcher.
        let init = HubInit {
            place: key.to_string(),
            root: root.to_path_buf(),
            fs_class: fsclass::classify(root),
            shallow: place::is_shallow(root),
            user_excludes: excludes,
            raw_capacity: self.raw_capacity,
            git: self.git.lock().clone(),
            git_slots: self.git_slots.clone(),
            agents: self.agents.clone(),
            remote: None,
        };
        let fresh = RootHub::new(init, self.slots.clone(), self.focus.clone(), self.watch_ctx());
        let mut hubs = self.hubs.lock();
        match hubs.get(key) {
            Some(h) if h.reusable() => h.clone(),
            _ => {
                hubs.insert(key.to_string(), fresh.clone());
                fresh
            }
        }
    }

    pub fn hub_of(&self, sub_id: u64) -> Result<Arc<RootHub>, String> {
        self.subs.lock().get(&sub_id).cloned().ok_or_else(|| format!("No Files subscription {sub_id}"))
    }

    /// End a subscription. The hub lives on for the grace period after its last one.
    pub fn close(&self, sub_id: u64) {
        let Some(hub) = self.subs.lock().remove(&sub_id) else { return };
        if hub.remove_sub(sub_id) == 0 {
            self.schedule_reap(hub);
        }
    }

    fn schedule_reap(&self, hub: Arc<RootHub>) {
        let token = hub.grace_token.fetch_add(1, Ordering::SeqCst) + 1;
        let me = self.me.clone();
        let grace = self.grace;
        let key = hub.place.clone();
        // A retired hub's key may hold its successor by then, whose token can be any number:
        // only this hub is reaped.
        let which = Arc::downgrade(&hub);
        drop(hub);
        let spawned = std::thread::Builder::new().name("ft-grace".into()).spawn(move || {
            std::thread::sleep(grace);
            let Some(state) = me.upgrade() else { return };
            let mut hubs = state.hubs.lock();
            if let Some(h) = hubs.get(&key) {
                if std::ptr::eq(Arc::as_ptr(h), which.as_ptr()) &&h.grace_token.load(Ordering::SeqCst) == token && h.sub_count() == 0 {
                    hubs.remove(&key);
                }
            }
        });
        if let Err(e) = spawned {
            log::warn!("filetree: couldn't schedule a hub's reap: {e}");
        }
    }

    /// Every subscription a window held: it closed, or its page reloaded (a Channel to a
    /// reloaded page still "sends" without error, so this is the only way to know).
    pub fn window_gone(&self, label: &str) {
        let mine: Vec<u64> = {
            let subs = self.subs.lock();
            subs.iter().filter(|(id, h)| h.sub_window(**id).as_deref() == Some(label)).map(|(id, _)| *id).collect()
        };
        for id in mine {
            self.close(id);
        }
    }

    pub fn window_destroyed(&self, label: &str) {
        self.window_gone(label);
        self.focus.forget(label);
    }

    /// `WindowEvent::Focused`. Back after a long absence: revalidate what the window shows.
    /// A network root revalidates on every return, having no watcher.
    pub fn window_focus(&self, label: &str, focused: bool) {
        let away = self.focus.set(label, focused);
        let Some(away) = away else { return };
        let hubs: Vec<Arc<RootHub>> = self.hubs.lock().values().cloned().collect();
        for h in hubs {
            // A remote root whose helper can't watch everything open (polled folders, no
            // python3) is looked at again on every return, like a network mount.
            let unwatched = h.remote.as_ref().is_some_and(|r| r.health().0 != "live");
            if away >= FOCUS_REVALIDATE_AFTER || h.is_network() || unwatched {
                h.revalidate_window(label);
            }
            // Regaining focus is a git edge (§7.2): a commit made in another app shows.
            if h.has_window(label) {
                h.git_signal(Trigger::Focus);
            }
        }
    }

    /// `file_tree_git` changed (or is read at `ft_open`): every hub follows.
    pub fn set_git_enabled(&self, on: bool) {
        {
            let mut cfg = self.git.lock();
            if cfg.enabled == on {
                return;
            }
            cfg.enabled = on;
        }
        let hubs: Vec<Arc<RootHub>> = self.hubs.lock().values().cloned().collect();
        for h in hubs {
            h.set_git_enabled(on);
        }
    }

    /// A command finished in a terminal (OSC 133 D), whose shell is `shell_pid`. Called on the
    /// PTY reader's thread, so it only hands the pid on: a thread looks up the shell's folder
    /// and signals the repository hubs it is in, so a `git commit` typed in a terminal
    /// decorates without waiting for the watcher's trailing delay. Nothing at all happens
    /// while no tree is open.
    pub fn command_done(&self, shell_pid: u32) {
        if self.hubs.lock().values().all(|h| h.git.is_none()) {
            return;
        }
        let tx = self.commands.get_or_init(|| {
            let (tx, rx) = std::sync::mpsc::channel::<u32>();
            let me = self.me.clone();
            let spawned = std::thread::Builder::new().name("ft-commands".into()).spawn(move || {
                // Blocks with no timeout: a terminal that runs nothing costs nothing.
                while let Ok(pid) = rx.recv() {
                    let Some(state) = me.upgrade() else { return };
                    let Some(cwd) = crate::pty::manager::cwd_of_pid(pid) else { continue };
                    state.note_command_in(Path::new(&cwd));
                }
            });
            if let Err(e) = spawned {
                log::warn!("filetree: couldn't start the command watcher: {e}");
            }
            tx
        });
        let _ = tx.send(shell_pid);
    }

    /// A command finished with its shell in `cwd`.
    pub fn note_command_in(&self, cwd: &Path) {
        let cwd = crate::share::canon(cwd);
        let hubs: Vec<Arc<RootHub>> = self.hubs.lock().values().filter(|h| h.git.is_some()).cloned().collect();
        for h in hubs {
            if h.covers(&cwd) {
                h.git_signal(Trigger::Command);
            }
        }
    }

    /// The user's `file_tree_exclude` changed.
    pub fn set_excludes(&self, excludes: &[String]) {
        let hubs: Vec<Arc<RootHub>> = self.hubs.lock().values().cloned().collect();
        for h in hubs {
            h.set_user_excludes(excludes);
        }
    }

    #[cfg(test)]
    pub(crate) fn hub_count(&self) -> usize {
        self.hubs.lock().len()
    }
}
