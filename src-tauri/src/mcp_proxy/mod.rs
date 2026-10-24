//! MCP servers through maiTerm (docs/mcp-proxy.md).
//!
//! maiTerm signs in to a remote MCP server once and serves its tools to every agent, each
//! proxied server as its own entry (`/mcp/x/<id>`) in the agents' configs, so names, permission
//! rules and runtime toggles behave as with a native install (§3).
//!
//! The rules from §4 that this module exists to keep:
//! - **The tool list is the upstream's, verbatim**, and it is persisted: a session that starts
//!   while the upstream is down, or before maiTerm reconnects after a restart, gets the list it
//!   got yesterday.
//! - **A failure never removes a tool.** A down upstream or a refused credential leaves the list
//!   alone, and a call answers `isError` with a sentence the model can pass on.
//! - **Only the upstream changes the list**, and a change is forwarded to every open downstream
//!   stream as `notifications/tools/list_changed` (Claude and Gemini re-list; Codex keeps what it
//!   started with).
//!
//! The configuration lives in its own file (`mcp-servers.json` in the data dir), Rust-owned: the
//! frontend never writes it back whole, so nothing can drop a server by saving a stale copy.
//! Credentials are in the keychain (`secrets`), never in that file.

pub mod entries;
pub mod oauth;
pub mod secrets;
pub mod upstream;

use crate::state::AppState;
use parking_lot::{Mutex, RwLock};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, LazyLock, OnceLock};
use std::time::Duration;
use tauri::{AppHandle, Emitter};
use tokio::sync::mpsc;
use upstream::{Session, UpErr, Upstream};

/// `ProxyServer::auth` values. Strings, never a serde enum: an unknown variant in stored state
/// wipes it on a downgrade.
pub const AUTH_NONE: &str = "none";
pub const AUTH_TOKEN: &str = "token";
pub const AUTH_OAUTH: &str = "oauth";

/// How often a server's list is re-read when nothing announced a change.
const REFRESH_EVERY: Duration = Duration::from_secs(60 * 60);
/// A downstream session unused this long gives its upstream session back.
const DOWN_IDLE_MS: u64 = 2 * 60 * 60 * 1000;
/// Names an entry may not take: maiTerm's own servers, current and legacy.
const RESERVED: [&str; 4] = ["maiterm", "maiterm-dev", "aiterm", "aiterm-dev"];
/// Protocol revisions a downstream client may ask for and get back as asked.
const DOWN_PROTOCOLS: [&str; 4] = ["2024-11-05", "2025-03-26", "2025-06-18", "2025-11-25"];

/// One proxied server, as configured.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ProxyServer {
    /// The entry's name in every runtime's config, and the last segment of `/mcp/x/<id>`.
    pub id: String,
    pub url: String,
    /// `none` | `token` | `oauth`.
    pub auth: String,
    #[serde(default = "yes")]
    pub enabled: bool,
    /// Workspaces whose tabs see the tools; empty means every workspace (§4.3).
    #[serde(default)]
    pub workspaces: Vec<String>,
    /// The last good `tools/list`, served while the upstream is unreachable.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub catalog: Option<Catalog>,
    /// OAuth only: the authorization server, client registration and resource (§5). No secret.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub oauth: Option<Value>,
    #[serde(default)]
    pub added_at: u64,
    /// `human`, or `agent request <id>` when an agent proposed it and the human approved.
    #[serde(default)]
    pub added_by: String,
}

fn yes() -> bool {
    true
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Catalog {
    pub tools: Vec<Value>,
    #[serde(default)]
    pub server_info: Value,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub instructions: Option<String>,
    #[serde(default)]
    pub fetched_at: u64,
}

impl Catalog {
    /// The same list, ignoring when it was read.
    fn same_as(&self, other: &Catalog) -> bool {
        self.tools == other.tools && self.instructions == other.instructions && self.server_info == other.server_info
    }
}

#[derive(Serialize, Deserialize, Default)]
struct StoreFile {
    #[serde(default)]
    servers: Vec<ProxyServer>,
}

/// The server a `/mcp/x/<name>` path names. Paths carry the ENTRY name (`entries::entry_name`),
/// so a dev build's entries (`linear-dev`) reach only a dev build: on an ssh host both flavors'
/// entries resolve `${MAITERM_PORT}` to whichever maiTerm launched the shell, and with a bare id
/// in both paths that one maiTerm served the same server under two names.
pub fn id_from_path(name: &str) -> Option<&str> {
    if cfg!(debug_assertions) {
        name.strip_suffix("-dev")
    } else {
        Some(name)
    }
}

/// A server's health as maiTerm last saw it. Never persisted.
#[derive(Clone, Debug, Serialize)]
pub struct Status {
    /// `connecting` | `connected` | `needs_sign_in` | `unreachable` | `error` | `disabled`.
    pub state: String,
    pub detail: String,
    pub since: u64,
}

impl Status {
    fn new(state: &str, detail: impl Into<String>) -> Self {
        Self { state: state.into(), detail: detail.into(), since: now_ms() }
    }
    fn is_outage(&self) -> bool {
        matches!(self.state.as_str(), "needs_sign_in" | "unreachable" | "error")
    }
}

/// One agent session on `/mcp/x/<id>`, with its own upstream session: a server with state per
/// session (a browser, a cursor) must not be shared between agents.
struct DownSession {
    upstream: tokio::sync::Mutex<Option<Session>>,
    last_used: AtomicU64,
}

#[derive(Default)]
struct Live {
    status: Option<Status>,
    watcher: Option<tokio::task::JoinHandle<()>>,
    down: HashMap<String, Arc<DownSession>>,
    streams: Vec<mpsc::UnboundedSender<String>>,
    /// The credential, read from the keychain once rather than on every call.
    cred: Option<secrets::Cred>,
    /// The upstream refused a tool call (401/403) and no call has succeeded since. Some servers
    /// (langsys.dev) list their tools to anyone and refuse only calls, so a good `tools/list`
    /// must not report the server healthy again while this holds.
    calls_refused: bool,
}

/// One list read at a time per server, so an older read can't land after a newer one.
static REFRESHING: LazyLock<Mutex<HashMap<String, Arc<tokio::sync::Mutex<()>>>>> = LazyLock::new(Default::default);

fn set_calls_refused(id: &str, refused: bool) {
    LIVE.lock().entry(id.to_string()).or_default().calls_refused = refused;
}

static SERVERS: LazyLock<RwLock<Vec<ProxyServer>>> = LazyLock::new(Default::default);
static LIVE: LazyLock<Mutex<HashMap<String, Live>>> = LazyLock::new(Default::default);
static SAVE: Mutex<()> = Mutex::new(());
static APP: OnceLock<(AppHandle, Arc<AppState>)> = OnceLock::new();
static LOADED: OnceLock<()> = OnceLock::new();
/// What the last `entries::sync` found: names a native entry holds.
static COLLISIONS: LazyLock<Mutex<entries::Collisions>> = LazyLock::new(Default::default);

/// Read the configured servers once. The entry writer runs from the MCP server's synchronous
/// startup, before `start`, and needs them.
pub fn ensure_loaded() {
    LOADED.get_or_init(|| *SERVERS.write() = load().servers);
}

/// Rewrite the agents' entries after a change, off the calling thread (file I/O).
fn sync_entries() {
    let Some((app, state)) = APP.get() else { return };
    let (Some(port), Some(auth)) = (*state.mcp_port.read(), state.mcp_auth.read().clone()) else { return };
    let prefs = state.app_data.read().preferences.clone();
    let app = app.clone();
    let run = move || {
        entries::sync(port, &auth, &prefs);
        // ssh hosts bridged now merge the new set at once (sshMcpBridge.svelte.ts), rather than
        // at their next reconnect.
        let _ = app.emit("mcp-proxy-entries-changed", ());
    };
    match tokio::runtime::Handle::try_current() {
        Ok(_) => {
            tokio::task::spawn_blocking(run);
        }
        Err(_) => run(),
    }
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as u64)
}

