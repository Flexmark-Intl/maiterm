//! Remote roots: a Files tree on an ssh host (docs/file-tree.md §15).
//!
//! **One helper per host** (`Conn`), whatever the number of tabs, roots or windows: one mux
//! session, inside OpenSSH's default `MaxSessions 10`. It is started over the tab's own
//! connection: `ssh <mux_client_args(tunnel key)> <connect args> exec python3 -u -c '<LOADER>'`
//! muxes over maiTerm's bridge tunnel when there is one (ready in a few hundred ms, no new
//! authentication) and is one direct BatchMode connection otherwise, paid once, never per
//! request. The tree never creates a tunnel or a ControlMaster of its own (the ControlPersist
//! leak history in `ssh_tunnel.rs`). The helper process is killed only through its own child
//! handle, never by pattern.
//!
//! **Lifetime.** Each root holds a lease on its host's helper; the helper outlives its last
//! lease by 5 minutes, so switching back costs nothing. While it lives, Rust pings it every
//! 30 s (the only timer an idle remote tree has) and the helper exits after 60 s without one,
//! when its stdin ends, or when its parent goes: ssh without a terminal sends no hangup.
//!
//! **Failures.** A helper that dies is started again with a backoff that recovers (1 s, 2 s,
//! 4 s … 30 s, and back to 1 s after a success), unlike the editor's remote poll, which gives up
//! for good. Key authentication failing is not retried on its own ("needs key-based ssh"), nor
//! is a host without python3, which gets a read-only `sh -s` per request instead. Each says so
//! in the tree's Health line.
//!
//! **Roots** (`RemoteRoot`) are what a hub holds: listings, stat, git and the find index go
//! through the helper, its inotify events come back as the same `TreeEvent`s a local watcher
//! produces, and file operations run there with the same containment rules.

pub mod proto;

#[cfg(test)]
mod tests;

use std::collections::{BTreeSet, HashMap};
use std::io::{BufReader, Read, Write};
use std::path::Path;
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{mpsc, Arc, Weak};
use std::time::{Duration, Instant};

use parking_lot::{Condvar, Mutex};
use serde_json::{json, Map, Value};

use super::git::{GitOut, Trigger};
use super::hub::RootHub;
use super::list::{self, ListError, Listing};
use super::ops::Tickets;
use super::paths::{join_rel, split_rel};
use super::search::Change;
use super::types::{DeleteTicket, FileOp, Needs, OpFail, OpItem, OpResult, RepoInfo, StatInfo};
use crate::util::proc::RunError;

/// A request's answer may take this long (a listing of a slow folder, a status run).
const CALL_TIMEOUT: Duration = Duration::from_secs(20);
/// Files bigger than this are not sent by an import: one frame holds a whole file.
const MAX_IMPORT_FILE: u64 = 256 << 20;

/// The helper's clock. Fields so tests needn't wait out real intervals.
#[derive(Debug, Clone, Copy)]
pub struct Timing {
    pub ping_every: Duration,
    pub ping_timeout: Duration,
    /// How long a started helper has to say it is ready.
    pub ready_timeout: Duration,
    pub backoff_first: Duration,
    pub backoff_cap: Duration,
    /// How long a helper outlives its last root.
    pub grace: Duration,
    /// Polled folders (out of inotify budget, or no inotify) are checked this often while a
    /// view can see them.
    pub poll_every: Duration,
    /// A host without python3: every open folder is listed again this often, while visible.
    pub limited_poll_every: Duration,
}

impl Default for Timing {
    fn default() -> Self {
        Timing {
            ping_every: Duration::from_secs(30),
            ping_timeout: Duration::from_secs(10),
            ready_timeout: Duration::from_secs(20),
            backoff_first: Duration::from_secs(1),
            backoff_cap: Duration::from_secs(30),
            grace: Duration::from_secs(300),
            poll_every: Duration::from_secs(2),
            limited_poll_every: Duration::from_secs(5),
        }
    }
}

/// Where to connect: the tunnel's own key (it names the ControlMaster socket) and the connect
/// args (`watch::connect_args` form, never a whole command line).
#[derive(Debug, Clone, PartialEq)]
pub struct Target {
    pub tunnel_key: String,
    pub ssh_args: String,
}

/// Finds the connection for a host (a place's host, `connect_args` form) each time the helper
/// starts: a tunnel that came up since is used from then on.
pub type Lookup = Arc<dyn Fn(&str) -> Target + Send + Sync>;

/// No tunnel known: connect to the host directly.
pub fn direct_lookup() -> Lookup {
    Arc::new(|host: &str| Target { tunnel_key: host.to_string(), ssh_args: host.to_string() })
}

/// The tab's bridge tunnel to the host, found the way the transcript mirror and remote watch
/// scripts find it (a tunnel whose `connect_args(host_key)` is the host), else a direct
/// connection: `mux_client_args` then names a socket that isn't there, and ssh connects
/// itself, once, in BatchMode.
pub fn tunnel_lookup(state: &Arc<crate::state::AppState>) -> Lookup {
    let state = state.clone();
    Arc::new(move |host: &str| {
        state
            .ssh_tunnels
            .read()
            .values()
            .find(|t| crate::watch::connect_args(&t.host_key) == host)
            .map(|t| Target { tunnel_key: t.host_key.clone(), ssh_args: crate::watch::connect_args(&t.ssh_args) })
            .unwrap_or_else(|| Target { tunnel_key: host.to_string(), ssh_args: host.to_string() })
    })
}

/// How a helper (and a fallback shell) is started. ssh in the app; local processes in tests.
pub trait Transport: Send + Sync {
    /// The helper: its stdin gets the bootstrap, then frames.
    fn helper(&self, t: &Target) -> Command;
    /// `sh -s` on the host, for a host without python3.
    fn shell(&self, t: &Target) -> Command;
}

pub struct Ssh;

impl Transport for Ssh {
    fn helper(&self, t: &Target) -> Command {
        let mut c = crate::util::proc::command("ssh");
        c.args(proto::ssh_args(&t.tunnel_key, &t.ssh_args, &proto::remote_command()));
        c
    }

    fn shell(&self, t: &Target) -> Command {
        let mut c = crate::util::proc::command("ssh");
        c.args(proto::ssh_args(&t.tunnel_key, &t.ssh_args, "sh -s"));
        c
    }
}

/// Where a host's helper stands. In memory only, never on a wire or in state.
#[derive(Debug, Clone, PartialEq)]
pub enum Status {
    /// Not started yet.
    Idle,
    Connecting,
    Live,
    /// It died or couldn't start; started again after a backoff. The reason, for Health.
    Down(String),
    /// No python3 (or one older than 3.6) on the host: read-only through `sh -s`.
    Limited,
    /// ssh refused to log in without a prompt. Not retried until the human refreshes.
    NeedsKey(String),
    Closed,
}

#[derive(Debug, Clone)]
pub enum CallError {
    /// The helper answered that it couldn't (`code`: gone, denied, exists, outside, …).
    Op { code: String, msg: String },
    Down(String),
    Limited,
    NeedsKey(String),
    Timeout,
}

impl std::fmt::Display for CallError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CallError::Op { msg, .. } => write!(f, "{msg}"),
            CallError::Down(r) => write!(f, "ssh: not connected ({r})"),
            CallError::Limited => write!(f, "ssh: the host has no python3, so Files is read-only there"),
            CallError::NeedsKey(r) => write!(f, "ssh: needs key-based ssh ({r})"),
            CallError::Timeout => write!(f, "ssh: no answer in time"),
        }
    }
}

type Reply = Result<proto::Frame, CallError>;

enum Waiter {
    Sync(mpsc::Sender<Reply>),
    Async(Box<dyn FnOnce(Reply) + Send>),
}

impl Waiter {
    fn answer(self, r: Reply) {
        match self {
            Waiter::Sync(tx) => {
                let _ = tx.send(r);
            }
            Waiter::Async(cb) => cb(r),
        }
    }
}

