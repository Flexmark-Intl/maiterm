//! maiLink mobile-companion LAN bridge (P2a: gated TLS listener + heartbeat).
//!
//! A *separate*, opt-in HTTPS listener bound to the LAN interface — distinct from the
//! localhost-only Claude-Code IDE/MCP server in `claude_code/server.rs`. It is started only
//! when `preferences.mailink_enabled` is true. The phone connects over self-signed TLS and
//! pins the cert by SHA-256 fingerprint (carried out-of-band in the pairing QR).
//!
//! P2a stands up the TLS stack and a `/heartbeat` probe so the cert + fingerprint pipeline
//! can be validated end-to-end. Pairing/auth and `/chats` land in P2b. Full contract:
//! `docs/mailink-protocol.md`.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use axum::{
    extract::{
        ws::{Message, WebSocket, WebSocketUpgrade},
        Path, Query, State,
    },
    http::{header, HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::{get, post},
    Json, Router,
};
use axum_server::tls_rustls::RustlsConfig;
use base64::Engine as _;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use crate::state::app_state::AgentSessionState;
use crate::state::workspace::TabType;
use crate::state::{AgentRuntime, AppState, MailinkDevice};

/// Default LAN port. The pairing QR carries the actual host:port, so this is just a
/// sensible default until a `mailink_port` preference is wired (P2b).
const DEFAULT_PORT: u16 = 8765;

/// Everything the async listener needs, resolved synchronously during app setup.
pub struct MailinkConfig {
    pub port: u16,
    pub cert_pem: String,
    pub key_pem: String,
    /// `"sha256/" + base64(SHA256(leaf-cert DER))` — the value the phone pins (see
    /// docs/mailink-protocol.md §3.1, agreed format with the maiLink app).
    pub fingerprint: String,
    /// Long-lived bearer token for development integration: lets the maiLink app point its
    /// pinned transport at the live endpoint without the full QR→/pair handshake (which lands
    /// in P2b proper). Persisted; logged at startup. NOT a substitute for per-device pairing.
    pub dev_token: String,
}

/// Shared, cheap-to-clone handler state for the API surface.
#[derive(Clone)]
struct ApiState {
    app: Arc<AppState>,
    server_name: String,
    fingerprint: String,
    dev_token: String,
}

/// `~/Library/Application Support/<slug>/mailink/` (or the OS equivalent).
fn mailink_dir() -> Option<PathBuf> {
    dirs::data_dir()
        .map(|p| p.join(crate::state::persistence::app_data_slug()).join("mailink"))
}

/// Load the persisted self-signed cert, or generate + persist one on first run. Persisting
/// keeps the fingerprint stable across restarts, so a paired phone's pin stays valid (the
/// pin only rotates when the cert is regenerated — e.g. the files are deleted).
fn load_or_generate_cert() -> Result<(String, String), String> {
    let dir = mailink_dir().ok_or("no data dir")?;
    let cert_path = dir.join("cert.pem");
    let key_path = dir.join("key.pem");

    if let (Ok(cert), Ok(key)) = (
        std::fs::read_to_string(&cert_path),
        std::fs::read_to_string(&key_path),
    ) {
        if !cert.trim().is_empty() && !key.trim().is_empty() {
            return Ok((cert, key));
        }
    }

    // SAN-agnostic: the phone verifies by pinned fingerprint only and bypasses hostname/SAN
    // (docs §3.1), so the same cert validates at any LAN/WireGuard IP.
    let certified = rcgen::generate_simple_self_signed(vec!["maiterm-mailink".to_string()])
        .map_err(|e| format!("rcgen: {e}"))?;
    let cert_pem = certified.cert.pem();
    let key_pem = certified.key_pair.serialize_pem();

    std::fs::create_dir_all(&dir).map_err(|e| format!("mkdir {dir:?}: {e}"))?;
    if let Err(e) = std::fs::write(&cert_path, &cert_pem) {
        log::warn!("[maiLink] could not persist cert: {e}");
    }
    if let Err(e) = std::fs::write(&key_path, &key_pem) {
        log::warn!("[maiLink] could not persist key: {e}");
    }
    Ok((cert_pem, key_pem))
}

/// Decode a single-cert PEM to its DER bytes (strip the armor lines, base64-decode the body).
fn pem_to_der(pem: &str) -> Vec<u8> {
    let body: String = pem
        .lines()
        .filter(|l| !l.starts_with("-----"))
        .collect::<Vec<_>>()
        .join("");
    base64::engine::general_purpose::STANDARD
        .decode(body.trim())
        .unwrap_or_default()
}