fn store_path() -> Option<PathBuf> {
    crate::state::persistence::app_data_dir().map(|p| p.join("mcp-servers.json"))
}

fn load() -> StoreFile {
    let Some(path) = store_path() else { return StoreFile::default() };
    let Ok(raw) = std::fs::read_to_string(&path) else { return StoreFile::default() };
    match serde_json::from_str::<StoreFile>(&raw) {
        Ok(f) => f,
        Err(e) => {
            // Keep the unreadable file for a human to look at, rather than overwriting it with
            // an empty list on the next save.
            let aside = path.with_extension(format!("unreadable-{}.json", now_ms()));
            let _ = std::fs::rename(&path, &aside);
            log::error!("[mcp-proxy] {} is unreadable ({e}); moved aside to {}", path.display(), aside.display());
            StoreFile::default()
        }
    }
}

/// Write the current list. Serialized, so two saves can't land out of order.
fn save() -> Result<(), String> {
    let _g = SAVE.lock();
    let Some(path) = store_path() else { return Err("no data directory".into()) };
    let file = StoreFile { servers: SERVERS.read().clone() };
    let json = serde_json::to_string_pretty(&file).map_err(|e| e.to_string())?;
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let tmp = path.with_extension(format!("tmp-{}.json", std::process::id()));
    std::fs::write(&tmp, json).map_err(|e| format!("cannot write {}: {e}", tmp.display()))?;
    std::fs::rename(&tmp, &path).map_err(|e| {
        let _ = std::fs::remove_file(&tmp);
        format!("cannot write {}: {e}", path.display())
    })
}

pub fn get(id: &str) -> Option<ProxyServer> {
    SERVERS.read().iter().find(|s| s.id == id).cloned()
}

pub fn all() -> Vec<ProxyServer> {
    SERVERS.read().clone()
}

fn update(id: &str, f: impl FnOnce(&mut ProxyServer)) -> Result<ProxyServer, String> {
    let out = {
        let mut list = SERVERS.write();
        let s = list.iter_mut().find(|s| s.id == id).ok_or_else(|| format!("no MCP server named {id}"))?;
        f(s);
        s.clone()
    };
    save()?;
    Ok(out)
}

pub fn status(id: &str) -> Status {
    if get(id).is_some_and(|s| !s.enabled) {
        return Status::new("disabled", "");
    }
    LIVE.lock().get(id).and_then(|l| l.status.clone()).unwrap_or_else(|| Status::new("connecting", ""))
}

/// Record a server's health. An outage starting, or ending, is told to the frontend once.
fn set_status(id: &str, next: Status) {
    let prev = {
        let mut live = LIVE.lock();
        let l = live.entry(id.to_string()).or_default();
        let prev = l.status.clone();
        if prev.as_ref().is_some_and(|p| p.state == next.state && p.detail == next.detail) {
            return;
        }
        l.status = Some(next.clone());
        prev
    };
    let was_out = prev.as_ref().is_some_and(Status::is_outage);
    if next.is_outage() {
        log::warn!("[mcp-proxy] {id}: {} ({})", next.state, next.detail);
    } else if was_out || prev.is_none() {
        log::info!("[mcp-proxy] {id}: {}", next.state);
    }
    if let Some((app, _)) = APP.get() {
        let _ = app.emit("mcp-proxy-status", json!({ "id": id, "status": next, "was_outage": was_out }));
    }
}

// ─── Credentials ────────────────────────────────────────────────────────────

/// The bearer token for a server: `Ok(None)` when it needs none.
async fn bearer(server: &ProxyServer) -> Result<Option<String>, String> {
    match server.auth.as_str() {
        AUTH_NONE => Ok(None),
        AUTH_OAUTH => oauth::access_token(server, None).await.map(Some),
        _ => cached_or_stored_cred(&server.id).await?.map(|c| Some(c.access_token)).ok_or_else(|| "no token stored".into()),
    }
}

/// The server's credential: the cached copy, else the keychain's (then cached).
async fn cached_or_stored_cred(id: &str) -> Result<Option<secrets::Cred>, String> {
    if let Some(c) = LIVE.lock().get(id).and_then(|l| l.cred.clone()) {
        return Ok(Some(c));
    }
    let key = id.to_string();
    let read = tokio::task::spawn_blocking(move || secrets::read(&key))
        .await
        .map_err(|e| e.to_string())?
        .map_err(|e| e.to_string())?;
    if let Some(c) = &read {
        cache_cred(id, c.clone());
    }
    Ok(read)
}

fn cache_cred(id: &str, cred: secrets::Cred) {
    LIVE.lock().entry(id.to_string()).or_default().cred = Some(cred);
}