/// What `hello` said about the host.
#[derive(Debug, Clone, Default)]
pub struct Hello {
    pub hostname: String,
    pub home: String,
    pub inotify: bool,
}

struct Io {
    stdin: Arc<Mutex<ChildStdin>>,
    child: Arc<Mutex<Child>>,
}

struct ConnState {
    status: Status,
    io: Option<Io>,
    /// Bumped by every start: a reader of an older process can't change today's state.
    epoch: u64,
    /// Failures in a row, for the backoff.
    attempt: u32,
    /// A restart is already scheduled.
    retry_scheduled: bool,
    /// Ever live: a later start re-attaches the roots.
    was_live: bool,
    pinger: bool,
}

type Registry = Mutex<HashMap<String, Arc<Conn>>>;

/// One host's helper.
pub struct Conn {
    me: Weak<Conn>,
    /// The place's host, `connect_args` form.
    pub host: String,
    transport: Arc<dyn Transport>,
    lookup: Mutex<Lookup>,
    timing: Timing,
    registry: Weak<Registry>,
    st: Mutex<ConnState>,
    cv: Condvar,
    pending: Mutex<HashMap<u64, Waiter>>,
    roots: Mutex<HashMap<u64, Weak<RemoteRoot>>>,
    next_id: AtomicU64,
    leases: AtomicUsize,
    grace_token: AtomicU64,
    hello: Mutex<Option<Hello>>,
}

/// A root's hold on its host's helper: the last one going starts the grace period. Taken from
/// `Remotes::lease`, under the registry's lock, which the grace period's end also takes: a
/// helper handed out can't be closed before its lease counts.
pub struct Lease(Arc<Conn>);

impl Lease {
    fn new(c: Arc<Conn>) -> Lease {
        c.leases.fetch_add(1, Ordering::SeqCst);
        c.grace_token.fetch_add(1, Ordering::SeqCst);
        Lease(c)
    }

    pub fn conn(&self) -> &Arc<Conn> {
        &self.0
    }
}

impl Drop for Lease {
    fn drop(&mut self) {
        if self.0.leases.fetch_sub(1, Ordering::SeqCst) == 1 {
            self.0.schedule_grace();
        }
    }
}

