//! Turn baselines (docs/file-tree.md §14): what a file held before an agent's first edit of it
//! in a turn, so "Compare with Turn Start" and `showDiff` `@turn` can show the whole turn's
//! change, however many edits it took.
//!
//! Claude's own `file-history` can't supply this (a file first touched in turn T has no
//! snapshot before T+1), so maiTerm reads the file itself, in the PreToolUse hook, before the
//! tool runs: PreToolUse is a blocking hook and the maiterm-tab mod awaits its relay.
//!
//! Rules:
//! - Only the FIRST write-capable call per (tab, session, turn, path) captures; a later edit in
//!   the same turn would read the agent's own work. The first capture per (tab, session, path)
//!   is also the session baseline. A tab keeps as many sessions' baselines as the ledger can
//!   resume a turn for; a session whose baselines were dropped and that comes back says
//!   `too_many_sessions` for what it lost instead of capturing again.
//! - The hook waits at most `DEADLINE` (25 ms). A read still running then is told to stop by a
//!   cancellation flag, and whatever it finds is discarded: by then the tool may have run, and
//!   post-edit bytes passed off as the turn's start would be a false claim. The slot records
//!   `timeout` instead.
//! - A file that doesn't exist yet is `content: None`: a new file, diffed against empty.
//! - Text only, at most 2 MB (the editor's own limits); else `binary` / `too_large`.
//! - At most `BUDGET` (64 MB) held, oldest closed turns' contents evicted first (a record that
//!   holds no bytes is never evicted: it frees nothing). In memory only: never
//!   persisted, never on any wire to the phone.
//! - Local tabs only: a hook must never wait on ssh.

use std::collections::{BTreeMap, HashMap, VecDeque};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use parking_lot::Mutex;
use serde::Serialize;

use super::extract::{is_write_op, Extracted};
use crate::filetree::paths;

/// The longest a hook waits for a baseline.
pub const DEADLINE: Duration = Duration::from_millis(25);
/// Larger files are not captured (the editor's and `read_file`'s limit).
pub const MAX_FILE: u64 = 2 * 1024 * 1024;
/// Total content held across every tab.
pub const BUDGET: usize = 64 * 1024 * 1024;
/// `read_file`'s binary sniff: a NUL in the first 8 KB.
const SNIFF: usize = 8192;
const CHUNK: usize = 64 * 1024;

/// What a read found.
#[derive(Debug, Clone, PartialEq)]
pub enum ReadOutcome {
    Text(String),
    /// Not there: a new file.
    Missing,
    Binary,
    TooLarge,
    /// A folder, no permission, an IO error.
    Unreadable,
    /// The hook gave up waiting; nothing read counts.
    Cancelled,
}

/// Reads one file for a baseline. Checks `cancel` between chunks and stops when it is set.
/// Injectable so the deadline can be tested with a reader that is slow on purpose.
pub type Reader = Arc<dyn Fn(&Path, &AtomicBool) -> ReadOutcome + Send + Sync>;

/// The file's text, refusing what the editor refuses: over 2 MB, a NUL in the first 8 KB, or
/// not UTF-8. Reads in chunks so a cancelled read stops early.
pub fn read_capped(path: &Path, cancel: &AtomicBool) -> ReadOutcome {
    let meta = match std::fs::metadata(path) {
        Ok(m) => m,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return ReadOutcome::Missing,
        Err(_) => return ReadOutcome::Unreadable,
    };
    if !meta.is_file() {
        return ReadOutcome::Unreadable;
    }
    if meta.len() > MAX_FILE {
        return ReadOutcome::TooLarge;
    }
    let mut file = match std::fs::File::open(path) {
        Ok(f) => f,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return ReadOutcome::Missing,
        Err(_) => return ReadOutcome::Unreadable,
    };
    let mut buf: Vec<u8> = Vec::with_capacity(meta.len() as usize);
    let mut chunk = vec![0u8; CHUNK];
    loop {
        if cancel.load(Ordering::SeqCst) {
            return ReadOutcome::Cancelled;
        }
        let n = match file.read(&mut chunk) {
            Ok(0) => break,
            Ok(n) => n,
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(_) => return ReadOutcome::Unreadable,
        };
        buf.extend_from_slice(&chunk[..n]);
        // The file may have grown since the stat.
        if buf.len() as u64 > MAX_FILE {
            return ReadOutcome::TooLarge;
        }
    }
    if buf[..buf.len().min(SNIFF)].contains(&0) {
        return ReadOutcome::Binary;
    }
    match String::from_utf8(buf) {
        Ok(s) => ReadOutcome::Text(s),
        Err(_) => ReadOutcome::Binary,
    }
}

/// The local paths a PreToolUse would change, as the ledger keys them: Edit, MultiEdit, Write,
/// NotebookEdit and a Codex patch's files. Reads, searches and shell calls name none (a shell
/// call's writes have no Pre path, §14).
pub fn write_paths(extracted: &Extracted, cwd: Option<&str>) -> Vec<String> {
    let Extracted::Files(touches) = extracted else { return Vec::new() };
    let mut out: Vec<String> = Vec::new();
    for t in touches.iter().filter(|t| is_write_op(t.op)) {
        if let Some(p) = super::entry_path(&t.path, cwd, None) {
            if !out.contains(&p) {
                out.push(p);
            }
        }
    }
    out
}

/// A baseline as `agent_files_baseline` serves it.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct Baseline {
    /// The file's text at the start; None with no `unavailable` is a new file.
    pub content: Option<String>,
    /// Why there is no content: "timeout" | "binary" | "too_large" | "unreadable" | "evicted"
    /// (dropped for the memory budget) | "too_many_sessions" (dropped with its session's other
    /// baselines, `SESSIONS_GONE`) | "not_captured" (no write-capable call of this tab touched the file in that turn or
    /// session, or it began before maiTerm started) | "pending" (being read right now).
    pub unavailable: Option<String>,
    /// The turn it was captured in.
    pub turn: u64,
    /// "maiterm": captured by maiTerm at PreToolUse (Claude's file-history is not a source).
    pub source: String,
    /// The tab's agent runtime ("claude", "codex", …), for the diff tab's title.
    pub runtime: String,
}