/// A sign-in completed: calls may work again, and the list is read now.
fn signed_in(id: &str) {
    set_calls_refused(id, false);
    set_status(id, Status::new("connecting", "signed in"));
    if get(id).is_some_and(|s| s.enabled) {
        start_watcher(id);
    }
}

fn forget_cached_cred(id: &str) {
    if let Some(l) = LIVE.lock().get_mut(id) {
        l.cred = None;
    }
}

/// What a refused or missing credential means for this server, in the status's words.
fn needs_sign_in(server: &ProxyServer, why: &str) -> Status {
    Status::new("needs_sign_in", match server.auth.as_str() {
        AUTH_NONE => format!("the server asks for a sign-in ({why})"),
        AUTH_TOKEN => format!("the token was refused or is missing ({why})"),
        _ => format!("sign in again ({why})"),
    })
}

fn status_for(server: &ProxyServer, e: &UpErr) -> Status {
    match e {
        UpErr::Unauthorized { .. } => needs_sign_in(server, "401"),
        UpErr::Forbidden { .. } => needs_sign_in(server, "403"),
        UpErr::Net(detail) => Status::new("unreachable", detail.clone()),
        UpErr::Http { .. } | UpErr::SessionExpired => Status::new("unreachable", e.to_string()),
        UpErr::Rpc { .. } | UpErr::Protocol(_) => Status::new("error", e.to_string()),
    }
}

/// The sentence a failed call answers with: what happened and who can fix it.
fn call_failure_text(server: &ProxyServer, st: &Status) -> String {
    let fix = "The human can fix this in maiTerm → Preferences → MCP servers.";
    match st.state.as_str() {
        "needs_sign_in" => format!("maiTerm could not call the {} MCP server: {}. {fix}", server.id, st.detail),
        "unreachable" => format!(
            "maiTerm could not reach the {} MCP server ({}). Its tools stay listed; try again shortly, or tell the human if it keeps failing.",
            server.id, st.detail
        ),
        _ => format!("The {} MCP server answered with something maiTerm could not use ({}). {fix}", server.id, st.detail),
    }
}

// ─── Catalog ────────────────────────────────────────────────────────────────

/// Read the server's tool list now. Returns whether it changed. A changed list is persisted and
/// announced; a failure leaves the stored list as it was.
pub async fn refresh(id: &str) -> Result<bool, String> {
    let turn = REFRESHING.lock().entry(id.to_string()).or_default().clone();
    let _turn = turn.lock().await;
    // Read after waiting: the comparison below must be against the newest stored list.
    let server = get(id).ok_or_else(|| format!("no MCP server named {id}"))?;
    let mut auth = match bearer(&server).await {
        Ok(a) => a,
        Err(why) => {
            set_status(id, needs_sign_in(&server, &why));
            return Err(why);
        }
    };
    let up = Upstream::new(&server.url);
    let read = |auth: Option<String>| {
        let up = &up;
        async move {
            let session = up.initialize(auth.as_deref()).await?;
            let tools = up.list_tools(auth.as_deref(), &session).await;
            up.end(auth.as_deref(), &session).await;
            Ok::<_, UpErr>((session, tools?))
        }
    };
    let mut result = read(auth.clone()).await;
    // A refused OAuth token is refreshed once and the read tried again, as a call would be.
    if matches!(result, Err(UpErr::Unauthorized { .. })) && server.auth == AUTH_OAUTH {
        match oauth::access_token(&server, auth.as_deref()).await {
            Ok(fresh) => {
                auth = Some(fresh);
                result = read(auth.clone()).await;
            }
            Err(why) => {
                set_status(id, needs_sign_in(&server, &why));
                return Err(why);
            }
        }
    }
    let (session, tools) = match result {
        Ok(v) => v,
        Err(e) => {
            set_status(id, status_for(&server, &e));
            return Err(e.to_string());
        }
    };
    let next = Catalog {
        tools,
        server_info: session.init.get("serverInfo").cloned().unwrap_or(Value::Null),
        instructions: session.init.get("instructions").and_then(Value::as_str).map(str::to_string),
        fetched_at: now_ms(),
    };
    let changed = !server.catalog.as_ref().is_some_and(|c| c.same_as(&next));
    let count = next.tools.len();
    update(id, |s| s.catalog = Some(next))?;
    if LIVE.lock().get(id).is_some_and(|l| l.calls_refused) {
        // Listing works, calls don't: the server is reachable, and what it needs is a sign-in.
        set_status(id, needs_sign_in(&server, "it refuses tool calls"));
    } else {
        set_status(id, Status::new("connected", format!("{count} tools")));
    }
    if changed {
        log::info!("[mcp-proxy] {id}: tool list {} ({count} tools)", if server.catalog.is_some() { "changed" } else { "read" });
        announce_list_changed(id);
    }
    Ok(changed)
}

fn announce_list_changed(id: &str) {
    let note = json!({ "jsonrpc": "2.0", "method": "notifications/tools/list_changed" }).to_string();
    if let Some(l) = LIVE.lock().get_mut(id) {
        l.streams.retain(|tx| tx.send(note.clone()).is_ok());
    }
    if let Some((app, _)) = APP.get() {
        let _ = app.emit("mcp-proxy-changed", json!({ "id": id }));
    }
}

