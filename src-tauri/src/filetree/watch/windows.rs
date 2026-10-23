//! Windows: ReadDirectoryChangesW through notify (docs/file-tree.md §6.3).
//!
//! One handle per hub, recursive on the root. A buffer overflow arrives as a rescan. The root
//! is given in `share::canon` form (no `\\?\` except on UNC), so event paths strip against it.
//! A shallow root (a drive root, the profile folder) watches each opened folder NonRecursive.

use std::path::Path;

use notify::{ReadDirectoryChangesWatcher, RecursiveMode, Watcher};

use super::{DirWatch, RawSender, WatchError};

pub fn recursive(root: &Path, sender: RawSender) -> notify::Result<Box<dyn Send>> {
    let mut w = ReadDirectoryChangesWatcher::new(move |res| sender.push_notify(res), notify::Config::default())?;
    w.watch(root, RecursiveMode::Recursive)?;
    Ok(Box::new(w))
}

struct PerDir(ReadDirectoryChangesWatcher);

impl DirWatch for PerDir {
    fn add(&mut self, dir: &Path) -> Result<(), WatchError> {
        self.0.watch(dir, RecursiveMode::NonRecursive).map_err(|e| WatchError::Other(e.to_string()))
    }

    fn remove(&mut self, dir: &Path) {
        let _ = self.0.unwatch(dir);
    }
}

pub fn per_dir(sender: RawSender) -> notify::Result<Box<dyn DirWatch>> {
    let w = ReadDirectoryChangesWatcher::new(move |res| sender.push_notify(res), notify::Config::default())?;
    Ok(Box::new(PerDir(w)))
}
