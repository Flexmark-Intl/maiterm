//! What each agent tab is reading and changing (docs/file-tree.md §9): the `AgentFileLedger`.
//!
//! Fed from `hooks_handler`'s tool, prompt and stop events, and from the Files watcher while a
//! shell call runs. Everything here is evidence and is labelled as such:
//! - `evidence: "tool"`: the call named the file (`extract.rs`).
//! - `evidence: "fs_during_tool"`: the watcher saw the file change while exactly one agent tab
//!   had a shell call running in that tree. The UI says "changed while claude's Bash ran",
//!   never "claude edited": a human's save in that window is attributed too (§9.2).
//! - `evidence: "commit"`: HEAD moved while exactly one agent tab had a turn open in the repo,
//!   and these are the files the new commits changed.
//!
//! The ledger is in memory only (no Tab field, nothing persisted) and holds 2,000 entries per
//! tab. Recording never waits: the hook pushes under a short lock and hands the marks to the
//! dispatcher thread, which routes them to the trees showing those paths, one coalesced `agent`
//! event per tree per 100 ms. Raw `tool_input` (a Write carries the whole file) never leaves
//! the hook: only paths do.
//!
//! **Paths.** A local entry's path is absolute, lexically normalized and NFC where the
//! platform's volumes are (`filetree::paths::norm`); it is canonicalized only when matched
//! against a tree, off the hook's path. An ssh tab's entry is `user@host:/abs` (the tunnel's
//! connect args, then the remote path), and no tree shows those until remote roots arrive.

pub mod baseline;
pub mod commands;
pub mod extract;

use std::collections::{HashMap, HashSet, VecDeque};
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, RecvTimeoutError};
use std::sync::{Arc, Weak};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use parking_lot::Mutex;
use serde::Serialize;

use crate::filetree::git::Trigger;
use crate::filetree::hub::RootHub;
use crate::filetree::paths;
use crate::filetree::types::AgentMark;
use crate::filetree::FileTreeState;
use extract::{is_write_op, Extracted};

/// Entries kept per tab, oldest dropped first.
pub const RING: usize = 2000;
/// At most one `agent` event per tree per this long.
pub const COALESCE: Duration = Duration::from_millis(100);
/// Pre events remembered by `tool_use_id`, to drop the second copy (mod relay and HTTP hook).
const SEEN_IDS: usize = 4096;
/// A tab missing from the window state is forgotten after this long: a reload deletes the old
/// tab before `agent_files_remap_tab` moves its ledger to the new id.
const MISSING_GRACE: Duration = Duration::from_secs(60);
/// A shell call that ended this recently still owns a change the watcher reports now: its
/// batch closes up to 250 ms after the write (`coalesce::TOTAL`).
const SHELL_TAIL: Duration = Duration::from_millis(300);
/// A shell call with no end seen after this long is over as far as attribution goes: the last
/// resort for one whose end no hook reports (a subagent cut short by Esc may send no
/// SubagentStop). Claude's Bash times out at 10 minutes. A call that really runs longer, or sat
/// longer behind its permission dialog, loses its label for the rest: no label beats crediting
/// the human's saves to it.
const SHELL_MAX: Duration = Duration::from_secs(600);
/// Sessions a tab switched away from whose turn numbers are remembered (`TabLedger::left`).
const LEFT_SESSIONS: usize = 8;

/// One thing an agent did to one path (§9.1).
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct LedgerEntry {
    /// Absolute (local) or `user@host:/abs` (an ssh tab's).
    pub path: String,
    /// "read" | "search" | "edit" | "write" | "create" | "delete" | "changed" | "commit".
    pub op: String,
    pub tool: String,
    pub tool_use_id: Option<String>,
    /// `gate::agent_key`: "" is the main agent, otherwise the subagent's id.
    pub agent: String,
    /// "pending" | "ok" | "failed".
    pub status: String,
    /// "tool" | "fs_during_tool" | "commit".
    pub evidence: String,
    pub turn: u64,
    pub t_pre_ms: i64,
    pub t_post_ms: Option<i64>,
}

/// A mark with its absolute path, before a tree has placed it (`agent_files_marks`).
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct AgentMarkAbs {
    pub path: String,
    pub op: String,
    pub phase: String,
    pub tab_id: String,
    pub agent: String,
    pub turn: u64,
    pub evidence: String,
    pub at_ms: i64,
}

impl AgentMarkAbs {
    fn from_entry(tab_id: &str, e: &LedgerEntry) -> Self {
        AgentMarkAbs {
            path: e.path.clone(),
            op: e.op.clone(),
            phase: e.status.clone(),
            tab_id: tab_id.to_string(),
            agent: e.agent.clone(),
            turn: e.turn,
            evidence: e.evidence.clone(),
            at_ms: e.t_post_ms.unwrap_or(e.t_pre_ms),
        }
    }

    pub fn with_rel(&self, rel: String) -> AgentMark {
        AgentMark {
            rel,
            op: self.op.clone(),
            phase: self.phase.clone(),
            tab_id: self.tab_id.clone(),
            agent: self.agent.clone(),
            turn: self.turn,
            evidence: self.evidence.clone(),
            at_ms: self.at_ms,
        }
    }
}

