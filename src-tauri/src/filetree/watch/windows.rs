//! Windows: ReadDirectoryChangesW through notify (docs/file-tree.md §6.3).
//!
//! One handle per hub, recursive on the root. A buffer overflow arrives as a rescan. The root
//! is given in `share::canon` form (no `\\?\` except on UNC), so event paths strip against it.
//! A shallow root (a drive root, the profile folder) watches each opened folder NonRecursive.
//!
//! A handle reports what happens inside its folder, never the folder itself going: a root
//! deleted (with nothing in it to report) or renamed away, the Recycle Bin included, is silent
//! on its own watch. So the root's parent is watched too, NonRecursive, and only an event that
//! names the root appearing, going or moving gets through.

use std::path::{Path, PathBuf};

use notify::{EventKind, ReadDirectoryChangesWatcher, RecursiveMode, Watcher};

use super::{DirWatch, RawEvent, RawSender, WatchError};

pub fn recursive(root: &Path, sender: RawSender) -> notify::Result<Box<dyn Send>> {
    let own = sender.clone();
    let mut w = ReadDirectoryChangesWatcher::new(move |res| own.push_notify(res), notify::Config::default())?;
    w.watch(root, RecursiveMode::Recursive)?;
    Ok(Box::new((w, root_watch(root, sender))))
}

/// The root's own entry in its parent. None (a drive root, or a parent that can't be watched)
/// leaves only the relisting a deleted root's children cause.
fn root_watch(root: &Path, sender: RawSender) -> Option<ReadDirectoryChangesWatcher> {
    let parent = root.parent()?;
    let name = root.file_name()?.to_os_string();
    let mut w = ReadDirectoryChangesWatcher::new(
        move |res: notify::Result<notify::Event>| {
            // The parent's own errors are not the root's: the recursive watch reports those.
            let Ok(ev) = res else { return };
            if !matches!(ev.kind, EventKind::Create(_) | EventKind::Remove(_) | EventKind::Modify(notify::event::ModifyKind::Name(_))) {
                return;
            }
            let paths: Vec<PathBuf> = ev.paths.into_iter().filter(|p| p.file_name().is_some_and(|n| n.eq_ignore_ascii_case(&name))).collect();
            if !paths.is_empty() {
                sender.push(RawEvent { paths, rescan: false, content_only: false });
            }
        },
        notify::Config::default(),
    )
    .ok()?;
    w.watch(parent, RecursiveMode::NonRecursive).ok()?;
    Some(w)
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