/// Lines added and removed since a baseline (the Turn lens tooltip's `+N −N`).
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct BaselineStat {
    pub added: u32,
    pub removed: u32,
    /// Set instead of counts when there is nothing to diff (as `Baseline.unavailable`, plus
    /// "current_unreadable" when the file on disk can't be read as text now).
    pub unavailable: Option<String>,
    pub turn: u64,
}

#[derive(Debug, Clone)]
enum State {
    Pending,
    Done { content: Option<Arc<String>>, unavailable: Option<&'static str> },
}

#[derive(Debug)]
struct Slot {
    tab: String,
    session: String,
    turn: u64,
    state: State,
    bytes: usize,
}

/// One session's baselines in a tab. Each session numbers its turns from 1, so turns are only
/// ever matched within their own session.
#[derive(Default)]
struct SessionState {
    session: String,
    /// The newest turn captured: anything older is a closed turn, evicted first.
    turn: u64,
    by_turn: HashMap<(u64, String), u64>,
    by_session: HashMap<String, u64>,
    /// Set when this session's baselines were dropped and it came back, to the last turn that may
    /// have begun before it came back (`Dropped::fresh_after`). What it captured before is gone,
    /// so a capture in such a turn could read its own earlier edits: its session baselines are
    /// `too_many_sessions` for good, and so are its turns up to this one. A later turn captures again, since
    /// a turn starts from whatever the file holds at its first write.
    lost: Option<u64>,
}

/// Sessions whose baselines a tab keeps: as many as the ledger can resume a turn for (the one
/// it is in, and `LEFT_SESSIONS` it switched away from). A headless `claude -p` the agent runs
/// from its Bash inherits the tab's id and posts its own session's events, and the parent's turn
/// goes on around it, however many children it runs: its baselines must outlive them.
const SESSIONS: usize = super::LEFT_SESSIONS + 1;
/// Ids of sessions whose baselines were dropped, remembered per tab so one that comes back is
/// known to have lost them (`SessionState::lost`) rather than taken for a new session, which
/// would recapture files after its own edits. Only sessions that captured land here.
const DROPPED: usize = 256;
/// Why a baseline is gone when its session's were dropped (more sessions than `SESSIONS`
/// captured, or the ledger forgot the session's turn), not the memory budget's "evicted": the
/// diff tab must not blame memory for what other sessions in the tab did.
const SESSIONS_GONE: &str = "too_many_sessions";

/// A session whose baselines were dropped.
struct Dropped {
    session: String,
    /// Turns after this one began after the session came back (the ledger saw a prompt open
    /// the next): they start clean. None until that prompt, when any turn may be one it was
    /// already in.
    fresh_after: Option<u64>,
}

impl Dropped {
    /// Whether turn `turn` (any turn, for the session's start) may have begun before the
    /// session lost its baselines.
    fn covers(&self, scope: &str, turn: u64) -> bool {
        scope == "session" || self.fresh_after.is_none_or(|t| turn <= t)
    }
}

#[derive(Default)]
struct TabState {
    runtime: String,
    /// Least recently captured first; the last is the session capturing now.
    sessions: Vec<SessionState>,
    /// Sessions whose baselines were dropped, oldest first.
    dropped: VecDeque<Dropped>,
}

impl TabState {
    /// `session` lost its baselines (again, maybe: whatever it had come back to is lost too).
    fn tombstone(&mut self, session: String) {
        self.dropped.retain(|d| d.session != session);
        self.dropped.push_back(Dropped { session, fresh_after: None });
        if self.dropped.len() > DROPPED {
            self.dropped.pop_front();
        }
    }

    fn dropped(&self, session: &str) -> Option<&Dropped> {
        self.dropped.iter().find(|d| d.session == session)
    }
}

impl TabState {
    /// `session`'s baselines; an empty id (an event that named none) means the current one.
    fn session(&self, session: &str) -> Option<&SessionState> {
        if session.is_empty() {
            self.sessions.last()
        } else {
            self.sessions.iter().find(|s| s.session == session)
        }
    }

    fn session_mut(&mut self, session: &str) -> Option<&mut SessionState> {
        if session.is_empty() {
            self.sessions.last_mut()
        } else {
            self.sessions.iter_mut().find(|s| s.session == session)
        }
    }
}

#[derive(Default)]
struct Inner {
    tabs: HashMap<String, TabState>,
    /// By capture order, oldest first.
    slots: BTreeMap<u64, Slot>,
    total: usize,
    next: u64,
}

impl Inner {
    fn drop_slots(&mut self, gone: impl Fn(&Slot) -> bool) {
        let keys: Vec<u64> = self.slots.iter().filter(|(_, s)| gone(s)).map(|(k, _)| *k).collect();
        for k in keys {
            if let Some(s) = self.slots.remove(&k) {
                self.total -= s.bytes;
            }
        }
    }

    fn drop_tab(&mut self, tab: &str) {
        if self.tabs.remove(tab).is_some() {
            self.drop_slots(|s| s.tab == tab);
        }
    }

    /// A slot no current turn can ask for: an older turn, or a session that is not capturing.
    fn closed(&self, s: &Slot) -> bool {
        match self.tabs.get(&s.tab).and_then(|t| t.sessions.last()) {
            Some(cur) if cur.session == s.session => s.turn < cur.turn,
            _ => true,
        }
    }

    /// Back under the budget: closed turns' baselines first, oldest first; then, only if those
    /// are not enough, the oldest of the open turns'. A lookup of an evicted one says so. Only
    /// slots that hold content are taken: a new file's or a refusal's record costs nothing, and
    /// dropping it would free nothing and lose a baseline that is still right.
    fn evict(&mut self, budget: usize) {
        let held = |s: &Slot| matches!(s.state, State::Done { .. }) && s.bytes > 0;
        while self.total > budget {
            let closed = self.slots.iter().find(|(_, s)| held(s) && self.closed(s)).map(|(k, _)| *k);
            let victim = closed.or_else(|| self.slots.iter().find(|(_, s)| held(s)).map(|(k, _)| *k));
            let Some(k) = victim else { return };
            if let Some(s) = self.slots.remove(&k) {
                self.total -= s.bytes;
            }
        }
    }
}

/// Every tab's baselines.
pub struct Baselines {
    inner: Mutex<Inner>,
    reader: Reader,
    deadline: Duration,
    budget: usize,
}

impl Baselines {
    pub fn new() -> Arc<Self> {
        Self::with(Arc::new(read_capped), DEADLINE, BUDGET)
    }

