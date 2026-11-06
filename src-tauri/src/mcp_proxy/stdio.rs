//! Stdio upstreams (docs/mcp-proxy.md §2.1, docs/hive-media.md §12.1): a local MCP server that
//! maiTerm starts itself, from a catalogue route `{kind: "stdio_mcp", command, args, env_key}`,
//! and serves at `/mcp/x/<id>` like any proxied server.
//!
//! - **One child per server**, shared by every downstream session. The proxy numbers every
//!   request it sends, so concurrent calls from different agents never collide and each answer
//!   reaches its caller; an agent's own JSON-RPC id never reaches the child.
//! - **Started as `watch.rs` starts a script**: `env_clear`, the login PATH, the key variable,
//!   the route's fixed settings and nothing else; its own process group, killed whole when it
//!   stops (and at the app's quit, `stop_all`); a working directory of its own outside maiTerm's
//!   data folder (`output_folder`).
//! - **The key reaches the child only through its environment.** It is read from the keychain at
//!   spawn; the child's `Running` keeps it only to take it back out of everything the child sends
//!   (`scrub`). The child's stderr is never logged or forwarded (a server may print its own key):
//!   it is drained, and only a line count and the exit status are logged. A child's own error
//!   words never reach a status either (`status_for`).
//! - **Lifecycle.** Restarted on exit, with backoff, while agents are using it; stopped after
//!   `IDLE` with no `tools/call` or `tools/list` from an agent. The proxy's hourly re-list
//!   neither counts as use nor starts it (`running` vs `ensure`). A stopped server is `idle`, not
//!   an outage, and answers `tools/list` from its saved list.

use super::upstream::{UpErr, PROTOCOL};
use super::{ProxyServer, Status};
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::{BTreeMap, HashMap};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, LazyLock, OnceLock};
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt};
use tokio::sync::oneshot;

/// `ProxyServer::transport` values. Strings, never a serde enum: an older build that meets an
/// unknown variant in `mcp-servers.json` can't read the file at all.
pub const TRANSPORT_HTTP: &str = "http";
pub const TRANSPORT_STDIO: &str = "stdio";

/// Stopped after this long with no `tools/call` or `tools/list` from an agent.
pub const IDLE: Duration = Duration::from_secs(10 * 60);
/// The first start of an `npx`/`uvx` server downloads the package: well over the proxy's 15 s.
pub const VERIFY_WAIT: Duration = Duration::from_secs(180);
/// A later start, from an agent's call: the package is cached by then, but be generous.
pub const START_WAIT: Duration = Duration::from_secs(120);
/// Listing is quick; anything slower is a server in trouble.
const QUICK: Duration = Duration::from_secs(30);
/// A tool call may legitimately run for minutes (a video render).
const CALL: Duration = Duration::from_secs(15 * 60);
const MAX_PAGES: usize = 50;
/// Restart backoff: doubling from the first to the last, and back to the first after a child
/// that stayed up `STABLE`.
const BACKOFF_FIRST: Duration = Duration::from_secs(1);
const BACKOFF_MAX: Duration = Duration::from_secs(60);
const STABLE: Duration = Duration::from_secs(60);
const SWEEP_EVERY: Duration = Duration::from_secs(30);

/// How a stdio server is started: what its catalogue route said. No secret: `env_key` is the
/// NAME of the variable the key goes in, never the key.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct StdioSpec {
    /// The catalogue route this server was set up from: what detection matches it by, since it
    /// has no upstream URL (docs/hive-media.md §5).
    pub route: String,
    /// The program, found on the login PATH (`npx`, `uvx`) or absolute.
    pub command: String,
    #[serde(default)]
    pub args: Vec<String>,
    /// The environment variable the key is given in (`MINIMAX_API_KEY`). `None`: no key.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub env_key: Option<String>,
    /// Fixed, non-secret variables the route sets besides the key (`MINIMAX_API_HOST`). From the
    /// catalogue only, never from a webview or an agent. `{output}` in a value is the server's
    /// `output_folder`.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub env: BTreeMap<String, String>,
}

/// The placeholder a fixed variable's value may hold for the server's output folder.
pub const OUTPUT_PLACEHOLDER: &str = "{output}";