/// Keeps one server's list current: an immediate read, then the upstream's own change notices on
/// its `GET` stream, and an hourly re-read either way. Runs until the server is removed or
/// disabled (the handle is aborted).
async fn watch(id: String) {
    let mut backoff = Duration::from_secs(30);
    loop {
        sweep_idle(&id);
        if refresh(&id).await.is_err() {
            tokio::time::sleep(backoff).await;
            backoff = (backoff * 2).min(Duration::from_secs(10 * 60));
            continue;
        }
        backoff = Duration::from_secs(30);
        let Some(server) = get(&id) else { return };
        let auth = bearer(&server).await.ok().flatten();
        let up = Upstream::new(&server.url);
        let stream = match up.initialize(auth.as_deref()).await {
            Ok(session) => match up.open_stream(auth.as_deref(), &session).await {
                Ok(Some(resp)) => Some((session, resp)),
                _ => {
                    up.end(auth.as_deref(), &session).await;
                    None
                }
            },
            Err(_) => None,
        };
        let Some((session, resp)) = stream else {
            tokio::time::sleep(REFRESH_EVERY).await;
            continue;
        };
        let (tx, mut rx) = mpsc::unbounded_channel::<()>();
        let reader = upstream::read_stream(resp, move |v| {
            if upstream::is_list_changed(&v) {
                let _ = tx.send(());
            }
        });
        tokio::pin!(reader);
        let opened = tokio::time::Instant::now();
        loop {
            tokio::select! {
                _ = &mut reader => break,
                Some(()) = rx.recv() => { let _ = refresh(&id).await; }
                _ = tokio::time::sleep(REFRESH_EVERY) => { sweep_idle(&id); let _ = refresh(&id).await; }
            }
        }
        up.end(auth.as_deref(), &session).await;
        // A stream that closes at once is a server that offers none in practice (mcp.svelte.dev
        // answers the GET and ends it): treat it as streamless and re-read hourly. Reconnecting
        // sooner re-read its whole list every minute. A stream that lived a while and dropped
        // reconnects after a pause, and the reconnect's read catches what it missed. The line is
        // seconds, not a minute: proxies that cut idle streams at 55-60 s (Heroku, nginx, ALB
        // defaults) must still count as streams, or their changes would wait an hour.
        if opened.elapsed() < Duration::from_secs(5) {
            tokio::time::sleep(REFRESH_EVERY).await;
        } else {
            tokio::time::sleep(Duration::from_secs(30)).await;
        }
    }
}

fn start_watcher(id: &str) {
    if tokio::runtime::Handle::try_current().is_err() {
        // Called from Tauri's synchronous setup: run on its async runtime.
        let id = id.to_string();
        tauri::async_runtime::spawn(async move { start_watcher(&id) });
        return;
    }
    let handle = tokio::spawn(watch(id.to_string()));
    let mut live = LIVE.lock();
    let l = live.entry(id.to_string()).or_default();
    if let Some(old) = l.watcher.replace(handle) {
        old.abort();
    }
}

fn stop_watcher(id: &str) {
    if let Some(h) = LIVE.lock().get_mut(id).and_then(|l| l.watcher.take()) {
        h.abort();
    }
}

/// Give back the upstream sessions of downstream sessions nobody has used in a while.
fn sweep_idle(id: &str) {
    let cutoff = now_ms().saturating_sub(DOWN_IDLE_MS);
    let stale: Vec<Arc<DownSession>> = {
        let mut live = LIVE.lock();
        let Some(l) = live.get_mut(id) else { return };
        let keys: Vec<String> = l.down.iter().filter(|(_, d)| d.last_used.load(Ordering::Relaxed) < cutoff).map(|(k, _)| k.clone()).collect();
        keys.iter().filter_map(|k| l.down.remove(k)).collect()
    };
    if stale.is_empty() {
        return;
    }
    let Some(server) = get(id) else { return };
    tokio::spawn(async move {
        let auth = bearer(&server).await.ok().flatten();
        let up = Upstream::new(&server.url);
        for d in stale {
            if let Some(s) = d.upstream.lock().await.take() {
                up.end(auth.as_deref(), &s).await;
            }
        }
    });
}

// ─── Downstream: what agents talk to (`/mcp/x/<id>`) ──────────────────────

/// The HTTP answer to one downstream POST.
pub struct DownReply {
    pub status: u16,
    pub body: Option<String>,
    /// Set on `initialize`: the `Mcp-Session-Id` the client echoes from then on.
    pub session_id: Option<String>,
}

impl DownReply {
    fn json(v: Value) -> Self {
        Self { status: 200, body: Some(v.to_string()), session_id: None }
    }
    fn result(id: Value, result: Value) -> Self {
        Self::json(json!({ "jsonrpc": "2.0", "id": id, "result": result }))
    }
    fn error(id: Value, code: i64, message: impl Into<String>, data: Option<Value>) -> Self {
        let mut err = json!({ "code": code, "message": message.into() });
        if let Some(d) = data {
            err["data"] = d;
        }
        Self::json(json!({ "jsonrpc": "2.0", "id": id, "error": err }))
    }
    fn status(status: u16) -> Self {
        Self { status, body: None, session_id: None }
    }
}

/// Is this tab's workspace one the server is offered to? An unknown tab is in scope only when
/// the server is offered everywhere.
fn in_scope(server: &ProxyServer, tab: Option<&str>) -> bool {
    if server.workspaces.is_empty() {
        return true;
    }
    let (Some(tab), Some((_, state))) = (tab, APP.get()) else { return false };
    let data = state.app_data.read();
    data.windows.iter().flat_map(|w| w.workspaces.iter()).any(|ws| {
        server.workspaces.contains(&ws.id) && ws.panes.iter().any(|p| p.tabs.iter().any(|t| t.id == tab))
    })
}

/// Downstream session ids that are unknown (a client that never echoes the id, or one from
/// before a maiTerm restart) share one session under this key.
const ANON: &str = "";

fn down_session(id: &str, sid: Option<&str>) -> Arc<DownSession> {
    let key = sid.unwrap_or(ANON).to_string();
    let mut live = LIVE.lock();
    let l = live.entry(id.to_string()).or_default();
    let d = l
        .down
        .entry(key)
        .or_insert_with(|| Arc::new(DownSession { upstream: tokio::sync::Mutex::new(None), last_used: AtomicU64::new(0) }))
        .clone();
    d.last_used.store(now_ms(), Ordering::Relaxed);
    d
}