    /// A store with another reader, deadline or budget (tests).
    pub fn with(reader: Reader, deadline: Duration, budget: usize) -> Arc<Self> {
        Arc::new(Baselines { inner: Mutex::new(Inner::default()), reader, deadline, budget })
    }

    /// Capture `paths` for `tab`'s turn `turn`, those not captured yet this turn. Waits at most
    /// the deadline. Returns, per path read now, whether the file existed (None: unknown, the
    /// read timed out or failed), so a Write's create-or-overwrite needs no second look. A path
    /// recorded `too_many_sessions` without a read (a session that lost its baselines) is listed as unknown,
    /// so the hook still moves it with `rekey`.
    pub async fn capture(self: &Arc<Self>, tab: &str, session: &str, runtime: &str, turn: u64, paths: Vec<String>) -> Vec<(String, Option<bool>)> {
        let (reserved, unread) = self.reserve(tab, session, runtime, turn, &paths);
        let unread = unread.into_iter().map(|p| (p, None));
        if reserved.is_empty() {
            return unread.collect();
        }
        let cancel = Arc::new(AtomicBool::new(false));
        let (me, flag, list) = (self.clone(), cancel.clone(), reserved.clone());
        let task = tokio::task::spawn_blocking(move || {
            for (seq, path) in list {
                if flag.load(Ordering::SeqCst) {
                    return;
                }
                let out = (me.reader)(Path::new(&path), &flag);
                me.fill(seq, out, &flag);
            }
        });
        if tokio::time::timeout(self.deadline, task).await.is_err() {
            self.time_out(&reserved, &cancel);
        }
        let inner = self.inner.lock();
        reserved
            .into_iter()
            .map(|(seq, path)| {
                let existed = match inner.slots.get(&seq).map(|s| &s.state) {
                    Some(State::Done { content: None, unavailable: None }) => Some(false),
                    Some(State::Done { unavailable: Some("timeout" | "unreadable"), .. }) | Some(State::Pending) | None => None,
                    Some(State::Done { .. }) => Some(true),
                };
                (path, existed)
            })
            .chain(unread)
            .collect()
    }

    /// Claim the slots this call captures, under the lock, so a second copy of the same Pre
    /// (the mod's and the settings hook's) or a parallel call never reads the file again.
    /// Answers the slots to read, and the paths recorded `too_many_sessions` with no read.
    fn reserve(&self, tab: &str, session: &str, runtime: &str, turn: u64, paths: &[String]) -> (Vec<(u64, String)>, Vec<String>) {
        let mut guard = self.inner.lock();
        let inner = &mut *guard;
        let st = inner.tabs.entry(tab.to_string()).or_default();
        if !runtime.is_empty() {
            st.runtime = runtime.to_string();
        }
        // This session becomes the current one. A new session (resume, /clear) numbers its
        // turns afresh, so it gets its own baselines; the oldest beyond `SESSIONS` go.
        let at = if session.is_empty() && !st.sessions.is_empty() { Some(st.sessions.len() - 1) } else { st.sessions.iter().position(|s| s.session == session) };
        let ss = match at {
            Some(i) => st.sessions.remove(i),
            None => {
                // One whose baselines were dropped is not new: a turn it was already in may have
                // edited these files, so what they hold now is not where that turn started. With
                // no prompt seen since it came back, this turn may be one of those.
                let lost = st.dropped.iter().position(|d| d.session == session).and_then(|i| st.dropped.remove(i)).map(|d| d.fresh_after.unwrap_or(turn));
                SessionState { session: session.to_string(), lost, ..Default::default() }
            }
        };
        st.sessions.push(ss);
        let mut gone: Vec<String> = Vec::new();
        while st.sessions.len() > SESSIONS {
            let s = st.sessions.remove(0).session;
            st.tombstone(s.clone());
            gone.push(s);
        }
        let Some(ss) = st.sessions.last_mut() else { return (Vec::new(), Vec::new()) };
        let sid = ss.session.clone();
        ss.turn = ss.turn.max(turn);
        let blind = ss.lost.is_some_and(|t| turn <= t);
        let (mut out, mut unread) = (Vec::new(), Vec::new());
        for p in paths {
            let key = (turn, p.clone());
            if ss.by_turn.contains_key(&key) {
                continue;
            }
            let seq = inner.next;
            inner.next += 1;
            ss.by_turn.insert(key, seq);
            ss.by_session.entry(p.clone()).or_insert(seq);
            // Recorded, not read: a later call this turn must not read the file either.
            let state = if blind { State::Done { content: None, unavailable: Some(SESSIONS_GONE) } } else { State::Pending };
            inner.slots.insert(seq, Slot { tab: tab.to_string(), session: sid.clone(), turn, state, bytes: 0 });
            if blind {
                unread.push(p.clone());
            } else {
                out.push((seq, p.clone()));
            }
        }
        if !gone.is_empty() {
            inner.drop_slots(|s| s.tab == tab && gone.contains(&s.session));
        }
        (out, unread)
    }

    /// The ledger forgot `session`'s turn (it fell out of `TabLedger.left`): if it comes back, the
    /// ledger numbers its turns afresh, and the baselines filed under its old numbers would be
    /// served for the new ones. They go, and the session is remembered as having lost them.
    pub fn lose_session(&self, tab: &str, session: &str) {
        if session.is_empty() {
            return;
        }
        let mut guard = self.inner.lock();
        let inner = &mut *guard;
        let Some(st) = inner.tabs.get_mut(tab) else { return };
        match st.sessions.iter().position(|s| s.session == session) {
            Some(i) => {
                st.sessions.remove(i);
            }
            // Lost before and not back to capturing since: the turns it came back to are
            // renumbered too, so what `came_back` said of them no longer holds.
            None if st.dropped(session).is_some() => {}
            // Never captured: it has nothing to lose, and is new when it comes.
            None => return,
        }
        st.tombstone(session.to_string());
        inner.drop_slots(|s| s.tab == tab && s.session == session);
    }