/// `"sha256/" + base64(SHA256(DER))` over the full leaf cert DER (NOT SPKI). Standard
/// Base64, `=`-padded. Matches `openssl x509 -outform DER | openssl dgst -sha256 -binary | base64`.
fn fingerprint_of_pem(cert_pem: &str) -> String {
    let der = pem_to_der(cert_pem);
    let digest = Sha256::digest(&der);
    format!(
        "sha256/{}",
        base64::engine::general_purpose::STANDARD.encode(digest)
    )
}

/// Load the persisted dev bearer token, or mint + persist a fresh 32-char one.
fn load_or_generate_dev_token() -> Result<String, String> {
    let dir = mailink_dir().ok_or("no data dir")?;
    let path = dir.join("dev-token.txt");
    if let Ok(t) = std::fs::read_to_string(&path) {
        let t = t.trim().to_string();
        if !t.is_empty() {
            return Ok(t);
        }
    }
    const CHARS: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789";
    let token: String = {
        use rand::Rng;
        let mut rng = rand::thread_rng();
        (0..32)
            .map(|_| CHARS[rng.gen_range(0..CHARS.len())] as char)
            .collect()
    };
    std::fs::create_dir_all(&dir).map_err(|e| format!("mkdir {dir:?}: {e}"))?;
    if let Err(e) = std::fs::write(&path, &token) {
        log::warn!("[maiLink] could not persist dev token: {e}");
    }
    Ok(token)
}

/// Synchronous setup during Tauri `setup()`: resolve the cert + fingerprint + dev token and
/// log the pin. Returns `None` (with a logged reason) if init fails — the app still boots.
pub fn prepare(app_state: &Arc<AppState>) -> Option<MailinkConfig> {
    let (cert_pem, key_pem) = match load_or_generate_cert() {
        Ok(v) => v,
        Err(e) => {
            log::error!("[maiLink] cert init failed, bridge not started: {e}");
            return None;
        }
    };
    let fingerprint = fingerprint_of_pem(&cert_pem);
    let dev_token = match load_or_generate_dev_token() {
        Ok(t) => t,
        Err(e) => {
            log::error!("[maiLink] dev-token init failed, bridge not started: {e}");
            return None;
        }
    };
    let port = DEFAULT_PORT;
    // Publish (fp, port) so the pairing-code command can build the QR payload.
    *app_state.mailink_info.write() = Some((fingerprint.clone(), port));
    log::info!("[maiLink] bridge enabled — listening on 0.0.0.0:{port} (TLS). Pin fp = {fingerprint}");
    log::info!("[maiLink] dev bearer token (Authorization: Bearer …): {dev_token}");
    Some(MailinkConfig {
        port,
        cert_pem,
        key_pem,
        fingerprint,
        dev_token,
    })
}

/// Background task: install the rustls crypto provider, build the router, and serve over TLS.
pub async fn serve(app_state: Arc<AppState>, cfg: MailinkConfig) {
    // rustls 0.23 needs a process-default crypto provider before any TLS config is built.
    // Pin ring explicitly (idempotent; ignore the Err if another component already set one).
    let _ = rustls::crypto::ring::default_provider().install_default();

    let api = ApiState {
        app: app_state,
        server_name: "maiTerm".to_string(),
        fingerprint: cfg.fingerprint.clone(),
        dev_token: cfg.dev_token.clone(),
    };
    let router = Router::new()
        .route("/mailink/v1/heartbeat", get(heartbeat))
        .route("/mailink/v1/chats", get(chats_list))
        .route("/mailink/v1/chats/{tab_id}", get(chat_detail))
        .route("/mailink/v1/chats/{tab_id}/context", get(chat_context))
        .route("/mailink/v1/chats/{tab_id}/message", post(post_message))
        .route("/mailink/v1/chats/{tab_id}/respond", post(post_respond))
        .route("/mailink/v1/chats/{tab_id}/interrupt", post(post_interrupt))
        .route("/mailink/v1/ws", get(ws_handler))
        .route("/mailink/v1/pair", post(post_pair))
        .route("/mailink/v1/push-register", post(post_push_register))
        .with_state(api);

    let tls = match RustlsConfig::from_pem(cfg.cert_pem.into_bytes(), cfg.key_pem.into_bytes()).await
    {
        Ok(t) => t,
        Err(e) => {
            log::error!("[maiLink] TLS config failed, bridge not started: {e}");
            return;
        }
    };

    let addr = std::net::SocketAddr::from(([0, 0, 0, 0], cfg.port));
    log::info!("[maiLink] serving https://0.0.0.0:{}", cfg.port);
    if let Err(e) = axum_server::bind_rustls(addr, tls)
        .serve(router.into_make_service())
        .await
    {
        log::error!("[maiLink] listener stopped: {e}");
    }
}