/// `path`: the `/mcp/x/<path>` segment, an entry name (`id_from_path`).
pub async fn downstream_post(path: &str, sid: Option<&str>, tab: Option<&str>, body: &str) -> DownReply {
    let Some(server) = id_from_path(path).and_then(get) else {
        if valid_id(path).is_err() {
            return DownReply::status(404);
        }
        return not_served(path, body);
    };
    let id = server.id.as_str();
    let msg: Value = match serde_json::from_str(body) {
        Ok(v) => v,
        Err(e) => return DownReply::error(Value::Null, -32700, format!("parse error: {e}"), None),
    };
    if msg.is_array() {
        return DownReply::error(Value::Null, -32600, "batches are not supported", None);
    }
    let Some(rid) = msg.get("id").cloned() else {
        // A notification (`notifications/initialized`, a cancel): nothing to answer.
        return DownReply::status(202);
    };
    let method = msg.get("method").and_then(Value::as_str).unwrap_or("");
    let params = msg.get("params").cloned().unwrap_or_else(|| json!({}));
    match method {
        "initialize" => {
            let asked = params.get("protocolVersion").and_then(Value::as_str).unwrap_or("");
            let protocol = if DOWN_PROTOCOLS.contains(&asked) { asked } else { "2025-06-18" };
            let catalog = server.catalog.clone();
            let mut result = json!({
                "protocolVersion": protocol,
                "capabilities": { "tools": { "listChanged": true } },
                "serverInfo": catalog.as_ref().map(|c| c.server_info.clone()).filter(|v| v.is_object())
                    .unwrap_or_else(|| json!({ "name": server.id, "version": "unknown" })),
            });
            if let Some(instr) = catalog.and_then(|c| c.instructions) {
                result["instructions"] = Value::String(instr);
            }
            let new_sid = uuid::Uuid::new_v4().to_string();
            down_session(id, Some(&new_sid));
            let mut reply = DownReply::result(rid, result);
            reply.session_id = Some(new_sid);
            reply
        }
        "ping" => DownReply::result(rid, json!({})),
        "tools/list" => {
            if !server.enabled || !in_scope(&server, tab) {
                return DownReply::result(rid, json!({ "tools": [] }));
            }
            let mut catalog = server.catalog.clone();
            if catalog.is_none() {
                // Never read yet (just added, or the upstream has been down since): try once.
                // As its own task, so a read that outlasts the wait still finishes (and ends its
                // upstream session) rather than being dropped mid-way.
                let owned = id.to_string();
                let read = tokio::spawn(async move { refresh(&owned).await });
                let _ = tokio::time::timeout(Duration::from_secs(15), read).await;
                catalog = get(id).and_then(|s| s.catalog);
            }
            DownReply::result(rid, json!({ "tools": catalog.map(|c| c.tools).unwrap_or_default() }))
        }
        "tools/call" => {
            if !server.enabled || !in_scope(&server, tab) {
                let text = format!("The {} MCP server is not enabled for this workspace in maiTerm.", server.id);
                return DownReply::result(rid, json!({ "content": [{ "type": "text", "text": text }], "isError": true }));
            }
            call(&server, sid, rid, params).await
        }
        _ => DownReply::error(rid, -32601, format!("Method not found: {method}"), None),
    }
}

/// A server name this maiTerm doesn't serve: an entry for one it removed, or, on an ssh account
/// shared with other people's maiTerms, one of THEIRS (remote entries name no instance, so each
/// person's agents reach their own maiTerm with every name on the account). It answers as an
/// empty server rather than failing, so such an entry costs nothing and alarms no one.
fn not_served(id: &str, body: &str) -> DownReply {
    let Ok(msg) = serde_json::from_str::<Value>(body) else { return DownReply::status(400) };
    let Some(rid) = msg.get("id").cloned() else { return DownReply::status(202) };
    let text = format!("This maiTerm doesn't serve an MCP server named {id}: it was removed, or another maiTerm on this account added it.");
    match msg.get("method").and_then(Value::as_str).unwrap_or("") {
        "initialize" => {
            let asked = msg.pointer("/params/protocolVersion").and_then(Value::as_str).unwrap_or("");
            let protocol = if DOWN_PROTOCOLS.contains(&asked) { asked } else { "2025-06-18" };
            DownReply::result(rid, json!({
                "protocolVersion": protocol,
                "capabilities": { "tools": {} },
                "serverInfo": { "name": id, "version": "none" },
                "instructions": text,
            }))
        }
        "ping" => DownReply::result(rid, json!({})),
        "tools/list" => DownReply::result(rid, json!({ "tools": [] })),
        "tools/call" => DownReply::result(rid, json!({ "content": [{ "type": "text", "text": text }], "isError": true })),
        m => DownReply::error(rid, -32601, format!("Method not found: {m}"), None),
    }
}

/// Forward a `tools/call` on this downstream session's own upstream session.
async fn call(server: &ProxyServer, sid: Option<&str>, rid: Value, params: Value) -> DownReply {
    let fail = |rid: Value, st: Status| {
        let text = call_failure_text(server, &st);
        set_status(&server.id, st);
        DownReply::result(rid, json!({ "content": [{ "type": "text", "text": text }], "isError": true }))
    };
    let mut auth = match bearer(server).await {
        Ok(a) => a,
        Err(why) => return fail(rid, needs_sign_in(server, &why)),
    };
    let up = Upstream::new(&server.url);
    let down = down_session(&server.id, sid);
    // A second attempt only where the upstream ran nothing: it forgot the session (404), or it
    // refused an OAuth token that a refresh then replaced (401). Nothing else is retried, since
    // a tool call need not be idempotent.
    for attempt in 0..2 {
        let opened = {
            let mut slot = down.upstream.lock().await;
            match slot.clone() {
                Some(s) => Ok(s),
                None => up.initialize(auth.as_deref()).await.inspect(|s| *slot = Some(s.clone())),
            }
        };
        let session = match opened {
            Ok(s) => s,
            // A server that wants the token on `initialize` refuses an expired one there, before
            // any call: refresh and open the session again.
            Err(UpErr::Unauthorized { .. }) if attempt == 0 && server.auth == AUTH_OAUTH => match oauth::access_token(server, auth.as_deref()).await {
                Ok(fresh) => {
                    auth = Some(fresh);
                    continue;
                }
                Err(why) => {
                    set_calls_refused(&server.id, true);
                    return fail(rid, needs_sign_in(server, &why));
                }
            },
            Err(e) => return fail(rid, status_for(server, &e)),
        };
        match up.call_tool(auth.as_deref(), &session, params.clone()).await {
            Ok((result, list_changed)) => {
                set_calls_refused(&server.id, false);
                if status(&server.id).state != "connected" {
                    set_status(&server.id, Status::new("connected", ""));
                }
                if list_changed {
                    let id = server.id.clone();
                    tokio::spawn(async move { let _ = refresh(&id).await; });
                }
                return DownReply::result(rid, result);
            }
            Err(UpErr::SessionExpired) if attempt == 0 => {
                // Only the session that expired: a concurrent call on this downstream session
                // may already have opened its replacement.
                let mut slot = down.upstream.lock().await;
                if slot.as_ref().is_some_and(|s| s.id == session.id) {
                    *slot = None;
                }
            }
            Err(UpErr::Unauthorized { .. }) if attempt == 0 && server.auth == AUTH_OAUTH => match oauth::access_token(server, auth.as_deref()).await {
                Ok(fresh) => auth = Some(fresh),
                Err(why) => {
                    set_calls_refused(&server.id, true);
                    return fail(rid, needs_sign_in(server, &why));
                }
            },
            // The server's own JSON-RPC error goes back as it was sent: an unknown tool, bad
            // arguments. It says nothing about the server's health.
            Err(UpErr::Rpc { code, message, data }) => return DownReply::error(rid, code, message, data),
            Err(e) => {
                if let UpErr::Forbidden { challenge } = &e {
                    if server.auth == AUTH_OAUTH {
                        oauth::note_step_up(&server.id, challenge.as_deref());
                    }
                }
                if matches!(e, UpErr::Unauthorized { .. } | UpErr::Forbidden { .. }) {
                    if server.auth != AUTH_OAUTH {
                        forget_cached_cred(&server.id);
                    }
                    set_calls_refused(&server.id, true);
                }
                return fail(rid, status_for(server, &e));
            }
        }
    }
    fail(rid, Status::new("unreachable", "the server kept ending the session"))
}