/// Which hook event a record comes from, read off the RAW `hook_event_name`: the shared
/// normalization folds `PostToolUseFailure` into PostToolUse, and the ledger keeps failures.
///
/// A call that is denied, or cut short with Esc, gets no Post, and Claude sends no Stop for that
/// turn either (claude_code/gate.rs: "A denied call fires no hook at all"). Without the last
/// three phases such a call stayed `pending`, and its shell call running, until the next prompt,
/// and a subagent's for good: the row pulsed, the rename guard asked, and every later change in
/// the tree (the human's saves included) was credited to that Bash.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Phase {
    Pre,
    Post { failed: bool },
    Prompt,
    Stop,
    /// Claude's `idle_prompt` Notification. It is never sent while any dialog is open (a
    /// subagent's included) or while the main thread works, so the main agent's turn is over and
    /// no call is waiting on a human: a pending call is either done or never ran.
    Idle,
    /// Codex's `Interrupt`: the human cancelled the turn (Esc). No turn completed, and Codex sends
    /// nothing for a call it was asking about.
    Interrupt,
    /// One subagent finished: whatever of its calls never got a Post never will.
    SubagentStop,
    /// A session started (startup, resume, /clear, compaction). It records nothing of its own:
    /// it carries the session id, so a new session starts its own history and numbering here,
    /// when the window hears of it, rather than at its first prompt or tool call. Until then the
    /// ledger went on reporting the old session's turn, and a Files column opened in that gap
    /// started its tracker there (file-tree.md §9.4). A compaction keeps the id and so changes
    /// nothing.
    SessionStart,
}

impl Phase {
    /// The phase of a raw hook event (its name, and for a Notification its type), or None for an
    /// event the ledger ignores.
    pub fn of(hook_event_name: &str, event: &serde_json::Value) -> Option<Phase> {
        match hook_event_name {
            "PreToolUse" => Some(Phase::Pre),
            "PostToolUse" => Some(Phase::Post { failed: false }),
            "PostToolUseFailure" => Some(Phase::Post { failed: true }),
            "UserPromptSubmit" => Some(Phase::Prompt),
            "Stop" => Some(Phase::Stop),
            "Notification" if event.get("notification_type").and_then(|v| v.as_str()) == Some("idle_prompt") => Some(Phase::Idle),
            "Interrupt" => Some(Phase::Interrupt),
            "SubagentStop" => Some(Phase::SubagentStop),
            "SessionStart" => Some(Phase::SessionStart),
            _ => None,
        }
    }
}

/// One hook event, reduced to what the ledger keeps. Built in the hook's path with no IO.
#[derive(Debug, Clone)]
pub struct HookRecord {
    pub phase: Phase,
    pub tab_id: String,
    pub session_id: String,
    pub runtime: String,
    /// The session's folder: a Codex patch's relative paths resolve against it, and a shell
    /// call is attributed in the tree that holds it.
    pub cwd: Option<String>,
    /// The tunnel's connect args for an ssh tab; None for a local one.
    pub host: Option<String>,
    pub tool: String,
    pub tool_use_id: Option<String>,
    pub agent: String,
    pub extracted: Extracted,
    /// A Write's file existed when the call was about to run (None: not looked, or unknown).
    pub existed: Option<bool>,
}

struct TabLedger {
    session_id: String,
    runtime: String,
    turn: u64,
    turn_open: bool,
    /// When the current turn began and ended (None: open or never seen to end), ms epoch.
    turn_span: (i64, Option<i64>),
    cwd: Option<String>,
    host: Option<String>,
    entries: VecDeque<LedgerEntry>,
    missing_since: Option<Instant>,
    /// Sessions the tab switched away from, with the turn and span each was at, newest last. A
    /// headless `claude -p` run from the agent's Bash inherits the tab's id and posts its own
    /// session's events, and the parent goes on in its turn around it: coming back, the parent's
    /// turn carries on under its own number instead of restarting at 1, where it would collide
    /// with the turn baselines (baseline.rs) its earlier turns captured. A session that falls off
    /// the end loses its baselines with its turn number (`Baselines::lose_session`).
    left: VecDeque<(String, u64, (i64, Option<i64>))>,
}

impl TabLedger {
    fn new() -> Self {
        TabLedger {
            session_id: String::new(),
            runtime: String::new(),
            turn: 0,
            turn_open: false,
            turn_span: (0, None),
            cwd: None,
            host: None,
            entries: VecDeque::new(),
            missing_since: None,
            left: VecDeque::new(),
        }
    }

    /// The turn `session` was at when the tab last switched away from it (0: never seen).
    fn left_turn(&self, session: &str) -> u64 {
        self.left.iter().rev().find(|(s, ..)| s == session).map(|(_, t, _)| *t).unwrap_or(0)
    }

    /// Stop, idle or an interrupt: the turn is over. Only the first of them is its end; an idle
    /// notification after the Stop must not move it later.
    fn end_turn(&mut self, now: i64) {
        if self.turn_open {
            self.turn_span.1 = Some(now);
        }
        self.turn_open = false;
    }

    fn push(&mut self, e: LedgerEntry) {
        if self.entries.len() >= RING {
            self.entries.pop_front();
        }
        self.entries.push_back(e);
    }