impl Drop for Conn {
    /// The helper never outlives its connection: killed through its own handle.
    fn drop(&mut self) {
        if let Some(io) = self.st.get_mut().io.take() {
            let mut child = io.child.lock();
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

/// Reads stderr to its end, keeping the last few KB: why a start failed is in there.
fn drain_tail(mut pipe: impl Read + Send + 'static) -> Arc<Mutex<Vec<u8>>> {
    let tail = Arc::new(Mutex::new(Vec::new()));
    let t = tail.clone();
    let spawned = std::thread::Builder::new().name("ft-ssh-err".into()).spawn(move || {
        let mut buf = [0u8; 4096];
        while let Ok(n) = pipe.read(&mut buf) {
            if n == 0 {
                break;
            }
            let mut v = t.lock();
            v.extend_from_slice(&buf[..n]);
            if v.len() > 8192 {
                let cut = v.len() - 8192;
                v.drain(..cut);
            }
        }
    });
    if let Err(e) = spawned {
        log::warn!("filetree: couldn't read a helper's stderr: {e}");
    }
    tail
}

/// The last thing ssh or the shell said, for a person.
fn last_line(stderr: &[u8]) -> String {
    let text = String::from_utf8_lossy(stderr);
    let line = text.lines().map(str::trim).rfind(|l| !l.is_empty()).unwrap_or("");
    line.chars().take(200).collect()
}

/// Why a helper ended, from how it exited and what it printed.
fn classify_exit(code: Option<i32>, stderr: &[u8], was_ready: bool) -> Status {
    let text = String::from_utf8_lossy(stderr).to_lowercase();
    if !was_ready {
        let no_python = text.contains("python3") && (text.contains("not found") || text.contains("no such file") || text.contains("unknown command"));
        // A python older than 3.6 can't even parse the helper.
        if code == Some(127) || no_python || text.contains("syntaxerror") {
            return Status::Limited;
        }
        let auth = text.contains("permission denied") || text.contains("host key verification failed") || text.contains("too many authentication failures");
        if code == Some(255) && auth {
            return Status::NeedsKey(last_line(stderr));
        }
    }
    let why = last_line(stderr);
    Status::Down(if why.is_empty() { "the connection closed".to_string() } else { why })
}

/// Run `cmd` with `input` on its stdin, capturing what it prints, killed at `timeout`.
fn run_with_input(cmd: &mut Command, input: &[u8], timeout: Duration) -> Result<(Option<i32>, Vec<u8>, Vec<u8>), String> {
    cmd.stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped());
    let mut child = cmd.spawn().map_err(|e| format!("ssh could not start: {e}"))?;
    let mut stdin = child.stdin.take();
    let input = input.to_vec();
    let writer = std::thread::spawn(move || {
        if let Some(s) = stdin.as_mut() {
            let _ = s.write_all(&input);
        }
        // Dropped: EOF ends `sh -s`'s script.
    });
    let (otx, orx) = mpsc::channel();
    let (etx, erx) = mpsc::channel();
    let mut out = child.stdout.take();
    let mut err = child.stderr.take();
    std::thread::spawn(move || {
        let mut b = Vec::new();
        if let Some(o) = out.as_mut() {
            let _ = o.read_to_end(&mut b);
        }
        let _ = otx.send(b);
    });
    std::thread::spawn(move || {
        let mut b = Vec::new();
        if let Some(e) = err.as_mut() {
            let _ = e.read_to_end(&mut b);
        }
        let _ = etx.send(b);
    });
    let deadline = Instant::now() + timeout;
    let code = loop {
        match child.try_wait() {
            Ok(Some(s)) => break s.code(),
            Ok(None) if Instant::now() >= deadline => {
                let _ = child.kill();
                let _ = child.wait();
                return Err("no answer in time".into());
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(10)),
            Err(e) => return Err(e.to_string()),
        }
    };
    let _ = writer.join();
    let left = || deadline.saturating_duration_since(Instant::now()).max(Duration::from_millis(100));
    let stdout = orx.recv_timeout(left()).unwrap_or_default();
    let stderr = erx.recv_timeout(left()).unwrap_or_default();
    Ok((code, stdout, stderr))
}

impl Conn {
    fn new(host: &str, transport: Arc<dyn Transport>, lookup: Lookup, registry: Weak<Registry>, timing: Timing) -> Arc<Conn> {
        Arc::new_cyclic(|me| Conn {
            me: me.clone(),
            host: host.to_string(),
            transport,
            lookup: Mutex::new(lookup),
            timing,
            registry,
            st: Mutex::new(ConnState { status: Status::Idle, io: None, epoch: 0, attempt: 0, retry_scheduled: false, was_live: false, pinger: false }),
            cv: Condvar::new(),
            pending: Mutex::new(HashMap::new()),
            roots: Mutex::new(HashMap::new()),
            next_id: AtomicU64::new(1),
            leases: AtomicUsize::new(0),
            grace_token: AtomicU64::new(0),
            hello: Mutex::new(None),
        })
    }

    pub fn status(&self) -> Status {
        self.st.lock().status.clone()
    }

    pub fn hello(&self) -> Option<Hello> {
        self.hello.lock().clone()
    }

    /// The host for a person (`ews@nova`).
    pub fn label(&self) -> String {
        crate::watch::host_label(&self.host)
    }

    fn target(&self) -> Target {
        let lookup = self.lookup.lock().clone();
        lookup(&self.host)
    }

    /// Start the helper if nothing is running or about to: on first use, and on a call after a
    /// failure whose backoff has passed with no restart scheduled (a probe's helper, which no
    /// root leases).
    fn ensure_started(&self) {
        let start = {
            let mut s = self.st.lock();
            let go = match s.status {
                Status::Idle => true,
                Status::Down(_) => !s.retry_scheduled,
                _ => false,
            };
            if go {
                s.status = Status::Connecting;
                s.epoch += 1;
            }
            go.then_some(s.epoch)
        };
        if let Some(epoch) = start {
            let Some(me) = self.me.upgrade() else { return };
            let spawned = std::thread::Builder::new().name("ft-ssh".into()).spawn(move || me.connect(epoch));
            if let Err(e) = spawned {
                log::warn!("filetree: couldn't start the ssh helper thread: {e}");
                self.went_down(epoch, Status::Down(e.to_string()));
            }
            self.broadcast_health();
        }
    }

    /// Start the helper (epoch already counted) and wait for its `ready`.
    fn connect(self: Arc<Self>, epoch: u64) {
        let target = self.target();
        let mut cmd = self.transport.helper(&target);
        cmd.stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped());
        let mut child = match cmd.spawn() {
            Ok(c) => c,
            Err(e) => {
                self.went_down(epoch, Status::Down(format!("ssh could not start: {e}")));
                return;
            }
        };
        let (Some(mut stdin), Some(stdout), Some(stderr)) = (child.stdin.take(), child.stdout.take(), child.stderr.take()) else {
            let _ = child.kill();
            let _ = child.wait();
            self.went_down(epoch, Status::Down("ssh gave no pipes".into()));
            return;
        };
        let tail = drain_tail(stderr);
        // A host that never reads it (ssh failed at once) breaks the pipe: the reader below
        // sees the end and says why.
        let _ = stdin.write_all(&proto::bootstrap()).and_then(|_| stdin.flush());
        let child = Arc::new(Mutex::new(child));
        {
            let mut s = self.st.lock();
            if s.epoch != epoch {
                drop(s);
                let _ = child.lock().kill();
                return;
            }
            s.io = Some(Io { stdin: Arc::new(Mutex::new(stdin)), child: child.clone() });
        }
        let weak = self.me.clone();
        let reader_child = child.clone();
        let spawned = std::thread::Builder::new().name("ft-ssh-read".into()).spawn(move || reader(weak, epoch, stdout, reader_child, tail));
        if let Err(e) = spawned {
            let _ = child.lock().kill();
            self.went_down(epoch, Status::Down(format!("couldn't read the helper: {e}")));
            return;
        }
        let deadline = Instant::now() + self.timing.ready_timeout;
        let mut s = self.st.lock();
        while s.epoch == epoch && s.status == Status::Connecting {
            if self.cv.wait_until(&mut s, deadline).timed_out() {
                break;
            }
        }
        if s.epoch == epoch && s.status == Status::Connecting {
            drop(s);
            // The reader then sees the end and reports it down.
            let _ = child.lock().kill();
        }
    }

    fn became_live(&self, epoch: u64) {
        let reattach = {
            let mut s = self.st.lock();
            if s.epoch != epoch || s.status != Status::Connecting {
                return;
            }
            s.status = Status::Live;
            s.attempt = 0;
            let again = s.was_live;
            s.was_live = true;
            let start_pinger = !s.pinger;
            s.pinger = true;
            self.cv.notify_all();
            if start_pinger {
                self.start_pinger();
            }
            again
        };
        // What the host is, for Health and the wrong-host guard.
        let weak = self.me.clone();
        let _ = self.call_async("hello", json!({}), &[], move |r| {
            let (Some(c), Ok(f)) = (weak.upgrade(), r) else { return };
            let h = &f.header;
            *c.hello.lock() = Some(Hello {
                hostname: h.get("hostname").and_then(Value::as_str).unwrap_or("").to_string(),
                home: h.get("home").and_then(Value::as_str).unwrap_or("").to_string(),
                inotify: h.get("inotify").and_then(|i| i.get("available")).and_then(Value::as_bool).unwrap_or(false),
            });
        });
        if reattach {
            // Every root asks for its folders and watches again: what changed while the helper
            // was gone was seen by nobody.
            let roots = self.live_roots();
            let _ = std::thread::Builder::new().name("ft-ssh-reattach".into()).spawn(move || {
                for r in roots {
                    r.reattach();
                }
            });
        }
        self.broadcast_health();
    }

    /// The process of `epoch` ended (or never started): answer every waiting call, decide what
    /// happens next, and tell the trees.
    fn went_down(&self, epoch: u64, why: Status) {
        let (retry_in, status) = {
            let mut s = self.st.lock();
            if s.epoch != epoch || s.status == Status::Closed {
                return;
            }
            s.io = None;
            let mut retry_in = None;
            if let Status::Down(_) = &why {
                s.attempt = s.attempt.saturating_add(1);
                let n = s.attempt.min(16) - 1;
                let delay = self.timing.backoff_first.saturating_mul(1u32 << n).min(self.timing.backoff_cap);
                if self.leases.load(Ordering::SeqCst) > 0 {
                    s.retry_scheduled = true;
                    retry_in = Some(delay);
                }
            }
            s.status = why.clone();
            self.cv.notify_all();
            (retry_in, why)
        };
        let err = match &status {
            Status::Limited => CallError::Limited,
            Status::NeedsKey(m) => CallError::NeedsKey(m.clone()),
            Status::Down(r) => CallError::Down(r.clone()),
            _ => CallError::Down("closed".into()),
        };
        let waiting: Vec<Waiter> = self.pending.lock().drain().map(|(_, w)| w).collect();
        for w in waiting {
            w.answer(Err(err.clone()));
        }
        if status != Status::Closed {
            log::info!("filetree: ssh helper for {} is {:?}", self.label(), status);
        }
        if let Some(delay) = retry_in {
            let weak = self.me.clone();
            let _ = std::thread::Builder::new().name("ft-ssh-retry".into()).spawn(move || {
                std::thread::sleep(delay);
                let Some(c) = weak.upgrade() else { return };
                c.st.lock().retry_scheduled = false;
                if c.leases.load(Ordering::SeqCst) > 0 {
                    c.ensure_started();
                }
            });
        }
        // A host without python3 is listed by `sh` from now on: everything open again.
        if status == Status::Limited {
            for r in self.live_roots() {
                r.went_limited();
            }
        }
        self.broadcast_health();
    }

    /// The human asked (Refresh): a helper stuck on "needs key", "no python3" or a backoff
    /// tries again now.
    pub fn retry(&self) {
        {
            let mut s = self.st.lock();
            match s.status {
                Status::NeedsKey(_) | Status::Limited | Status::Down(_) => {
                    s.status = Status::Idle;
                    s.retry_scheduled = false;
                    s.attempt = 0;
                }
                _ => return,
            }
        }
        self.ensure_started();
    }

    fn start_pinger(&self) {
        let weak = self.me.clone();
        let every = self.timing.ping_every;
        let timeout = self.timing.ping_timeout;
        let spawned = std::thread::Builder::new().name("ft-ssh-ping".into()).spawn(move || loop {
            std::thread::sleep(every);
            let Some(c) = weak.upgrade() else { return };
            match c.status() {
                Status::Closed => return,
                Status::Live => {
                    if c.call("ping", json!({}), &[], timeout).is_err() && c.status() == Status::Live {
                        // A helper that doesn't answer a ping is wedged or cut off: kill it and
                        // let the backoff start a new one.
                        log::info!("filetree: ssh helper for {} missed a ping", c.label());
                        c.kill_current();
                    }
                }
                _ => {}
            }
        });
        if let Err(e) = spawned {
            log::warn!("filetree: couldn't start the helper's ping: {e}");
        }
    }

    fn kill_current(&self) {
        let child = self.st.lock().io.as_ref().map(|io| io.child.clone());
        if let Some(c) = child {
            let _ = c.lock().kill();
        }
    }

    /// Send a request; `waiter` gets the answer. Waits up to `deadline` for the helper to be
    /// live. The id is returned so a timed-out wait can be forgotten.
    fn send(&self, op: &str, args: Value, payload: &[u8], waiter: Waiter, deadline: Instant) -> Result<u64, (CallError, Waiter)> {
        self.ensure_started();
        let stdin = {
            let mut s = self.st.lock();
            loop {
                match &s.status {
                    Status::Live => break,
                    Status::Limited => return Err((CallError::Limited, waiter)),
                    Status::NeedsKey(m) => return Err((CallError::NeedsKey(m.clone()), waiter)),
                    Status::Closed => return Err((CallError::Down("closed".into()), waiter)),
                    _ => {}
                }
                if self.cv.wait_until(&mut s, deadline).timed_out() {
                    let e = match &s.status {
                        Status::Down(r) => CallError::Down(r.clone()),
                        _ => CallError::Timeout,
                    };
                    return Err((e, waiter));
                }
            }
            s.io.as_ref().map(|io| io.stdin.clone())
        };
        let Some(stdin) = stdin else { return Err((CallError::Down("no connection".into()), waiter)) };
        let id = self.next_id.fetch_add(1, Ordering::SeqCst);
        let mut header = match args {
            Value::Object(m) => m,
            _ => Map::new(),
        };
        header.insert("id".into(), Value::from(id));
        header.insert("op".into(), Value::from(op));
        self.pending.lock().insert(id, waiter);
        let frame = proto::encode(&header, payload);
        let wrote = {
            let mut w = stdin.lock();
            w.write_all(&frame).and_then(|_| w.flush())
        };
        if wrote.is_err() {
            let w = self.pending.lock().remove(&id);
            self.kill_current();
            return match w {
                Some(w) => Err((CallError::Down("the connection broke".into()), w)),
                // The reader already answered it (down).
                None => Ok(id),
            };
        }
        Ok(id)
    }

    /// One request and its answer, blocking up to `timeout` in all.
    pub fn call(&self, op: &str, args: Value, payload: &[u8], timeout: Duration) -> Reply {
        let deadline = Instant::now() + timeout;
        let (tx, rx) = mpsc::channel();
        let id = match self.send(op, args, payload, Waiter::Sync(tx), deadline) {
            Ok(id) => id,
            Err((e, _)) => return Err(e),
        };
        match rx.recv_timeout(deadline.saturating_duration_since(Instant::now())) {
            Ok(r) => r,
            Err(_) => {
                self.pending.lock().remove(&id);
                Err(CallError::Timeout)
            }
        }
    }

    /// `call`, carried through a restart of the helper: a request the dying helper never
    /// answered is sent again to the new one, within the same `timeout`. For reads only: a
    /// listing must not fail (and drop the folder from the view) for a blip the backoff heals.
    pub fn call_through(&self, op: &str, args: Value, payload: &[u8], timeout: Duration) -> Reply {
        let deadline = Instant::now() + timeout;
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            match self.call(op, args.clone(), payload, left) {
                Err(CallError::Down(_)) if Instant::now() < deadline && self.status() != Status::Closed => {
                    // `call` waits for the helper to be live again; this keeps a fast failure
                    // (a restart that fails at once) from spinning.
                    std::thread::sleep(Duration::from_millis(50));
                }
                other => return other,
            }
        }
    }