    /// The ledger saw `session`'s first prompt since the tab switched back to it, opening turn
    /// `after + 1`: turns after `after` began after it came back, so if it lost its baselines
    /// they start clean (a capture reads the file as the turn found it). Only the first return
    /// counts until it is lost again; a session that hasn't lost anything is left alone.
    pub fn came_back(&self, tab: &str, session: &str, after: u64) {
        let mut inner = self.inner.lock();
        let Some(st) = inner.tabs.get_mut(tab) else { return };
        if st.session(session).is_some() {
            return;
        }
        if let Some(d) = st.dropped.iter_mut().find(|d| d.session == session) {
            d.fresh_after.get_or_insert(after);
        }
    }

    /// The ledger recorded the call in turn `to`, not the `from` its baselines were reserved
    /// under: a prompt landed while the file was being read (a background subagent's call racing
    /// the human's next prompt). The read still came before the tool ran, so it is the file as
    /// the call found it, and belongs to the turn the ledger shows the edit in. `paths` are the
    /// ones this call reserved; one the new turn already has (an earlier call's) stays put.
    pub fn rekey(&self, tab: &str, session: &str, from: u64, to: u64, paths: &[String]) {
        if from == to {
            return;
        }
        let mut guard = self.inner.lock();
        let inner = &mut *guard;
        let Some(ss) = inner.tabs.get_mut(tab).and_then(|t| t.session_mut(session)) else { return };
        for p in paths {
            if ss.by_turn.contains_key(&(to, p.clone())) {
                continue;
            }
            let Some(seq) = ss.by_turn.remove(&(from, p.clone())) else { continue };
            ss.by_turn.insert((to, p.clone()), seq);
            if let Some(slot) = inner.slots.get_mut(&seq) {
                slot.turn = to;
            }
        }
        ss.turn = ss.turn.max(to);
    }

    /// A read finished. Dropped if the hook stopped waiting first: the flag is set under this
    /// same lock, so a result can never land after its slot was marked `timeout`.
    fn fill(&self, seq: u64, out: ReadOutcome, cancel: &AtomicBool) {
        let mut inner = self.inner.lock();
        if cancel.load(Ordering::SeqCst) {
            return;
        }
        let (content, unavailable) = match out {
            ReadOutcome::Text(s) => (Some(Arc::new(s)), None),
            ReadOutcome::Missing => (None, None),
            ReadOutcome::Binary => (None, Some("binary")),
            ReadOutcome::TooLarge => (None, Some("too_large")),
            ReadOutcome::Unreadable => (None, Some("unreadable")),
            ReadOutcome::Cancelled => return,
        };
        let bytes = content.as_ref().map(|c| c.len()).unwrap_or(0);
        let Some(slot) = inner.slots.get_mut(&seq) else { return };
        if !matches!(slot.state, State::Pending) {
            return;
        }
        slot.state = State::Done { content, unavailable };
        slot.bytes = bytes;
        inner.total += bytes;
        let budget = self.budget;
        inner.evict(budget);
    }

    /// The deadline passed: stop the read and record `timeout` for whatever it hadn't done.
    fn time_out(&self, reserved: &[(u64, String)], cancel: &AtomicBool) {
        let mut inner = self.inner.lock();
        cancel.store(true, Ordering::SeqCst);
        for (seq, _) in reserved {
            if let Some(slot) = inner.slots.get_mut(seq) {
                if matches!(slot.state, State::Pending) {
                    slot.state = State::Done { content: None, unavailable: Some("timeout") };
                }
            }
        }
    }

    /// `tab`'s baseline of `path`: "turn" for its turn `turn`, "session" for the session's.
    /// `session` is the ledger's current session for the tab; baselines of another are never
    /// served. May touch the disk (a path spelled another way is matched canonicalized), so
    /// never call it in a hook.
    pub fn lookup(&self, tab: &str, session: &str, path: &str, scope: &str, turn: u64) -> Baseline {
        let none = |why: &str, runtime: String| Baseline { content: None, unavailable: Some(why.into()), turn, source: "maiterm".into(), runtime };
        let want = paths::norm(path).into_owned();
        // The candidates, under the lock; matched outside it.
        let (direct, candidates, runtime) = {
            let inner = self.inner.lock();
            let Some(tab) = inner.tabs.get(tab) else { return none("not_captured", String::new()) };
            let runtime = tab.runtime.clone();
            let Some(ss) = tab.session(session) else {
                let why = if tab.dropped(session).is_some_and(|d| d.covers(scope, turn)) { SESSIONS_GONE } else { "not_captured" };
                return none(why, runtime);
            };
            // A session that lost its baselines can't say where it started, nor where a turn it
            // was already in started, for any file: an empty answer there would read as "never
            // touched".
            if ss.lost.is_some_and(|t| scope == "session" || turn <= t) {
                return none(SESSIONS_GONE, runtime);
            }
            if scope == "session" {
                (ss.by_session.get(&want).copied(), ss.by_session.iter().map(|(p, s)| (p.clone(), *s)).collect::<Vec<_>>(), runtime)
            } else {
                (ss.by_turn.get(&(turn, want.clone())).copied(), ss.by_turn.iter().filter(|((t, _), _)| *t == turn).map(|((_, p), s)| (p.clone(), *s)).collect::<Vec<_>>(), runtime)
            }
        };
        let seq = direct.or_else(|| {
            // `/tmp` against `/private/tmp`, a symlinked folder, another case on a volume that
            // folds it: the tree names files by its canonical root, the agent as it typed them.
            let w = paths::canon_existing(Path::new(&want));
            let fold = w.parent().is_some_and(crate::relocate::case_insensitive);
            candidates.into_iter().find(|(p, _)| paths::strip_root(&paths::canon_existing(Path::new(p)), &w, fold).is_some_and(|rest| rest.is_empty())).map(|(_, s)| s)
        });
        let Some(seq) = seq else { return none("not_captured", runtime) };
        let inner = self.inner.lock();
        match inner.slots.get(&seq) {
            None => none("evicted", runtime),
            Some(Slot { state: State::Pending, turn, .. }) => Baseline { content: None, unavailable: Some("pending".into()), turn: *turn, source: "maiterm".into(), runtime },
            Some(Slot { state: State::Done { content, unavailable }, turn, .. }) => Baseline {
                content: content.as_ref().map(|c| c.as_str().to_string()),
                unavailable: unavailable.map(String::from),
                turn: *turn,
                source: "maiterm".into(),
                runtime,
            },
        }
    }