    /// Calls still pending when their agent stopped (a denial, Esc, a crash) never get a Post:
    /// they did not complete, so they stop pulsing as failed. `agent` None settles every agent's;
    /// `keep` is the call that is starting now. A Post that does turn up later still wins: it
    /// matches on `tool_use_id`, whatever the status.
    fn settle_unfinished(&mut self, tab_id: &str, agent: Option<&str>, keep: Option<&str>, writes_only: bool, now: i64, marks: &mut Vec<(AgentMarkAbs, bool)>) {
        let local = self.host.is_none();
        let settles = |e: &LedgerEntry| {
            e.status == "pending"
                && e.evidence == "tool"
                && agent.is_none_or(|a| e.agent == a)
                && (keep.is_none() || e.tool_use_id.as_deref() != keep)
                && (!writes_only || !matches!(e.op.as_str(), "read" | "search"))
        };
        for e in self.entries.iter_mut().filter(|e| settles(e)) {
            e.status = "failed".into();
            e.t_post_ms = Some(now);
            if local {
                marks.push((AgentMarkAbs::from_entry(tab_id, e), false));
            }
        }
    }
}

#[derive(Debug, Clone)]
struct ShellCall {
    tab_id: String,
    tool_use_id: String,
    tool: String,
    agent: String,
    turn: u64,
    cwd: PathBuf,
    started: Instant,
    /// When its Post (or other proof that it is over) arrived; None while it runs.
    ended: Option<Instant>,
}

impl ShellCall {
    /// Still running as far as attribution goes: no end seen, and younger than `SHELL_MAX`.
    fn running(&self) -> bool {
        self.ended.is_none() && self.started.elapsed() < SHELL_MAX
    }

    /// Can still own a change the watcher reports now.
    fn attributable(&self) -> bool {
        self.running() || self.ended.is_some_and(|t| t.elapsed() < SHELL_TAIL)
    }
}

/// End `tab_id`'s running shell calls: `agent` None for every agent's, `keep` the one starting.
fn end_shells(shells: &mut [ShellCall], tab_id: &str, agent: Option<&str>, keep: Option<&str>) {
    let now = Instant::now();
    for s in shells.iter_mut().filter(|s| s.tab_id == tab_id && s.ended.is_none() && agent.is_none_or(|a| s.agent == a) && keep.is_none_or(|k| s.tool_use_id != k)) {
        s.ended = Some(now);
    }
}

/// The one shell call a tree's changes are attributed to.
#[derive(Debug, Clone, PartialEq)]
pub struct ShellRef {
    pub tab_id: String,
    pub tool_use_id: String,
    pub tool: String,
    pub agent: String,
    pub turn: u64,
}

#[derive(Default)]
struct Inner {
    tabs: HashMap<String, TabLedger>,
    seen_order: VecDeque<String>,
    seen: HashSet<String>,
    shells: Vec<ShellCall>,
}

/// A mark on its way to the trees, and whether a status run is due for it (a write that
/// completed: §7.2's ToolPost trigger).
struct Routed {
    mark: AgentMarkAbs,
    git: bool,
}

pub struct AgentFiles {
    inner: Mutex<Inner>,
    tree: Weak<FileTreeState>,
    tx: Mutex<Option<mpsc::Sender<Routed>>>,
    /// `file_tree_agent_activity`: marks reach the trees. The ledger itself always records,
    /// since `getAgentFileActivity` and the agent-in-flight guard (§10.3) read it.
    routing: AtomicBool,
    /// What each file held before a tab's first edit of it this turn and this session (§14).
    /// Keyed by the same tab ids, so it follows the ledger's remaps and forgets with it.
    pub baselines: Arc<baseline::Baselines>,
}

pub fn now_ms() -> i64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis() as i64).unwrap_or(0)
}

/// `p` with `.` dropped and `..` folded, lexically: hooks name paths as typed.
fn lexical(p: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for c in p.components() {
        match c {
            Component::CurDir => {}
            Component::ParentDir => {
                out.pop();
            }
            other => out.push(other.as_os_str()),
        }
    }
    out
}

/// A POSIX path on a remote host, folded the same way.
fn lexical_posix(p: &str) -> String {
    let mut out: Vec<&str> = Vec::new();
    for part in p.split('/') {
        match part {
            "" | "." => {}
            ".." => {
                out.pop();
            }
            other => out.push(other),
        }
    }
    format!("/{}", out.join("/"))
}

/// The ledger's key for a path a call named: absolute, normalized, and for an ssh tab prefixed
/// with its host. None when it can't be made absolute.
pub fn entry_path(raw: &str, cwd: Option<&str>, host: Option<&str>) -> Option<String> {
    match host {
        Some(h) => {
            let abs = if raw.starts_with('/') {
                raw.to_string()
            } else {
                let base = cwd.filter(|c| c.starts_with('/'))?;
                format!("{base}/{raw}")
            };
            Some(format!("{h}:{}", lexical_posix(&abs)))
        }
        None => {
            let p = Path::new(raw);
            let abs = if p.is_absolute() {
                p.to_path_buf()
            } else {
                let base = Path::new(cwd?);
                if !base.is_absolute() {
                    return None;
                }
                base.join(p)
            };
            Some(paths::norm(&lexical(&abs).to_string_lossy()).into_owned())
        }
    }
}

/// `inner` is at or under `outer`, matched as spelled and then canonicalized (`/tmp` vs
/// `/private/tmp`). May touch the disk: never call it under the ledger's lock.
fn within(inner: &Path, outer: &Path, fold: bool) -> bool {
    paths::strip_root(inner, outer, fold).is_some() || paths::strip_root(&paths::canon_existing(inner), &crate::share::canon(outer), fold).is_some()
}

impl AgentFiles {
    pub fn new(tree: Weak<FileTreeState>) -> Arc<Self> {
        Arc::new(AgentFiles { inner: Mutex::new(Inner::default()), tree, tx: Mutex::new(None), routing: AtomicBool::new(true), baselines: baseline::Baselines::new() })
    }

