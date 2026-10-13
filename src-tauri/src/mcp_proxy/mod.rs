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
}

static SERVERS: LazyLock<RwLock<Vec<ProxyServer>>> = LazyLock::new(Default::default);
static LIVE: LazyLock<Mutex<HashMap<String, Live>>> = LazyLock::new(Default::default);
static SAVE: Mutex<()> = Mutex::new(());
static APP: OnceLock<(AppHandle, Arc<AppState>)> = OnceLock::new();

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as u64)
}

fn store_path() -> Option<PathBuf> {
    dirs::data_dir().map(|p| p.join(crate::state::persistence::app_data_slug()).join("mcp-servers.json"))
}

fn load() -> Vec<ProxyServer> {
    let Some(path) = store_path() else { return vec![] };
    let Ok(raw) = std::fs::read_to_string(&path) else { return vec![] };
    match serde_json::from_str::<StoreFile>(&raw) {
        Ok(f) => f.servers,
        Err(e) => {
            // Keep the unreadable file for a human to look at, rather than overwriting it with
            // an empty list on the next save.
            let aside = path.with_extension(format!("unreadable-{}.json", now_ms()));
            let _ = std::fs::rename(&path, &aside);
            log::error!("[mcp-proxy] {} is unreadable ({e}); moved aside to {}", path.display(), aside.display());
            vec![]
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
    if server.auth == AUTH_NONE {
        return Ok(None);
    }
    if let Some(c) = LIVE.lock().get(&server.id).and_then(|l| l.cred.clone()) {
        return Ok(Some(c.access_token));
    }
    let id = server.id.clone();
    let read = tokio::task::spawn_blocking(move || secrets::read(&id))
        .await
        .map_err(|e| e.to_string())?
        .map_err(|e| e.to_string())?;
    match read {
        Some(c) => {
            let tok = c.access_token.clone();
            LIVE.lock().entry(server.id.clone()).or_default().cred = Some(c);
            Ok(Some(tok))
        }
        None => Err(match server.auth.as_str() {
            AUTH_OAUTH => "not signed in yet".into(),
            _ => "no token stored".into(),
        }),
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
    let server = get(id).ok_or_else(|| format!("no MCP server named {id}"))?;
    let auth = match bearer(&server).await {
        Ok(a) => a,
        Err(why) => {
            set_status(id, needs_sign_in(&server, &why));
            return Err(why);
        }
    };
    let up = Upstream::new(&server.url);
    let result = async {
        let session = up.initialize(auth.as_deref()).await?;
        let tools = up.list_tools(auth.as_deref(), &session).await;
        up.end(auth.as_deref(), &session).await;
        Ok::<_, UpErr>((session, tools?))
    }
    .await;
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
    set_status(id, Status::new("connected", format!("{count} tools")));
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
        // A stream that dies at once would otherwise reconnect in a tight loop.
        if opened.elapsed() < Duration::from_secs(60) {
            tokio::time::sleep(Duration::from_secs(60)).await;
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

pub async fn downstream_post(id: &str, sid: Option<&str>, tab: Option<&str>, body: &str) -> DownReply {
    let Some(server) = get(id) else { return DownReply::status(404) };
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
                let _ = tokio::time::timeout(Duration::from_secs(15), refresh(id)).await;
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

/// Forward a `tools/call` on this downstream session's own upstream session.
async fn call(server: &ProxyServer, sid: Option<&str>, rid: Value, params: Value) -> DownReply {
    let fail = |rid: Value, st: Status| {
        let text = call_failure_text(server, &st);
        set_status(&server.id, st);
        DownReply::result(rid, json!({ "content": [{ "type": "text", "text": text }], "isError": true }))
    };
    let auth = match bearer(server).await {
        Ok(a) => a,
        Err(why) => return fail(rid, needs_sign_in(server, &why)),
    };
    let up = Upstream::new(&server.url);
    let down = down_session(&server.id, sid);
    // Two attempts only when the upstream forgot the session: it ran nothing, so a retry on a
    // fresh one is safe. Nothing else is retried, since a tool call need not be idempotent.
    for attempt in 0..2 {
        let session = {
            let mut slot = down.upstream.lock().await;
            if slot.is_none() {
                match up.initialize(auth.as_deref()).await {
                    Ok(s) => *slot = Some(s),
                    Err(e) => return fail(rid, status_for(server, &e)),
                }
            }
            slot.clone().expect("set above")
        };
        match up.call_tool(auth.as_deref(), &session, params.clone()).await {
            Ok((result, list_changed)) => {
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
                *down.upstream.lock().await = None;
            }
            // The server's own JSON-RPC error goes back as it was sent: an unknown tool, bad
            // arguments. It says nothing about the server's health.
            Err(UpErr::Rpc { code, message, data }) => return DownReply::error(rid, code, message, data),
            Err(e) => {
                if matches!(e, UpErr::Unauthorized { .. }) {
                    forget_cached_cred(&server.id);
                }
                return fail(rid, status_for(server, &e));
            }
        }
    }
    fail(rid, Status::new("unreachable", "the server kept ending the session"))
}

/// Open a downstream `GET` stream: `None` for an unknown server (404).
pub fn downstream_stream(id: &str) -> Option<mpsc::UnboundedReceiver<String>> {
    get(id)?;
    let (tx, rx) = mpsc::unbounded_channel();
    LIVE.lock().entry(id.to_string()).or_default().streams.push(tx);
    Some(rx)
}

/// The client ended its session (`DELETE`): give back its upstream session.
pub fn downstream_end(id: &str, sid: &str) {
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
    let servers = load();
    let enabled: Vec<String> = servers.iter().filter(|s| s.enabled).map(|s| s.id.clone()).collect();
    *SERVERS.write() = servers;
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

/// Add a server. `token` is required for `token` auth and ignored otherwise.
pub fn add(id: &str, url: &str, auth: &str, token: Option<&str>, added_by: &str) -> Result<ProxyServer, String> {
    valid_id(id)?;
    valid_url(url)?;
    if ![AUTH_NONE, AUTH_TOKEN, AUTH_OAUTH].contains(&auth) {
        return Err(format!("unknown auth {auth}"));
    }
    if get(id).is_some() {
        return Err(format!("maiTerm already has an MCP server named {id}"));
    }
    if auth == AUTH_TOKEN {
        let tok = token.map(str::trim).filter(|t| !t.is_empty()).ok_or("a token is required")?;
        secrets::store(id, &secrets::Cred { access_token: tok.to_string(), ..Default::default() }).map_err(|e| e.to_string())?;
    }
    let server = ProxyServer {
        id: id.to_string(),
        url: url.to_string(),
        auth: auth.to_string(),
        enabled: true,
        workspaces: vec![],
        catalog: None,
        oauth: None,
        added_at: now_ms(),
        added_by: added_by.to_string(),
    };
    SERVERS.write().push(server.clone());
    save()?;
    log::info!("[mcp-proxy] added {id} ({auth}) by {added_by}");
    start_watcher(id);
    Ok(server)
}

pub fn remove(id: &str) -> Result<(), String> {
    get(id).ok_or_else(|| format!("no MCP server named {id}"))?;
    stop_watcher(id);
    let live = LIVE.lock().remove(id);
    SERVERS.write().retain(|s| s.id != id);
    save()?;
    if let Err(e) = secrets::delete(id) {
        log::warn!("[mcp-proxy] {id}: removed, but its keychain item stays: {e}");
    }
    // Agents still connected learn the list is gone the same way they learn of any change.
    if let Some(l) = live {
        for tx in l.streams {
            let _ = tx.send(json!({ "jsonrpc": "2.0", "method": "notifications/tools/list_changed" }).to_string());
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
    start_watcher(id);
    Ok(())
}

pub fn set_enabled(id: &str, enabled: bool) -> Result<ProxyServer, String> {
    let s = update(id, |s| s.enabled = enabled)?;
    if enabled {
        start_watcher(id);
    } else {
        stop_watcher(id);
    }
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
}

pub fn view(s: &ProxyServer) -> ServerView {
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
    }
}

#[tauri::command]
pub fn mcp_servers_list() -> Vec<ServerView> {
    all().iter().map(view).collect()
}

#[tauri::command]
pub fn mcp_server_add(id: String, url: String, auth: String, token: Option<String>) -> Result<ServerView, String> {
    add(&id, &url, &auth, token.as_deref(), "human").map(|s| view(&s))
}

#[tauri::command]
pub fn mcp_server_remove(id: String) -> Result<(), String> {
    remove(&id)
}

#[tauri::command]
pub fn mcp_server_set_token(id: String, token: String) -> Result<(), String> {
    set_token(&id, &token)
}

#[tauri::command]
pub fn mcp_server_set_enabled(id: String, enabled: bool) -> Result<ServerView, String> {
    set_enabled(&id, enabled).map(|s| view(&s))
}

#[tauri::command]
pub fn mcp_server_set_workspaces(id: String, workspaces: Vec<String>) -> Result<ServerView, String> {
    set_workspaces(&id, workspaces).map(|s| view(&s))
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
    fn a_catalog_read_again_is_not_a_change() {
        let a = Catalog { tools: vec![json!({ "name": "a" })], server_info: json!({}), instructions: None, fetched_at: 1 };
        let b = Catalog { fetched_at: 2, ..a.clone() };
        assert!(a.same_as(&b));
        let c = Catalog { tools: vec![json!({ "name": "b" })], ..a.clone() };
        assert!(!a.same_as(&c));
    }
}