    /// A request whose answer goes to `cb`, on the reader's thread (so `cb` must not call the
    /// helper itself). Sent only while live: false otherwise, and `cb` is never called (what
    /// it was for is sent again when the helper is back).
    pub fn call_async(&self, op: &str, args: Value, payload: &[u8], cb: impl FnOnce(Reply) + Send + 'static) -> bool {
        if self.status() != Status::Live {
            return false;
        }
        self.send(op, args, payload, Waiter::Async(Box::new(cb)), Instant::now()).is_ok()
    }

    fn dispatch(&self, f: proto::Frame) {
        if let Some(id) = f.id() {
            let Some(w) = self.pending.lock().remove(&id) else { return };
            let ok = f.header.get("ok").and_then(Value::as_bool).unwrap_or(false);
            if ok {
                w.answer(Ok(f));
            } else {
                let code = f.str("code").unwrap_or("io").to_string();
                let msg = f.str("msg").unwrap_or("the helper failed").to_string();
                w.answer(Err(CallError::Op { code, msg }));
            }
            return;
        }
        if f.str("ev") == Some("fs") {
            let tag = f.header.get("tag").and_then(Value::as_u64).unwrap_or(0);
            let root = self.roots.lock().get(&tag).and_then(Weak::upgrade);
            if let Some(r) = root {
                r.fs_event(&f);
            }
        }
    }

    fn live_roots(&self) -> Vec<Arc<RemoteRoot>> {
        self.roots.lock().values().filter_map(Weak::upgrade).collect()
    }

    fn broadcast_health(&self) {
        for r in self.live_roots() {
            if let Some(h) = r.hub() {
                h.broadcast_health();
            }
        }
    }

    fn schedule_grace(&self) {
        let token = self.grace_token.fetch_add(1, Ordering::SeqCst) + 1;
        let weak = self.me.clone();
        let grace = self.timing.grace;
        let spawned = std::thread::Builder::new().name("ft-ssh-grace".into()).spawn(move || {
            std::thread::sleep(grace);
            let Some(c) = weak.upgrade() else { return };
            // Decided under the registry's lock, where `Remotes::lease` hands this helper out
            // and counts the lease: checked before it, a root opening at this moment got the
            // helper and then had it closed under it, for good (a closed helper never restarts).
            let reg = c.registry.upgrade();
            {
                let mut map = reg.as_ref().map(|r| r.lock());
                if c.grace_token.load(Ordering::SeqCst) != token || c.leases.load(Ordering::SeqCst) > 0 {
                    return;
                }
                if let Some(map) = map.as_mut() {
                    if map.get(&c.host).is_some_and(|x| Arc::ptr_eq(x, &c)) {
                        map.remove(&c.host);
                    }
                }
            }
            c.close();
        });
        if let Err(e) = spawned {
            log::warn!("filetree: couldn't schedule the ssh helper's end: {e}");
        }
    }

    /// End the helper for good.
    fn close(&self) {
        let io = {
            let mut s = self.st.lock();
            s.status = Status::Closed;
            self.cv.notify_all();
            s.io.take()
        };
        if let Some(io) = io {
            drop(io.stdin);
            let _ = io.child.lock().kill();
        }
        let waiting: Vec<Waiter> = self.pending.lock().drain().map(|(_, w)| w).collect();
        for w in waiting {
            w.answer(Err(CallError::Down("closed".into())));
        }
    }

    /// One `sh -s` run on the host (the no-python fallback), the script on stdin.
    pub fn shell(&self, script: &str, timeout: Duration) -> Result<Vec<u8>, CallError> {
        let target = self.target();
        let mut cmd = self.transport.shell(&target);
        match run_with_input(&mut cmd, script.as_bytes(), timeout) {
            Ok((_, out, _)) if !out.is_empty() => Ok(out),
            Ok((code, _, err)) => match classify_exit(code, &err, false) {
                Status::NeedsKey(m) => Err(CallError::NeedsKey(m)),
                Status::Down(r) => Err(CallError::Down(r)),
                _ => Err(CallError::Down("no answer".into())),
            },
            Err(e) if e.contains("in time") => Err(CallError::Timeout),
            Err(e) => Err(CallError::Down(e)),
        }
    }