/// Variables neither the key nor a fixed setting may be: the one maiTerm sets itself, and those
/// that change how the child, its shell or its launcher (`npx`, `uvx`) behaves: where it loads
/// code from, where its home, temp and caches are, whom it trusts and which proxy it uses.
const FORBIDDEN_ENV: [&str; 27] = [
    "PATH", "HOME", "TMPDIR", "TMP", "TEMP", "SHELL", "USER", "LOGNAME", "PWD", "IFS", "CDPATH",
    "ENV", "BASH_ENV", "ZDOTDIR", "PERL5OPT", "PERL5LIB", "RUBYOPT", "RUBYLIB", "JAVA_TOOL_OPTIONS",
    "SSL_CERT_FILE", "SSL_CERT_DIR", "HTTP_PROXY", "HTTPS_PROXY", "ALL_PROXY", "NO_PROXY",
    "REQUESTS_CA_BUNDLE", "CURL_CA_BUNDLE",
];
/// `NODE_*` (NODE_OPTIONS, NODE_PATH, NODE_EXTRA_CA_CERTS), `PYTHON*` (PYTHONPATH, PYTHONHOME,
/// PYTHONSTARTUP), npm's, uv's and pip's own settings, the dynamic loaders', Java's and XDG's.
const FORBIDDEN_ENV_PREFIX: [&str; 11] =
    ["DYLD_", "LD_", "NODE_", "PYTHON", "NPM_CONFIG_", "UV_", "PIP_", "JAVA_", "_JAVA_", "XDG_", "MALLOC"];
/// What a key variable's name ends in: `^[A-Z][A-Z0-9_]*_(API_KEY|KEY|TOKEN|SECRET)$`.
const KEY_SUFFIX: [&str; 3] = ["_KEY", "_TOKEN", "_SECRET"];

/// A variable a key or a fixed setting may not be, compared without regard to case (npm reads
/// `npm_config_*` either way).
fn forbidden_env(name: &str) -> bool {
    let up = name.to_ascii_uppercase();
    FORBIDDEN_ENV.contains(&up.as_str()) || FORBIDDEN_ENV_PREFIX.iter().any(|p| up.starts_with(p))
}

fn env_name_shaped(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 64
        && name.chars().next().is_some_and(|c| c.is_ascii_uppercase() || c == '_')
        && name.chars().all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_')
}

impl StdioSpec {
    pub fn validate(&self) -> Result<(), String> {
        let route_ok = !self.route.is_empty()
            && self.route.len() <= 80
            && self.route.chars().all(|c| c.is_ascii_alphanumeric() || "-_./:".contains(c));
        if !route_ok {
            return Err("a route id is 1–80 letters, digits or - _ . / :".into());
        }
        if self.command.trim().is_empty() {
            return Err("the server has no command".into());
        }
        let bad = |s: &str| s.chars().any(|c| c == '\0' || c == '\n' || c == '\r');
        if bad(&self.command) || self.args.iter().any(|a| bad(a)) {
            return Err("the server's command line has a line break or NUL in it".into());
        }
        if let Some(name) = &self.env_key {
            let keyish = name.chars().next().is_some_and(|c| c.is_ascii_uppercase())
                && env_name_shaped(name)
                && KEY_SUFFIX.iter().any(|s| name.ends_with(s));
            if !keyish {
                return Err("a key variable is capital letters, digits or _ ending in _KEY, _TOKEN or _SECRET".into());
            }
            if forbidden_env(name) {
                return Err(format!("{name} can't carry a key"));
            }
        }
        for (name, value) in &self.env {
            if !env_name_shaped(name) {
                return Err("a setting's variable is 1–64 capital letters, digits or _".into());
            }
            if forbidden_env(name) {
                return Err(format!("the server can't set {name}"));
            }
            if self.env_key.as_deref() == Some(name.as_str()) {
                return Err(format!("{name} is the key's variable"));
            }
            if bad(value) {
                return Err(format!("{name}'s value has a line break or NUL in it"));
            }
        }
        Ok(())
    }

    /// The key is somewhere it must never be: the command line or a fixed setting.
    pub fn carries(&self, key: &str) -> bool {
        !key.is_empty()
            && (self.command.contains(key)
                || self.args.iter().any(|a| a.contains(key))
                || self.env.iter().any(|(n, v)| n.contains(key) || v.contains(key)))
    }

    /// What `ProxyServer::url` holds for a stdio server: no URL to dial, but a self-describing
    /// value an older build reads as a URL it can't reach (it never dials anything for it).
    pub fn url(&self) -> String {
        format!("stdio:{}", self.route)
    }
}