    /// The turn a PreToolUse of `session` in `tab` will be recorded in, asked before it is
    /// recorded (the baseline is read first, so a Write's create-or-overwrite comes from that
    /// same read). Mirrors `record`: a new session starts at turn 1, a session the tab comes back
    /// to carries on at its own turn, and a call opens turn 1 when no turn was seen yet;
    /// otherwise it is the current turn. A prompt can still land between this and `record`,
    /// which is why `record` answers the turn it really used.
    pub fn turn_for_pre(&self, tab_id: &str, session_id: &str) -> u64 {
        let inner = self.inner.lock();
        match inner.tabs.get(tab_id) {
            Some(t) if session_id.is_empty() || t.session_id.is_empty() || t.session_id == session_id => t.turn.max(1),
            Some(t) => t.left_turn(session_id).max(1),
            None => 1,
        }
    }

    /// The tab's current (turn, session) in the ledger, for a baseline lookup. None for a tab
    /// with no turn yet.
    pub fn turn_and_session(&self, tab_id: &str) -> Option<(u64, String)> {
        self.inner.lock().tabs.get(tab_id).filter(|t| t.turn > 0).map(|t| (t.turn, t.session_id.clone()))
    }

    pub fn routing(&self) -> bool {
        self.routing.load(Ordering::SeqCst)
    }

    /// `file_tree_agent_activity` changed (or is read at `ft_open`).
    pub fn set_routing(&self, on: bool) {
        self.routing.store(on, Ordering::SeqCst);
    }

