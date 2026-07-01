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

mod transcript;

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

/// Decrements the live-WS coverage count when a WS connection ends (any exit path), and stamps
/// the drop time so the doorbell can hold a short grace window before treating the tab as
/// uncovered (a foregrounded phone's WS blip must not ring the bell).
struct WsCoverageGuard(Arc<AppState>);
impl Drop for WsCoverageGuard {
    fn drop(&mut self) {
        self.0
            .mailink_ws_count
            .fetch_sub(1, std::sync::atomic::Ordering::SeqCst);
        self.0
            .mailink_ws_last_drop_ms
            .store(now_ms(), std::sync::atomic::Ordering::SeqCst);
    }
}

/// How long after a WS disconnect the doorbell keeps treating tabs as covered. A foregrounded
/// phone that briefly loses its socket reconnects in well under a second; this window (spanning a
/// couple of doorbell ticks) absorbs that blip so a coincident attention transition doesn't ring.
const WS_COVERAGE_GRACE_MS: u64 = 3000;

/// Doorbell coverage decision: a phone is receiving events directly (suppress the push) if a WS is
/// live now, OR one disconnected within the grace window (`last_drop_ms == 0` means never dropped).
fn ws_covered(live: bool, last_drop_ms: u64, now_ms: u64) -> bool {
    live || (last_drop_ms != 0 && now_ms.saturating_sub(last_drop_ms) < WS_COVERAGE_GRACE_MS)
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

/// Start the bridge if it isn't already running. Idempotent (a no-op if `mailink_info` is
/// already published). Called at boot when the pref is on, and on a runtime enable toggle.
/// Returns `Err` (already logged) if cert/token/TLS init fails.
pub fn start(app_state: &Arc<AppState>) -> Result<(), String> {
    if app_state.mailink_info.read().is_some() {
        return Ok(()); // already running
    }
    let cfg = prepare(app_state).ok_or("maiLink bridge failed to initialize (see logs)")?;
    let st = Arc::clone(app_state);
    tauri::async_runtime::spawn(async move {
        serve(st, cfg).await;
    });
    Ok(())
}

/// Stop a running bridge (runtime disable). Clears the published info so `create_pairing`
/// reports not-running and the doorbell loop self-exits on its next tick, then graceful-
/// shutdowns the axum listener so the port is released. Idempotent.
pub fn shutdown(app_state: &Arc<AppState>) {
    *app_state.mailink_info.write() = None;
    if let Some(handle) = app_state.mailink_shutdown.write().take() {
        handle.graceful_shutdown(Some(std::time::Duration::from_secs(1)));
        log::info!("[maiLink] bridge disabled — listener stopped");
    }
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

    // Doorbell: a single global trigger task that watches maiLink-native tabs and fires a
    // content-free push when one needs a human and no phone is connected (docs §6).
    tokio::spawn(doorbell_loop(app_state.clone()));

    // A shutdown handle stored in shared state so a runtime disable can stop this listener
    // (set before `app_state` is moved into ApiState below).
    let handle = axum_server::Handle::new();
    *app_state.mailink_shutdown.write() = Some(handle.clone());

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
        .handle(handle)
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
    if !is_designated(&s.app, &tab_id) {
        return Err(StatusCode::NOT_FOUND);
    }
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
    if !is_designated(&s.app, &tab_id) {
        return Err(StatusCode::NOT_FOUND);
    }
    let pty = pty_for_tab(&s.app, &tab_id).ok_or(StatusCode::CONFLICT)?;
    inject_text(&s.app, &pty, &body.text, body.submit)
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    Ok(Json(json!({ "status": "delivered", "msg_id": format!("m_{}", now_ms()) })))
}

#[derive(serde::Deserialize)]
struct RespondBody {
    /// Permission prompts only: the chosen menu label/number. Absent for question prompts.
    #[serde(default)]
    choice: Option<String>,
    /// AskUserQuestion prompts: one entry per question (aligned by index to `questions[]`),
    /// each carrying the chosen option LABELS. Absent for permission prompts. See docs §12.1.
    #[serde(default)]
    answers: Option<Vec<Answer>>,
    #[serde(default)]
    prompt_id: Option<String>,
}

/// One question's answer from the phone (docs §12.1). `selected` holds the chosen option labels
/// verbatim from `questions[i].options[].label`; multiSelect ⇒ 0..n; a chosen "Other" ⇒ `other`
/// is set (and for single-select, `selected` is empty when Other is used).
#[derive(serde::Deserialize)]
struct Answer {
    #[serde(default)]
    selected: Vec<String>,
    #[serde(default)]
    other: Option<String>,
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
    if !is_designated(&s.app, &tab_id) {
        return Err(StatusCode::NOT_FOUND);
    }
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
            let key = permission_key(body.choice.as_deref().unwrap_or(""));
            crate::pty::write_pty(&s.app, &pty, key.as_bytes())
                .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
        }
        // AskUserQuestion: replay the phone's per-question answers into the open selector.
        "question" => {
            let tool_input = match pending_question_for_tab(&s.app, &tab_id) {
                Some(t) => t,
                None => return Ok(Json(json!({ "ok": false, "reason": "stale" }))),
            };
            let answers = match body.answers.as_deref() {
                Some(a) if !a.is_empty() => a,
                _ => return Ok(Json(json!({ "ok": false, "reason": "bad_request" }))),
            };
            if let Err(e) = drive_question_answers(&s.app, &pty, &tool_input, answers).await {
                log::warn!("[maiLink] AskUserQuestion answer injection failed: {e}");
                return Ok(Json(json!({ "ok": false, "reason": "inject_failed", "detail": e })));
            }
        }
        // free-text fallback: treat choice as a plain message → paste + submit
        _ => {
            if let Some(choice) = body.choice.as_deref() {
                inject_text(&s.app, &pty, choice, true)
                    .await
                    .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
            }
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
    if !is_designated(&s.app, &tab_id) {
        return Err(StatusCode::NOT_FOUND);
    }
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
        push_cap: None,
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
    /// The per-device capability the phone minted from the shared relay's /push-capability
    /// (HMAC over platform:push_token). Required for the multi-tenant doorbell to ring it.
    #[serde(default)]
    cap: Option<String>,
}

/// POST /push-register — store the device's push token (APNs/FCM) + relay capability so the
/// doorbell can reach it. Must be called by a PAIRED device (not the dev token), since it
/// attaches to a device record.
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
                d.push_cap = body.cap;
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
    // Coverage: while this WS is alive, a phone is receiving events directly → suppress the
    // doorbell. The guard decrements on any exit path (return, error, close).
    s.app
        .mailink_ws_count
        .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    let _coverage = WsCoverageGuard(s.app.clone());

    let mut last: HashMap<String, String> = HashMap::new();
    // Streaming state (mailink-protocol §12): per-tab last-window msg_ids + transcript mtime, so the
    // message ticker diffs cheaply and emits only newly-appended turns.
    let mut seen: HashMap<String, std::collections::HashSet<String>> = HashMap::new();
    let mut mtimes: HashMap<String, u64> = HashMap::new();

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
    // A faster, mtime-gated ticker for per-turn message streaming: near-instant delivery without
    // paying the full chat rebuild (build_chats) at this cadence.
    let mut msg_ticker = tokio::time::interval(std::time::Duration::from_millis(400));
    loop {
        tokio::select! {
            _ = msg_ticker.tick() => {
                if stream_new_messages(&mut socket, &s.app, &mut seen, &mut mtimes).await.is_err() {
                    return;
                }
            }
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
    let mut ev = json!({
        "type": "chat_state",
        "tabId": c["tabId"],
        "state": c["state"],
        "runtime": c["runtime"],
        "ts": now_ms(),
    });
    // Carry the per-agent meta so the phone's context gauge steps live per turn (peer merges it
    // into the acting participant). Present only for Claude tabs with a resolvable transcript.
    if let Some(meta) = c.get("meta") {
        ev["meta"] = meta.clone();
    }
    ev
}

/// A `message` WS frame for one appended transcript turn (mailink-protocol §12 streaming). Carries
/// the SAME msg_id/role/text/ts that GET returns for this turn (turns_for_session), so the phone's
/// dedup-by-msg_id collapses the streamed frame and the REST re-fetch into one entry.
fn message_event(tab_id: &str, turn: &Value) -> Value {
    json!({
        "type": "message",
        "tabId": tab_id,
        "role": turn.get("role"),
        "text": turn.get("text"),
        "msg_id": turn.get("msg_id"),
        "ts": turn.get("ts"),
    })
}

/// Stream newly-appended agent/tool turns for every designated tab as `message` frames. Never
/// streams the phone's OWN user turns (peer option iii: those are rendered optimistically on send
/// and stay in GET for the full-replace refresh) — only turns the phone can't already have. Cheap
/// when idle: an mtime gate skips tabs whose transcript hasn't changed. `seen` holds the last-window
/// msg_ids per tab; a tab's FIRST observation baselines silently (no history replay), then only ids
/// not previously seen are emitted. Returns Err if the socket died (caller exits the loop).
async fn stream_new_messages(
    socket: &mut WebSocket,
    app: &AppState,
    seen: &mut HashMap<String, std::collections::HashSet<String>>,
    mtimes: &mut HashMap<String, u64>,
) -> Result<(), ()> {
    for t in designated_tabs(app) {
        let Some(sid) = resolved_session_id_for_tab(app, &t.tab_id) else { continue };
        // mtime gate: an unchanged transcript means no new turns, so skip the tail re-parse.
        if let Some(mt) = transcript::session_jsonl_mtime(&sid) {
            if mtimes.get(&t.tab_id) == Some(&mt) {
                continue;
            }
            mtimes.insert(t.tab_id.clone(), mt);
        }
        // Same call GET uses (limit 40, Marker) so streamed ids are byte-identical to the REST path.
        let Some(turns) = transcript::turns_for_session(&sid, 40, transcript::ToolRender::Marker)
        else {
            continue;
        };
        let entry = seen.entry(t.tab_id.clone()).or_default();
        let baseline = entry.is_empty();
        let mut window: std::collections::HashSet<String> = std::collections::HashSet::new();
        for turn in &turns {
            let Some(id) = turn.get("msg_id").and_then(|v| v.as_str()) else { continue };
            window.insert(id.to_string());
            if baseline || entry.contains(id) {
                continue;
            }
            // Skip the phone's own user turns; stream agent/tool/system content only.
            if turn.get("role").and_then(|v| v.as_str()) == Some("user") {
                continue;
            }
            if socket
                .send(Message::Text(message_event(&t.tab_id, turn).to_string().into()))
                .await
                .is_err()
            {
                return Err(());
            }
        }
        // Replace (not merge) → bounded to the window; the transcript only grows, so an id that
        // leaves the window never returns, making replacement safe against re-emitting.
        *entry = window;
    }
    Ok(())
}

/// Build an `attention` event for a tab, inlining the open prompt (delta 1) so the client can
/// render decision buttons on the live path without a follow-up GET.
fn attention_event(app: &AppState, tab_id: &str, state: &str, title: &str) -> Value {
    let detail = build_chat_detail(app, tab_id);
    let pp = detail.as_ref().and_then(|d| d.get("pendingPrompt"));
    // Prefer the actual pending prompt's kind: an open AskUserQuestion yields kind:"question" even
    // though its coincident state is "permission" (see build_chat_detail). Fall back to state.
    let (kind, what) = match pp.and_then(|p| p.get("kind")).and_then(|k| k.as_str()) {
        Some("question") => ("question", "Has a question"),
        Some("permission") => ("permission", "Needs your approval"),
        _ => match state {
            "permission" => ("permission", "Needs your approval"),
            "idle" => ("idle_done", "Finished"),
            _ => ("question", "Has a question"),
        },
    };
    let mut ev = json!({
        "type": "attention",
        "tabId": tab_id,
        "kind": kind,
        "summary": format!("{title}: {what}"),
        "ts": now_ms(),
    });
    if let Some(p) = pp {
        ev["prompt"] = p.clone();
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

/// Settle delays so the TUI redraws between keystrokes (mirrors inject_text's paste settle).
const NAV_SETTLE_MS: u64 = 130;
const ADVANCE_SETTLE_MS: u64 = 280;

/// Inject one keystroke, then wait `settle_ms` so the selector repaints before the next.
async fn send_key(app: &Arc<AppState>, pty_id: &str, bytes: &[u8], settle_ms: u64) -> Result<(), String> {
    crate::pty::write_pty(app, pty_id, bytes)?;
    tokio::time::sleep(std::time::Duration::from_millis(settle_ms)).await;
    Ok(())
}

/// Move the selector highlight from row `from` to row `to` with arrow keys. Returns `to`.
async fn nav_to(app: &Arc<AppState>, pty_id: &str, from: usize, to: usize) -> Result<usize, String> {
    if to > from {
        for _ in 0..(to - from) {
            send_key(app, pty_id, b"\x1b[B", NAV_SETTLE_MS).await?; // Down
        }
    } else {
        for _ in 0..(from - to) {
            send_key(app, pty_id, b"\x1b[A", NAV_SETTLE_MS).await?; // Up
        }
    }
    Ok(to)
}

/// Replay the phone's per-question answers into Claude Code's open AskUserQuestion selector by
/// injecting keystrokes, in question order.
///
/// RUNTIME-FRAGILE — the selector is a TUI, not a stable API. Mechanics pinned live against
/// Claude Code 2.1.x (docs §12.3):
///   (a) each question's highlight starts at row 0;                          [VERIFIED]
///   (b) single-select: arrow to the row, Enter selects AND advances;        [VERIFIED e2e]
///   (c) after the LAST question, focus lands on a "Submit" tab — a single-question form submits
///       on its own Enter, a multi-question form needs one more Enter (handled below); [VERIFIED]
///   (d) the selector is arrow-only — the shown 1..n are labels, NOT digit-select keys; [VERIFIED]
///   (e) multiSelect toggles with Space; the free-text "Other" row sits after the last option and
///       opens an inline input.                                              [NOT yet reliable]
/// NOTE: (e) is why `build_chat_detail` still keeps multiSelect/Other prompts non-respondable —
/// live probes lost a multiSelect toggle and mis-fired the Other row. All mapping is resolved
/// BEFORE any keystroke is sent, so an unmappable answer rejects the whole batch rather than
/// half-answering a live prompt.
async fn drive_question_answers(
    app: &Arc<AppState>,
    pty_id: &str,
    tool_input: &Value,
    answers: &[Answer],
) -> Result<(), String> {
    let questions = tool_input
        .get("questions")
        .and_then(|v| v.as_array())
        .ok_or("pending ask has no questions[]")?;
    if answers.len() != questions.len() {
        return Err(format!(
            "answer count {} != question count {}",
            answers.len(),
            questions.len()
        ));
    }

    struct Plan {
        multi: bool,
        option_count: usize,
        indices: Vec<usize>,   // option rows to select/toggle (empty ⇒ Other only)
        other: Option<String>, // free-text for the "Other" row
    }

    // Resolve every answer to concrete selector actions up front (fail-closed on any bad map).
    let mut plans: Vec<Plan> = Vec::with_capacity(questions.len());
    for (qi, (q, ans)) in questions.iter().zip(answers).enumerate() {
        let labels: Vec<&str> = q
            .get("options")
            .and_then(|v| v.as_array())
            .map(|a| {
                a.iter()
                    .filter_map(|o| o.get("label").and_then(|v| v.as_str()))
                    .collect()
            })
            .unwrap_or_default();
        let multi = q.get("multiSelect").and_then(|v| v.as_bool()).unwrap_or(false);

        let mut indices = Vec::new();
        for sel in &ans.selected {
            match labels.iter().position(|l| l == sel) {
                Some(idx) => indices.push(idx),
                None => return Err(format!("question {qi}: label {sel:?} not among options")),
            }
        }
        let other = ans.other.clone().filter(|s| !s.trim().is_empty());
        if indices.is_empty() && other.is_none() {
            return Err(format!("question {qi}: no selection and no Other text"));
        }
        if !multi && (indices.len() > 1 || (indices.len() == 1 && other.is_some())) {
            return Err(format!("question {qi}: single-select received multiple picks"));
        }
        indices.sort_unstable();
        indices.dedup();
        plans.push(Plan { multi, option_count: labels.len(), indices, other });
    }

    // Inject, question by question.
    for plan in &plans {
        let mut cur = 0usize; // assumed highlight origin (a)
        if plan.multi {
            for &idx in &plan.indices {
                cur = nav_to(app, pty_id, cur, idx).await?;
                send_key(app, pty_id, b" ", NAV_SETTLE_MS).await?; // Space toggles (d)
            }
        } else if let Some(&idx) = plan.indices.first() {
            cur = nav_to(app, pty_id, cur, idx).await?;
        }

        if let Some(text) = &plan.other {
            // Other row follows the options (c); open it, then paste the free text.
            cur = nav_to(app, pty_id, cur, plan.option_count).await?;
            send_key(app, pty_id, b"\r", ADVANCE_SETTLE_MS).await?;
            inject_text(app, pty_id, text, true).await?;
        } else {
            send_key(app, pty_id, b"\r", ADVANCE_SETTLE_MS).await?; // select/confirm + advance (b)
        }
        let _ = cur;
    }

    // Multi-question forms do NOT auto-submit: answering the last question moves focus to the
    // "Submit" tab in the header bar, which needs one final Enter. A single-question form submits
    // on the question's own Enter. (Verified live — docs §12.3.)
    if plans.len() > 1 {
        send_key(app, pty_id, b"\r", ADVANCE_SETTLE_MS).await?;
    }
    Ok(())
}

/// The tab's currently-open prompt, as (kind, prompt_id). Mirrors what `build_chat_detail`
/// synthesizes, so `/respond`'s stale-guard agrees with what the client was shown.
fn current_prompt(app: &AppState, tab_id: &str) -> Option<(&'static str, String)> {
    let states = session_states(app);
    let (st, _rt, tool) = states.get(tab_id)?;
    // AskUserQuestion first: it coincides with a permission_prompt state (see build_chat_detail),
    // but the open ask is the structured question — the stale-guard must agree with what was shown.
    if tool.as_deref() == Some("AskUserQuestion") {
        Some(("question", format!("q_{tab_id}")))
    } else if map_state(*st) == "permission" {
        Some(("permission", format!("p_{tab_id}")))
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

/// Enumerate the terminal tabs available to maiLink. Two modes, chosen by the
/// `mailink_expose_all` preference:
///   * expose-all (default): every *agent* tab (a runtime was ever detected) is available,
///     minus per-tab opt-outs (`Tab.mailink_excluded`). Runtime persists after the agent
///     process dies, so a downed agent stays reachable for maiLink auto-resume.
///   * designate-only: only tabs the user explicitly marks (`Tab.mailink_native` OR the
///     workspace-wide `Workspace.mailink_native`).
fn designated_tabs(app: &AppState) -> Vec<TabMeta> {
    let data = app.app_data.read();
    let expose_all = data.preferences.mailink_expose_all;
    let mut out = Vec::new();
    for win in &data.windows {
        for ws in &win.workspaces {
            let ws_native = ws.mailink_native;
            for pane in &ws.panes {
                for tab in &pane.tabs {
                    if !matches!(tab.tab_type, TabType::Terminal) {
                        continue;
                    }
                    let available = if expose_all {
                        tab.runtime.is_some() && !tab.mailink_excluded
                    } else {
                        tab.mailink_native || ws_native
                    };
                    if !available {
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

/// True if `tab_id` is currently available to maiLink — the same gate that governs the chat
/// list, WS stream, and doorbell. The write + context endpoints call this so a tab held back
/// via the exposure settings (designate-only mode, or `mailink_excluded` in expose-all mode)
/// is genuinely unreachable, not merely hidden from discovery. Returns NOT_FOUND-worthy false
/// for unknown or non-designated tab_ids alike.
fn is_designated(app: &AppState, tab_id: &str) -> bool {
    designated_tabs(app).iter().any(|t| t.tab_id == tab_id)
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

/// The session id whose transcript we read for a tab. If a tab has more than one tracked session
/// (e.g. after a resume minted a new id), prefer the most attention-worthy — consistent with how
/// `session_states` picks the tab's displayed state.
fn session_id_for_tab(app: &AppState, tab_id: &str) -> Option<String> {
    let sessions = app.agent_sessions.read();
    sessions
        .iter()
        .filter(|(_, s)| s.tab_id == tab_id)
        .max_by_key(|(_, s)| rank(s.state))
        .map(|(id, _)| id.clone())
}

/// The tab's persisted resume session id — the runtime's `*SessionId` trigger variable that the
/// auto-resume command interpolates (`claude --resume %claudeSessionId`). Used to resolve a
/// transcript for an agent that has auto-resumed but NOT yet re-run initSession: in that window
/// `agent_sessions` has no live entry, so without this the phone falls back to a raw terminal
/// scrape (wide, unwrapped) or empty, and the app shows stale/duplicated detail. Claude only.
fn persisted_session_id_for_tab(app: &AppState, tab_id: &str) -> Option<String> {
    let data = app.app_data.read();
    for win in &data.windows {
        for ws in &win.workspaces {
            for pane in &ws.panes {
                for tab in &pane.tabs {
                    if tab.id == tab_id {
                        return tab
                            .trigger_variables
                            .get("claudeSessionId")
                            .cloned()
                            .filter(|s| !s.is_empty());
                    }
                }
            }
        }
    }
    None
}

/// Session id for reading a tab's transcript: the LIVE session if one is registered, else the
/// PERSISTED resume id (covers the resume-before-initSession window after an app relaunch).
fn resolved_session_id_for_tab(app: &AppState, tab_id: &str) -> Option<String> {
    session_id_for_tab(app, tab_id).or_else(|| persisted_session_id_for_tab(app, tab_id))
}

/// The captured AskUserQuestion `tool_input` for a tab (most attention-worthy session), if an
/// elicitation is currently open. Mirrors how `session_id_for_tab` resolves the tab's session.
fn pending_question_for_tab(app: &AppState, tab_id: &str) -> Option<Value> {
    let sessions = app.agent_sessions.read();
    sessions
        .iter()
        .filter(|(_, s)| s.tab_id == tab_id)
        .max_by_key(|(_, s)| rank(s.state))
        .and_then(|(_, s)| s.pending_question.clone())
}

/// Map Claude's AskUserQuestion `tool_input` into the mailink-protocol §12.1 AskQuestion[] shape
/// (header, question, multiSelect, options:[{label, description}], allowOther). Returns None on an
/// unrecognized shape so the caller falls back to a generic prompt. `allowOther` is always true —
/// Claude's elicitation always offers a free-text "Other".
fn map_ask_questions(tool_input: &Value) -> Option<Value> {
    let arr = tool_input.get("questions")?.as_array()?;
    if arr.is_empty() {
        return None;
    }
    let out: Vec<Value> = arr
        .iter()
        .map(|q| {
            let options: Vec<Value> = q
                .get("options")
                .and_then(|v| v.as_array())
                .map(|opts| {
                    opts.iter()
                        .map(|o| {
                            json!({
                                "label": o.get("label").and_then(|v| v.as_str()).unwrap_or(""),
                                "description": o.get("description").and_then(|v| v.as_str()),
                            })
                        })
                        .collect()
                })
                .unwrap_or_default();
            json!({
                "header": q.get("header").and_then(|v| v.as_str()).unwrap_or(""),
                "question": q.get("question").and_then(|v| v.as_str()).unwrap_or(""),
                "multiSelect": q.get("multiSelect").and_then(|v| v.as_bool()).unwrap_or(false),
                "options": options,
                "allowOther": true,
            })
        })
        .collect();
    Some(json!(out))
}

/// Build the chat transcript: per-turn source markdown from the Claude session JSONL when we can
/// find it, otherwise the old single-system-turn terminal scrape (other runtimes / robustness).
fn build_transcript(app: &AppState, tab_id: &str, runtime: &str, now: u64) -> Vec<Value> {
    if runtime == "claude" {
        // Resolve via the LIVE session, or (post-relaunch, pre-initSession) the persisted resume
        // id — so a dormant/resuming agent still shows its real distilled conversation, keyed to
        // THIS tab, instead of a raw terminal scrape or empty (which the app rendered as
        // stale/duplicated "all agents look the same" detail).
        if let Some(sid) = resolved_session_id_for_tab(app, tab_id) {
            if let Some(turns) =
                transcript::turns_for_session(&sid, 40, transcript::ToolRender::Marker)
            {
                if !turns.is_empty() {
                    return turns;
                }
            }
        }
        // No JSONL resolvable → empty, NOT the raw terminal scrape: the scrape is wide,
        // unwrapped, and easily misread as another agent's content on a phone.
        return Vec::new();
    }
    // Non-Claude runtimes (no JSONL distillation): distilled recent terminal text as a single
    // system turn. Uses the LIVE tab→pty map, not the persisted tab.pty_id which can be stale.
    let recent = pty_for_tab(app, tab_id)
        .and_then(|p| crate::commands::terminal::recent_text(app, &p, 40).ok())
        .unwrap_or_default();
    let mut out = Vec::new();
    if !recent.trim().is_empty() {
        out.push(json!({
            "msg_id": format!("ctx_{tab_id}"),
            "role": "system",
            "text": recent,
            "ts": now,
        }));
    }
    out
}

/// Short, state-derived inbox preview. (Real distilled previews from terminal text are a
/// later refinement — keeps the list path off the terminal lock.)
fn preview_for(state: &str, tool: Option<&str>) -> String {
    // An open AskUserQuestion is a human ask regardless of the coincident session state
    // (permission_prompt Notification, or active if a build stops sending it) — label it as such.
    if tool == Some("AskUserQuestion") {
        return "Has a question".to_string();
    }
    match state {
        "permission" => "Needs your approval".to_string(),
        "active" => tool
            .map(|t| format!("Working… ({t})"))
            .unwrap_or_else(|| "Working…".to_string()),
        "idle" => "Waiting for you".to_string(),
        _ => "Idle".to_string(),
    }
}

/// The context window for a model id: 1M-context variants vs the 200k default. The transcript/hook
/// model id never carries the `[1m]` variant marker (Claude Code exposure gap — see
/// SessionMeta::model_id), so we can't detect the 1M variant from the id alone. Opus 4.8 defaults to
/// the 1M variant in this deployment, so assume 1M whenever Opus 4.8 is in use. Mirrors the maiTerm
/// statusline's limit derivation.
fn context_limit_for(model_id: &str) -> u64 {
    if model_id.contains("[1m]") || model_id.contains("-1m") || model_id.contains("opus-4-8") {
        1_000_000
    } else {
        200_000
    }
}

/// Normalize a Claude model id to a friendly display string: "claude-opus-4-8[1m]" → "Opus 4.8".
/// Strips the provider prefix and 1M marker, title-cases the family, and dot-joins the version.
fn display_model(model_id: &str) -> String {
    let s = model_id.trim();
    let s = s.strip_prefix("claude-").unwrap_or(s);
    let s = s.replace("[1m]", "");
    let s = s.strip_suffix("-1m").unwrap_or(&s);
    let parts: Vec<&str> = s.split('-').filter(|p| !p.is_empty()).collect();
    let Some((family, version)) = parts.split_first() else {
        return model_id.to_string();
    };
    let family_disp = {
        let mut chars = family.chars();
        match chars.next() {
            Some(f) => f.to_uppercase().collect::<String>() + chars.as_str(),
            None => family.to_string(),
        }
    };
    if version.is_empty() {
        family_disp
    } else {
        format!("{family_disp} {}", version.join("."))
    }
}

/// Per-agent telemetry (mailink-protocol §12.1 `meta`): model display name + context gauge, read
/// from the Claude transcript JSONL (the SessionStart hook's model is often null). Live/persisted
/// session id so it also resolves during the resume-before-init window. `effort` is intentionally
/// omitted — it's only in Claude Code's statusLine payload, which maiTerm doesn't receive. None for
/// non-Claude tabs (no Claude JSONL) or before the first assistant turn.
fn build_meta(app: &AppState, tab_id: &str) -> Option<Value> {
    let sid = resolved_session_id_for_tab(app, tab_id)?;
    let meta = transcript::session_meta(&sid)?;
    let model_id = meta.model_id.as_deref().unwrap_or("");
    let limit = context_limit_for(model_id);
    let pct = ((meta.context_tokens as f64 / limit as f64) * 100.0)
        .round()
        .clamp(0.0, 100.0) as u64;
    let mut m = json!({
        "contextUsed": meta.context_tokens,
        "contextLimit": limit,
        "contextPct": pct,
    });
    if !model_id.is_empty() {
        m["model"] = json!(display_model(model_id));
    }
    Some(m)
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
            let ask_open = tool.as_deref() == Some("AskUserQuestion");
            let mut chat = json!({
                "tabId": t.tab_id,
                "title": t.title,
                "workspace": t.workspace,
                "runtime": runtime,
                "state": state,
                // ask_open guards the case where a build leaves an open AskUserQuestion at
                // state=="active" — it still needs to surface as unread in the inbox.
                "unread": ask_open || state == "permission" || state == "idle",
                "lastActivityTs": now,
                "preview": preview_for(state, tool.as_deref()),
            });
            if let Some(meta) = build_meta(app, &t.tab_id) {
                chat["meta"] = meta;
            }
            chat
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

    // Per-turn source markdown from the session transcript (Claude) so the phone's GFM renderer
    // lights up; falls back to the distilled terminal scrape for other runtimes / when no
    // transcript is found. See mailink/transcript.rs.
    let transcript = build_transcript(app, tab_id, runtime, now);

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

    // Per-agent telemetry strip (model + context gauge). See build_meta.
    if let Some(agent_meta) = build_meta(app, tab_id) {
        detail["meta"] = agent_meta;
    }

    // pendingPrompt: the agent's native human ask (mailink-protocol §12). thread_id == tab_id
    // for a solo thread.
    //
    // AskUserQuestion is checked FIRST, keyed on tool_name (NOT state): while an AskUserQuestion
    // waits, Claude fires a permission_prompt Notification that flips the session to
    // WaitingPermission (state=="permission"). If we checked state first we'd synthesize a generic
    // "approve AskUserQuestion?" card — exactly the bug where the phone showed something totally
    // different from the real question the desktop was showing. The open ask IS the structured
    // question; render THAT. It carries the REAL questions captured from the PreToolUse hook
    // (tool_input.questions). The answer-injection path (`drive_question_answers` + the "question"
    // arm of post_respond) is built but the selector keystroke mechanics are UNVERIFIED against a
    // live TUI — keep respondable:false until that live check passes, then flip to true (docs §12.3).
    if tool.as_deref() == Some("AskUserQuestion") {
        let mut pp = json!({
            "prompt_id": format!("q_{tab_id}"),
            "thread_id": tab_id,
            "kind": "question",
            "respondable": false,
        });
        match pending_question_for_tab(app, tab_id).as_ref().and_then(map_ask_questions) {
            Some(qs) => { pp["questions"] = qs; }
            None => { pp["text"] = json!("The agent is asking a question — see the terminal for details."); }
        }
        detail["pendingPrompt"] = pp;
    } else if state == "permission" {
        // A real permission prompt (some other tool, e.g. Bash). Synthesized: the hook carries no
        // structured options; that numeric-keystroke respond path is proven, so respondable now.
        let text = tool
            .as_deref()
            .map(|t| format!("{t} — approve?"))
            .unwrap_or_else(|| "Permission requested".to_string());
        detail["pendingPrompt"] = json!({
            "prompt_id": format!("p_{tab_id}"),
            "thread_id": tab_id,
            "kind": "permission",
            "respondable": true,
            "text": text,
            "options": ["Yes", "Yes, don't ask again", "No"],
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

/// Default shared push relay (Flexmark-operated Cloudflare worker). maiLink is multi-tenant:
/// every install points here by default so the doorbell works with zero config. A user can
/// override it via Preferences.mailink_relay_url (e.g. to self-host). See docs/mailink-protocol.md
/// §6.1.
const DEFAULT_MAILINK_RELAY_URL: &str = "https://updates.maiterm.dev/push";

/// Global doorbell trigger. Every ~2s: when a maiLink-native tab transitions INTO an
/// attention state (permission / idle-done) AND no phone holds a live WS (uncovered), POST a
/// content-free wake to the relay for each paired device that registered a push token + relay
/// capability. The shared relay (Cloudflare worker) verifies the capability, signs, and forwards
/// to APNs/FCM; the phone wakes and pulls the real content over LAN. See docs/mailink-protocol.md
/// §6. No-op while no such device exists.
async fn doorbell_loop(app: Arc<AppState>) {
    let client = reqwest::Client::new();
    let mut last: HashMap<String, String> = HashMap::new();
    let mut primed = false;
    let mut ticker = tokio::time::interval(std::time::Duration::from_millis(2000));
    loop {
        ticker.tick().await;
        // Stop ringing once the bridge is disabled (runtime toggle clears mailink_info). A fresh
        // enable spawns a new loop, so this one can exit cleanly.
        if app.mailink_info.read().is_none() {
            break;
        }
        // The relay URL is baked in (shared infra); an explicit pref overrides it for self-hosters.
        let relay_url = {
            let p = &app.app_data.read().preferences;
            p.mailink_relay_url
                .as_deref()
                .map(str::trim)
                .filter(|u| !u.is_empty())
                .map(str::to_string)
                .unwrap_or_else(|| DEFAULT_MAILINK_RELAY_URL.to_string())
        };
        // Covered if a phone is connected now, OR one disconnected within the grace window (its
        // WS may just be blipping while foregrounded — don't ring on that momentary count==0).
        let covered = ws_covered(
            app.mailink_ws_count
                .load(std::sync::atomic::Ordering::SeqCst)
                > 0,
            app.mailink_ws_last_drop_ms
                .load(std::sync::atomic::Ordering::SeqCst),
            now_ms(),
        );

        let chats = build_chats(&app);
        let mut current = std::collections::HashSet::new();
        for c in &chats {
            let tab = c["tabId"].as_str().unwrap_or_default().to_string();
            let st = c["state"].as_str().unwrap_or_default().to_string();
            let title = c["title"].as_str().unwrap_or_default().to_string();
            current.insert(tab.clone());
            let prev = last.get(&tab).cloned();
            last.insert(tab.clone(), st.clone());

            // Fire only on a fresh transition into attention, after priming, while uncovered.
            if !primed || covered {
                continue;
            }
            let is_attn = st == "permission" || st == "idle";
            let was_attn = matches!(prev.as_deref(), Some("permission") | Some("idle"));
            if is_attn && !was_attn {
                // Distinguish an open AskUserQuestion (state coincides with "permission") from a
                // real approval prompt so the push line/route matches what the card will show.
                let kind = match current_prompt(&app, &tab) {
                    Some(("question", _)) => "question",
                    Some(_) => "permission",
                    None => "idle_done",
                };
                ring_devices(&client, &app, &relay_url, &tab, &title, kind).await;
            }
        }
        last.retain(|k, _| current.contains(k));
        primed = true;
    }
}

/// POST the content-free wake to the shared relay, once per paired device that registered BOTH a
/// push token and a relay capability (without the cap the multi-tenant relay rejects the wake).
/// Payload carries ONLY {push_token, platform, env, cap, tab_id, kind, title} — never terminal
/// content (docs §6: content-light boundary; tab title + kind are allowed).
async fn ring_devices(
    client: &reqwest::Client,
    app: &Arc<AppState>,
    url: &str,
    tab_id: &str,
    title: &str,
    kind: &str,
) {
    let targets: Vec<(String, String, Option<String>, String)> = app
        .app_data
        .read()
        .preferences
        .mailink_devices
        .iter()
        .filter_map(|d| match (d.push_token.as_ref(), d.push_cap.as_ref()) {
            (Some(t), Some(cap)) => Some((
                t.clone(),
                d.push_platform.clone().unwrap_or_else(|| "apns".to_string()),
                d.push_env.clone(),
                cap.clone(),
            )),
            _ => None,
        })
        .collect();

    for (push_token, platform, env, cap) in targets {
        let body = json!({
            "push_token": push_token,
            "platform": platform,
            "env": env,
            "cap": cap,
            "tab_id": tab_id,
            "kind": kind,
            "title": title,
        });
        match client.post(url).json(&body).send().await {
            Ok(resp) => log::info!(
                "[maiLink] doorbell → {platform} for tab {tab_id} ({kind}): {}",
                resp.status()
            ),
            Err(e) => log::warn!("[maiLink] doorbell POST failed: {e}"),
        }
    }
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
    fn ws_coverage_grace_window() {
        let now = 100_000u64;
        // A live WS is always covered, regardless of drop time.
        assert!(ws_covered(true, 0, now));
        assert!(ws_covered(true, now, now));
        // No WS and never dropped ⇒ uncovered (a real, un-covered attention should ring).
        assert!(!ws_covered(false, 0, now));
        // No WS but dropped just now / within grace ⇒ still covered (absorb the blip).
        assert!(ws_covered(false, now, now));
        assert!(ws_covered(false, now - (WS_COVERAGE_GRACE_MS - 1), now));
        // No WS and the drop is older than the grace ⇒ uncovered again (phone really left).
        assert!(!ws_covered(false, now - WS_COVERAGE_GRACE_MS, now));
        assert!(!ws_covered(false, now - 60_000, now));
    }

    #[test]
    fn model_display_and_context_limit() {
        assert_eq!(display_model("claude-opus-4-8[1m]"), "Opus 4.8");
        assert_eq!(display_model("claude-opus-4-8"), "Opus 4.8");
        assert_eq!(display_model("claude-sonnet-4-5"), "Sonnet 4.5");
        assert_eq!(display_model("claude-haiku-4-5-20251001"), "Haiku 4.5.20251001");
        assert_eq!(display_model("opus-4-8-1m"), "Opus 4.8");
        // 1M-context variants vs the 200k default. Opus 4.8 is assumed 1M even without a marker
        // (the transcript id never carries one), so the bare id also resolves to 1M.
        assert_eq!(context_limit_for("claude-opus-4-8[1m]"), 1_000_000);
        assert_eq!(context_limit_for("claude-opus-4-8-1m"), 1_000_000);
        assert_eq!(context_limit_for("claude-opus-4-8"), 1_000_000);
        assert_eq!(context_limit_for("claude-sonnet-4-5"), 200_000);
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