    /// Lines added and removed in `path` since its baseline. Reads the file now; off the hook.
    pub fn stat(&self, tab: &str, session: &str, path: &str, scope: &str, turn: u64) -> BaselineStat {
        let b = self.lookup(tab, session, path, scope, turn);
        let unavailable = |why: String| BaselineStat { added: 0, removed: 0, unavailable: Some(why), turn: b.turn };
        if let Some(why) = b.unavailable.clone() {
            return unavailable(why);
        }
        let now = match read_capped(&PathBuf::from(path), &AtomicBool::new(false)) {
            ReadOutcome::Text(s) => s,
            ReadOutcome::Missing => String::new(),
            _ => return unavailable("current_unreadable".into()),
        };
        let (added, removed) = line_counts(b.content.as_deref().unwrap_or(""), &now);
        BaselineStat { added, removed, unavailable: None, turn: b.turn }
    }

    /// A reload gave the tab a new id.
    pub fn remap_tab(&self, old: &str, new: &str) {
        let mut inner = self.inner.lock();
        if let Some(st) = inner.tabs.remove(old) {
            inner.drop_tab(new);
            inner.tabs.insert(new.to_string(), st);
            for s in inner.slots.values_mut().filter(|s| s.tab == old) {
                s.tab = new.to_string();
            }
        }
    }

    /// Tabs the ledger forgot (closed for a minute).
    pub fn forget(&self, tabs: &[String]) {
        let mut inner = self.inner.lock();
        for t in tabs {
            inner.drop_tab(t);
        }
    }

    /// `file_tree_turn_baselines` was turned off: nothing is kept.
    pub fn clear(&self) {
        *self.inner.lock() = Inner::default();
    }

    #[cfg(test)]
    pub fn held_bytes(&self) -> usize {
        self.inner.lock().total
    }
}

/// How long the Turn lens's `+N −N` may spend diffing. Myers is quadratic when most lines
/// changed: a rewritten 2 MB file took ~10 s, on every hover, with nothing to cancel it.
const COUNT_DEADLINE: Duration = Duration::from_millis(200);

/// (added, removed) lines from `old` to `new`. Past `COUNT_DEADLINE` the diff stops looking for
/// the shortest edit and counts what is left as replaced, so a heavy rewrite may count a few
/// unchanged lines too; for a file where nearly every line changed that is what happened anyway.
pub fn line_counts(old: &str, new: &str) -> (u32, u32) {
    line_counts_within(old, new, COUNT_DEADLINE)
}

fn line_counts_within(old: &str, new: &str, limit: Duration) -> (u32, u32) {
    use similar::{ChangeTag, TextDiff};
    let diff = TextDiff::configure().timeout(limit).diff_lines(old, new);
    let (mut added, mut removed) = (0u32, 0u32);
    for c in diff.iter_all_changes() {
        match c.tag() {
            ChangeTag::Insert => added += 1,
            ChangeTag::Delete => removed += 1,
            ChangeTag::Equal => {}
        }
    }
    (added, removed)
}

#[cfg(test)]
mod tests {
    //! docs/file-tree.md §20 phase 5: first-write-only capture, new files, the 25 ms deadline
    //! (a slow reader injected), the 2 MB / binary refusals, eviction order, sessions.
    use super::*;
    use std::time::Instant;

    fn dir(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("af-base-{tag}-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn p(d: &Path, name: &str) -> String {
        d.join(name).to_string_lossy().into_owned()
    }

    fn content(b: &Baseline) -> Option<&str> {
        b.content.as_deref()
    }

    #[tokio::test]
    async fn only_the_first_write_in_a_turn_captures() {
        let d = dir("first");
        let f = p(&d, "a.ts");
        std::fs::write(&f, "v1\n").unwrap();
        let b = Baselines::new();
        let got = b.capture("t1", "s1", "claude", 1, vec![f.clone()]).await;
        assert_eq!(got, vec![(f.clone(), Some(true))]);
        // The agent's first edit lands; its second edit in the same turn captures nothing.
        std::fs::write(&f, "v2\n").unwrap();
        assert!(b.capture("t1", "s1", "claude", 1, vec![f.clone()]).await.is_empty());
        assert_eq!(content(&b.lookup("t1", "s1", &f, "turn", 1)), Some("v1\n"));
        // The next turn starts from what the last one left; the session still from the first.
        b.capture("t1", "s1", "claude", 2, vec![f.clone()]).await;
        assert_eq!(content(&b.lookup("t1", "s1", &f, "turn", 2)), Some("v2\n"));
        let s = b.lookup("t1", "s1", &f, "session", 2);
        assert_eq!((content(&s), s.turn, s.runtime.as_str()), (Some("v1\n"), 1, "claude"));
        // Another tab never sees these.
        assert_eq!(b.lookup("t2", "s1", &f, "turn", 1).unavailable.as_deref(), Some("not_captured"));
        let _ = std::fs::remove_dir_all(&d);
    }

    #[tokio::test]
    async fn a_new_file_is_none_and_said_not_to_exist() {
        let d = dir("new");
        let f = p(&d, "new.ts");
        let b = Baselines::new();
        assert_eq!(b.capture("t1", "s1", "claude", 1, vec![f.clone()]).await, vec![(f.clone(), Some(false))]);
        let got = b.lookup("t1", "s1", &f, "turn", 1);
        assert_eq!((got.content, got.unavailable), (None, None));
        // Diffed against empty once it exists.
        std::fs::write(&f, "a\nb\n").unwrap();
        let st = b.stat("t1", "s1", &f, "turn", 1);
        assert_eq!((st.added, st.removed, st.unavailable), (2, 0, None));
        let _ = std::fs::remove_dir_all(&d);
    }

    /// The hook must answer within the deadline however slow the disk, and what a read that
    /// outlived it found (maybe the post-edit bytes) must never become the baseline.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn the_deadline_holds_and_a_late_read_is_discarded() {
        let saw_cancel = Arc::new(AtomicBool::new(false));
        let saw = saw_cancel.clone();
        let slow: Reader = Arc::new(move |_p: &Path, cancel: &AtomicBool| {
            std::thread::sleep(Duration::from_millis(200));
            saw.store(cancel.load(Ordering::SeqCst), Ordering::SeqCst);
            // Ignores the flag on purpose: the store must drop the result itself.
            ReadOutcome::Text("post-edit".into())
        });
        let b = Baselines::with(slow, DEADLINE, BUDGET);
        let started = Instant::now();
        let got = b.capture("t1", "s1", "claude", 1, vec!["/x/slow.ts".into()]).await;
        let waited = started.elapsed();
        assert!(waited < Duration::from_millis(100), "the hook waited {waited:?}");
        assert_eq!(got, vec![("/x/slow.ts".to_string(), None)]);
        assert_eq!(b.lookup("t1", "s1", "/x/slow.ts", "turn", 1).unavailable.as_deref(), Some("timeout"));
        // Let the read finish: its result is thrown away, and it was told to stop.
        tokio::time::sleep(Duration::from_millis(350)).await;
        let after = b.lookup("t1", "s1", "/x/slow.ts", "turn", 1);
        assert_eq!((after.content, after.unavailable.as_deref()), (None, Some("timeout")));
        assert!(saw_cancel.load(Ordering::SeqCst), "the late read was not cancelled");
        assert_eq!(b.held_bytes(), 0);
        // A retry in the same turn would read post-edit bytes, so it doesn't happen.
        assert!(b.capture("t1", "s1", "claude", 1, vec!["/x/slow.ts".into()]).await.is_empty());
    }