/// The folder a server's child works in, where a server that writes files writes them:
/// `<OS cache dir>/<app_data_slug>/mcp-out/<id>` (`~/Library/Caches/com.aiterm.app/mcp-out/<id>`).
/// Never inside maiTerm's data folder, which `check_output_folders` refuses (docs/hive.md §10.3):
/// this is the folder a Hive adds to its output folders when it gets the tool.
pub fn output_folder(id: &str) -> Option<PathBuf> {
    #[cfg(test)]
    let base = Some(std::env::temp_dir().join(format!("maiterm-test-mcp-out-{}", std::process::id())));
    #[cfg(not(test))]
    let base = dirs::cache_dir().map(|c| c.join(crate::state::persistence::app_data_slug()));
    base.map(|b| b.join("mcp-out").join(id))
}

// ─── The child ──────────────────────────────────────────────────────────────

/// A running child, shared by every downstream session of its server.
pub(super) struct Running {
    stdin: tokio::sync::Mutex<tokio::process::ChildStdin>,
    /// Our request id → the caller waiting for its answer.
    pending: Mutex<HashMap<u64, oneshot::Sender<Value>>>,
    /// The child's `initialize` result: `serverInfo`, `capabilities`, `instructions`.
    pub(super) init: Mutex<Value>,
    pgid: Option<libc::pid_t>,
    pub(super) pid: Option<u32>,
    alive: AtomicBool,
    started: std::time::Instant,
    /// The key this child was given, kept only to take it back out of everything the child sends
    /// (`scrub`): a server may echo its key in an error, a result or a tool's description, and
    /// what it sends reaches statuses, the saved list, Verify's words and the webview.
    key: Option<crate::accounts::vault::Secret>,
}

/// What stands where the child wrote its own key.
pub const KEY_REMOVED: &str = "[key removed]";

/// Replace every occurrence of `key` in `v`'s strings (and object keys) with `KEY_REMOVED`.
fn scrub(v: &mut Value, key: &str) {
    if key.is_empty() {
        return;
    }
    match v {
        Value::String(s) if s.contains(key) => *s = s.replace(key, KEY_REMOVED),
        Value::Array(a) => a.iter_mut().for_each(|x| scrub(x, key)),
        Value::Object(o) => {
            if o.keys().any(|k| k.contains(key)) {
                let old = std::mem::take(o);
                for (k, x) in old {
                    o.insert(k.replace(key, KEY_REMOVED), x);
                }
            }
            o.values_mut().for_each(|x| scrub(x, key));
        }
        _ => {}
    }
}

/// A request's place in `pending`, given up however its caller leaves: answered, timed out, or
/// dropped (an agent that disconnects drops its handler). A call nobody waits for any more must
/// not keep the child from its idle stop.
struct Waiting<'a> {
    r: &'a Running,
    n: u64,
}

impl Drop for Waiting<'_> {
    fn drop(&mut self) {
        self.r.pending.lock().remove(&self.n);
    }
}

impl std::fmt::Debug for Running {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Running").field("pid", &self.pid).field("alive", &self.alive.load(Ordering::Relaxed)).finish()
    }
}

impl Running {
    pub(super) fn is_alive(&self) -> bool {
        self.alive.load(Ordering::SeqCst)
    }

    /// Stopped on purpose: marked dead (so its exit is not restarted), its group killed, and
    /// every call waiting on it told.
    fn halt(&self) {
        self.alive.store(false, Ordering::SeqCst);
        self.kill_group();
        self.pending.lock().clear();
    }

    fn kill_group(&self) {
        if let Some(pg) = self.pgid {
            // SAFETY: signalling the process group this child leads (process_group(0)); a stale
            // id at worst names an empty group.
            unsafe { libc::killpg(pg, libc::SIGKILL) };
        }
    }

    async fn send(&self, msg: &Value) -> Result<(), UpErr> {
        let mut line = msg.to_string();
        line.push('\n');
        let mut stdin = self.stdin.lock().await;
        stdin.write_all(line.as_bytes()).await.map_err(|_| UpErr::Net("its process stopped reading".into()))?;
        stdin.flush().await.map_err(|_| UpErr::Net("its process stopped reading".into()))
    }

