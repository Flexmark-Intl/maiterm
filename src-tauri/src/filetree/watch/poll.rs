//! Polling, for the folders no watcher covers (docs/file-tree.md §6.2, §6.4).
//!
//! Two cases, and only these, because idle must stay free:
//! - Linux past its inotify budget: the folders that didn't get a watch, every 2 s;
//! - a network mount, which has no watcher at all: its open folders, every 15 s.
//!
//! Either way a tick does nothing unless a subscription to the hub is unpaused in a focused
//! window, and the thread exits as soon as there is nothing left to poll. A tick is one `stat`
//! per folder against the mtime recorded when the folder started being polled; a folder whose
//! mtime moved is handed to the coalescer, which re-lists it.

use std::sync::Weak;
use std::time::Duration;

use super::RawEvent;
use crate::filetree::hub::RootHub;

pub const LIMIT_INTERVAL: Duration = Duration::from_secs(2);
pub const NETWORK_INTERVAL: Duration = Duration::from_secs(15);

pub fn spawn(hub: Weak<RootHub>, interval: Duration) {
    let spawned = std::thread::Builder::new().name("ft-poll".into()).spawn(move || loop {
        std::thread::sleep(interval);
        let Some(h) = hub.upgrade() else { return };
        // None: nothing to poll any more, and the hub has recorded that this thread is gone.
        let Some(changed) = h.poll_tick() else { return };
        if !changed.is_empty() {
            h.raw_sender().push(RawEvent { paths: changed, rescan: false });
        }
    });
    if let Err(e) = spawned {
        log::warn!("filetree: couldn't start the poller: {e}");
    }
}