    #[tokio::test]
    async fn binary_and_large_files_are_refused() {
        let d = dir("refuse");
        let big = p(&d, "big.txt");
        std::fs::write(&big, vec![b'a'; MAX_FILE as usize + 1]).unwrap();
        let nul = p(&d, "nul.bin");
        std::fs::write(&nul, b"abc\0def").unwrap();
        let latin = p(&d, "latin1.txt");
        std::fs::write(&latin, [0x63, 0x61, 0x66, 0xe9]).unwrap();
        let ok = p(&d, "exactly.txt");
        std::fs::write(&ok, vec![b'a'; MAX_FILE as usize]).unwrap();
        let b = Baselines::new();
        b.capture("t1", "s1", "claude", 1, vec![big.clone(), nul.clone(), latin.clone(), ok.clone()]).await;
        let why = |f: &str| b.lookup("t1", "s1", f, "turn", 1).unavailable;
        assert_eq!(why(&big).as_deref(), Some("too_large"));
        assert_eq!(why(&nul).as_deref(), Some("binary"));
        assert_eq!(why(&latin).as_deref(), Some("binary"));
        assert_eq!(why(&ok), None);
        assert_eq!(b.stat("t1", "s1", &nul, "turn", 1).unavailable.as_deref(), Some("binary"));
        let _ = std::fs::remove_dir_all(&d);
    }

    #[tokio::test]
    async fn eviction_takes_closed_turns_first_oldest_first() {
        let d = dir("lru");
        let files: Vec<String> = (0..6).map(|i| p(&d, &format!("f{i}.txt"))).collect();
        for f in &files {
            std::fs::write(f, "x".repeat(40)).unwrap();
        }
        let b = Baselines::with(Arc::new(read_capped), DEADLINE, 100);
        let state = |f: &str, turn: u64| b.lookup("t1", "s1", f, "turn", turn).unavailable;
        b.capture("t1", "s1", "claude", 1, vec![files[0].clone(), files[1].clone()]).await;
        // A turn-2 capture closes turn 1 and goes over the budget: turn 1's oldest goes.
        b.capture("t1", "s1", "claude", 2, vec![files[2].clone()]).await;
        assert_eq!(state(&files[0], 1).as_deref(), Some("evicted"));
        assert_eq!(state(&files[1], 1), None);
        b.capture("t1", "s1", "claude", 2, vec![files[3].clone()]).await;
        assert_eq!(state(&files[1], 1).as_deref(), Some("evicted"));
        assert_eq!((state(&files[2], 2), state(&files[3], 2)), (None, None));
        // No closed turn left: the open turn's oldest goes.
        b.capture("t1", "s1", "claude", 2, vec![files[4].clone()]).await;
        assert_eq!(state(&files[2], 2).as_deref(), Some("evicted"));
        assert_eq!((state(&files[3], 2), state(&files[4], 2)), (None, None));
        assert!(b.held_bytes() <= 100);
        // The session baseline of an evicted capture says so rather than serving a later one.
        assert_eq!(b.lookup("t1", "s1", &files[0], "session", 2).unavailable.as_deref(), Some("evicted"));
        let _ = std::fs::remove_dir_all(&d);
    }

    /// A new file's baseline (and a refusal's record) holds no bytes: evicting it frees nothing,
    /// so it outlives the contents evicted around it.
    #[tokio::test]
    async fn eviction_never_takes_a_record_that_holds_nothing() {
        let d = dir("zero");
        let new = p(&d, "new.ts");
        let nul = p(&d, "nul.bin");
        std::fs::write(&nul, b"a\0b").unwrap();
        let files: Vec<String> = (0..3).map(|i| p(&d, &format!("f{i}.txt"))).collect();
        for f in &files {
            std::fs::write(f, "x".repeat(40)).unwrap();
        }
        let b = Baselines::with(Arc::new(read_capped), DEADLINE, 100);
        b.capture("t1", "s1", "claude", 1, vec![new.clone(), nul.clone(), files[0].clone(), files[1].clone()]).await;
        b.capture("t1", "s1", "claude", 2, vec![files[2].clone()]).await;
        // Turn 1's oldest content went; the two older empty records did not.
        assert_eq!(b.lookup("t1", "s1", &files[0], "turn", 1).unavailable.as_deref(), Some("evicted"));
        let n = b.lookup("t1", "s1", &new, "session", 2);
        assert_eq!((n.content, n.unavailable), (None, None));
        assert_eq!(b.lookup("t1", "s1", &nul, "turn", 1).unavailable.as_deref(), Some("binary"));
        assert_eq!(b.held_bytes(), 80);
        let _ = std::fs::remove_dir_all(&d);
    }