    /// Record one hook event. Never waits on anything but the ledger's own short lock. Answers
    /// the tab's turn as recorded (None: a second copy of a Pre, dropped).
    pub fn record(&self, rec: HookRecord) -> Option<u64> {
        let now = now_ms();
        let mut out: Vec<(AgentMarkAbs, bool)> = Vec::new();
        let mut forgot: Option<String> = None;
        let recorded = {
            let mut inner = self.inner.lock();
            let inner = &mut *inner;
            // The maiterm-tab mod and the settings hook both deliver PreToolUse; whichever
            // arrives first is the one recorded.
            if rec.phase == Phase::Pre {
                if let Some(id) = rec.tool_use_id.as_deref().filter(|s| !s.is_empty()) {
                    if !inner.seen.insert(id.to_string()) {
                        return None;
                    }
                    inner.seen_order.push_back(id.to_string());
                    if inner.seen_order.len() > SEEN_IDS {
                        if let Some(old) = inner.seen_order.pop_front() {
                            inner.seen.remove(&old);
                        }
                    }
                }
            }
            let tab = inner.tabs.entry(rec.tab_id.clone()).or_insert_with(TabLedger::new);
            tab.missing_since = None;
            // A new session (a resume mints one, so does /clear) starts its own history.
            if !rec.session_id.is_empty() && tab.session_id != rec.session_id {
                // A session the tab comes back to carries on where it was (its turn left open
                // or not, the next event says). Taken out before the outgoing session is filed:
                // with `left` full, filing first would push out the very session coming back.
                let back = tab.left.iter().position(|(s, ..)| *s == rec.session_id).and_then(|i| tab.left.remove(i));
                if !tab.session_id.is_empty() {
                    tab.left.retain(|(s, ..)| *s != tab.session_id);
                    tab.left.push_back((tab.session_id.clone(), tab.turn, tab.turn_span));
                    if tab.left.len() > LEFT_SESSIONS {
                        // Its turn number is gone: coming back, it starts again at turn 1, so
                        // its baselines go too (below, outside this lock) rather than being
                        // served for turns that only share their numbers.
                        forgot = tab.left.pop_front().map(|(s, ..)| s);
                    }
                    tab.entries.clear();
                    tab.turn = 0;
                    tab.turn_open = false;
                    tab.turn_span = (0, None);
                    // The old session's shell calls are over too: left running, they would keep
                    // claiming the tree's changes for a session that is gone.
                    end_shells(&mut inner.shells, &rec.tab_id, None, None);
                }
                if let Some((_, turn, span)) = back {
                    tab.turn = turn;
                    tab.turn_span = span;
                }
                tab.session_id = rec.session_id.clone();
            }
            if !rec.runtime.is_empty() {
                tab.runtime = rec.runtime.clone();
            }
            if rec.cwd.is_some() {
                tab.cwd = rec.cwd.clone();
            }
            tab.host = rec.host.clone();
            let local = rec.host.is_none();
            match rec.phase {
                Phase::Prompt | Phase::Stop => {
                    // The main agent's turn is over; a background subagent may still be working.
                    tab.settle_unfinished(&rec.tab_id, Some(""), None, false, now, &mut out);
                    if rec.phase == Phase::Prompt {
                        tab.turn += 1;
                        tab.turn_open = true;
                        tab.turn_span = (now, None);
                    } else {
                        tab.end_turn(now);
                    }
                    end_shells(&mut inner.shells, &rec.tab_id, Some(""), None);
                }
                Phase::Idle => {
                    // No dialog is open anywhere, so no call is waiting on a human, and an edit
                    // or a read that was let through has long finished: every agent's pending
                    // call is settled. A background subagent's Bash may really still run, so only
                    // the main agent's shell calls end. The turn is over (an Esc sends no Stop).
                    tab.settle_unfinished(&rec.tab_id, None, None, false, now, &mut out);
                    tab.end_turn(now);
                    end_shells(&mut inner.shells, &rec.tab_id, Some(""), None);
                }
                Phase::Interrupt => {
                    tab.settle_unfinished(&rec.tab_id, None, None, false, now, &mut out);
                    tab.end_turn(now);
                    end_shells(&mut inner.shells, &rec.tab_id, None, None);
                }
                // The session change above is all it does.
                Phase::SessionStart => {}
                Phase::SubagentStop => {
                    if !rec.agent.is_empty() {
                        tab.settle_unfinished(&rec.tab_id, Some(&rec.agent), None, false, now, &mut out);
                        end_shells(&mut inner.shells, &rec.tab_id, Some(&rec.agent), None);
                    }
                }
                Phase::Pre => {
                    // Claude runs a write or a shell command only on its own: while one runs,
                    // its agent starts nothing else. So this agent's earlier write or shell call
                    // still open is one that never ran (denied with feedback, and the agent went
                    // on), the same evidence claude_code/gate.rs uses. Reads may run side by side
                    // and are left alone. Codex runs calls in parallel, so not there.
                    if rec.runtime == "claude" {
                        if let Some(id) = rec.tool_use_id.as_deref().filter(|s| !s.is_empty()) {
                            tab.settle_unfinished(&rec.tab_id, Some(&rec.agent), Some(id), true, now, &mut out);
                            end_shells(&mut inner.shells, &rec.tab_id, Some(&rec.agent), Some(id));
                        }
                    }
                    // A call proves a turn is running, whether or not its prompt was seen (maiTerm
                    // started mid-turn, or a background subagent outlived its parent's).
                    if !tab.turn_open {
                        tab.turn_open = true;
                        // A call after the turn ended (a background subagent) carries the turn
                        // on; one with no turn yet starts the first, as of now.
                        if tab.turn == 0 {
                            tab.turn_span = (now, None);
                        } else {
                            tab.turn_span.1 = None;
                        }
                        tab.turn = tab.turn.max(1);
                    }
                    match &rec.extracted {
                        Extracted::Files(touches) => {
                            for t in touches {
                                let Some(path) = entry_path(&t.path, rec.cwd.as_deref(), rec.host.as_deref()) else { continue };
                                let op = if t.op == "write" && rec.existed == Some(false) { "create" } else { t.op };
                                let e = LedgerEntry {
                                    path,
                                    op: op.into(),
                                    tool: rec.tool.clone(),
                                    tool_use_id: rec.tool_use_id.clone(),
                                    agent: rec.agent.clone(),
                                    status: "pending".into(),
                                    evidence: "tool".into(),
                                    turn: tab.turn,
                                    t_pre_ms: now,
                                    t_post_ms: None,
                                };
                                if local {
                                    out.push((AgentMarkAbs::from_entry(&rec.tab_id, &e), false));
                                }
                                tab.push(e);
                            }
                        }
                        Extracted::Shell => {
                            if let (true, Some(id), Some(cwd)) = (local, rec.tool_use_id.clone(), rec.cwd.as_deref().filter(|c| Path::new(c).is_absolute())) {
                                inner.shells.push(ShellCall {
                                    tab_id: rec.tab_id.clone(),
                                    tool_use_id: id,
                                    tool: rec.tool.clone(),
                                    agent: rec.agent.clone(),
                                    turn: tab.turn,
                                    cwd: PathBuf::from(cwd),
                                    started: Instant::now(),
                                    ended: None,
                                });
                            }
                        }
                        Extracted::Nothing => {}
                    }
                }
                Phase::Post { failed } => {
                    let status = if failed { "failed" } else { "ok" };
                    let id = rec.tool_use_id.as_deref().filter(|s| !s.is_empty());
                    let mut matched = false;
                    if let Some(id) = id {
                        for e in tab.entries.iter_mut().filter(|e| e.tool_use_id.as_deref() == Some(id) && e.evidence == "tool") {
                            matched = true;
                            e.status = status.into();
                            e.t_post_ms = Some(now);
                            if local {
                                out.push((AgentMarkAbs::from_entry(&rec.tab_id, e), !failed && is_write_op(&e.op)));
                            }
                        }
                        let instant = Instant::now();
                        for s in inner.shells.iter_mut().filter(|s| s.tool_use_id == id && s.ended.is_none()) {
                            s.ended = Some(instant);
                        }
                    }
                    // The Pre never reached us (maiTerm started mid-call, or it had no id): the
                    // Post alone still says what the call did.
                    if !matched {
                        if let Extracted::Files(touches) = &rec.extracted {
                            for t in touches {
                                let Some(path) = entry_path(&t.path, rec.cwd.as_deref(), rec.host.as_deref()) else { continue };
                                let e = LedgerEntry {
                                    path,
                                    op: t.op.into(),
                                    tool: rec.tool.clone(),
                                    tool_use_id: rec.tool_use_id.clone(),
                                    agent: rec.agent.clone(),
                                    status: status.into(),
                                    evidence: "tool".into(),
                                    turn: tab.turn,
                                    t_pre_ms: now,
                                    t_post_ms: Some(now),
                                };
                                if local {
                                    out.push((AgentMarkAbs::from_entry(&rec.tab_id, &e), !failed && is_write_op(&e.op)));
                                }
                                tab.push(e);
                            }
                        }
                    }
                }
            }
            let turn = tab.turn;
            // Ended shell calls are kept only as long as a late batch can still name them.
            inner.shells.retain(|s| s.running() || s.ended.is_some_and(|t| t.elapsed() < SHELL_TAIL * 4));
            turn
        };
        if let Some(s) = forgot {
            self.baselines.lose_session(&rec.tab_id, &s);
        }
        self.route(out);
        Some(recorded)
    }

