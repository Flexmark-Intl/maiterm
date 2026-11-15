//! Waiting on a Files hub's own account of what it did (`HubReport`) instead of the clock.
//!
//! A test that sleeps "to give the watcher a moment" or "so the coalescer's window passes"
//! and then asserts depends on how loaded the machine is. These helpers wait on events the
//! hub reports: its watcher started, a batch of disk events processed, its listings done.
//! The only clock left is `HANG`, which turns a hang into a failure that names what it was
//! waiting for; it never decides whether a test passes.

use std::cell::Cell;
use std::path::Path;
use std::sync::mpsc::Receiver;
use std::time::Duration;

use super::hub::{HubReport, RootHub};
use super::FileTreeState;

/// How long any one wait may take before it is a hang.
pub const HANG: Duration = Duration::from_secs(60);

pub struct Probe {
    rx: Receiver<HubReport>,
    /// `WatchStarted` reports seen so far, by any wait.
    started: Cell<usize>,
    /// Batches seen so far that said events were lost.
    lost: Cell<usize>,
}

impl Probe {
    /// Call before opening anything: only hubs made after this report here.
    pub fn new(ft: &FileTreeState) -> Probe {
        Probe { rx: ft.probe_hubs(), started: Cell::new(0), lost: Cell::new(0) }
    }

    fn next(&self, what: &str) -> HubReport {
        match self.rx.recv_timeout(HANG) {
            Ok(r) => {
                match &r {
                    HubReport::WatchStarted => self.started.set(self.started.get() + 1),
                    HubReport::Batch { lost: true, .. } => self.lost.set(self.lost.get() + 1),
                    _ => {}
                }
                r
            }
            Err(e) => panic!("the hub reported nothing while waiting for {what} ({e})"),
        }
    }

    /// Batches the hub processed, among those this probe has seen, that said events were lost
    /// (an overflow, or the OS dropping events): each one re-listed what they could touch.
    /// Counts what has arrived since the last wait too.
    pub fn lost(&self) -> usize {
        while let Ok(r) = self.rx.try_recv() {
            match r {
                HubReport::WatchStarted => self.started.set(self.started.get() + 1),
                HubReport::Batch { lost: true, .. } => self.lost.set(self.lost.get() + 1),
                _ => {}
            }
        }
        self.lost.get()
    }

    /// The hub's watcher has started: it is watching, has asked every open folder to list
    /// again, and has sent Health. (Watching only, for a test with one hub.)
    pub fn watching(&self, hub: &RootHub) {
        while self.started.get() == 0 {
            self.next("the watcher to start");
        }
        assert!(!hub.watch_pending());
    }

    /// Every change made on disk before this call has reached the hub and been processed.
    ///
    /// Writes a new empty file `ft-barrier-<uuid>` into `dir` (a folder the hub's watcher
    /// covers) and waits for the batch that carries it. A watcher reports changes in the
    /// order they happened, and the coalescer hands batches over in order, so by the time
    /// that batch is processed every earlier change has been too, or was covered by a
    /// re-list from the root when events were lost. If the barrier's own event was lost (an
    /// overflowing channel), the batch that says so is followed by a fresh barrier.
    /// Returns the names of the barrier files written: they are real files, and a listing
    /// shows them.
    pub fn barrier(&self, dir: &Path) -> Vec<String> {
        let mut written = Vec::new();
        loop {
            let name = format!("ft-barrier-{}", uuid::Uuid::new_v4());
            std::fs::write(dir.join(&name), "").unwrap();
            written.push(name.clone());
            loop {
                if let HubReport::Batch { paths, lost } = self.next("the barrier file's event") {
                    if paths.iter().any(|p| p.file_name().is_some_and(|n| n.to_string_lossy() == name)) {
                        return written;
                    }
                    if lost {
                        break;
                    }
                }
            }
        }
    }

    /// No listing is queued or running (and the watcher has started): everything the hub
    /// was asked to list has been listed and sent.
    pub fn idle(&self, hub: &RootHub) {
        while !hub.test_idle() {
            self.next("the hub's listings to finish");
        }
    }

    /// `barrier`, then `idle`: what the disk did before this call has been listed and sent.
    pub fn settle(&self, hub: &RootHub, dir: &Path) -> Vec<String> {
        let names = self.barrier(dir);
        self.idle(hub);
        names
    }
}