    /// A prompt landed while the Pre was reading: the ledger put the call in the next turn, so its
    /// baseline moves there, unless that turn already has one from an earlier call.
    #[tokio::test]
    async fn rekey_follows_the_turn_the_ledger_recorded() {
        let d = dir("rekey");
        let (a, c) = (p(&d, "a.ts"), p(&d, "c.ts"));
        std::fs::write(&a, "a1\n").unwrap();
        std::fs::write(&c, "c1\n").unwrap();
        let b = Baselines::new();
        b.capture("t1", "s1", "claude", 3, vec![c.clone()]).await;
        b.capture("t1", "s1", "claude", 2, vec![a.clone(), c.clone()]).await;
        b.rekey("t1", "s1", 2, 3, &[a.clone(), c.clone()]);
        assert_eq!(content(&b.lookup("t1", "s1", &a, "turn", 3)), Some("a1\n"));
        assert_eq!(b.lookup("t1", "s1", &a, "turn", 2).unavailable.as_deref(), Some("not_captured"));
        assert_eq!(b.lookup("t1", "s1", &a, "turn", 3).turn, 3);
        // c.ts's turn 3 was already captured: it stays, and so does the turn-2 one.
        assert_eq!(content(&b.lookup("t1", "s1", &c, "turn", 2)), Some("c1\n"));
        std::fs::write(&a, "a2\n").unwrap();
        assert!(b.capture("t1", "s1", "claude", 3, vec![a.clone()]).await.is_empty());
        let _ = std::fs::remove_dir_all(&d);
    }

    #[tokio::test]
    async fn sessions_keep_their_own_turns_and_remap_and_forget_follow_the_tab() {
        let d = dir("session");
        let f = p(&d, "a.ts");
        std::fs::write(&f, "old\n").unwrap();
        let b = Baselines::new();
        b.capture("t1", "s1", "claude", 1, vec![f.clone()]).await;
        // A lookup in the ledger's new session never serves the old session's turn 1.
        assert_eq!(b.lookup("t1", "s2", &f, "turn", 1).unavailable.as_deref(), Some("not_captured"));
        std::fs::write(&f, "new\n").unwrap();
        b.capture("t1", "s2", "claude", 1, vec![f.clone()]).await;
        assert_eq!(content(&b.lookup("t1", "s2", &f, "session", 1)), Some("new\n"));
        // The other session's baselines are its own still: a headless child's session capturing
        // in the tab doesn't make the parent's next edit recapture its file.
        assert_eq!(content(&b.lookup("t1", "s1", &f, "turn", 1)), Some("old\n"));
        assert!(b.capture("t1", "s1", "claude", 1, vec![f.clone()]).await.is_empty());
        assert_eq!(b.held_bytes(), "old\n".len() + "new\n".len());
        b.forget(&["t1".to_string()]);
        b.capture("t1", "s2", "claude", 1, vec![f.clone()]).await;
        b.remap_tab("t1", "t9");
        assert_eq!(content(&b.lookup("t9", "s2", &f, "turn", 1)), Some("new\n"));
        assert_eq!(b.lookup("t1", "s2", &f, "turn", 1).unavailable.as_deref(), Some("not_captured"));
        b.forget(&["t9".to_string()]);
        assert_eq!(b.lookup("t9", "s2", &f, "turn", 1).unavailable.as_deref(), Some("not_captured"));
        assert_eq!(b.held_bytes(), 0);
        let _ = std::fs::remove_dir_all(&d);
    }

    /// A parent's turn runs `claude -p 'fix lint' && claude -p 'add tests'`: both children
    /// capture, back to back, under the tab. The parent's next edit of a file it already changed
    /// this turn must still find the turn's real start, not recapture its own edit.
    #[tokio::test]
    async fn children_capturing_in_a_parent_turn_keep_the_parents_baselines() {
        let d = dir("children");
        let (a, x) = (p(&d, "a.ts"), p(&d, "x.ts"));
        std::fs::write(&a, "v0\n").unwrap();
        std::fs::write(&x, "x\n").unwrap();
        let b = Baselines::new();
        b.capture("t1", "s1", "claude", 3, vec![a.clone()]).await;
        std::fs::write(&a, "v1\n").unwrap();
        for child in 0..SESSIONS - 1 {
            b.capture("t1", &format!("c{child}"), "claude", 1, vec![x.clone()]).await;
        }
        assert!(b.capture("t1", "s1", "claude", 3, vec![a.clone()]).await.is_empty());
        assert_eq!(content(&b.lookup("t1", "s1", &a, "turn", 3)), Some("v0\n"));
        assert_eq!(content(&b.lookup("t1", "s1", &a, "session", 3)), Some("v0\n"));
        let _ = std::fs::remove_dir_all(&d);
    }

    /// More sessions capture than a tab keeps, and the parent's baselines go. When it comes back
    /// mid-turn, the file holds its own edits: that turn and its session start say so (as other
    /// sessions, not memory), never the edited bytes. Its next turn captures again.
    #[tokio::test]
    async fn a_session_whose_baselines_were_dropped_never_recaptures_its_open_turn() {
        let d = dir("dropped");
        let (a, c, x) = (p(&d, "a.ts"), p(&d, "c.ts"), p(&d, "x.ts"));
        std::fs::write(&a, "v0\n").unwrap();
        std::fs::write(&c, "c0\n").unwrap();
        std::fs::write(&x, "x\n").unwrap();
        let b = Baselines::new();
        b.capture("t1", "s1", "claude", 3, vec![a.clone()]).await;
        std::fs::write(&a, "v1\n").unwrap();
        for child in 0..SESSIONS {
            b.capture("t1", &format!("c{child}"), "claude", 1, vec![x.clone()]).await;
        }
        assert_eq!(b.lookup("t1", "s1", &a, "turn", 3).unavailable.as_deref(), Some(SESSIONS_GONE));
        // Back in turn 3: nothing is read, and a second edit doesn't read either.
        assert_eq!(b.capture("t1", "s1", "claude", 3, vec![a.clone()]).await, vec![(a.clone(), None)]);
        assert!(b.capture("t1", "s1", "claude", 3, vec![a.clone()]).await.is_empty());
        for scope in ["turn", "session"] {
            let got = b.lookup("t1", "s1", &a, scope, 3);
            assert_eq!((got.content, got.unavailable.as_deref()), (None, Some(SESSIONS_GONE)), "{scope}");
        }
        // A file not touched since it came back may have been touched before.
        assert_eq!(b.lookup("t1", "s1", &c, "turn", 3).unavailable.as_deref(), Some(SESSIONS_GONE));
        // Turn 4 starts from what turn 3 left; the session's start stays unknown.
        b.capture("t1", "s1", "claude", 4, vec![a.clone()]).await;
        assert_eq!(content(&b.lookup("t1", "s1", &a, "turn", 4)), Some("v1\n"));
        assert_eq!(b.lookup("t1", "s1", &a, "session", 4).unavailable.as_deref(), Some(SESSIONS_GONE));
        // Nothing here came near the memory budget.
        assert!(b.held_bytes() < 64);
        let _ = std::fs::remove_dir_all(&d);
    }