    /// One request, under maiTerm's own id: the answer is matched back by it, whoever asked.
    async fn request(&self, method: &str, params: Value, timeout: Duration) -> Result<Value, UpErr> {
        static NEXT: AtomicU64 = AtomicU64::new(1);
        let n = NEXT.fetch_add(1, Ordering::Relaxed);
        let (tx, rx) = oneshot::channel();
        self.pending.lock().insert(n, tx);
        let _waiting = Waiting { r: self, n };
        // Checked AFTER the insert: an exit marks the child dead before it clears what waits, so
        // a request that lands in between is either cleared or sees it dead here, never stranded
        // until its timeout.
        if !self.is_alive() {
            return Err(UpErr::Net("its process exited".into()));
        }
        self.send(&json!({ "jsonrpc": "2.0", "id": n, "method": method, "params": params })).await?;
        match tokio::time::timeout(timeout, rx).await {
            Ok(Ok(mut resp)) => {
                if let Some(key) = &self.key {
                    scrub(&mut resp, key.expose());
                }
                super::upstream::result_of(resp)
            }
            Ok(Err(_)) => Err(UpErr::Net("its process exited".into())),
            Err(_) => Err(UpErr::Net(format!("no answer within {}s", timeout.as_secs()))),
        }
    }

    pub(super) async fn list_tools(&self) -> Result<Vec<Value>, UpErr> {
        let mut tools = Vec::new();
        let mut cursor: Option<String> = None;
        for _ in 0..MAX_PAGES {
            let params = match &cursor {
                Some(c) => json!({ "cursor": c }),
                None => json!({}),
            };
            let result = self.request("tools/list", params, QUICK).await?;
            if let Some(page) = result.get("tools").and_then(Value::as_array) {
                tools.extend(page.iter().cloned());
            }
            cursor = result.get("nextCursor").and_then(Value::as_str).filter(|c| !c.is_empty()).map(str::to_string);
            if cursor.is_none() {
                return Ok(tools);
            }
        }
        Err(UpErr::Protocol(format!("tools/list kept paginating past {MAX_PAGES} pages")))
    }

    /// The caller's `params` as sent (`name`, `arguments`, `_meta`); the result verbatim.
    pub(super) async fn call_tool(&self, params: Value) -> Result<Value, UpErr> {
        self.request("tools/call", params, CALL).await
    }
}

/// One server's child, across restarts.
#[derive(Default)]
struct Slot {
    /// One start or stop at a time.
    gate: tokio::sync::Mutex<()>,
    running: Mutex<Option<Arc<Running>>>,
    /// A child still in its handshake, so a stop (a removal, a new key, a disable, the app's quit)
    /// kills it too rather than finding nothing and letting it come up afterwards.
    starting: Mutex<Option<Arc<Running>>>,
    /// The last `tools/call` or `tools/list` from an AGENT, or a start someone asked for (Verify,
    /// a Refresh, a balance check), in ms. Never a restart's. What the idle stop
    /// reads; the proxy's own reads never touch it.
    last_use: AtomicU64,
    /// Bumped by every deliberate stop: the exit of a child from before it is not restarted, and a
    /// child that was starting meanwhile is killed, not kept.
    generation: AtomicU64,
    backoff_ms: AtomicU64,
    /// A restart loop is running: one per server, however many children exit meanwhile.
    restarting: AtomicBool,
}

static SLOTS: LazyLock<Mutex<HashMap<String, Arc<Slot>>>> = LazyLock::new(Default::default);

fn slot(id: &str) -> Arc<Slot> {
    SLOTS.lock().entry(id.to_string()).or_default().clone()
}

fn now_ms() -> u64 {
    super::now_ms()
}

/// The child, if it is running now. Never starts one: what the hourly re-list asks.
pub(super) fn running(id: &str) -> Option<Arc<Running>> {
    let s = SLOTS.lock().get(id).cloned()?;
    let r = s.running.lock().clone()?;
    r.is_alive().then_some(r)
}

/// An agent listed or called a tool: the child stays up another `IDLE`.
pub(super) fn note_agent_use(id: &str) {
    slot(id).last_use.store(now_ms(), Ordering::SeqCst);
}

/// Why a child could not be had.
#[derive(Debug)]
pub(super) enum StartErr {
    /// The server needs a key and none is stored, or the keychain refused to give it.
    NoKey(String),
    /// It could not be spawned, or didn't complete its handshake.
    Up(UpErr),
}

/// Whether a start counts as use, keeping the child up `IDLE` from it.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum Start {
    /// An agent's call, Verify, a human's Refresh, a balance check.
    Asked,
    /// The restart loop's own: never use, or a child that keeps failing would be restarted for
    /// as long as the app runs.
    Restart,
}