    /// The resolver's question for an ssh tab's candidate folders: each one's real path when it
    /// is a folder there, and its repository's top level. None when the host can't be asked
    /// in time (the resolver then keeps its first candidate).
    pub fn probe(&self, paths: &[String], timeout: Duration) -> Option<Vec<proto::Probed>> {
        match self.call("probe", json!({ "paths": paths }), &[], timeout) {
            Ok(f) => {
                let items = f.header.get("items")?.as_array()?;
                Some(
                    items
                        .iter()
                        .map(|i| {
                            let s = |k: &str| i.get(k).and_then(Value::as_str).map(String::from);
                            let is_dir = i.get("dir").and_then(Value::as_bool).unwrap_or(false);
                            proto::Probed { real: s("real").filter(|_| is_dir), top: s("top") }
                        })
                        .collect(),
                )
            }
            Err(CallError::Limited) => {
                let marker = new_marker();
                let out = self.shell(&proto::sh_probe_script(paths, &marker), timeout).ok()?;
                proto::parse_sh_probe(&out, &marker, paths.len())
            }
            Err(_) => None,
        }
    }
}

fn new_marker() -> String {
    format!("__MAITERM_FT_{}__", uuid::Uuid::new_v4().simple())
}

/// The reader of one helper process: frames to their callers and roots, then, at the end of the
/// stream, how it ended.
fn reader(weak: Weak<Conn>, epoch: u64, stdout: std::process::ChildStdout, child: Arc<Mutex<Child>>, tail: Arc<Mutex<Vec<u8>>>) {
    let mut r = BufReader::with_capacity(1 << 18, stdout);
    let mut ready = false;
    loop {
        match proto::read_frame(&mut r) {
            Ok(proto::Read::Frame(f)) => {
                let Some(conn) = weak.upgrade() else { break };
                if !ready {
                    // Before `ready` anything else is a login profile talking.
                    if f.is_ready() {
                        ready = true;
                        conn.became_live(epoch);
                    }
                    continue;
                }
                conn.dispatch(f);
            }
            // Noise before `ready` is skipped; after it the stream is broken.
            Ok(proto::Read::Noise) if !ready => continue,
            Ok(_) | Err(_) => break,
        }
    }
    // The process ends (or is ended) and is reaped here.
    let deadline = Instant::now() + Duration::from_secs(2);
    let code = loop {
        let got = child.lock().try_wait();
        match got {
            Ok(Some(s)) => break s.code(),
            Ok(None) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(20)),
            _ => {
                let mut c = child.lock();
                let _ = c.kill();
                break c.wait().ok().and_then(|s| s.code());
            }
        }
    };
    // Let the stderr reader catch the last line.
    std::thread::sleep(Duration::from_millis(30));
    let stderr = tail.lock().clone();
    if let Some(conn) = weak.upgrade() {
        conn.went_down(epoch, classify_exit(code, &stderr, ready));
    }
}

/// Every host's helper, by host.
pub struct Remotes {
    map: Arc<Registry>,
    transport: Mutex<Arc<dyn Transport>>,
    timing: Mutex<Timing>,
}

impl Default for Remotes {
    fn default() -> Self {
        Remotes { map: Arc::new(Mutex::new(HashMap::new())), transport: Mutex::new(Arc::new(Ssh)), timing: Mutex::new(Timing::default()) }
    }
}

impl Remotes {
    /// Test seam: how helpers start, and their clock.
    #[cfg(test)]
    pub(crate) fn set_transport(&self, t: Arc<dyn Transport>, timing: Timing) {
        *self.transport.lock() = t;
        *self.timing.lock() = timing;
    }

    /// A hold on the helper for `host`, made if there is none. The lease is counted before the
    /// registry's lock is let go, so the grace period of the helper's last root can't close it
    /// between here and the caller's first call.
    pub fn lease(&self, host: &str, lookup: Option<Lookup>) -> Lease {
        let mut map = self.map.lock();
        if let Some(c) = map.get(host) {
            if c.status() != Status::Closed {
                if let Some(l) = lookup {
                    *c.lookup.lock() = l;
                }
                return Lease::new(c.clone());
            }
        }
        let c = Conn::new(host, self.transport.lock().clone(), lookup.unwrap_or_else(direct_lookup), Arc::downgrade(&self.map), *self.timing.lock());
        map.insert(host.to_string(), c.clone());
        Lease::new(c)
    }

    /// The helper for `host`, made if there is none, held by nobody: it ends after the grace
    /// period like any other unless a root leases it.
    pub fn conn(&self, host: &str, lookup: Option<Lookup>) -> Arc<Conn> {
        self.lease(host, lookup).conn().clone()
    }

    #[cfg(test)]
    pub(crate) fn count(&self) -> usize {
        self.map.lock().len()
    }
}

// ── One root on a host ──────────────────────────────────────────────────────────────────

static NEXT_TAG: AtomicU64 = AtomicU64::new(1);

/// A hub's root on an ssh host.
pub struct RemoteRoot {
    me: Weak<RemoteRoot>,
    pub conn: Arc<Conn>,
    _lease: Lease,
    /// Names this root's watches and events in the helper.
    pub tag: u64,
    /// The place's host (`connect_args` form): agent paths on it are `host:/abs`.
    pub host: String,
    /// The real POSIX path there.
    pub root: String,
    pub repo: Option<RepoInfo>,
    hub: Mutex<Weak<RootHub>>,
    excludes: Mutex<Vec<String>>,
    /// The open folders asked to be watched, sent again after a reconnect.
    watched: Mutex<BTreeSet<String>>,
    /// The ones the helper polls instead (budget, no inotify).
    polled: Mutex<BTreeSet<String>>,
    inotify: AtomicBool,
    poller: AtomicBool,
}

fn parse_repo(v: &Value) -> Option<RepoInfo> {
    let s = |k: &str| v.get(k).and_then(Value::as_str).map(String::from);
    Some(RepoInfo { toplevel_rel: s("toplevel_rel")?, git_dir: s("git_dir")?, common_dir: s("common_dir")? })
}

fn strings(v: Option<&Value>) -> Vec<String> {
    v.and_then(Value::as_array).map(|a| a.iter().filter_map(Value::as_str).map(String::from).collect()).unwrap_or_default()
}

impl RemoteRoot {
    /// Open `root` (POSIX, maybe `~`-based) on the leased helper's host: its real path and
    /// repository. Blocking: the first open of a host starts its helper. The lease
    /// (`Remotes::lease`) was taken before the first call: a grace period ending while the
    /// helper starts must not close it under this root.
    pub fn open(lease: Lease, root: &str, excludes: Vec<String>) -> Result<Arc<RemoteRoot>, String> {
        let conn = lease.conn().clone();
        let (real, repo) = match conn.call("root", json!({ "root": root, "excludes": excludes }), &[], CALL_TIMEOUT) {
            Ok(f) => (f.str("real").unwrap_or(root).to_string(), f.header.get("repo").and_then(parse_repo)),
            Err(CallError::Limited) => {
                let real = conn.probe(&[root.to_string()], CALL_TIMEOUT).and_then(|p| p.into_iter().next()).and_then(|p| p.real);
                match real {
                    Some(r) => (r, None),
                    None => return Err(format!("Folder missing on {}: {root}", conn.label())),
                }
            }
            Err(CallError::Op { code, .. }) if code == "gone" => return Err(format!("Folder missing on {}: {root}", conn.label())),
            Err(e) => return Err(e.to_string()),
        };
        let tag = NEXT_TAG.fetch_add(1, Ordering::SeqCst);
        let r = Arc::new_cyclic(|me| RemoteRoot {
            me: me.clone(),
            host: conn.host.clone(),
            conn: conn.clone(),
            _lease: lease,
            tag,
            root: real,
            repo,
            hub: Mutex::new(Weak::new()),
            excludes: Mutex::new(excludes),
            watched: Mutex::new(BTreeSet::new()),
            polled: Mutex::new(BTreeSet::new()),
            inotify: AtomicBool::new(conn.hello().is_some_and(|h| h.inotify)),
            poller: AtomicBool::new(false),
        });
        conn.roots.lock().insert(tag, Arc::downgrade(&r));
        Ok(r)
    }

    pub fn hub(&self) -> Option<Arc<RootHub>> {
        self.hub.lock().upgrade()
    }

