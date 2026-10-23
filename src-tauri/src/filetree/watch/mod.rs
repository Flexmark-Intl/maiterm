//! Watching a root, per OS (docs/file-tree.md §6).
//!
//! Every backend does one thing: push raw paths into the hub's bounded channel, which the
//! coalescer (`coalesce.rs`) drains. The channel holds 65,536 events; when it is full the
//! overflow flag is raised instead, and the coalescer re-lists every folder it serves. Events
//! are never dropped silently.
//!
//! - macOS (`fsevents.rs`): one FSEvents stream per hub, recursive on the root, created once.
//!   notify restarts the stream on every `watch()` call, so the path set never changes per
//!   expand. kqueue stays off (`Cargo.toml`): one fd per file is the EMFILE trap.
//! - Linux (`inotify.rs`): ONE inotify instance for the whole app, NonRecursive watches on the
//!   folders that are open, within a budget; past it a folder is polled while on screen.
//! - Windows (`windows.rs`): one ReadDirectoryChangesW handle per hub, recursive on the root.
//! - Shallow roots (home, `/`, a volume root, §6.5) watch only the opened folders, so a home
//!   directory's caches never stream events into an idle app.
//! - Network mounts: no watcher at all (it would miss other machines' writes); `poll.rs`.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{sync_channel, Receiver, SyncSender, TrySendError};
use std::sync::Arc;

#[cfg(target_os = "macos")]
pub mod fsevents;
#[cfg(target_os = "linux")]
pub mod inotify;
pub mod poll;
#[cfg(windows)]
pub mod windows;

/// The raw event channel's capacity (§6).
pub const RAW_CAPACITY: usize = 65_536;

/// Paths a backend saw change. `rescan`: the backend lost events under these paths (FSEvents
/// MustScanSubDirs, an RDCW buffer overflow, inotify `IN_Q_OVERFLOW`); empty `paths` with
/// `rescan` means anywhere.
#[derive(Debug, Clone, Default)]
pub struct RawEvent {
    pub paths: Vec<PathBuf>,
    pub rescan: bool,
    /// The backend said only that these paths' content or metadata changed: nothing appeared,
    /// went or was renamed. On a folder that is routine, not a change of shape: Windows
    /// reports a folder MODIFIED whenever an entry in it is added, removed or renamed (its
    /// last-write time moves, reported apart from the entry's own event, and as late as the
    /// cache flushes it), and FSEvents reports a folder's inode metadata the same way.
    pub content_only: bool,
}

/// `kind` says only that content or metadata changed (see `RawEvent::content_only`). `Any`
/// and every create, remove and rename are not: they may be a folder's shape changing.
pub fn content_only(kind: &notify::EventKind) -> bool {
    use notify::event::ModifyKind;
    matches!(kind, notify::EventKind::Modify(ModifyKind::Any | ModifyKind::Data(_) | ModifyKind::Metadata(_)))
}

/// The sending half every backend holds.
#[derive(Clone)]
pub struct RawSender {
    tx: SyncSender<RawEvent>,
    overflow: Arc<AtomicBool>,
}

impl RawSender {
    /// Never blocks a watcher's thread: a full channel raises the overflow flag instead, and
    /// the coalescer, which is busy draining that full channel, sees it on its next batch.
    pub fn push(&self, ev: RawEvent) {
        if let Err(TrySendError::Full(_)) = self.tx.try_send(ev) {
            self.overflow.store(true, Ordering::SeqCst);
        }
    }

    /// A notify callback's result as a raw event. An error means events were lost somewhere.
    pub fn push_notify(&self, res: notify::Result<notify::Event>) {
        match res {
            Ok(ev) => {
                let rescan = ev.need_rescan();
                if !ev.paths.is_empty() || rescan {
                    self.push(RawEvent { content_only: content_only(&ev.kind), paths: ev.paths, rescan });
                }
            }
            Err(e) => {
                log::debug!("filetree: watcher error, rescanning: {e}");
                self.push(RawEvent { paths: e.paths, rescan: true, content_only: false });
            }
        }
    }
}

/// A raw channel of `capacity` and the overflow flag the coalescer reads.
pub fn channel(capacity: usize) -> (RawSender, Receiver<RawEvent>, Arc<AtomicBool>) {
    let (tx, rx) = sync_channel(capacity);
    let overflow = Arc::new(AtomicBool::new(false));
    (RawSender { tx, overflow: overflow.clone() }, rx, overflow)
}

#[derive(Debug, Clone, PartialEq)]
pub enum WatchError {
    /// Out of watches (inotify `ENOSPC` or our budget): the folder is polled instead. Only
    /// Linux has a watch budget.
    #[cfg_attr(not(target_os = "linux"), allow(dead_code))]
    Limit,
    Other(String),
}

/// Per-folder watching: shallow roots, and every Linux root.
pub trait DirWatch: Send {
    fn add(&mut self, dir: &Path) -> Result<(), WatchError>;
    fn remove(&mut self, dir: &Path);
}

