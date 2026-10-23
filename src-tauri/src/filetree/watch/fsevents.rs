//! macOS: FSEvents through notify (docs/file-tree.md §6.1).
//!
//! One stream per hub, recursive on the root, created once: notify stops and restarts the
//! stream on every `watch()` call, so the watched path set must not change per expand. A
//! recursive stream costs about one fd whatever the tree's size, and the coalescer filters the
//! volume. A shallow root (home, a volume) instead watches each opened folder NonRecursive and
//! accepts a stream restart per expand, which is harmless: a newly expanded folder is listed
//! anyway.

use std::path::Path;

use notify::{FsEventWatcher, RecursiveMode, Watcher};

use super::{DirWatch, RawSender, WatchError};

pub fn recursive(root: &Path, sender: RawSender) -> notify::Result<Box<dyn Send>> {
    let mut w = FsEventWatcher::new(move |res| sender.push_notify(res), notify::Config::default())?;
    w.watch(root, RecursiveMode::Recursive)?;
    Ok(Box::new(w))
}

struct PerDir(FsEventWatcher);

impl DirWatch for PerDir {
    fn add(&mut self, dir: &Path) -> Result<(), WatchError> {
        self.0.watch(dir, RecursiveMode::NonRecursive).map_err(|e| WatchError::Other(e.to_string()))
    }

    fn remove(&mut self, dir: &Path) {
        let _ = self.0.unwatch(dir);
    }
}

pub fn per_dir(sender: RawSender) -> notify::Result<Box<dyn DirWatch>> {
    let w = FsEventWatcher::new(move |res| sender.push_notify(res), notify::Config::default())?;
    Ok(Box::new(PerDir(w)))
}