    /// The hub that shows this root: its events go there. The repository's own folders are
    /// watched from now on.
    pub fn attach(&self, hub: Weak<RootHub>) {
        *self.hub.lock() = hub;
        self.send_watch(Vec::new(), true);
        if self.conn.status() == Status::Limited {
            self.ensure_poller();
        }
    }

    fn git_dirs(&self) -> Vec<String> {
        let Some(r) = &self.repo else { return Vec::new() };
        let mut v = vec![r.git_dir.clone()];
        if r.common_dir != r.git_dir {
            v.push(r.common_dir.clone());
        }
        v
    }

    fn send_watch(&self, rels: Vec<String>, git: bool) {
        let git = if git { self.git_dirs() } else { Vec::new() };
        if rels.is_empty() && git.is_empty() {
            return;
        }
        let me = self.me.clone();
        self.conn.call_async("watch", json!({ "root": self.root, "tag": self.tag, "rels": rels, "git": git }), &[], move |r| {
            if let (Some(me), Ok(f)) = (me.upgrade(), r) {
                me.watch_answer(&f);
            }
        });
    }

    fn watch_answer(&self, f: &proto::Frame) {
        let polled: BTreeSet<String> = strings(f.header.get("polled")).into_iter().collect();
        let inotify = f.header.get("inotify").and_then(Value::as_bool).unwrap_or(false);
        let inotify_changed = self.inotify.swap(inotify, Ordering::SeqCst) != inotify;
        let any = {
            let mut p = self.polled.lock();
            let changed = *p != polled;
            *p = polled;
            if !changed && !inotify_changed {
                return;
            }
            !p.is_empty()
        };
        if any {
            self.ensure_poller();
        }
        if let Some(h) = self.hub() {
            h.broadcast_health();
        }
    }

    /// The hub opened `rel`: watch it there (or poll it, out of budget).
    pub fn watch(&self, rel: &str) {
        if !self.watched.lock().insert(rel.to_string()) {
            return;
        }
        match self.conn.status() {
            Status::Live => self.send_watch(vec![rel.to_string()], false),
            Status::Limited => self.ensure_poller(),
            // Sent with the rest when the helper is live again.
            _ => {}
        }
    }

    pub fn unwatch(&self, rel: &str) {
        if !self.watched.lock().remove(rel) {
            return;
        }
        let was_polled = self.polled.lock().remove(rel);
        self.conn.call_async("unwatch", json!({ "root": self.root, "tag": self.tag, "rels": [rel] }), &[], |_| {});
        if was_polled {
            if let Some(h) = self.hub() {
                h.broadcast_health();
            }
        }
    }

    /// Polled folders are checked while a view can see them (unpaused, in a focused window);
    /// on a host without python3, every open folder is listed again. One thread, only while
    /// there is something to poll.
    fn ensure_poller(&self) {
        if self.poller.swap(true, Ordering::SeqCst) {
            return;
        }
        let weak = self.me.clone();
        let timing = self.conn.timing;
        let spawned = std::thread::Builder::new().name("ft-remote-poll".into()).spawn(move || loop {
            let limited = weak.upgrade().is_some_and(|me| me.conn.status() == Status::Limited);
            std::thread::sleep(if limited { timing.limited_poll_every } else { timing.poll_every });
            let Some(me) = weak.upgrade() else { return };
            let Some(hub) = me.hub() else {
                me.poller.store(false, Ordering::SeqCst);
                return;
            };
            let status = me.conn.status();
            {
                let polled = me.polled.lock();
                if status != Status::Limited && polled.is_empty() {
                    me.poller.store(false, Ordering::SeqCst);
                    return;
                }
            }
            if !hub.poll_allowed() {
                continue;
            }
            match status {
                Status::Limited => {
                    for rel in hub.open_dirs() {
                        hub.request(&rel, None);
                    }
                }
                Status::Live => {
                    let rels: Vec<String> = me.polled.lock().iter().cloned().collect();
                    if let Ok(f) = me.conn.call("poll", json!({ "tag": me.tag, "rels": rels }), &[], Duration::from_secs(10)) {
                        hub.remote_changed(&strings(f.header.get("changed")));
                    }
                }
                _ => {}
            }
        });
        if let Err(e) = spawned {
            self.poller.store(false, Ordering::SeqCst);
            log::warn!("filetree: couldn't start a remote poller: {e}");
        }
    }

    /// The helper is back after a restart: the root again, every watch again, every open folder
    /// listed again, and git looks again.
    fn reattach(&self) {
        let excludes = self.excludes.lock().clone();
        if let Err(e) = self.conn.call("root", json!({ "root": self.root, "excludes": excludes }), &[], CALL_TIMEOUT) {
            log::info!("filetree: re-opening {} on {}: {e}", self.root, self.conn.label());
        }
        self.polled.lock().clear();
        let rels: Vec<String> = self.watched.lock().iter().cloned().collect();
        self.send_watch(rels, true);
        if let Some(h) = self.hub() {
            // Every folder a view has open, snapshot or not: one whose listing failed while the
            // helper was gone has none, and is exactly the one that must be listed again.
            for rel in h.open_dirs() {
                h.request(&rel, None);
            }
            h.git_signal(Trigger::Open);
            h.broadcast_health();
        }
    }

    /// The host turned out to have no python3: list everything through `sh` from now on.
    fn went_limited(&self) {
        self.ensure_poller();
        if let Some(h) = self.hub() {
            for rel in h.open_dirs() {
                h.request(&rel, None);
            }
        }
    }

    /// The helper's watch saw changes under this root (coalesced over 150 ms).
    fn fs_event(&self, f: &proto::Frame) {
        let Some(hub) = self.hub() else { return };
        let flag = |k: &str| f.header.get(k).and_then(Value::as_bool).unwrap_or(false);
        if flag("many") {
            // Events were lost (the queue overflowed, or too many to name): look again.
            hub.revalidate_under("");
            if hub.search.wanted() {
                hub.search.note(vec![Change::Folder]);
            }
            hub.git_signal(Trigger::Worktree);
            if flag("index") {
                hub.git_signal(Trigger::Index);
            }
            return;
        }
        hub.remote_changed(&strings(f.header.get("dirs")));
        if flag("index") {
            hub.git_signal(Trigger::Index);
        }
        if flag("worktree") {
            hub.git_signal(Trigger::Worktree);
        }
        if hub.search.wanted() {
            let changes: Vec<Change> = f
                .header
                .get("paths")
                .and_then(Value::as_array)
                .map(|a| {
                    a.iter()
                        .filter_map(|p| {
                            let rel = p.get(0)?.as_str()?.to_string();
                            Some(match p.get(1)?.as_str()? {
                                "f" => Change::File(rel),
                                "d" => Change::Folder,
                                _ => Change::Gone(rel),
                            })
                        })
                        .collect()
                })
                .unwrap_or_default();
            hub.search.note(changes);
        }
    }

    /// "live" | "partial" | "poll" | "none", and the footer's note (None: healthy).
    pub fn health(&self) -> (&'static str, Option<String>) {
        let label = self.conn.label();
        match self.conn.status() {
            Status::Idle | Status::Connecting => ("live", Some(format!("ssh: connecting to {label}…"))),
            Status::Live => {
                let n = self.polled.lock().len();
                if !self.inotify.load(Ordering::SeqCst) {
                    ("poll", Some(format!("watch: polling open folders (no inotify on {label})")))
                } else if n > 0 {
                    ("partial", Some(format!("watch: polling {n} folder{} (inotify limit on {label})", if n == 1 { "" } else { "s" })))
                } else {
                    ("live", None)
                }
            }
            Status::Down(why) => ("none", Some(format!("ssh: reconnecting to {label}… ({why})"))),
            Status::Limited => ("poll", Some(format!("ssh: limited (no python3 on {label}), read-only"))),
            Status::NeedsKey(_) => ("none", Some(format!("ssh: needs key-based ssh to {label}"))),
            Status::Closed => ("none", Some(format!("ssh: {label} closed"))),
        }
    }