// ─── handlers ───────────────────────────────────────────────────────────────────────────

/// Unauthenticated liveness probe: confirms the bridge is up and echoes the pinned
/// fingerprint so a client (or a human with curl) can cross-check the trust anchor.
async fn heartbeat(State(s): State<ApiState>) -> Json<Value> {
    Json(json!({
        "ok": true,
        "now": now_ms(),
        "server_name": s.server_name,
        "fp": s.fingerprint,
    }))
}

/// GET /mailink/v1/chats — the maiLink-native tabs as chats, with live agent state.
async fn chats_list(
    State(s): State<ApiState>,
    headers: HeaderMap,
) -> Result<Json<Value>, StatusCode> {
    authorize(&s, &headers)?;
    Ok(Json(json!(build_chats(&s.app))))
}

/// GET /mailink/v1/chats/{tabId} — one chat with a (v1: distilled-tail) transcript + any
/// open prompt. `before`/`limit` paging params are accepted but ignored in v1 (reserved).
async fn chat_detail(
    State(s): State<ApiState>,
    headers: HeaderMap,
    Path(tab_id): Path<String>,
) -> Result<Json<Value>, StatusCode> {
    authorize(&s, &headers)?;
    build_chat_detail(&s.app, &tab_id)
        .map(Json)
        .ok_or(StatusCode::NOT_FOUND)
}

#[derive(serde::Deserialize)]
struct ContextQuery {
    lines: Option<usize>,
}

/// GET /mailink/v1/chats/{tabId}/context — distilled recent plain-text for the tab.
async fn chat_context(
    State(s): State<ApiState>,
    headers: HeaderMap,
    Path(tab_id): Path<String>,
    Query(q): Query<ContextQuery>,
) -> Result<Json<Value>, StatusCode> {
    authorize(&s, &headers)?;
    let lines = q.lines.unwrap_or(40).min(500);
    let text = pty_for_tab(&s.app, &tab_id)
        .and_then(|pty| crate::commands::terminal::recent_text(&s.app, &pty, lines).ok())
        .unwrap_or_default();
    Ok(Json(json!({ "text": text, "truncated": false })))
}

#[derive(serde::Deserialize)]
struct MessageBody {
    text: String,
    #[serde(default)]
    submit: bool,
}

/// POST /chats/{tabId}/message — inject a free-text message / proactive command into the
/// tab's agent. Rides the same bracketed-paste + deferred-CR convention the agent bridge
/// uses. 409 if the tab has no live PTY (dormant — nothing to inject into yet).
async fn post_message(
    State(s): State<ApiState>,
    headers: HeaderMap,
    Path(tab_id): Path<String>,
    Json(body): Json<MessageBody>,
) -> Result<Json<Value>, StatusCode> {
    authorize(&s, &headers)?;
    let pty = pty_for_tab(&s.app, &tab_id).ok_or(StatusCode::CONFLICT)?;
    inject_text(&s.app, &pty, &body.text, body.submit)
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    Ok(Json(json!({ "status": "delivered", "msg_id": format!("m_{}", now_ms()) })))
}

#[derive(serde::Deserialize)]
struct RespondBody {
    choice: String,
    #[serde(default)]
    prompt_id: Option<String>,
}

/// POST /chats/{tabId}/respond — answer the tab's currently-open prompt. `prompt_id` is the
/// stale-guard: if it doesn't match the open prompt, we reject with `{ok:false,
/// reason:"stale"}` rather than inject the keystroke into whatever prompt is open NOW.
async fn post_respond(
    State(s): State<ApiState>,
    headers: HeaderMap,
    Path(tab_id): Path<String>,
    Json(body): Json<RespondBody>,
) -> Result<Json<Value>, StatusCode> {
    authorize(&s, &headers)?;
    let current = current_prompt(&s.app, &tab_id);
    let (kind, cur_id) = match current {
        Some(p) => p,
        None => return Ok(Json(json!({ "ok": false, "reason": "stale" }))),
    };
    if let Some(pid) = &body.prompt_id {
        if pid != &cur_id {
            return Ok(Json(json!({ "ok": false, "reason": "stale" })));
        }
    }
    let pty = pty_for_tab(&s.app, &tab_id).ok_or(StatusCode::CONFLICT)?;
    match kind {
        // permission menu: a single numeric keystroke selects the option (no bracketed paste)
        "permission" => {
            let key = permission_key(&body.choice);
            crate::pty::write_pty(&s.app, &pty, key.as_bytes())
                .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
        }
        // free-text question: the choice IS the answer text → paste + submit
        _ => {
            inject_text(&s.app, &pty, &body.choice, true)
                .await
                .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
        }
    }
    Ok(Json(json!({ "ok": true })))
}