fn stopped_while_starting() -> StartErr {
    StartErr::Up(UpErr::Net("it was stopped while it was starting".into()))
}

/// The running child, started if it isn't (one start at a time per server).
pub(super) async fn ensure(server: &ProxyServer, wait: Duration, why: Start) -> Result<Arc<Running>, StartErr> {
    let s = slot(&server.id);
    let _gate = s.gate.lock().await;
    if let Some(r) = s.running.lock().clone().filter(|r| r.is_alive()) {
        return Ok(r);
    }
    let generation = s.generation.load(Ordering::SeqCst);
    let spawned = spawn(server, wait, generation, &s).await;
    *s.starting.lock() = None;
    let r = match spawned {
        Ok(r) => r,
        Err(_) if s.generation.load(Ordering::SeqCst) != generation => return Err(stopped_while_starting()),
        Err(e) => return Err(e),
    };
    {
        // A stop that ran while it started (the server removed, its key replaced, disabled)
        // bumped the generation, or a removal dropped the slot: the child is not kept. Checked
        // under the lock `stop` takes after its bump, so one of the two always sees the other.
        let current = SLOTS.lock().get(&server.id).is_some_and(|cur| Arc::ptr_eq(cur, &s));
        let mut running = s.running.lock();
        if !current || s.generation.load(Ordering::SeqCst) != generation {
            drop(running);
            r.halt();
            return Err(stopped_while_starting());
        }
        *running = Some(r.clone());
    }
    if why == Start::Asked {
        s.last_use.fetch_max(now_ms(), Ordering::SeqCst);
    }
    start_sweeper();
    Ok(r)
}

/// Stop a server's child, whole process group, on purpose: it is not restarted. A child still
/// starting is killed too. Whether there was one.
pub(super) fn stop(id: &str) -> bool {
    let Some(s) = SLOTS.lock().get(id).cloned() else { return false };
    s.generation.fetch_add(1, Ordering::SeqCst);
    let starting = s.starting.lock().take();
    let running = s.running.lock().take();
    let mut any = false;
    for r in starting.iter().chain(running.iter()) {
        r.halt();
        any = true;
    }
    any
}

/// Stop every server's child: the app is quitting. A child runs in its own process group, outside
/// maiTerm's, so nothing else ends it, and it holds its key in its environment.
pub fn stop_all() -> usize {
    stop_where(|_| true)
}

fn stop_where(which: impl Fn(&str) -> bool) -> usize {
    let ids: Vec<String> = SLOTS.lock().keys().filter(|id| which(id)).cloned().collect();
    let n = ids.iter().filter(|id| stop(id)).count();
    if n > 0 {
        log::info!("[mcp-proxy] stopped {n} stdio servers");
    }
    n
}

/// Forget a removed server's child and its slot.
pub(super) fn forget(id: &str) {
    stop(id);
    SLOTS.lock().remove(id);
}

/// Stop every child no agent has used for `idle`. Returns the ids stopped.
pub(super) fn stop_idle(now: u64, idle: Duration) -> Vec<String> {
    stop_idle_where(now, idle, |_| true)
}

fn stop_idle_where(now: u64, idle: Duration, which: impl Fn(&str) -> bool) -> Vec<String> {
    let slots: Vec<(String, Arc<Slot>)> =
        SLOTS.lock().iter().filter(|(k, _)| which(k)).map(|(k, v)| (k.clone(), v.clone())).collect();
    let mut out = Vec::new();
    for (id, s) in slots {
        // A call still running (a long render) is use, however long ago it was made.
        let up = s.running.lock().as_ref().is_some_and(|r| r.is_alive() && r.pending.lock().is_empty());
        if up && now.saturating_sub(s.last_use.load(Ordering::SeqCst)) >= idle.as_millis() as u64 && stop(&id) {
            log::info!("[mcp-proxy] {id}: stopped its stdio server, unused for {} min", idle.as_secs() / 60);
            super::set_status(&id, Status::new(super::STATE_IDLE, ""));
            out.push(id);
        }
    }
    out
}

fn start_sweeper() {
    // Tests drive `stop_idle` themselves, with their own clock.
    if cfg!(test) {
        return;
    }
    static STARTED: OnceLock<()> = OnceLock::new();
    if STARTED.set(()).is_err() {
        return;
    }
    tokio::spawn(async {
        loop {
            tokio::time::sleep(SWEEP_EVERY).await;
            stop_idle(now_ms(), IDLE);
        }
    });
}