    /// File operations and the trash work (and Find has an index) only with the helper.
    pub fn writable(&self) -> bool {
        self.conn.status() != Status::Limited
    }

    /// How much of an agent's shell writes this tree sees: the open folders while watched.
    pub fn coverage(&self) -> &'static str {
        if self.conn.status() == Status::Live && self.inotify.load(Ordering::SeqCst) {
            "expanded_only"
        } else {
            "none"
        }
    }

    /// The user's `file_tree_exclude` changed: the helper's rules follow before anything is
    /// listed again.
    pub fn set_excludes(&self, excludes: &[String]) {
        *self.excludes.lock() = excludes.to_vec();
        if let Err(e) = self.conn.call("root", json!({ "root": self.root, "excludes": excludes }), &[], Duration::from_secs(5)) {
            log::info!("filetree: excludes for {} on {}: {e}", self.root, self.conn.label());
        }
    }

    /// One folder's listing, sorted and capped as locally.
    pub fn list(&self, rel: &str) -> Result<Listing, ListError> {
        match self.conn.call_through("list", json!({ "root": self.root, "rel": rel }), &[], CALL_TIMEOUT) {
            Ok(f) => {
                let total = f.header.get("total").and_then(Value::as_u64).unwrap_or(0) as usize;
                Ok(list::finish(proto::parse_listing(&f.payload), total))
            }
            Err(CallError::Op { code, msg }) => Err(proto::list_error(&code, &msg)),
            Err(CallError::Limited) => self.sh_list(rel),
            Err(e) => Err(ListError::Other(e.to_string())),
        }
    }

    fn sh_list(&self, rel: &str) -> Result<Listing, ListError> {
        let marker = new_marker();
        let script = proto::sh_list_script(&self.root, rel, &marker).map_err(ListError::Other)?;
        let out = self.conn.shell(&script, CALL_TIMEOUT).map_err(|e| ListError::Other(e.to_string()))?;
        match proto::parse_sh_list(&out, &marker) {
            Some(Ok(entries)) => {
                let n = entries.len();
                Ok(list::finish(entries, n))
            }
            Some(Err(e)) => Err(e),
            None => Err(ListError::Other(format!("{} sent back no listing", self.conn.label()))),
        }
    }

    /// The hover's details, and the file's stamp (size, mtime) for the numstat cache.
    pub fn stat(&self, rel: &str) -> Result<(StatInfo, bool, super::git::FileStamp), String> {
        let f = self.conn.call_through("stat", json!({ "root": self.root, "rel": rel }), &[], Duration::from_secs(10)).map_err(|e| e.to_string())?;
        let h = &f.header;
        let size = h.get("size").and_then(Value::as_u64).unwrap_or(0);
        let mtime_ms = h.get("mtime_ms").and_then(Value::as_i64).unwrap_or(0);
        let is_file = h.get("file").and_then(Value::as_bool).unwrap_or(false);
        let stamp_time = (mtime_ms >= 0).then(|| std::time::UNIX_EPOCH + Duration::from_millis(mtime_ms as u64));
        Ok((
            StatInfo {
                size,
                mtime_ms,
                exec: h.get("exec").and_then(Value::as_bool).unwrap_or(false),
                link_target: h.get("link").and_then(Value::as_str).map(String::from),
                numstat: None,
            },
            is_file,
            (size, stamp_time),
        ))
    }

    /// One git command in the root, there (a fixed set of read-only subcommands).
    pub fn git(&self, args: &[String], timeout: Duration) -> Result<GitOut, RunError> {
        let req = json!({ "root": self.root, "args": args, "timeout": timeout.as_secs_f64() });
        match self.conn.call("git", req, &[], timeout + Duration::from_secs(10)) {
            Ok(f) => {
                let stderr = f.str("stderr").unwrap_or("").as_bytes().to_vec();
                Ok(GitOut { ok: f.header.get("code").and_then(Value::as_i64) == Some(0), stdout: f.payload, stderr })
            }
            Err(CallError::Op { code, msg }) if code == "missing" => Err(RunError::Spawn(std::io::Error::new(std::io::ErrorKind::NotFound, msg))),
            Err(CallError::Op { code, .. }) if code == "timeout" => Err(RunError::TimedOut),
            Err(CallError::Timeout) => Err(RunError::TimedOut),
            Err(e) => Err(RunError::Io(std::io::Error::other(e.to_string()))),
        }
    }

    /// Every file Find should know.
    pub fn files(&self) -> Result<Vec<String>, String> {
        let f = self.conn.call("files", json!({ "root": self.root }), &[], Duration::from_secs(60)).map_err(|e| e.to_string())?;
        Ok(f.payload.split(|b| *b == 0).filter(|p| !p.is_empty()).map(|p| String::from_utf8_lossy(p).into_owned()).collect())
    }

    /// A rel of this root as the host's absolute path.
    pub fn abs(&self, rel: &str) -> String {
        if rel.is_empty() {
            self.root.clone()
        } else {
            format!("{}/{rel}", self.root.trim_end_matches('/'))
        }
    }

    /// `abs` (an absolute path on the host) as a rel of this root; None outside it.
    pub fn locate(&self, abs: &str) -> Option<String> {
        let base = self.root.trim_end_matches('/');
        let rest = if abs == base || (base.is_empty() && abs == "/") { "" } else { abs.strip_prefix(base)?.strip_prefix('/')? };
        // Lexically: `.` dropped, `..` refused (it could step out).
        let mut parts = Vec::new();
        for p in rest.split('/') {
            match p {
                "" | "." => {}
                ".." => return None,
                p => parts.push(p),
            }
        }
        Some(parts.join("/"))
    }
}

impl Drop for RemoteRoot {
    fn drop(&mut self) {
        self.conn.roots.lock().remove(&self.tag);
        self.conn.call_async("forget", json!({ "tag": self.tag }), &[], |_| {});
    }
}

// ── File operations on a remote root ────────────────────────────────────────────────────

/// What a remote op runs against.
pub struct RemoteEnv<'a> {
    pub place: &'a str,
    pub remote: &'a RemoteRoot,
    pub tickets: &'a Tickets,
    /// Agent sessions working on this host: (tab id, tab name, their cwd there).
    pub holders: &'a dyn Fn() -> Vec<(String, String, String)>,
}

fn parse_items(v: Option<&Value>) -> Vec<OpItem> {
    v.and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(|i| Some(OpItem { from: i.get("from")?.as_str()?.to_string(), to: i.get("to").and_then(Value::as_str).map(String::from) }))
                .collect()
        })
        .unwrap_or_default()
}

fn parse_fails(v: Option<&Value>) -> Vec<OpFail> {
    v.and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(|i| {
                    let s = |k: &str| i.get(k).and_then(Value::as_str).map(String::from);
                    Some(OpFail { rel: s("rel")?, code: s("code")?, msg: s("msg").unwrap_or_default() })
                })
                .collect()
        })
        .unwrap_or_default()
}

fn parse_result(h: &Map<String, Value>) -> OpResult {
    OpResult {
        done: parse_items(h.get("done")),
        failed: parse_fails(h.get("failed")),
        skipped: strings(h.get("skipped")),
        undo: h.get("undo").filter(|u| !u.is_null()).and_then(|u| serde_json::from_value::<FileOp>(u.clone()).ok()),
        needs: h.get("needs").filter(|n| !n.is_null()).and_then(|n| {
            let s = |k: &str| n.get(k).and_then(Value::as_str).map(String::from);
            Some(Needs { code: s("code")?, detail: s("detail").unwrap_or_default(), tabs: strings(n.get("tabs")) })
        }),
    }
}

/// Keep-both names in the style of the file manager the human uses here (§10.2).
fn copy_style() -> &'static str {
    if cfg!(target_os = "macos") {
        "mac"
    } else if cfg!(windows) {
        "windows"
    } else {
        "linux"
    }
}

