//! Waiting on a Files hub's own account of what it did (`HubReport`) instead of the clock.
//!
//! A test that sleeps "to give the watcher a moment" or "so the coalescer's window passes"
//! and then asserts depends on how loaded the machine is. These helpers wait on events the
//! hub reports: its watcher started, a batch of disk events processed, its listings done.
//! The only clock left is `HANG`, which turns a hang into a failure that names what it was
//! waiting for; it never decides whether a test passes.
//!
//! What the hub reports about itself (`watching`, `idle`) is exact. What the OS reports is
//! not: macOS FSEvents under disk load delivers changes up to 20 s late, after later ones, or
//! never, with no flag saying so (worker #116, shared memory macos-files-watcher-silent-loss).
//! A test that isn't about the watcher must not wait for an OS event (`barrier`).

use std::cell::{Cell, RefCell};
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{Receiver, RecvTimeoutError};
use std::time::{Duration, Instant};

use super::hub::{HubReport, RootHub};
use super::watch::RawEvent;
use super::FileTreeState;

/// The name every test-delivered event's marker path starts with.
const DELIVER_MARKER: &str = "ft-deliver-";

/// Did a test hand this event over (`Probe::push`)? What a deaf hub keeps.
pub fn delivered(ev: &RawEvent) -> bool {
    ev.paths.iter().any(|p| p.file_name().is_some_and(|n| n.to_string_lossy().starts_with(DELIVER_MARKER)))
}

/// A hook record as Claude sends it, for the agent edges.
pub fn hook(event: serde_json::Value, tab: &str) -> crate::agent_files::HookRecord {
    let name = event["hook_event_name"].as_str().unwrap().to_string();
    let tool = event.get("tool_name").and_then(|v| v.as_str()).unwrap_or("").to_string();
    crate::agent_files::HookRecord {
        phase: crate::agent_files::Phase::of(&name, &event).unwrap(),
        tab_id: tab.into(),
        session_id: "s1".into(),
        runtime: "claude".into(),
        cwd: event.get("cwd").and_then(|v| v.as_str()).map(String::from),
        host: None,
        tool: tool.clone(),
        tool_use_id: event.get("tool_use_id").and_then(|v| v.as_str()).map(String::from),
        agent: crate::claude_code::gate::agent_key(&event),
        extracted: event.get("tool_input").map(|i| crate::agent_files::extract::extract(&tool, i)).unwrap_or(crate::agent_files::extract::Extracted::Nothing),
        existed: None,
    }
}

/// A PreToolUse or PostToolUse event in `cwd`.
pub fn tool_event(phase: &str, tool: &str, input: serde_json::Value, id: &str, cwd: &Path) -> serde_json::Value {
    serde_json::json!({ "hook_event_name": phase, "session_id": "s1", "cwd": cwd.to_string_lossy(), "tool_name": tool, "tool_input": input, "tool_use_id": id, "tool_response": {} })
}

/// A UserPromptSubmit in `cwd`: the agent's turn starts.
pub fn prompt_event(cwd: &Path) -> serde_json::Value {
    serde_json::json!({ "hook_event_name": "UserPromptSubmit", "session_id": "s1", "cwd": cwd.to_string_lossy(), "prompt": "go" })
}

/// How long any one wait may take before it is a hang.
pub const HANG: Duration = Duration::from_secs(60);

pub struct Probe {
    rx: Receiver<HubReport>,
    /// `WatchStarted` reports seen so far, by any wait.
    started: Cell<usize>,
    /// Batches seen so far that said events were lost.
    lost: Cell<usize>,
    /// Reconcile passes reported and not yet taken by `reconciled`: (`upto`, re-listed).
    passes: RefCell<Vec<(u64, Vec<String>)>>,
}

impl Probe {
    /// Call before opening anything: only hubs made after this report here.
    pub fn new(ft: &FileTreeState) -> Probe {
        Probe { rx: ft.probe_hubs(), started: Cell::new(0), lost: Cell::new(0), passes: RefCell::new(Vec::new()) }
    }