/// The child's environment: the login PATH and the key, and nothing else. Built apart from the
/// spawn so a test can see exactly what goes where.
pub(super) struct Plan {
    pub(super) program: String,
    pub(super) args: Vec<String>,
    pub(super) cwd: PathBuf,
    pub(super) path: String,
    /// The key's variable and value. `Secret`'s `Debug` is redacted.
    pub(super) key: Option<(String, crate::accounts::vault::Secret)>,
    /// The route's fixed settings, `{output}` expanded. Set after the key.
    pub(super) env: Vec<(String, String)>,
}

impl std::fmt::Debug for Plan {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Plan")
            .field("program", &self.program)
            .field("args", &self.args)
            .field("cwd", &self.cwd)
            .field("key", &self.key.as_ref().map(|(name, s)| (name, s)))
            .field("env", &self.env)
            .finish()
    }
}

pub(super) async fn plan(server: &ProxyServer) -> Result<Plan, StartErr> {
    let spec = server.stdio.as_ref().ok_or_else(|| StartErr::Up(UpErr::Protocol("its command is missing".into())))?;
    let key = match &spec.env_key {
        None => None,
        Some(name) => {
            let id = server.id.clone();
            let read = tokio::task::spawn_blocking(move || super::secrets::read_env_key(&id))
                .await
                .map_err(|e| StartErr::NoKey(e.to_string()))?
                .map_err(|e| StartErr::NoKey(e.to_string()))?;
            Some((name.clone(), read.ok_or_else(|| StartErr::NoKey("no key stored".into()))?))
        }
    };
    // A key replaced since the route was added could be one its command line or settings hold.
    if key.as_ref().is_some_and(|(_, k)| spec.carries(k.expose())) {
        return Err(StartErr::NoKey("the stored key is on its command line or in its settings".into()));
    }
    let cwd = output_folder(&server.id).ok_or_else(|| StartErr::Up(UpErr::Protocol("no cache folder on this computer".into())))?;
    let out = cwd.to_string_lossy();
    let env = spec.env.iter().map(|(n, v)| (n.clone(), v.replace(OUTPUT_PLACEHOLDER, &out))).collect();
    Ok(Plan { program: spec.command.clone(), args: spec.args.clone(), cwd, path: crate::watch::login_path().await, key, env })
}

#[cfg(not(unix))]
async fn spawn(_server: &ProxyServer, _wait: Duration, _generation: u64, _s: &Slot) -> Result<Arc<Running>, StartErr> {
    Err(StartErr::Up(UpErr::Protocol("stdio MCP servers run on macOS and Linux only".into())))
}

#[cfg(unix)]
async fn spawn(server: &ProxyServer, wait: Duration, generation: u64, s: &Slot) -> Result<Arc<Running>, StartErr> {
    use std::process::Stdio;
    let id = server.id.clone();
    let mut plan = plan(server).await?;
    std::fs::create_dir_all(&plan.cwd).map_err(|e| StartErr::Up(UpErr::Protocol(format!("could not create its folder: {}", e.kind()))))?;
    let mut cmd = tokio::process::Command::new(&plan.program);
    cmd.args(&plan.args).current_dir(&plan.cwd).env_clear().env("PATH", &plan.path);
    if let Some((name, key)) = &plan.key {
        cmd.env(name, key.expose());
    }
    cmd.envs(plan.env.iter().map(|(n, v)| (n, v)));
    cmd.stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped()).process_group(0).kill_on_drop(true);
    // The spawn error is the OS's ("No such file or directory"), never the command line.
    let mut child = cmd.spawn().map_err(|e| StartErr::Up(UpErr::Net(format!("its command could not start ({})", e.kind()))))?;
    drop(cmd);
    let key = plan.key.take().map(|(_, k)| k);
    drop(plan);
    let pid = child.id();
    let (Some(stdin), Some(stdout), Some(stderr)) = (child.stdin.take(), child.stdout.take(), child.stderr.take()) else {
        let _ = child.start_kill();
        return Err(StartErr::Up(UpErr::Protocol("its pipes could not be opened".into())));
    };
    let r = Arc::new(Running {
        stdin: tokio::sync::Mutex::new(stdin),
        pending: Mutex::new(HashMap::new()),
        init: Mutex::new(Value::Null),
        pgid: pid.map(|p| p as libc::pid_t),
        pid,
        alive: AtomicBool::new(true),
        started: std::time::Instant::now(),
        key,
    });
    let stderr_lines = Arc::new(AtomicU64::new(0));
    tokio::spawn(read_stdout(id.clone(), Arc::downgrade(&r), stdout));
    tokio::spawn(drain_stderr(stderr, stderr_lines.clone()));
    tokio::spawn(wait_exit(id.clone(), r.clone(), child, stderr_lines, generation));
    {
        // Seen by a stop from here on; a stop before this bumped the generation.
        let mut starting = s.starting.lock();
        if s.generation.load(Ordering::SeqCst) != generation {
            drop(starting);
            r.halt();
            return Err(stopped_while_starting());
        }
        *starting = Some(r.clone());
    }

    let init = r
        .request(
            "initialize",
            json!({
                "protocolVersion": PROTOCOL,
                "capabilities": {},
                "clientInfo": { "name": "maiterm-mcp-proxy", "title": crate::APP_DISPLAY_NAME, "version": crate::APP_VERSION },
            }),
            wait,
        )
        .await;
    let init = match init {
        Ok(v) => v,
        Err(e) => {
            // A handshake that failed is not restarted by its exit: whoever asked decides.
            r.halt();
            return Err(StartErr::Up(e));
        }
    };
    *r.init.lock() = init;
    if let Err(e) = r.send(&json!({ "jsonrpc": "2.0", "method": "notifications/initialized" })).await {
        r.halt();
        return Err(StartErr::Up(e));
    }
    log::info!("[mcp-proxy] {id}: started its stdio server ({})", server.stdio.as_ref().map_or("", |s| s.route.as_str()));
    Ok(r)
}