/// Open a downstream `GET` stream: `None` for an unknown server (404).
pub fn downstream_stream(path: &str) -> Option<mpsc::UnboundedReceiver<String>> {
    let id = get(id_from_path(path)?)?.id;
    let id = id.as_str();
    let (tx, rx) = mpsc::unbounded_channel();
    LIVE.lock().entry(id.to_string()).or_default().streams.push(tx);
    Some(rx)
}

/// The client ended its session (`DELETE`): give back its upstream session.
pub fn downstream_end(path: &str, sid: &str) {
    let Some(id) = id_from_path(path) else { return };
    let removed = LIVE.lock().get_mut(id).and_then(|l| l.down.remove(sid));
    let (Some(d), Some(server)) = (removed, get(id)) else { return };
    tokio::spawn(async move {
        if let Some(s) = d.upstream.lock().await.take() {
            let auth = bearer(&server).await.ok().flatten();
            Upstream::new(&server.url).end(auth.as_deref(), &s).await;
        }
    });
}

// ─── Lifecycle ──────────────────────────────────────────────────────────────

/// Load the configured servers and start keeping their lists current. Called once at startup.
pub fn start(app: AppHandle, state: Arc<AppState>) {
    let _ = APP.set((app, state));
    ensure_loaded();
    let enabled: Vec<String> = SERVERS.read().iter().filter(|s| s.enabled).map(|s| s.id.clone()).collect();
    for id in enabled {
        start_watcher(&id);
    }
}

pub fn valid_id(id: &str) -> Result<(), String> {
    let ok_chars = id.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' || c == '_');
    let starts = id.chars().next().is_some_and(|c| c.is_ascii_lowercase() || c.is_ascii_digit());
    if id.is_empty() || id.len() > 40 || !ok_chars || !starts {
        return Err("a name is 1–40 lowercase letters, digits, - or _, starting with a letter or digit".into());
    }
    if RESERVED.contains(&id) {
        return Err(format!("{id} is maiTerm's own server name"));
    }
    Ok(())
}

/// HTTPS, or plain HTTP to this machine.
pub fn valid_url(url: &str) -> Result<(), String> {
    let u = reqwest::Url::parse(url).map_err(|e| format!("not a URL: {e}"))?;
    let local = matches!(u.host_str(), Some("127.0.0.1" | "localhost" | "[::1]"));
    match u.scheme() {
        "https" => Ok(()),
        "http" if local => Ok(()),
        _ => Err("the URL must be https (or http to this machine)".into()),
    }
}

/// What `add` is given beyond the URL.
#[derive(Default)]
pub struct AddOptions {
    /// `token` auth: the pasted token.
    pub token: Option<String>,
    /// `oauth` auth: what `oauth::probe` found (no secret).
    pub oauth_plan: Option<Value>,
    /// `oauth` auth on a server that doesn't let apps register: a client id the human made.
    pub client_id: Option<String>,
    pub client_secret: Option<String>,
    /// Carrying out an agent's approved `mcp_add` request (see the URL check in `add`).
    pub from_agent_request: bool,
}

/// One URL, as servers compare: scheme and host are case-insensitive, the path is not; a trailing
/// slash doesn't count.
fn same_url(a: &str, b: &str) -> bool {
    match (reqwest::Url::parse(a.trim()), reqwest::Url::parse(b.trim())) {
        (Ok(x), Ok(y)) => {
            x.scheme() == y.scheme()
                && x.host_str() == y.host_str()
                && x.port_or_known_default() == y.port_or_known_default()
                && x.path().trim_end_matches('/') == y.path().trim_end_matches('/')
                && x.query() == y.query()
        }
        _ => a.trim().trim_end_matches('/') == b.trim().trim_end_matches('/'),
    }
}