/// POST /chats/{tabId}/interrupt — send Esc to the agent (the documented "human interrupts"
/// gesture).
async fn post_interrupt(
    State(s): State<ApiState>,
    headers: HeaderMap,
    Path(tab_id): Path<String>,
) -> Result<Json<Value>, StatusCode> {
    authorize(&s, &headers)?;
    let pty = pty_for_tab(&s.app, &tab_id).ok_or(StatusCode::CONFLICT)?;
    crate::pty::write_pty(&s.app, &pty, b"\x1b").map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    Ok(Json(json!({ "ok": true })))
}

#[derive(serde::Deserialize)]
struct PairBody {
    code: String,
    #[serde(default)]
    device_name: Option<String>,
}

/// POST /pair — redeem a one-time pairing code (from the QR) → mint a per-device bearer
/// token, persist the device (token stored hashed), return the raw token ONCE.
async fn post_pair(
    State(s): State<ApiState>,
    Json(body): Json<PairBody>,
) -> Result<Json<Value>, StatusCode> {
    // validate + consume the code atomically
    let valid = {
        let mut codes = s.app.mailink_pairing_codes.write();
        match codes.remove(&body.code) {
            Some(expiry) => expiry > std::time::Instant::now(),
            None => false,
        }
    };
    if !valid {
        return Err(StatusCode::UNAUTHORIZED);
    }

    let token = gen_token(32);
    let device = MailinkDevice {
        id: uuid::Uuid::new_v4().to_string(),
        name: body
            .device_name
            .filter(|n| !n.trim().is_empty())
            .unwrap_or_else(|| "maiLink device".to_string()),
        token_hash: sha256_hex(token.as_bytes()),
        push_token: None,
        push_platform: None,
        push_env: None,
        created_at: now_ms() as i64,
        last_seen_at: now_ms() as i64,
    };
    let device_id = device.id.clone();
    let data_clone = {
        let mut data = s.app.app_data.write();
        data.preferences.mailink_devices.push(device);
        data.clone()
    };
    let _ = crate::state::save_state(&data_clone);
    log::info!("[maiLink] paired new device {device_id}");
    Ok(Json(json!({
        "device_id": device_id,
        "token": token,
        "server_name": s.server_name,
    })))
}

#[derive(serde::Deserialize)]
struct PushRegBody {
    token: String,
    platform: String,
    #[serde(default)]
    env: Option<String>,
}

/// POST /push-register — store the device's push token (APNs/FCM) so the doorbell relay can
/// reach it. Must be called by a PAIRED device (not the dev token), since it attaches to a
/// device record.
async fn post_push_register(
    State(s): State<ApiState>,
    headers: HeaderMap,
    Json(body): Json<PushRegBody>,
) -> Result<Json<Value>, StatusCode> {
    authorize(&s, &headers)?;
    let hash = sha256_hex(bearer_token(&headers).as_bytes());
    let data_clone = {
        let mut data = s.app.app_data.write();
        match data
            .preferences
            .mailink_devices
            .iter_mut()
            .find(|d| d.token_hash == hash)
        {
            Some(d) => {
                d.push_token = Some(body.token);
                d.push_platform = Some(body.platform);
                d.push_env = body.env;
                d.last_seen_at = now_ms() as i64;
            }
            // authed via the dev token (no device record) — push must target a paired device
            None => return Err(StatusCode::CONFLICT),
        }
        data.clone()
    };
    let _ = crate::state::save_state(&data_clone);
    Ok(Json(json!({ "ok": true })))
}

#[derive(serde::Deserialize)]
struct WsQuery {
    token: Option<String>,
}