    /// Hand marks to the dispatcher. Nothing is routed while `file_tree_agent_activity` is off,
    /// except the status run a completed write asks for.
    fn route(&self, marks: Vec<(AgentMarkAbs, bool)>) {
        if marks.is_empty() {
            return;
        }
        let routing = self.routing();
        let marks: Vec<Routed> = marks.into_iter().filter(|(_, git)| routing || *git).map(|(mark, git)| Routed { mark, git }).collect();
        if marks.is_empty() {
            return;
        }
        let mut tx = self.tx.lock();
        if tx.is_none() {
            *tx = self.start_dispatcher();
        }
        if let Some(t) = tx.as_ref() {
            for m in marks {
                let _ = t.send(m);
            }
        }
    }

    fn start_dispatcher(&self) -> Option<mpsc::Sender<Routed>> {
        let (tx, rx) = mpsc::channel::<Routed>();
        let tree = self.tree.clone();
        match std::thread::Builder::new().name("agent-files".into()).spawn(move || dispatch(tree, rx)) {
            Ok(_) => Some(tx),
            Err(e) => {
                log::warn!("agent_files: couldn't start the dispatcher: {e}");
                None
            }
        }
    }

    /// The shell call a tree's changes belong to: one agent tab has a shell call running (or
    /// just ended) whose folder is in the tree, or that holds the tree. Two tabs: nobody's.
    pub fn sole_shell_in(&self, root: &Path, fold: bool) -> Option<ShellRef> {
        let calls: Vec<ShellCall> = {
            let mut inner = self.inner.lock();
            // Dropped here too, not only on the next hook: an idle ledger then costs the
            // watcher nothing again (§17).
            inner.shells.retain(|s| s.running() || s.ended.is_some_and(|t| t.elapsed() < SHELL_TAIL * 4));
            if inner.shells.is_empty() {
                return None;
            }
            inner.shells.iter().filter(|s| s.attributable()).cloned().collect()
        };
        let mine: Vec<&ShellCall> = calls.iter().filter(|s| within(&s.cwd, root, fold) || within(root, &s.cwd, fold)).collect();
        let tabs: HashSet<&str> = mine.iter().map(|s| s.tab_id.as_str()).collect();
        if tabs.len() != 1 {
            return None;
        }
        // Parallel calls of the one tab: the latest started.
        let s = mine.last()?;
        Some(ShellRef { tab_id: s.tab_id.clone(), tool_use_id: s.tool_use_id.clone(), tool: s.tool.clone(), agent: s.agent.clone(), turn: s.turn })
    }

    /// Paths the watcher saw change while `shell` ran. One entry per path per call.
    pub fn record_fs(&self, shell: &ShellRef, abs_paths: &[PathBuf]) {
        let now = now_ms();
        let mut out = Vec::new();
        {
            let mut inner = self.inner.lock();
            let Some(tab) = inner.tabs.get_mut(&shell.tab_id) else { return };
            for p in abs_paths {
                let path = paths::norm(&p.to_string_lossy()).into_owned();
                let same = |e: &LedgerEntry| e.evidence == "fs_during_tool" && e.path == path && e.tool_use_id.as_deref() == Some(shell.tool_use_id.as_str());
                if let Some(e) = tab.entries.iter_mut().find(|e| same(e)) {
                    e.t_post_ms = Some(now);
                    out.push((AgentMarkAbs::from_entry(&shell.tab_id, e), false));
                    continue;
                }
                let e = LedgerEntry {
                    path,
                    op: "changed".into(),
                    tool: shell.tool.clone(),
                    tool_use_id: Some(shell.tool_use_id.clone()),
                    agent: shell.agent.clone(),
                    status: "ok".into(),
                    evidence: "fs_during_tool".into(),
                    turn: shell.turn,
                    t_pre_ms: now,
                    t_post_ms: Some(now),
                };
                out.push((AgentMarkAbs::from_entry(&shell.tab_id, &e), false));
                tab.push(e);
            }
        }
        self.route(out);
    }

    /// The one local agent tab with a turn open whose folder `covers` says is in the repository:
    /// whom a HEAD move is credited to (§9.2). None with none or several.
    pub fn sole_open_turn(&self, covers: &dyn Fn(&Path) -> bool) -> Option<(String, u64)> {
        let open: Vec<(String, u64, PathBuf)> = {
            let inner = self.inner.lock();
            inner
                .tabs
                .iter()
                .filter(|(_, t)| t.turn_open && t.host.is_none())
                .filter_map(|(id, t)| t.cwd.as_ref().map(|c| (id.clone(), t.turn, PathBuf::from(c))))
                .collect()
        };
        let mut hits = open.into_iter().filter(|(_, _, cwd)| cwd.is_absolute() && (covers(cwd) || covers(&crate::share::canon(cwd))));
        let first = hits.next()?;
        if hits.next().is_some() {
            return None;
        }
        Some((first.0, first.1))
    }

