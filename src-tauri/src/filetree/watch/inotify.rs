//! Linux: one inotify instance for the whole app (docs/file-tree.md §6.2).
//!
//! `max_user_instances` defaults to 128 and is shared with every other program the user runs,
//! so maiTerm takes ONE: a single `INotifyWatcher` owned by `FileTreeState`, whose handler
//! routes each event to the hubs whose root contains it.
//!
//! Watches are NonRecursive, on the folders a tree has open. The budget is
//! `min(max_user_watches / 8, 8192)` across every hub, read from
//! `/proc/sys/fs/inotify/max_user_watches`. Past it (or on `ENOSPC`, which other programs can
//! cause) the hub moves a folder to visible-only polling and says so in Health.
//!
//! The stated limitation: nothing is recursive, so a change deep inside a folder nobody has
//! opened is not seen until it is opened.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use notify::{INotifyWatcher, RecursiveMode, Watcher};
use parking_lot::Mutex;

use super::{DirWatch, RawEvent, RawSender, WatchError};

/// The ceiling no matter how generous the system is.
const BUDGET_CAP: usize = 8192;

struct Route {
    id: u64,
    root: PathBuf,
    sender: RawSender,
}

pub struct SharedInotify {
    watcher: Mutex<Option<INotifyWatcher>>,
    routes: Arc<Mutex<Vec<Route>>>,
    /// Watched folder → how many hubs asked for it (nested roots can share a folder).
    watched: Mutex<HashMap<PathBuf, usize>>,
    budget: usize,
    next_route: AtomicU64,
}

/// `min(max_user_watches / 8, 8192)`; 8192/8 when the limit can't be read.
pub fn budget_from_proc() -> usize {
    let max = std::fs::read_to_string("/proc/sys/fs/inotify/max_user_watches")
        .ok()
        .and_then(|s| s.trim().parse::<usize>().ok())
        .unwrap_or(8192);
    (max / 8).clamp(1, BUDGET_CAP)
}

impl SharedInotify {
    pub fn new() -> Arc<Self> {
        Self::with_budget(budget_from_proc())
    }

    /// Test seam: an injected budget.
    pub fn with_budget(budget: usize) -> Arc<Self> {
        Arc::new(SharedInotify {
            watcher: Mutex::new(None),
            routes: Arc::new(Mutex::new(Vec::new())),
            watched: Mutex::new(HashMap::new()),
            budget,
            next_route: AtomicU64::new(1),
        })
    }

    pub fn budget(&self) -> usize {
        self.budget
    }

    /// Folders watched right now, across every hub.
    pub fn watch_count(&self) -> usize {
        self.watched.lock().len()
    }

    /// The instance, created on first use: an app that never opens Files never holds one.
    fn with_watcher<T>(&self, f: impl FnOnce(&mut INotifyWatcher) -> T) -> Result<T, WatchError> {
        let mut guard = self.watcher.lock();
        if guard.is_none() {
            let routes = self.routes.clone();
            let w = INotifyWatcher::new(
                move |res: notify::Result<notify::Event>| {
                    let routes = routes.lock();
                    let (paths, rescan, content_only) = match res {
                        Ok(ev) => {
                            let rescan = ev.need_rescan();
                            (ev.paths, rescan, super::content_only(&ev.kind))
                        }
                        Err(e) => (e.paths, true, false),
                    };
                    for r in routes.iter() {
                        if paths.is_empty() {
                            // Lost events with no path (IN_Q_OVERFLOW): every hub rescans.
                            r.sender.push(RawEvent { paths: vec![], rescan: true, content_only: false });
                            continue;
                        }
                        let mine: Vec<PathBuf> = paths.iter().filter(|p| p.starts_with(&r.root)).cloned().collect();
                        if !mine.is_empty() {
                            r.sender.push(RawEvent { paths: mine, rescan, content_only });
                        }
                    }
                },
                notify::Config::default(),
            )
            .map_err(|e| WatchError::Other(e.to_string()))?;
            *guard = Some(w);
        }
        Ok(f(guard.as_mut().expect("just created")))
    }

    /// A hub's handle: its route, and the folders it asked for.
    pub fn handle(self: &Arc<Self>, root: &Path, sender: RawSender) -> Handle {
        let id = self.next_route.fetch_add(1, Ordering::Relaxed);
        self.routes.lock().push(Route { id, root: root.to_path_buf(), sender });
        Handle { shared: self.clone(), route: id, mine: HashSet::new() }
    }

    fn add(&self, dir: &Path) -> Result<(), WatchError> {
        let mut watched = self.watched.lock();
        if let Some(n) = watched.get_mut(dir) {
            *n += 1;
            return Ok(());
        }
        if watched.len() >= self.budget {
            return Err(WatchError::Limit);
        }
        let r = self.with_watcher(|w| w.watch(dir, RecursiveMode::NonRecursive))?;
        match r {
            Ok(()) => {
                watched.insert(dir.to_path_buf(), 1);
                Ok(())
            }
            Err(e) if matches!(e.kind, notify::ErrorKind::MaxFilesWatch) => Err(WatchError::Limit),
            Err(e) => Err(WatchError::Other(e.to_string())),
        }
    }

    fn remove(&self, dir: &Path) {
        let mut watched = self.watched.lock();
        let Some(n) = watched.get_mut(dir) else { return };
        *n -= 1;
        if *n == 0 {
            watched.remove(dir);
            let _ = self.with_watcher(|w| w.unwatch(dir));
        }
    }
}

/// One hub's share of the instance. Dropping it releases every watch it held and its route.
pub struct Handle {
    shared: Arc<SharedInotify>,
    route: u64,
    mine: HashSet<PathBuf>,
}

impl DirWatch for Handle {
    fn add(&mut self, dir: &Path) -> Result<(), WatchError> {
        if self.mine.contains(dir) {
            return Ok(());
        }
        self.shared.add(dir)?;
        self.mine.insert(dir.to_path_buf());
        Ok(())
    }

    fn remove(&mut self, dir: &Path) {
        if self.mine.remove(dir) {
            self.shared.remove(dir);
        }
    }
}

impl Drop for Handle {
    fn drop(&mut self) {
        for d in std::mem::take(&mut self.mine) {
            self.shared.remove(&d);
        }
        self.shared.routes.lock().retain(|r| r.id != self.route);
    }
}
