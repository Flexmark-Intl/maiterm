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

pub mod commands;
mod coalesce;
pub mod fsclass;
pub mod hub;
mod ignore_cache;
pub mod list;
pub mod paths;
pub mod place;
pub mod types;
pub mod watch;

#[cfg(test)]
mod tests;

use std::collections::HashMap;
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, OnceLock, Weak};
use std::time::{Duration, Instant};

use parking_lot::{Condvar, Mutex};

use hub::{HubInit, RootHub, Sink};
use types::{DirReq, OpenAck};

/// How long a hub outlives its last subscriber: switching back to a root within this is free.
pub const HUB_GRACE: Duration = Duration::from_secs(30);
/// Listings running at once across the app. A hung `stat` on a dead mount pins at most its
/// hub's own share of these, never the blocking pool or the main thread.
pub const GLOBAL_IO_SLOTS: usize = 32;
/// After this long unfocused, regaining focus revalidates the window's folders: FSEvents can
/// coalesce changes away across a sleep.
pub const FOCUS_REVALIDATE_AFTER: Duration = Duration::from_secs(60);

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
}

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
        })
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
    pub fn open(&self, window: &str, place: &str, mut restore: Vec<DirReq>, sink: Arc<dyn Sink>, excludes: Vec<String>) -> Result<OpenAck, String> {
        let given = Path::new(place.trim());
        if !given.is_absolute() {
            return Err(format!("Files can't open {place} here: folders on an ssh host aren't supported yet"));
        }
        let root = crate::share::canon(given);
        if !root.is_dir() {
            return Err(format!("Folder missing: {place}"));
        }
        let key = root.to_string_lossy().into_owned();
        let hub = self.hub_for(&key, &root, excludes);
        let sub_id = self.next_sub.fetch_add(1, Ordering::Relaxed);
        self.subs.lock().insert(sub_id, hub.clone());
        // A new subscriber cancels any pending reap.
        hub.grace_token.fetch_add(1, Ordering::SeqCst);
        hub.add_sub(sub_id, window, sink);
        if !restore.iter().any(|r| r.rel.is_empty()) {
            restore.insert(0, DirReq { rel: String::new(), known_gen: None });
        }
        hub.expand(sub_id, restore);
        Ok(hub.open_ack(sub_id))
    }

    fn hub_for(&self, key: &str, root: &Path, excludes: Vec<String>) -> Arc<RootHub> {
        if let Some(h) = self.hubs.lock().get(key) {
            if !h.is_gone() {
                return h.clone();
            }
        }
        // Built outside the map's lock: it runs `git rev-parse` and starts a watcher.
        let init = HubInit {
            place: key.to_string(),
            root: root.to_path_buf(),
            fs_class: fsclass::classify(root),
            shallow: place::is_shallow(root),
            user_excludes: excludes,
            raw_capacity: self.raw_capacity,
        };
        let fresh = RootHub::new(init, self.slots.clone(), self.focus.clone(), self.watch_ctx());
        let mut hubs = self.hubs.lock();
        match hubs.get(key) {
            Some(h) if !h.is_gone() => h.clone(),
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
        drop(hub);
        let spawned = std::thread::Builder::new().name("ft-grace".into()).spawn(move || {
            std::thread::sleep(grace);
            let Some(state) = me.upgrade() else { return };
            let mut hubs = state.hubs.lock();
            if let Some(h) = hubs.get(&key) {
                if h.grace_token.load(Ordering::SeqCst) == token && h.sub_count() == 0 {
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
            if away >= FOCUS_REVALIDATE_AFTER || h.is_network() {
                h.revalidate_window(label);
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