/// Add a server.
pub fn add(id: &str, url: &str, auth: &str, opts: AddOptions, added_by: &str) -> Result<ProxyServer, String> {
    valid_id(id)?;
    valid_url(url)?;
    if ![AUTH_NONE, AUTH_TOKEN, AUTH_OAUTH].contains(&auth) {
        return Err(format!("unknown auth {auth}"));
    }
    if get(id).is_some() {
        return Err(format!("maiTerm already has an MCP server named {id}"));
    }
    // Nothing of an earlier server under this name may carry over: no cached credential, and
    // no keychain item a late sign-in or refresh left behind.
    LIVE.lock().remove(id);
    for key in [id.to_string(), format!("{id}#client")] {
        let _ = secrets::delete(&key);
    }
    // An agent's proposal never adds a second server for a URL maiTerm already serves: that would
    // put every tool in every session twice. Checked here, at the moment of adding, because the
    // request can be approved after the same URL was added another way. The human may do it on
    // purpose, though: two accounts of one service (work and personal Linear), each signed in
    // separately and offered to different workspaces.
    if opts.from_agent_request {
        if let Some(other) = all().into_iter().find(|s| same_url(&s.url, url)) {
            return Err(format!("maiTerm already serves {url} as {}", other.id));
        }
    }
    let mut oauth_plan = None;
    // Without a token (an agent's proposal can't carry one) the server waits as "needs a token"
    // until the human pastes one in Preferences.
    if let Some(tok) = opts.token.as_deref().map(str::trim).filter(|t| !t.is_empty()) {
        if auth == AUTH_TOKEN {
            secrets::store(id, &secrets::Cred { access_token: tok.to_string(), ..Default::default() }).map_err(|e| e.to_string())?;
        }
    }
    if auth == AUTH_OAUTH {
        let mut plan = opts.oauth_plan.ok_or("the server's sign-in details are missing")?;
        let manual = opts.client_id.as_deref().map(str::trim).filter(|c| !c.is_empty());
        if let Some(cid) = manual {
            plan["manual_client_id"] = cid.into();
            if let Some(secret) = opts.client_secret.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
                secrets::store(&format!("{id}#client"), &secrets::Cred { access_token: secret.to_string(), ..Default::default() }).map_err(|e| e.to_string())?;
            }
        } else if plan.get("registration_endpoint").and_then(Value::as_str).is_none() {
            return Err("this server doesn't let apps register themselves: give a client id, or use a token".into());
        }
        oauth_plan = Some(plan);
    }
    let server = ProxyServer {
        id: id.to_string(),
        url: url.to_string(),
        auth: auth.to_string(),
        enabled: true,
        workspaces: vec![],
        catalog: None,
        oauth: oauth_plan,
        added_at: now_ms(),
        added_by: added_by.to_string(),
    };
    SERVERS.write().push(server.clone());
    save()?;
    log::info!("[mcp-proxy] added {id} ({auth}) by {added_by}");
    start_watcher(id);
    sync_entries();
    Ok(server)
}

pub fn remove(id: &str) -> Result<(), String> {
    let server_url_for_cleanup = get(id).ok_or_else(|| format!("no MCP server named {id}"))?.url;
    stop_watcher(id);
    oauth::forget(id);
    let live = LIVE.lock().remove(id);
    SERVERS.write().retain(|s| s.id != id);
    save()?;
    sync_entries();
    for key in [id.to_string(), format!("{id}#client")] {
        if let Err(e) = secrets::delete(&key) {
            log::warn!("[mcp-proxy] {id}: removed, but a keychain item stays: {e}");
        }
    }
    // Agents still connected learn the list is gone the same way they learn of any change,
    // and the upstream sessions they held are ended.
    if let Some(l) = live {
        for tx in l.streams {
            let _ = tx.send(json!({ "jsonrpc": "2.0", "method": "notifications/tools/list_changed" }).to_string());
        }
        let auth = l.cred.map(|c| c.access_token);
        let url = server_url_for_cleanup.clone();
        let downs: Vec<Arc<DownSession>> = l.down.into_values().collect();
        if !downs.is_empty() && tokio::runtime::Handle::try_current().is_ok() {
            tokio::spawn(async move {
                let up = Upstream::new(&url);
                for d in downs {
                    if let Some(s) = d.upstream.lock().await.take() {
                        up.end(auth.as_deref(), &s).await;
                    }
                }
            });
        }
    }
    log::info!("[mcp-proxy] removed {id}");
    Ok(())
}

pub fn set_token(id: &str, token: &str) -> Result<(), String> {
    let server = get(id).ok_or_else(|| format!("no MCP server named {id}"))?;
    if server.auth != AUTH_TOKEN {
        return Err(format!("{id} does not use a pasted token"));
    }
    secrets::store(id, &secrets::Cred { access_token: token.trim().to_string(), ..Default::default() }).map_err(|e| e.to_string())?;
    forget_cached_cred(id);
    set_calls_refused(id, false);
    // A disabled server stays quiet: its watcher starts when it is switched back on.
    if server.enabled {
        start_watcher(id);
    }
    Ok(())
}

pub fn set_enabled(id: &str, enabled: bool) -> Result<ProxyServer, String> {
    let s = update(id, |s| s.enabled = enabled)?;
    if enabled {
        start_watcher(id);
    } else {
        stop_watcher(id);
    }
    sync_entries();
    announce_list_changed(id);
    Ok(s)
}

pub fn set_workspaces(id: &str, workspaces: Vec<String>) -> Result<ProxyServer, String> {
    let s = update(id, |s| s.workspaces = workspaces)?;
    announce_list_changed(id);
    Ok(s)
}

// ─── What the frontend sees ────────────────────────────────────────────────

/// A server as the Preferences pane draws it. No credential, ever.
#[derive(Serialize)]
pub struct ServerView {
    pub id: String,
    pub url: String,
    pub auth: String,
    pub enabled: bool,
    pub workspaces: Vec<String>,
    pub status: Status,
    pub tools: Vec<String>,
    pub fetched_at: Option<u64>,
    pub added_at: u64,
    pub added_by: String,
    /// The entry name agents see (`<id>-dev` from a dev build).
    pub entry_name: String,
    /// Runtimes where a native entry already holds that name, so ours isn't written there.
    pub collisions: Vec<String>,
    /// Native entries reaching the same URL ("Claude: linear-native"): every tool twice.
    pub duplicates: Vec<String>,
}

pub fn view(s: &ProxyServer) -> ServerView {
    let collisions = {
        let c = COLLISIONS.lock();
        let mut v = Vec::new();
        if c.claude.contains(&s.id) {
            v.push("Claude".to_string());
        }
        if c.codex.contains(&s.id) {
            v.push("Codex".to_string());
        }
        v
    };
    ServerView {
        id: s.id.clone(),
        url: s.url.clone(),
        auth: s.auth.clone(),
        enabled: s.enabled,
        workspaces: s.workspaces.clone(),
        status: status(&s.id),
        tools: s
            .catalog
            .as_ref()
            .map(|c| c.tools.iter().filter_map(|t| t.get("name").and_then(Value::as_str).map(str::to_string)).collect())
            .unwrap_or_default(),
        fetched_at: s.catalog.as_ref().map(|c| c.fetched_at),
        added_at: s.added_at,
        added_by: s.added_by.clone(),
        entry_name: entries::entry_name(&s.id),
        collisions,
        duplicates: entries::native_duplicates(&s.url),
    }
}

// Async, and the keychain calls on a blocking thread: a keychain access prompt (routine on dev
// builds, whose signature changes every rebuild) must not hold up the UI or an async worker.

async fn blocking<T: Send + 'static>(f: impl FnOnce() -> Result<T, String> + Send + 'static) -> Result<T, String> {
    tokio::task::spawn_blocking(f).await.map_err(|e| e.to_string())?
}