/// The child's stdout: one JSON-RPC message per line. Answers go to whoever waits for them, a
/// list change is announced, a request the child makes is answered (the proxy advertises no
/// client capabilities, so only `ping` is served). Anything that isn't JSON is skipped: a
/// package's banner on stdout is common.
async fn read_stdout(id: String, r: std::sync::Weak<Running>, stdout: tokio::process::ChildStdout) {
    let mut reader = tokio::io::BufReader::new(stdout);
    let mut line = Vec::new();
    loop {
        line.clear();
        match reader.read_until(b'\n', &mut line).await {
            Ok(0) | Err(_) => return,
            Ok(_) => {}
        }
        let Ok(msg) = serde_json::from_slice::<Value>(&line) else { continue };
        let Some(r) = r.upgrade() else { return };
        let method = msg.get("method").and_then(Value::as_str);
        match (method, msg.get("id")) {
            (None, Some(n)) => {
                if let Some(tx) = n.as_u64().and_then(|n| r.pending.lock().remove(&n)) {
                    let _ = tx.send(msg);
                }
            }
            (Some(m), Some(rid)) => {
                let reply = if m == "ping" {
                    json!({ "jsonrpc": "2.0", "id": rid, "result": {} })
                } else {
                    json!({ "jsonrpc": "2.0", "id": rid, "error": { "code": -32601, "message": "maiTerm's proxy answers no requests from a server" } })
                };
                let _ = r.send(&reply).await;
            }
            (Some(m), None) if m == "notifications/tools/list_changed" => {
                let id = id.clone();
                read_list_soon(id.clone());
            }
            _ => {}
        }
    }
}

/// Read stderr to its end and keep nothing but the count of lines.
async fn drain_stderr(stderr: tokio::process::ChildStderr, lines: Arc<AtomicU64>) {
    let mut reader = tokio::io::BufReader::new(stderr);
    let mut buf = Vec::new();
    loop {
        buf.clear();
        match reader.read_until(b'\n', &mut buf).await {
            Ok(0) | Err(_) => return,
            Ok(_) => {
                lines.fetch_add(1, Ordering::Relaxed);
            }
        }
    }
}