    /// Files a commit made during `tab_id`'s turn `turn` changed.
    pub fn record_commit(&self, tab_id: &str, turn: u64, abs_paths: &[PathBuf]) {
        let now = now_ms();
        let mut out = Vec::new();
        {
            let mut inner = self.inner.lock();
            let Some(tab) = inner.tabs.get_mut(tab_id) else { return };
            for p in abs_paths {
                let e = LedgerEntry {
                    path: paths::norm(&p.to_string_lossy()).into_owned(),
                    op: "commit".into(),
                    tool: "git".into(),
                    tool_use_id: None,
                    agent: String::new(),
                    status: "ok".into(),
                    evidence: "commit".into(),
                    turn,
                    t_pre_ms: now,
                    t_post_ms: Some(now),
                };
                out.push((AgentMarkAbs::from_entry(tab_id, &e), false));
                tab.push(e);
            }
        }
        self.route(out);
    }

    /// The tabs whose agent has a call in flight on any of `items` (or inside one), or wrote one
    /// in its open turn (§10.3, `needs: agent_active`). Local tabs only: an ssh agent's paths
    /// name another machine.
    pub fn active_tabs(&self, items: &[PathBuf], fold: bool) -> Vec<String> {
        let candidates: Vec<(String, String)> = {
            let inner = self.inner.lock();
            inner
                .tabs
                .iter()
                .filter(|(_, t)| t.host.is_none())
                .flat_map(|(id, t)| {
                    t.entries
                        .iter()
                        .filter(|e| {
                            let in_flight = e.status == "pending" && !matches!(e.op.as_str(), "read" | "search");
                            let this_turn = t.turn_open && e.turn == t.turn && e.status == "ok" && is_write_op(&e.op);
                            in_flight || this_turn
                        })
                        .map(move |e| (id.clone(), e.path.clone()))
                })
                .collect()
        };
        let mut tabs: Vec<String> = Vec::new();
        for (tab, path) in candidates {
            if tabs.contains(&tab) {
                continue;
            }
            if items.iter().any(|item| within(Path::new(&path), item, fold)) {
                tabs.push(tab);
            }
        }
        tabs
    }

    /// `agent_files_activity` / `getAgentFileActivity`: a tab's entries this turn or session,
    /// newest last. Reads and searches only when asked. None for a tab with no record.
    pub fn activity(&self, tab_id: &str, scope: &str, include_reads: bool) -> Option<Activity> {
        let inner = self.inner.lock();
        let t = inner.tabs.get(tab_id)?;
        let entries = t
            .entries
            .iter()
            .filter(|e| scope != "turn" || e.turn == t.turn)
            .filter(|e| include_reads || !matches!(e.op.as_str(), "read" | "search"))
            .cloned()
            .collect();
        Some(Activity { entries, turn: t.turn, turn_open: t.turn_open, cwd: t.cwd.clone(), host: t.host.clone(), runtime: t.runtime.clone() })
    }

    /// The marks that still stand for `tabs` (None: every tab): each local path's latest mark
    /// per tab and agent, this session. Reads and searches leave no lasting mark, so none.
    pub fn standing_marks(&self, tabs: Option<&HashSet<String>>) -> Vec<AgentMarkAbs> {
        let inner = self.inner.lock();
        let mut latest: HashMap<(&str, &str, &str), AgentMarkAbs> = HashMap::new();
        for (id, t) in inner.tabs.iter().filter(|(id, t)| t.host.is_none() && tabs.is_none_or(|s| s.contains(id.as_str()))) {
            for e in t.entries.iter().filter(|e| !matches!(e.op.as_str(), "read" | "search")) {
                latest.insert((id.as_str(), e.path.as_str(), e.agent.as_str()), AgentMarkAbs::from_entry(id, e));
            }
        }
        let mut out: Vec<AgentMarkAbs> = latest.into_values().collect();
        out.sort_by_key(|m| m.at_ms);
        out
    }

    /// Each of `tabs`' current turn (None: every tab), for a window that was not listening to
    /// the hooks: the standing marks carry only the turns something was written in, so a turn
    /// that has only read, or not yet called a tool, is known here alone. Tabs with no turn yet
    /// are left out.
    pub fn turns(&self, tabs: Option<&HashSet<String>>) -> Vec<TabTurn> {
        let inner = self.inner.lock();
        inner
            .tabs
            .iter()
            .filter(|(id, t)| t.turn > 0 && tabs.is_none_or(|s| s.contains(id.as_str())))
            .map(|(id, t)| TabTurn { tab_id: id.clone(), turn: t.turn, open: t.turn_open, start_ms: t.turn_span.0, end_ms: t.turn_span.1 })
            .collect()
    }

    /// The tab's current turn in the ledger's own numbering (None: no turn yet). Sent with the
    /// prompt hook's event, so a window sets its tracker to the ledger's number instead of
    /// counting one on from what it has seen: a count can't be squared with a `turns()` read
    /// that raced the same prompt.
    pub fn turn_of(&self, tab_id: &str) -> Option<u64> {
        self.inner.lock().tabs.get(tab_id).map(|t| t.turn).filter(|&t| t > 0)
    }

    /// A reload replaced `old` with `new`: the same agent session goes on under the new id.
    pub fn remap_tab(&self, old: &str, new: &str) {
        let mut inner = self.inner.lock();
        if let Some(mut t) = inner.tabs.remove(old) {
            t.missing_since = None;
            inner.tabs.insert(new.to_string(), t);
        }
        for s in inner.shells.iter_mut().filter(|s| s.tab_id == old) {
            s.tab_id = new.to_string();
        }
        drop(inner);
        self.baselines.remap_tab(old, new);
    }