#[tauri::command]
pub async fn mcp_servers_list() -> Vec<ServerView> {
    blocking(|| Ok(all().iter().map(view).collect())).await.unwrap_or_default()
}

/// What a URL needs before it is added: no sign-in, a token, or an OAuth sign-in (§5).
#[tauri::command]
pub async fn mcp_server_probe(url: String) -> Result<oauth::Probe, String> {
    valid_url(&url)?;
    let mut p = oauth::probe(&url).await?;
    let u = url.clone();
    p.duplicates = blocking(move || Ok(entries::native_duplicates(&u))).await.unwrap_or_default();
    Ok(p)
}

/// `request_id`: set when this carries out an agent's approved `mcp_add` request, which is then
/// what the server records as having added it.
#[tauri::command]
pub async fn mcp_server_add(
    id: String,
    url: String,
    auth: String,
    token: Option<String>,
    client_id: Option<String>,
    client_secret: Option<String>,
    request_id: Option<String>,
) -> Result<ServerView, String> {
    let oauth_plan = if auth == AUTH_OAUTH {
        valid_url(&url)?;
        let p = oauth::probe(&url).await?;
        if p.auth != AUTH_OAUTH {
            return Err(format!("{url} doesn't offer a sign-in maiTerm can use: {}", p.detail));
        }
        p.plan
    } else {
        None
    };
    let opts = AddOptions { token, oauth_plan, client_id, client_secret, from_agent_request: request_id.is_some() };
    let by = match request_id {
        Some(r) => format!("agent request {r}"),
        None => "human".to_string(),
    };
    blocking(move || add(&id, &url, &auth, opts, &by).map(|s| view(&s))).await
}

/// Names the human's own MCP entries hold in Claude's and Codex's configs (a proxied server under
/// one isn't written there).
#[tauri::command]
pub async fn mcp_native_names() -> Vec<String> {
    blocking(|| Ok(entries::native_names())).await.unwrap_or_default()
}

/// Start a sign-in: the URL for the browser. The sign-in finishes at `/oauth/callback`.
#[tauri::command]
pub async fn mcp_server_sign_in(id: String) -> Result<String, String> {
    oauth::begin(&id).await
}

#[tauri::command]
pub async fn mcp_server_remove(id: String) -> Result<(), String> {
    blocking(move || remove(&id)).await
}

#[tauri::command]
pub async fn mcp_server_set_token(id: String, token: String) -> Result<(), String> {
    blocking(move || set_token(&id, &token)).await
}

#[tauri::command]
pub async fn mcp_server_set_enabled(id: String, enabled: bool) -> Result<ServerView, String> {
    set_enabled(&id, enabled).map(|s| view(&s))
}

#[tauri::command]
pub async fn mcp_server_set_workspaces(id: String, workspaces: Vec<String>) -> Result<ServerView, String> {
    set_workspaces(&id, workspaces).map(|s| view(&s))
}

/// **Move to maiTerm**: remove the human's native entries for this server (same name or same
/// URL, in Claude and Codex), then write ours. Only ever from the human's click.
#[tauri::command]
pub async fn mcp_server_adopt_native(id: String) -> Result<Vec<String>, String> {
    let server = get(&id).ok_or_else(|| format!("no MCP server named {id}"))?;
    let removed = blocking(move || entries::adopt_native(&server.id, &server.url)).await?;
    sync_entries();
    Ok(removed)
}

#[tauri::command]
pub async fn mcp_server_refresh(id: String) -> Result<ServerView, String> {
    refresh(&id).await?;
    get(&id).map(|s| view(&s)).ok_or_else(|| format!("no MCP server named {id}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_are_entry_safe_and_never_maiterms_own() {
        assert!(valid_id("linear").is_ok());
        assert!(valid_id("my-server_2").is_ok());
        assert!(valid_id("maiterm").is_err());
        assert!(valid_id("Linear").is_err());
        assert!(valid_id("-x").is_err());
        assert!(valid_id("a b").is_err());
        assert!(valid_id("").is_err());
    }

    #[test]
    fn urls_are_https_or_this_machine() {
        assert!(valid_url("https://mcp.linear.app/mcp").is_ok());
        assert!(valid_url("http://127.0.0.1:9000/mcp").is_ok());
        assert!(valid_url("http://localhost/mcp").is_ok());
        assert!(valid_url("http://mcp.example.com/mcp").is_err());
        assert!(valid_url("ftp://x").is_err());
    }

    #[test]
    fn a_name_not_served_here_is_an_empty_server_not_a_failure() {
        let body = |m: &str| json!({ "jsonrpc": "2.0", "id": 1, "method": m, "params": { "protocolVersion": "2025-06-18" } }).to_string();
        let parse = |r: DownReply| -> Value { serde_json::from_str(&r.body.unwrap()).unwrap() };
        let init = parse(not_served("theirs", &body("initialize")));
        assert_eq!(init["result"]["protocolVersion"], "2025-06-18");
        assert_eq!(parse(not_served("theirs", &body("tools/list")))["result"]["tools"], json!([]));
        assert_eq!(parse(not_served("theirs", &body("tools/call")))["result"]["isError"], true);
        let note = not_served("theirs", &json!({ "jsonrpc": "2.0", "method": "notifications/initialized" }).to_string());
        assert_eq!(note.status, 202);
    }

    #[test]
    fn urls_compare_with_a_case_sensitive_path() {
        assert!(same_url("https://MCP.Linear.app/mcp/", "https://mcp.linear.app/mcp"));
        assert!(same_url("https://mcp.linear.app:443/mcp", "https://mcp.linear.app/mcp"));
        assert!(!same_url("https://x.example/mcp/A", "https://x.example/mcp/a"));
        assert!(!same_url("https://x.example/mcp", "http://x.example/mcp"));
    }

    #[test]
    fn a_catalog_read_again_is_not_a_change() {
        let a = Catalog { tools: vec![json!({ "name": "a" })], server_info: json!({}), instructions: None, fetched_at: 1 };
        let b = Catalog { fetched_at: 2, ..a.clone() };
        assert!(a.same_as(&b));
        let c = Catalog { tools: vec![json!({ "name": "b" })], ..a.clone() };
        assert!(!a.same_as(&c));
    }
}