/// Wait for the child to end; then kill what is left of its group, fail the calls still waiting,
/// and restart it if agents are still using it and nobody stopped it on purpose.
async fn wait_exit(id: String, r: Arc<Running>, mut child: tokio::process::Child, stderr_lines: Arc<AtomicU64>, generation: u64) {
    let status = child.wait().await;
    let deliberate = !r.alive.swap(false, Ordering::SeqCst);
    r.kill_group();
    r.pending.lock().clear();
    // Let the stderr reader count the last lines.
    tokio::time::sleep(Duration::from_millis(50)).await;
    let how = match &status {
        Ok(s) => match s.code() {
            Some(c) => format!("exit {c}"),
            None => "killed by a signal".to_string(),
        },
        Err(_) => "its status could not be read".to_string(),
    };
    log::info!("[mcp-proxy] {id}: stdio server ended ({how}); {} stderr lines", stderr_lines.load(Ordering::Relaxed));
    let Some(s) = SLOTS.lock().get(&id).cloned() else { return };
    {
        let mut slot_running = s.running.lock();
        if slot_running.as_ref().is_some_and(|cur| Arc::ptr_eq(cur, &r)) {
            *slot_running = None;
        }
    }
    if deliberate || s.generation.load(Ordering::SeqCst) != generation {
        return;
    }
    if r.started.elapsed() >= STABLE {
        s.backoff_ms.store(0, Ordering::SeqCst);
    }
    if s.restarting.swap(true, Ordering::SeqCst) {
        return;
    }
    restart_soon(id, s, generation, how);
}

// The two below are plain functions on purpose: a child's tasks start reads and restarts, which
// start children, and an `async fn` calling back into `spawn` makes its future's type recursive.

/// Re-read the list the child just said changed, on a task of its own.
fn read_list_soon(id: String) {
    tokio::spawn(async move {
        let _ = super::refresh_stdio(&id, false).await;
    });
}

fn restart_soon(id: String, s: Arc<Slot>, generation: u64, how: String) {
    tokio::spawn(async move {
        restart(id, s.clone(), generation, how).await;
        s.restarting.store(false, Ordering::SeqCst);
    });
}

fn recently_used(s: &Slot) -> bool {
    now_ms().saturating_sub(s.last_use.load(Ordering::SeqCst)) < IDLE.as_millis() as u64
}

async fn restart(id: String, s: Arc<Slot>, generation: u64, how: String) {
    loop {
        let wanted = super::get(&id).filter(|srv| srv.enabled && srv.is_stdio());
        let Some(server) = wanted else { return };
        if !recently_used(&s) {
            super::set_status(&id, Status::new(super::STATE_IDLE, ""));
            return;
        }
        let prev = s.backoff_ms.load(Ordering::SeqCst);
        let wait = if prev == 0 { BACKOFF_FIRST } else { (Duration::from_millis(prev) * 2).min(BACKOFF_MAX) };
        s.backoff_ms.store(wait.as_millis() as u64, Ordering::SeqCst);
        super::set_status(&id, Status::new("unreachable", format!("its process ended ({how}); restarting in {}s", wait.as_secs())));
        tokio::time::sleep(wait).await;
        if s.generation.load(Ordering::SeqCst) != generation {
            return;
        }
        match ensure(&server, START_WAIT, Start::Restart).await {
            Ok(_) => {
                super::set_status(&id, Status::new("connected", "restarted"));
                return;
            }
            Err(e) => {
                super::set_status(&id, super::stdio_status(&server, &e));
            }
        }
    }
}

/// What a failed start or exchange means, in words of maiTerm's: a child's own error message
/// never reaches a status (it is logged and sent to the frontend), since a server may put its key
/// in one. A tool call's JSON-RPC error still goes back to the agent that made it, as sent.
pub(super) fn status_for(e: &UpErr) -> Status {
    match e {
        UpErr::Net(detail) => Status::new("unreachable", detail.clone()),
        UpErr::Rpc { code, .. } => Status::new("error", format!("it answered with error {code}")),
        UpErr::Protocol(_) => Status::new("error", "it answered with something that isn't MCP"),
        other => Status::new("error", other.to_string()),
    }
}

#[cfg(test)]
pub(super) fn age_for_test(id: &str, by: Duration) {
    let s = slot(id);
    let back = now_ms().saturating_sub(by.as_millis() as u64);
    s.last_use.store(back, Ordering::SeqCst);
}

/// The idle stop for these servers only: a test's sweep never stops another test's child.
#[cfg(test)]
pub(super) fn stop_idle_for_test(id: &str, now: u64, idle: Duration) -> Vec<String> {
    stop_idle_where(now, idle, |i| i == id)
}

#[cfg(test)]
pub(super) fn stop_for_test(ids: &[&str]) -> usize {
    stop_where(|i| ids.contains(&i))
}

#[cfg(test)]
pub(super) fn last_use_for_test(id: &str) -> u64 {
    slot(id).last_use.load(Ordering::SeqCst)
}

#[cfg(test)]
#[path = "stdio_tests.rs"]
mod tests;