/// The live-cwd guard on a host: agent sessions there working in an item (or below it).
fn live_cwd_guard(env: &RemoteEnv, rels: &[String]) -> Option<Needs> {
    let items: Vec<String> = rels.iter().map(|r| env.remote.abs(r)).collect();
    let home = env.remote.conn.hello().map(|h| h.home).unwrap_or_default();
    let mut tabs: Vec<(String, String)> = Vec::new();
    for (id, name, cwd) in (env.holders)() {
        let cwd = match cwd.strip_prefix('~') {
            Some(rest) if !home.is_empty() && (rest.is_empty() || rest.starts_with('/')) => format!("{home}{rest}"),
            _ => cwd,
        };
        let inside = items.iter().any(|i| cwd == *i || cwd.starts_with(&format!("{}/", i.trim_end_matches('/'))));
        if inside && !tabs.iter().any(|(t, _)| *t == id) {
            tabs.push((id, name));
        }
    }
    super::ops::live_cwd_needs(tabs)
}

/// One op on a remote root (§10, §15.2): refused whole for a bad rel or ticket, guarded by the
/// live-cwd rule, run by the helper with the same containment rules and result as locally.
pub fn run_op(env: &RemoteEnv, op: &FileOp) -> Result<OpResult, String> {
    if !env.remote.writable() {
        return Err(format!("Files is read-only on {}: it has no python3", env.remote.conn.label()));
    }
    for r in op.src.iter().chain(op.to.iter()).chain(op.dest.iter()) {
        if op.kind != "import" || op.dest.as_ref() == Some(r) {
            split_rel(r)?;
        }
    }
    match op.conflict.as_str() {
        "" | "fail" | "keep_both" | "skip" => {}
        other => return Err(format!("Unknown conflict rule: {other}")),
    }
    if op.kind == "import" {
        return import(env, op);
    }
    let moves_or_removes = matches!(op.kind.as_str(), "rename" | "move" | "trash" | "delete");
    if op.kind == "delete" {
        env.tickets.check(op.token.as_deref(), env.place, &op.src)?;
    }
    if moves_or_removes {
        if let Some(needs) = live_cwd_guard(env, &op.src) {
            return Ok(OpResult { needs: Some(needs), ..Default::default() });
        }
    }
    if op.kind == "delete" {
        env.tickets.take(op.token.as_deref(), env.place, &op.src)?;
    }
    let file = serde_json::to_value(op).map_err(|e| e.to_string())?;
    let req = json!({ "root": env.remote.root, "file": file, "style": copy_style() });
    match env.remote.conn.call("fileop", req, &[], Duration::from_secs(300)) {
        Ok(f) => Ok(parse_result(&f.header)),
        Err(e) => Err(e.to_string()),
    }
}

/// `ft_prepare_delete` on a remote root.
pub fn prepare_delete(env: &RemoteEnv, rels: &[String]) -> Result<DeleteTicket, String> {
    for r in rels {
        split_rel(r)?;
    }
    let f = env.remote.conn.call("tally", json!({ "root": env.remote.root, "rels": rels }), &[], Duration::from_secs(60)).map_err(|e| e.to_string())?;
    let n = |k: &str| f.header.get(k).and_then(Value::as_u64).unwrap_or(0);
    Ok(DeleteTicket { token: env.tickets.mint(env.place, rels), files: n("files"), dirs: n("dirs") })
}

/// Files dropped from this computer onto a remote root: each item is walked here, without
/// following links, and made there piece by piece, each piece refusing to replace anything.
fn import(env: &RemoteEnv, op: &FileOp) -> Result<OpResult, String> {
    let r = env.remote;
    let dest = op.dest.clone().unwrap_or_default();
    if op.src.is_empty() {
        return Err("No items given".into());
    }
    for s in &op.src {
        if !Path::new(s).is_absolute() {
            return Err(format!("{s}: an import takes absolute paths"));
        }
    }
    let mut result = OpResult::default();
    for src in &op.src {
        let p = Path::new(src);
        let Ok(meta) = std::fs::symlink_metadata(p) else {
            result.failed.push(OpFail { rel: src.clone(), code: "gone".into(), msg: "It is no longer there".into() });
            continue;
        };
        let Some(name) = p.file_name().map(|n| n.to_string_lossy().into_owned()) else {
            result.failed.push(OpFail { rel: src.clone(), code: "io".into(), msg: "It has no name to copy under".into() });
            continue;
        };
        let req = json!({ "root": r.root, "dest": dest, "name": name, "is_dir": meta.is_dir(), "conflict": op.conflict, "style": copy_style() });
        let landed = match r.conn.call("land", req, &[], CALL_TIMEOUT) {
            Ok(f) => match f.str("name") {
                Some(n) => n.to_string(),
                None if f.header.get("skip").and_then(Value::as_bool) == Some(true) => {
                    result.skipped.push(src.clone());
                    continue;
                }
                None => {
                    result.failed.push(OpFail { rel: src.clone(), code: "exists".into(), msg: format!("“{name}” is already there") });
                    continue;
                }
            },
            Err(e) => return Err(e.to_string()),
        };
        let to = join_rel(&dest, &landed);
        let mut made = false;
        match upload(r, p, &to, &mut made) {
            Ok(()) => result.done.push(OpItem { from: src.clone(), to: Some(to) }),
            Err((code, msg)) => {
                // Only what this import made goes: something another writer put there first
                // made the first piece fail, and is left alone.
                if made {
                    let _ = r.conn.call("discard", json!({ "root": r.root, "rel": to }), &[], CALL_TIMEOUT);
                }
                result.failed.push(OpFail { rel: src.clone(), code, msg });
            }
        }
    }
    let made: Vec<String> = result.done.iter().filter_map(|i| i.to.clone()).collect();
    result.undo = (!made.is_empty()).then(|| FileOp { kind: "trash".into(), src: made, ..Default::default() });
    Ok(result)
}

fn upload(r: &RemoteRoot, local: &Path, rel: &str, made: &mut bool) -> Result<(), (String, String)> {
    let io = |e: std::io::Error| ("io".to_string(), e.to_string());
    let remote_err = |e: CallError| match e {
        CallError::Op { code, msg } => (if code == "exists" { code } else { "io".to_string() }, msg),
        other => ("io".to_string(), other.to_string()),
    };
    let meta = std::fs::symlink_metadata(local).map_err(io)?;
    if meta.file_type().is_symlink() {
        let target = std::fs::read_link(local).map_err(io)?.to_string_lossy().into_owned();
        r.conn.call("symlink", json!({ "root": r.root, "rel": rel, "target": target }), &[], CALL_TIMEOUT).map_err(remote_err)?;
        *made = true;
    } else if meta.is_dir() {
        r.conn.call("mkdir", json!({ "root": r.root, "rel": rel }), &[], CALL_TIMEOUT).map_err(remote_err)?;
        *made = true;
        let mut names: Vec<_> = std::fs::read_dir(local).map_err(io)?.flatten().map(|e| e.file_name()).collect();
        names.sort();
        for n in names {
            let child = join_rel(rel, &n.to_string_lossy());
            upload(r, &local.join(&n), &child, &mut true)?;
        }
    } else if meta.is_file() {
        if meta.len() > MAX_IMPORT_FILE {
            return Err(("io".into(), format!("{} is larger than Files sends to a host (256 MB)", local.display())));
        }
        let bytes = std::fs::read(local).map_err(io)?;
        #[cfg(unix)]
        let mode = {
            use std::os::unix::fs::PermissionsExt;
            meta.permissions().mode() & 0o777
        };
        #[cfg(not(unix))]
        let mode = 0o644;
        r.conn.call("put", json!({ "root": r.root, "rel": rel, "mode": mode }), &bytes, Duration::from_secs(300)).map_err(remote_err)?;
        *made = true;
    } else {
        return Err(("io".into(), format!("{}: only files, folders and links are copied", local.display())));
    }
    Ok(())
}