    /// The ledger forgot the session's turn: its baselines go with it, and coming back as "turn 1"
    /// it gets neither the old turn 1's bytes nor a fresh read of its own edits.
    #[tokio::test]
    async fn a_session_the_ledger_forgot_loses_its_baselines() {
        let d = dir("forgot");
        let a = p(&d, "a.ts");
        std::fs::write(&a, "v0\n").unwrap();
        let b = Baselines::new();
        b.capture("t1", "s1", "claude", 1, vec![a.clone()]).await;
        std::fs::write(&a, "v5\n").unwrap();
        b.lose_session("t1", "s1");
        assert_eq!(b.held_bytes(), 0);
        assert_eq!(b.capture("t1", "s1", "claude", 1, vec![a.clone()]).await, vec![(a.clone(), None)]);
        assert_eq!(b.lookup("t1", "s1", &a, "turn", 1).unavailable.as_deref(), Some(SESSIONS_GONE));
        // A session that never captured has nothing to lose: it is new when it comes.
        b.lose_session("t1", "c1");
        b.capture("t1", "c1", "claude", 1, vec![a.clone()]).await;
        assert_eq!(content(&b.lookup("t1", "c1", &a, "turn", 1)), Some("v5\n"));
        let _ = std::fs::remove_dir_all(&d);
    }

    /// A session that lost its baselines and came back: only the turns that may have begun before
    /// it came back are blind. One it opened with a prompt after is read as usual, however many
    /// clean turns went before its first edit, and those turns never touched the file.
    #[tokio::test]
    async fn only_turns_begun_before_a_session_came_back_are_blind() {
        let d = dir("back");
        let a = p(&d, "a.ts");
        std::fs::write(&a, "v0\n").unwrap();
        let b = Baselines::new();
        b.capture("t1", "s1", "claude", 3, vec![a.clone()]).await;
        std::fs::write(&a, "v1\n").unwrap();
        b.lose_session("t1", "s1");
        // Back mid-turn 1 (renumbered), then a prompt opens turn 2.
        b.came_back("t1", "s1", 1);
        let why = |scope: &str, turn: u64| b.lookup("t1", "s1", &a, scope, turn).unavailable;
        assert_eq!(why("turn", 1).as_deref(), Some(SESSIONS_GONE));
        assert_eq!(why("turn", 2).as_deref(), Some("not_captured"));
        assert_eq!(why("session", 2).as_deref(), Some(SESSIONS_GONE));
        // A later return doesn't move it: turn 2 began after the first.
        b.came_back("t1", "s1", 5);
        assert_eq!(why("turn", 3).as_deref(), Some("not_captured"));
        assert_eq!(b.capture("t1", "s1", "claude", 3, vec![a.clone()]).await, vec![(a.clone(), Some(true))]);
        assert_eq!(content(&b.lookup("t1", "s1", &a, "turn", 3)), Some("v1\n"));
        assert_eq!((why("turn", 1).as_deref(), why("session", 3).as_deref()), (Some(SESSIONS_GONE), Some(SESSIONS_GONE)));
        // Lost again before it captured: what it came back to is forgotten with it.
        b.lose_session("t1", "s1");
        b.came_back("t1", "s1", 1);
        b.lose_session("t1", "s1");
        assert_eq!(why("turn", 2).as_deref(), Some(SESSIONS_GONE));
        assert_eq!(b.capture("t1", "s1", "claude", 2, vec![a.clone()]).await, vec![(a.clone(), None)]);
        let _ = std::fs::remove_dir_all(&d);
    }

    /// The tree names a file by its canonical root (`/private/var/…` on macOS), the agent as it
    /// typed it (`/var/…`): one file.
    #[tokio::test]
    async fn a_lookup_matches_the_canonical_spelling() {
        let d = dir("canon");
        let f = p(&d, "a.ts");
        std::fs::write(&f, "x\n").unwrap();
        let b = Baselines::new();
        b.capture("t1", "s1", "claude", 1, vec![f.clone()]).await;
        let canon = std::fs::canonicalize(&f).unwrap().to_string_lossy().into_owned();
        assert_eq!(content(&b.lookup("t1", "s1", &canon, "turn", 1)), Some("x\n"));
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn write_paths_are_the_write_capable_ones() {
        use super::super::extract::extract;
        use serde_json::json;
        // The root is built from the temp dir: "/r" is not absolute on Windows, so a local
        // path under it would be refused there rather than resolved.
        let r = std::env::temp_dir().join("r");
        let a = p(&r, "a.ts");
        let edit = extract("Edit", &json!({ "file_path": a, "old_string": "a", "new_string": "b" }));
        assert_eq!(write_paths(&edit, None), vec![a.clone()]);
        assert!(write_paths(&extract("Read", &json!({ "file_path": a })), None).is_empty());
        assert!(write_paths(&extract("Bash", &json!({ "command": "touch x" })), None).is_empty());
        let patch = "*** Begin Patch\n*** Update File: src/x.ts\n*** Add File: src/y.ts\n+y\n*** End Patch";
        let src = r.join("src");
        assert_eq!(write_paths(&extract("apply_patch", &json!(patch)), r.to_str()), vec![p(&src, "x.ts"), p(&src, "y.ts")]);
    }

    #[test]
    fn line_counts_count_lines() {
        assert_eq!(line_counts("a\nb\nc\n", "a\nB\nc\nd\n"), (2, 1));
        assert_eq!(line_counts("", ""), (0, 0));
    }

    /// A rewrite of every line of a big file is quadratic for Myers; the count gives up looking
    /// for the shortest edit at its deadline instead of spinning for seconds per hover.
    #[test]
    fn line_counts_of_a_full_rewrite_stop_at_the_deadline() {
        let old: String = (0..40_000).map(|i| format!("old line {i}\n")).collect();
        let new: String = (0..40_000).map(|i| format!("new line {i}\n")).collect();
        let started = Instant::now();
        let (added, removed) = line_counts_within(&old, &new, Duration::from_millis(50));
        let took = started.elapsed();
        assert!(took < Duration::from_secs(2), "counting took {took:?}");
        assert_eq!((added, removed), (40_000, 40_000));
    }
}