/// GET /mailink/v1/ws — upgrade to the live event stream. Auth via `Authorization: Bearer`
/// header (native clients) or `?token=` query (browsers can't set WS headers).
async fn ws_handler(
    State(s): State<ApiState>,
    headers: HeaderMap,
    Query(q): Query<WsQuery>,
    ws: WebSocketUpgrade,
) -> Response {
    let header_ok = token_valid(&s, bearer_token(&headers));
    let query_ok = q.token.as_deref().map(|t| token_valid(&s, t)).unwrap_or(false);
    if !header_ok && !query_ok {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    ws.on_upgrade(move |socket| ws_event_loop(socket, s))
}

/// Live event loop. v1 is an internal poller (~1.5s): it diffs the chat snapshot and pushes
/// `chat_state` on any state change, `attention` when a tab enters permission/idle, and
/// `chats_changed` when the roster changes. (A push-based variant driven directly off the
/// hook state machine is a later refinement — this gives the client the WS interface now.)
async fn ws_event_loop(mut socket: WebSocket, s: ApiState) {
    let mut last: HashMap<String, String> = HashMap::new();

    // initial snapshot: one chat_state per chat
    for c in build_chats(&s.app) {
        let tab = c["tabId"].as_str().unwrap_or_default().to_string();
        let st = c["state"].as_str().unwrap_or_default().to_string();
        if socket.send(Message::Text(chat_state_event(&c).to_string().into())).await.is_err() {
            return;
        }
        last.insert(tab, st);
    }

    let mut ticker = tokio::time::interval(std::time::Duration::from_millis(1500));
    loop {
        tokio::select! {
            _ = ticker.tick() => {
                let chats = build_chats(&s.app);
                let mut current_ids = std::collections::HashSet::new();
                let mut roster_changed = false;
                for c in &chats {
                    let tab = c["tabId"].as_str().unwrap_or_default().to_string();
                    let st = c["state"].as_str().unwrap_or_default().to_string();
                    current_ids.insert(tab.clone());
                    let prev = last.get(&tab);
                    if prev.is_none() {
                        roster_changed = true;
                    }
                    if prev != Some(&st) {
                        if socket.send(Message::Text(chat_state_event(c).to_string().into())).await.is_err() {
                            return;
                        }
                        if st == "permission" || st == "idle" {
                            let ev = attention_event(&s.app, &tab, &st, c["title"].as_str().unwrap_or_default());
                            if socket.send(Message::Text(ev.to_string().into())).await.is_err() {
                                return;
                            }
                        }
                        last.insert(tab, st);
                    }
                }
                // drop tabs that disappeared from the designated set
                let removed: Vec<String> = last.keys().filter(|k| !current_ids.contains(*k)).cloned().collect();
                if !removed.is_empty() {
                    roster_changed = true;
                    for k in removed { last.remove(&k); }
                }
                if roster_changed {
                    let _ = socket.send(Message::Text(json!({ "type": "chats_changed" }).to_string().into())).await;
                }
            }
            msg = socket.recv() => {
                match msg {
                    Some(Ok(Message::Close(_))) | None => break,
                    Some(Err(_)) => break,
                    // inbound client frames are ignored in v1 — the client uses REST for actions
                    Some(Ok(_)) => {}
                }
            }
        }
    }
}

fn chat_state_event(c: &Value) -> Value {
    json!({
        "type": "chat_state",
        "tabId": c["tabId"],
        "state": c["state"],
        "runtime": c["runtime"],
        "ts": now_ms(),
    })
}

/// Build an `attention` event for a tab, inlining the open prompt (delta 1) so the client can
/// render decision buttons on the live path without a follow-up GET.
fn attention_event(app: &AppState, tab_id: &str, state: &str, title: &str) -> Value {
    let (kind, what) = match state {
        "permission" => ("permission", "Needs your approval"),
        "idle" => ("idle_done", "Finished"),
        _ => ("question", "Has a question"),
    };
    let mut ev = json!({
        "type": "attention",
        "tabId": tab_id,
        "kind": kind,
        "summary": format!("{title}: {what}"),
        "ts": now_ms(),
    });
    if let Some(detail) = build_chat_detail(app, tab_id) {
        if let Some(pp) = detail.get("pendingPrompt") {
            ev["prompt"] = pp.clone();
        }
    }
    ev
}

// ─── helpers ────────────────────────────────────────────────────────────────────────────

/// Inject text into a PTY: bracketed paste, then (if submit) a deferred CR — the same
/// convention as `agentPrompt.ts::bracketedPasteSubmit`, so a multi-line message stays one
/// prompt and submits cleanly into the agent's TUI.
async fn inject_text(
    app: &Arc<AppState>,
    pty_id: &str,
    text: &str,
    submit: bool,
) -> Result<(), String> {
    let paste = format!("\x1b[200~{text}\x1b[201~");
    crate::pty::write_pty(app, pty_id, paste.as_bytes())?;
    if submit {
        // settle delay so the TUI finishes absorbing the paste before the CR submits it
        let settle = 120 + (text.len() as u64 / 8).min(800);
        tokio::time::sleep(std::time::Duration::from_millis(settle)).await;
        crate::pty::write_pty(app, pty_id, b"\r")?;
    }
    Ok(())
}

/// The tab's currently-open prompt, as (kind, prompt_id). Mirrors what `build_chat_detail`
/// synthesizes, so `/respond`'s stale-guard agrees with what the client was shown.
fn current_prompt(app: &AppState, tab_id: &str) -> Option<(&'static str, String)> {
    let states = session_states(app);
    let (st, _rt, tool) = states.get(tab_id)?;
    if map_state(*st) == "permission" {
        Some(("permission", format!("p_{tab_id}")))
    } else if tool.as_deref() == Some("AskUserQuestion") {
        Some(("question", format!("q_{tab_id}")))
    } else {
        None
    }
}

/// Map a permission `choice` to the TUI keystroke. Standard Claude menu is 1=yes,
/// 2=yes+don't-ask, 3=no. A bare digit passes through; an unknown label defaults to deny.
/// (Fragile by nature — depends on the runtime's current affordance; the robust path is a
/// free-text /message. See docs §5.)
fn permission_key(choice: &str) -> String {
    let c = choice.trim();
    if c.len() == 1 && c.chars().next().is_some_and(|ch| ch.is_ascii_digit()) {
        return c.to_string();
    }
    match c.to_lowercase().as_str() {
        "yes" | "approve" | "allow" => "1",
        "yes, don't ask again" | "yes, and don't ask again" | "always" => "2",
        _ => "3", // safe default: deny
    }
    .to_string()
}

/// Extract the `Authorization: Bearer <token>` value (empty string if absent).
fn bearer_token(headers: &HeaderMap) -> &str {
    headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .unwrap_or("")
}

fn sha256_hex(bytes: &[u8]) -> String {
    Sha256::digest(bytes).iter().map(|b| format!("{b:02x}")).collect()
}

/// True if `token` is the dev token OR a paired device's token (compared by hash).
fn token_valid(s: &ApiState, token: &str) -> bool {
    if token.is_empty() {
        return false;
    }
    if token == s.dev_token && !s.dev_token.is_empty() {
        return true;
    }
    let hash = sha256_hex(token.as_bytes());
    s.app
        .app_data
        .read()
        .preferences
        .mailink_devices
        .iter()
        .any(|d| d.token_hash == hash)
}

/// Bearer-token gate for authed endpoints. 401 unless the token is the dev token or a paired
/// device token.
fn authorize(s: &ApiState, headers: &HeaderMap) -> Result<(), StatusCode> {
    if token_valid(s, bearer_token(headers)) {
        Ok(())
    } else {
        Err(StatusCode::UNAUTHORIZED)
    }
}

/// Owned snapshot of a maiLink-native tab (taken under the app_data lock, then released).
struct TabMeta {
    tab_id: String,
    title: String,
    workspace: String,
    runtime: AgentRuntime,
}

/// Enumerate maiLink-native *terminal* tabs (per-tab flag OR workspace-wide flag).
fn designated_tabs(app: &AppState) -> Vec<TabMeta> {
    let data = app.app_data.read();
    let mut out = Vec::new();
    for win in &data.windows {
        for ws in &win.workspaces {
            let ws_native = ws.mailink_native;
            for pane in &ws.panes {
                for tab in &pane.tabs {
                    if !(tab.mailink_native || ws_native) {
                        continue;
                    }
                    if !matches!(tab.tab_type, TabType::Terminal) {
                        continue;
                    }
                    out.push(TabMeta {
                        tab_id: tab.id.clone(),
                        title: tab.name.clone(),
                        workspace: ws.name.clone(),
                        runtime: tab.runtime.unwrap_or_default(),
                    });
                }
            }
        }
    }
    out
}

/// tab_id → (state, runtime, current tool), choosing the most attention-worthy session if a
/// tab somehow has more than one tracked session.
fn session_states(app: &AppState) -> HashMap<String, (AgentSessionState, AgentRuntime, Option<String>)> {
    let sessions = app.agent_sessions.read();
    let mut map: HashMap<String, (AgentSessionState, AgentRuntime, Option<String>)> = HashMap::new();
    for sess in sessions.values() {
        let candidate = (sess.state, sess.runtime, sess.tool_name.clone());
        map.entry(sess.tab_id.clone())
            .and_modify(|cur| {
                if rank(sess.state) > rank(cur.0) {
                    *cur = (sess.state, sess.runtime, sess.tool_name.clone());
                }
            })
            .or_insert(candidate);
    }
    map
}

fn rank(s: AgentSessionState) -> u8 {
    match s {
        AgentSessionState::WaitingPermission => 3,
        AgentSessionState::Active => 2,
        AgentSessionState::WaitingInput => 1,
        AgentSessionState::Stopped => 0,
    }
}

/// Map backend session state → the contract's chat state. No live session ⇒ "dormant".
fn map_state(s: AgentSessionState) -> &'static str {
    match s {
        AgentSessionState::Active => "active",
        AgentSessionState::WaitingPermission => "permission",
        AgentSessionState::WaitingInput | AgentSessionState::Stopped => "idle",
    }
}