    /// Forget tabs that have been gone from every window for a minute (closed). Not at once:
    /// a reload deletes the old tab before its ledger is moved to the new id.
    pub fn prune(&self, existing: &HashSet<String>) {
        let mut inner = self.inner.lock();
        let now = Instant::now();
        let mut dead = Vec::new();
        for (id, t) in inner.tabs.iter_mut() {
            if existing.contains(id) {
                t.missing_since = None;
                continue;
            }
            let since = *t.missing_since.get_or_insert(now);
            if now.duration_since(since) >= MISSING_GRACE {
                dead.push(id.clone());
            }
        }
        for id in &dead {
            inner.tabs.remove(id);
        }
        inner.shells.retain(|s| !dead.contains(&s.tab_id));
        drop(inner);
        if !dead.is_empty() {
            self.baselines.forget(&dead);
        }
    }

    #[cfg(test)]
    fn age_missing(&self, by: Duration) {
        let mut inner = self.inner.lock();
        for t in inner.tabs.values_mut() {
            if let Some(s) = t.missing_since.as_mut() {
                *s -= by;
            }
        }
    }
}

/// A tab's current turn as the ledger counts it (`agent_files_turns`).
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct TabTurn {
    pub tab_id: String,
    pub turn: u64,
    pub open: bool,
    /// When it began (its prompt, or the first call when the prompt was not seen), ms epoch.
    pub start_ms: i64,
    /// When it ended; None while open, or when its end was never seen.
    pub end_ms: Option<i64>,
}

/// One tab's ledger, for the tool and the command.
#[derive(Debug, Clone, Serialize)]
pub struct Activity {
    pub entries: Vec<LedgerEntry>,
    pub turn: u64,
    pub turn_open: bool,
    pub cwd: Option<String>,
    pub host: Option<String>,
    pub runtime: String,
}

// ── Delivery ─────────────────────────────────────────────────────────────────────────────────

/// One tree's pending marks and when it last got some.
struct Slot<H> {
    hub: H,
    pending: Vec<AgentMark>,
    last_sent: Option<Instant>,
}

/// At most one batch per tree per `COALESCE`: the first mark after a quiet spell goes at once
/// (a Pre's pulse is due within 150 ms), anything inside the window waits for its end. Within a
/// batch only a path's latest mark per tab and agent is kept: the client draws the latest.
pub struct Coalescer<H> {
    slots: HashMap<String, Slot<H>>,
}

impl<H: Clone> Default for Coalescer<H> {
    fn default() -> Self {
        Coalescer { slots: HashMap::new() }
    }
}

impl<H: Clone> Coalescer<H> {
    pub fn push(&mut self, key: &str, hub: &H, mark: AgentMark) {
        let slot = self.slots.entry(key.to_string()).or_insert_with(|| Slot { hub: hub.clone(), pending: Vec::new(), last_sent: None });
        slot.pending.retain(|m| !(m.rel == mark.rel && m.tab_id == mark.tab_id && m.agent == mark.agent));
        slot.pending.push(mark);
    }

    /// When the next batch is due, if any is waiting.
    pub fn next_due(&self) -> Option<Instant> {
        self.slots
            .values()
            .filter(|s| !s.pending.is_empty())
            .map(|s| match s.last_sent {
                Some(t) => t + COALESCE,
                None => Instant::now(),
            })
            .min()
    }

    /// The batches due at `now`.
    pub fn take_due(&mut self, now: Instant) -> Vec<(H, Vec<AgentMark>)> {
        let mut out = Vec::new();
        for slot in self.slots.values_mut() {
            if slot.pending.is_empty() || slot.last_sent.is_some_and(|t| now < t + COALESCE) {
                continue;
            }
            slot.last_sent = Some(now);
            out.push((slot.hub.clone(), std::mem::take(&mut slot.pending)));
        }
        // A tree quiet for a while needs no record: its next mark goes at once anyway.
        self.slots.retain(|_, s| !s.pending.is_empty() || s.last_sent.is_some_and(|t| now.duration_since(t) < COALESCE));
        out
    }
}

/// The dispatcher: blocks with no timeout while nothing is pending, so an idle ledger costs no
/// wakeups.
fn dispatch(tree: Weak<FileTreeState>, rx: mpsc::Receiver<Routed>) {
    let mut co: Coalescer<Weak<RootHub>> = Coalescer::default();
    loop {
        let got = match co.next_due() {
            None => rx.recv().map_err(|_| RecvTimeoutError::Disconnected),
            Some(at) => rx.recv_timeout(at.saturating_duration_since(Instant::now())),
        };
        match got {
            Ok(r) => {
                let Some(state) = tree.upgrade() else { return };
                let hubs = state.live_hubs();
                drop(state);
                place(&hubs, r, &mut co);
                // Whatever else is already queued joins this batch.
                while let Ok(r) = rx.try_recv() {
                    place(&hubs, r, &mut co);
                }
            }
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => return,
        }
        for (hub, marks) in co.take_due(Instant::now()) {
            if let Some(h) = hub.upgrade() {
                h.agent_marks(marks);
            }
        }
    }
}

/// Route one mark to every tree that shows its path.
fn place(hubs: &[Arc<RootHub>], r: Routed, co: &mut Coalescer<Weak<RootHub>>) {
    for h in hubs {
        let Some(rel) = h.locate_agent_path(&r.mark.path) else { continue };
        if r.git {
            h.git_signal(Trigger::Agent);
        }
        if h.agents_routing() {
            co.push(&h.place, &Arc::downgrade(h), r.mark.with_rel(rel));
        }
    }
}

#[cfg(test)]
mod tests;