    fn next(&self, what: &str) -> HubReport {
        match self.rx.recv_timeout(HANG) {
            Ok(r) => {
                self.note(&r);
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
            self.note(&r);
        }
        self.lost.get()
    }

    fn note(&self, r: &HubReport) {
        match r {
            HubReport::WatchStarted => self.started.set(self.started.get() + 1),
            HubReport::Batch { lost: true, .. } => self.lost.set(self.lost.get() + 1),
            HubReport::Reconciled { relisted, upto } => self.passes.borrow_mut().push((*upto, relisted.clone())),
            _ => {}
        }
    }

    /// Hand the hub `paths` as if its watcher had reported them, without waiting. A marker path
    /// that doesn't exist rides in the same event; it is what a deaf hub (`RootHub::go_deaf`)
    /// lets through, and what `deliver` waits for. Returns the marker.
    pub fn push(&self, hub: &RootHub, paths: &[PathBuf]) -> PathBuf {
        let marker = hub.root.join(format!("{DELIVER_MARKER}{}", uuid::Uuid::new_v4()));
        let mut all = paths.to_vec();
        all.push(marker.clone());
        hub.raw_sender().push(RawEvent { paths: all, rescan: false, content_only: false });
        marker
    }

    /// `push`, then return once the hub has processed it (and everything pushed before it: the
    /// channel and the batches are in order) and listed and sent what it asked for. This is how
    /// a test says "the OS delivered this event" without depending on when, or whether, it does
    /// (§6.1): the real watcher may report the same change too, before or after, which changes
    /// nothing. If a full channel dropped the event, the batch that says events were lost has
    /// re-listed what they could touch, and a fresh one is pushed.
    pub fn deliver(&self, hub: &RootHub, paths: &[PathBuf]) {
        let mut marker = self.push(hub, paths);
        loop {
            if let HubReport::Batch { paths: got, lost } = self.next("a delivered event to be processed") {
                if got.contains(&marker) {
                    break;
                }
                if lost {
                    marker = self.push(hub, paths);
                }
            }
        }
        self.idle(hub);
    }

    /// An edge at `base` (`RootHub::reconcile`), then `reconciled`.
    pub fn reconcile(&self, hub: &RootHub, base: &str) -> BTreeSet<String> {
        let before = hub.reconcile_asked();
        hub.reconcile(base);
        self.reconciled(hub, before)
    }

    /// An edge fired after `hub.reconcile_asked()` returned `before` (a command, focus, an
    /// agent's hook, `reconcile`) has been reconciled: a pass that covers it is done, and so is
    /// everything the hub had to list. The folders that pass and any after it re-listed. An
    /// edge that reaches the hub from another thread (an agent's write goes through the
    /// ledger's dispatcher) is waited for too.
    pub fn reconciled(&self, hub: &RootHub, before: u64) -> BTreeSet<String> {
        while !self.passes.borrow().iter().any(|(upto, _)| *upto > before) {
            self.next("a reconcile pass covering the edge");
        }
        self.idle(hub);
        self.lost();
        let mut passes = self.passes.borrow_mut();
        let out = passes.iter().filter(|(upto, _)| *upto > before).flat_map(|(_, r)| r.iter().cloned()).collect();
        passes.clear();
        out
    }

    /// The hub's watcher has started: it is watching, has asked every open folder to list
    /// again, and has sent Health. (Watching only, for a test with one hub.)
    pub fn watching(&self, hub: &RootHub) {
        while self.started.get() == 0 {
            self.next("the watcher to start");
        }
        assert!(!hub.watch_pending());
    }

    /// The hub has processed the event for a new file written now, `ft-barrier-<uuid>` in
    /// `dir` (a folder the hub's watcher covers). Returns the names of the files written: they
    /// are real files, and a listing shows them. If a batch says events were lost (an
    /// overflowing channel) or if no event arrives within 2 s (macOS FSEvents can silently drop
    /// a single create under parallel disk load without setting `lost`, docs/file-tree.md §6.1),
    /// a fresh probe file is written until any of them is observed or `HANG` expires.
    ///
    /// **Not a barrier.** It says nothing about changes made before it: the OS doesn't report
    /// in order. Use it only where OS event delivery is the test's subject (the watcher reporting
    /// a change at all), never to wait for "everything before" in a test about something else.
    pub fn barrier(&self, dir: &Path) -> Vec<String> {
        const RETRY: Duration = Duration::from_secs(2);
        let deadline = Instant::now() + HANG;
        let mut written = Vec::new();
        loop {
            let name = format!("ft-barrier-{}", uuid::Uuid::new_v4());
            std::fs::write(dir.join(&name), "").unwrap();
            written.push(name);
            loop {
                let remaining = deadline.saturating_duration_since(Instant::now());
                if remaining.is_zero() {
                    panic!(
                        "the hub reported nothing while waiting for the barrier file's event (after {} attempts)",
                        written.len()
                    );
                }
                match self.rx.recv_timeout(RETRY.min(remaining)) {
                    Ok(r) => {
                        self.note(&r);
                        if let HubReport::Batch { paths, lost } = r {
                            if paths.iter().any(|p| {
                                p.file_name()
                                    .and_then(|n| n.to_str())
                                    .is_some_and(|n| written.iter().any(|w| w == n))
                            }) {
                                return written;
                            }
                            if lost {
                                break;
                            }
                        }
                    }
                    Err(RecvTimeoutError::Timeout) => break,
                    Err(RecvTimeoutError::Disconnected) => {
                        panic!("the hub reported nothing while waiting for the barrier file's event (Disconnected)");
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

    /// `barrier`, then `idle`. Its event was processed and whatever it asked to list has been
    /// listed and sent; as `barrier` says, earlier changes may still be on their way.
    pub fn settle(&self, hub: &RootHub, dir: &Path) -> Vec<String> {
        let names = self.barrier(dir);
        self.idle(hub);
        names
    }
}