fn runtime_key(r: AgentRuntime) -> &'static str {
    match r {
        AgentRuntime::Claude => "claude",
        AgentRuntime::Codex => "codex",
        AgentRuntime::Gemini => "gemini",
    }
}

fn pty_for_tab(app: &AppState, tab_id: &str) -> Option<String> {
    app.tab_pty_map.read().get(tab_id).cloned()
}

/// Short, state-derived inbox preview. (Real distilled previews from terminal text are a
/// later refinement — keeps the list path off the terminal lock.)
fn preview_for(state: &str, tool: Option<&str>) -> String {
    match state {
        "permission" => "Needs your approval".to_string(),
        "active" => tool
            .map(|t| format!("Working… ({t})"))
            .unwrap_or_else(|| "Working…".to_string()),
        "idle" => "Waiting for you".to_string(),
        _ => "Idle".to_string(),
    }
}

fn build_chats(app: &AppState) -> Vec<Value> {
    let tabs = designated_tabs(app);
    let states = session_states(app);
    let now = now_ms();
    tabs.into_iter()
        .map(|t| {
            let (state, runtime, tool) = match states.get(&t.tab_id) {
                Some((st, rt, tool)) => (map_state(*st), runtime_key(*rt), tool.clone()),
                None => ("dormant", runtime_key(t.runtime), None),
            };
            json!({
                "tabId": t.tab_id,
                "title": t.title,
                "workspace": t.workspace,
                "runtime": runtime,
                "state": state,
                "unread": state == "permission" || state == "idle",
                "lastActivityTs": now,
                "preview": preview_for(state, tool.as_deref()),
            })
        })
        .collect()
}