/// How a hub is watched. The watcher lives as long as this does.
pub enum HubWatch {
    /// One recursive watcher on the root, held only so it stays alive.
    Recursive(#[allow(dead_code)] Box<dyn Send>),
    /// Each opened folder is watched on its own.
    PerDir(Box<dyn DirWatch>),
    /// Nothing watches this root: a network mount, or no backend on this OS.
    None,
}

/// Shared across hubs: Linux's one inotify instance. Empty elsewhere.
#[derive(Default)]
pub struct WatchContext {
    #[cfg(target_os = "linux")]
    pub inotify: Option<Arc<inotify::SharedInotify>>,
}

/// The watcher for a new hub, and why it is degraded if it is.
pub fn create(root: &Path, shallow: bool, network: bool, sender: RawSender, ctx: &WatchContext) -> (HubWatch, Option<String>) {
    if network {
        return (HubWatch::None, None);
    }
    #[cfg(target_os = "linux")]
    {
        let _ = (root, shallow);
        return match &ctx.inotify {
            Some(shared) => (HubWatch::PerDir(Box::new(shared.handle(root, sender))), None),
            None => (HubWatch::None, Some("watch: inotify unavailable".to_string())),
        };
    }
    #[cfg(target_os = "macos")]
    {
        let _ = ctx;
        return native(root, shallow, sender, fsevents::recursive, fsevents::per_dir);
    }
    #[cfg(windows)]
    {
        let _ = ctx;
        return native(root, shallow, sender, windows::recursive, windows::per_dir);
    }
    #[allow(unreachable_code)]
    {
        let _ = (root, shallow, sender, ctx);
        (HubWatch::None, Some("watch: not supported on this system".to_string()))
    }
}

/// Watches on a repository's own folders, for git decorations (§6, §7.2): each git dir
/// NonRecursive (index, HEAD, packed-refs, MERGE_HEAD) plus its `refs/heads`. Used when the
/// root's watcher doesn't already cover them. None when nothing could be watched: decorations
/// then refresh on the other edges (a command finishing, focus, a manual refresh).
pub fn create_git(git_dirs: &[PathBuf], sender: RawSender, ctx: &WatchContext) -> Option<Box<dyn Send>> {
    let mut dirs: Vec<PathBuf> = Vec::new();
    for g in git_dirs {
        for d in [g.clone(), g.join("refs").join("heads")] {
            if d.is_dir() && !dirs.contains(&d) {
                dirs.push(d);
            }
        }
    }
    if dirs.is_empty() {
        return None;
    }
    #[cfg(target_os = "linux")]
    {
        // The shared instance routes by path prefix, so each folder gets a route of its own:
        // a git dir outside the root would otherwise be dropped as belonging to no hub.
        let shared = ctx.inotify.as_ref()?;
        let mut handles = Vec::new();
        for d in &dirs {
            let mut h = shared.handle(d, sender.clone());
            match h.add(d) {
                Ok(()) => handles.push(h),
                Err(e) => log::debug!("filetree: no git watch on {}: {e:?}", d.display()),
            }
        }
        return (!handles.is_empty()).then(|| Box::new(handles) as Box<dyn Send>);
    }
    #[cfg(target_os = "macos")]
    {
        let _ = ctx;
        return per_dir_git(&dirs, fsevents::per_dir(sender));
    }
    #[cfg(windows)]
    {
        let _ = ctx;
        return per_dir_git(&dirs, windows::per_dir(sender));
    }
    #[allow(unreachable_code)]
    {
        let _ = (sender, ctx);
        None
    }
}

#[cfg(any(target_os = "macos", windows))]
fn per_dir_git(dirs: &[PathBuf], made: notify::Result<Box<dyn DirWatch>>) -> Option<Box<dyn Send>> {
    let mut w = match made {
        Ok(w) => w,
        Err(e) => {
            log::debug!("filetree: no git watcher: {e}");
            return None;
        }
    };
    let mut any = false;
    for d in dirs {
        any |= w.add(d).is_ok();
    }
    any.then(|| Box::new(w) as Box<dyn Send>)
}

#[cfg(any(target_os = "macos", windows))]
fn native(
    root: &Path,
    shallow: bool,
    sender: RawSender,
    recursive: fn(&Path, RawSender) -> notify::Result<Box<dyn Send>>,
    per_dir: fn(RawSender) -> notify::Result<Box<dyn DirWatch>>,
) -> (HubWatch, Option<String>) {
    let made = if shallow { per_dir(sender).map(HubWatch::PerDir) } else { recursive(root, sender).map(HubWatch::Recursive) };
    match made {
        Ok(w) => (w, None),
        Err(e) => {
            log::warn!("filetree: no watcher for {}: {e}", root.display());
            (HubWatch::None, Some(format!("watch: unavailable ({e})")))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_full_channel_raises_overflow_instead_of_blocking() {
        let (tx, rx, overflow) = channel(2);
        for _ in 0..5 {
            tx.push(RawEvent { paths: vec![PathBuf::from("/x")], ..Default::default() });
        }
        assert!(overflow.load(Ordering::SeqCst));
        assert_eq!(rx.try_iter().count(), 2);
    }
}