fn build_chat_detail(app: &AppState, tab_id: &str) -> Option<Value> {
    let meta = designated_tabs(app).into_iter().find(|t| t.tab_id == tab_id)?;
    let states = session_states(app);
    let now = now_ms();
    let (state, runtime, tool) = match states.get(tab_id) {
        Some((st, rt, tool)) => (map_state(*st), runtime_key(*rt), tool.clone()),
        None => ("dormant", runtime_key(meta.runtime), None),
    };

    // v1 transcript: the distilled recent terminal text as a single system turn. Uses the
    // LIVE tab→pty map (same as /context), not the persisted tab.pty_id which can be stale or
    // unset. Real turn-by-turn distillation is a later refinement (P3).
    let recent = pty_for_tab(app, tab_id)
        .and_then(|p| crate::commands::terminal::recent_text(app, &p, 40).ok())
        .unwrap_or_default();
    let mut transcript = Vec::new();
    if !recent.trim().is_empty() {
        transcript.push(json!({
            "msg_id": format!("ctx_{tab_id}"),
            "role": "system",
            "text": recent,
            "ts": now,
        }));
    }

    let mut detail = json!({
        "tabId": meta.tab_id,
        "title": meta.title,
        "workspace": meta.workspace,
        "runtime": runtime,
        "state": state,
        "unread": state == "permission" || state == "idle",
        "lastActivityTs": now,
        "transcript": transcript,
    });

    // pendingPrompt: synthesized from session state. NOTE: the real prompt text/options and a
    // stable prompt_id need deeper hook capture (the prompt lives in the TUI, not in
    // agent_sessions) — that's P3. The shape is contract-correct so the app renders today.
    if state == "permission" {
        let text = tool
            .as_deref()
            .map(|t| format!("{t} — approve?"))
            .unwrap_or_else(|| "Permission requested".to_string());
        detail["pendingPrompt"] = json!({
            "prompt_id": format!("p_{tab_id}"),
            "kind": "permission",
            "text": text,
            "options": ["Yes", "Yes, don't ask again", "No"],
        });
    } else if tool.as_deref() == Some("AskUserQuestion") {
        detail["pendingPrompt"] = json!({
            "prompt_id": format!("q_{tab_id}"),
            "kind": "question",
            "text": "The agent is asking a question — see the terminal for details.",
        });
    }

    Some(detail)
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// Mint a one-time pairing code (120s TTL) and build the QR payload the desktop displays for
/// scanning. Errors if the listener isn't running yet (no fp/port published).
pub fn create_pairing(app: &Arc<AppState>) -> Result<Value, String> {
    let (fp, port) = app
        .mailink_info
        .read()
        .clone()
        .ok_or("maiLink listener is not running")?;
    let code = gen_token(8).to_uppercase();
    app.mailink_pairing_codes.write().insert(
        code.clone(),
        std::time::Instant::now() + std::time::Duration::from_secs(120),
    );
    let host = local_ip().unwrap_or_else(|| "127.0.0.1".to_string());
    Ok(json!({
        "v": 1,
        "host": host,
        "port": port,
        "fp": fp,
        "code": code,
        "name": "maiTerm",
    }))
}

fn gen_token(n: usize) -> String {
    const CHARS: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789";
    use rand::Rng;
    let mut rng = rand::thread_rng();
    (0..n)
        .map(|_| CHARS[rng.gen_range(0..CHARS.len())] as char)
        .collect()
}

/// Best-effort primary LAN IPv4, resolved via the routing table (no packets sent).
fn local_ip() -> Option<String> {
    let sock = std::net::UdpSocket::bind("0.0.0.0:0").ok()?;
    sock.connect("8.8.8.8:80").ok()?;
    sock.local_addr().ok().map(|a| a.ip().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The one subtle, breakage-prone property: our PEM→DER extraction (which feeds the
    /// pinned fingerprint) must yield the exact bytes `openssl x509 -outform DER` produces.
    /// A mismatch silently breaks pairing. Skips gracefully if openssl is unavailable.
    #[test]
    fn pem_to_der_matches_openssl() {
        let certified =
            rcgen::generate_simple_self_signed(vec!["maiterm-mailink".to_string()]).unwrap();
        let cert_pem = certified.cert.pem();
        let my_der = pem_to_der(&cert_pem);
        assert!(!my_der.is_empty(), "pem_to_der returned empty");

        // fingerprint is well-formed regardless of openssl availability
        let fp = fingerprint_of_pem(&cert_pem);
        assert!(fp.starts_with("sha256/"));
        assert!(fp.len() > "sha256/".len() + 40);

        let dir = std::env::temp_dir();
        let pid = std::process::id();
        let pem_path = dir.join(format!("mailink-test-{pid}.pem"));
        let der_path = dir.join(format!("mailink-test-{pid}.der"));
        std::fs::write(&pem_path, &cert_pem).unwrap();

        let out = std::process::Command::new("openssl")
            .args([
                "x509",
                "-in",
                pem_path.to_str().unwrap(),
                "-outform",
                "DER",
                "-out",
                der_path.to_str().unwrap(),
            ])
            .output();
        let _ = std::fs::remove_file(&pem_path);

        match out {
            Ok(o) if o.status.success() => {
                let openssl_der = std::fs::read(&der_path).unwrap();
                let _ = std::fs::remove_file(&der_path);
                assert_eq!(
                    my_der, openssl_der,
                    "pem_to_der must equal openssl -outform DER (pin would mismatch otherwise)"
                );
            }
            _ => {
                let _ = std::fs::remove_file(&der_path);
                eprintln!("[mailink test] openssl unavailable — skipped DER cross-check");
            }
        }
    }

    #[test]
    fn state_mapping_is_contract_correct() {
        assert_eq!(map_state(AgentSessionState::Active), "active");
        assert_eq!(map_state(AgentSessionState::WaitingPermission), "permission");
        assert_eq!(map_state(AgentSessionState::WaitingInput), "idle");
        assert_eq!(map_state(AgentSessionState::Stopped), "idle");
        // attention ordering: permission outranks active outranks idle/stopped
        assert!(rank(AgentSessionState::WaitingPermission) > rank(AgentSessionState::Active));
        assert!(rank(AgentSessionState::Active) > rank(AgentSessionState::WaitingInput));
        assert!(rank(AgentSessionState::WaitingInput) > rank(AgentSessionState::Stopped));
        assert_eq!(runtime_key(AgentRuntime::Claude), "claude");
    }
}
