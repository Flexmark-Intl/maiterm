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
        DefaultBodyLimit, Path, Query, State,
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
use tauri::Emitter;

use crate::state::app_state::AgentSessionState;
use crate::state::workspace::TabType;
use crate::state::{AgentRuntime, AppState, MailinkDevice};

pub(crate) mod mirror;
pub(crate) mod transcript;

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
    /// Emit target for desktop-side reflection of backend-initiated mutations (e.g. a phone
    /// rename must update the live tab title in every open window, not just on next reload).
    /// `None` only in router unit tests, which construct `ApiState` directly and never emit.
    app_handle: Option<tauri::AppHandle>,
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

/// The per-tab attention signature the WS/doorbell tickers diff: chat state PLUS the open-prompt
/// kind (the `prompt` field build_chats computes). Including the prompt kind means an
/// AskUserQuestion that opens without moving `state` (no coincident permission notification)
/// still registers as a transition — and a permission that resolves straight into a question
/// re-fires rather than being masked by the unchanged state.
fn attn_key(state: &str, prompt: Option<&str>) -> String {
    format!("{state}|{}", prompt.unwrap_or(""))
}

/// Whether an `attn_key` means "the agent wants a human": an open permission/question prompt, or
/// a finished turn waiting on input.
fn is_attn(key: &str) -> bool {
    let (state, prompt) = key.split_once('|').unwrap_or((key, ""));
    state == "permission" || state == "idle" || prompt == "question"
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
pub fn start(app_state: &Arc<AppState>, app_handle: tauri::AppHandle) -> Result<(), String> {
    if app_state.mailink_info.read().is_some() {
        return Ok(()); // already running
    }
    let cfg = prepare(app_state).ok_or("maiLink bridge failed to initialize (see logs)")?;
    mirror::prune_stale_shadows();
    let st = Arc::clone(app_state);
    tauri::async_runtime::spawn(async move {
        serve(st, cfg, app_handle).await;
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

/// Max body for POST /chats/{tabId}/message. Sized to the published 6-inline-image contract with
/// headroom — axum's 2 MiB default rejects a realistic batch at the extractor, before the handler
/// runs (no log line, generic failure on the phone).
const MAX_MESSAGE_BODY_BYTES: usize = 32 * 1024 * 1024;

/// The v1 route table. Split out of `serve` so tests can exercise the real router (notably the
/// per-route body limit) without standing up TLS or a listener.
fn build_router(api: ApiState) -> Router {
    Router::new()
        .route("/mailink/v1/heartbeat", get(heartbeat))
        .route("/mailink/v1/chats", get(chats_list))
        .route("/mailink/v1/chats/{tab_id}", get(chat_detail))
        .route("/mailink/v1/chats/{tab_id}/context", get(chat_context))
        // Image sends carry base64 inline (mailink-protocol §12), so this is the one route that
        // outgrows axum's 2 MiB default: 6 images × ~750 KB × 1.37 base64 inflation ≈ 6 MB worst
        // case. 32 MiB leaves real headroom; every other route keeps the tight default.
        .route(
            "/mailink/v1/chats/{tab_id}/message",
            post(post_message).layer(DefaultBodyLimit::max(MAX_MESSAGE_BODY_BYTES)),
        )
        .route("/mailink/v1/chats/{tab_id}/respond", post(post_respond))
        .route("/mailink/v1/chats/{tab_id}/interrupt", post(post_interrupt))
        .route("/mailink/v1/chats/{tab_id}/rename", post(post_rename))
        .route("/mailink/v1/ws", get(ws_handler))
        .route("/mailink/v1/pair", post(post_pair))
        .route("/mailink/v1/push-register", post(post_push_register))
        .with_state(api)
}

/// Background task: install the rustls crypto provider, build the router, and serve over TLS.
pub async fn serve(app_state: Arc<AppState>, cfg: MailinkConfig, app_handle: tauri::AppHandle) {
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
        app_handle: Some(app_handle),
        server_name: "maiTerm".to_string(),
        fingerprint: cfg.fingerprint.clone(),
        dev_token: cfg.dev_token.clone(),
    };
    let router = build_router(api);

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
    /// Optional so the phone can send images with no caption (empty allowed once `images` is set).
    #[serde(default)]
    text: String,
    #[serde(default)]
    submit: bool,
    /// Inline images to attach to this message (mailink-protocol §12 image send). Absent/empty ⇒
    /// today's plain-text message, unchanged for every runtime. See ImageInput.
    #[serde(default)]
    images: Vec<ImageInput>,
}

/// One inline image from the phone. `data` is base64 with NO `data:` prefix; `mime` carries the
/// type separately ("image/png" | "image/jpeg" | "image/webp"); `name` is display/debug only. The
/// phone pre-downscales each to <=1568px and caps 6/message. Per-image size varies a lot by
/// source — JPEG photos land under ~500 KB, but PNG screenshots have been seen at ~750 KB, and
/// base64 inflates ~1.37× on top. Budget by TOTAL bytes, not count: the real ceiling is
/// MAX_MESSAGE_BODY_BYTES on the route, and exceeding it is a 413 from the extractor.
#[derive(serde::Deserialize)]
struct ImageInput {
    data: String,
    mime: String,
    #[serde(default)]
    #[allow(dead_code)] // display/debug only; the temp filename is uuid-based, not name-derived.
    name: Option<String>,
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

    // Image attach: Claude only. Gate BEFORE touching the PTY and return a machine-readable
    // `status:"unsupported"` (HTTP 200) so the phone reframes it as an in-app notice — never a
    // "do it on the desktop" deferral. Text-only messages are unchanged for all runtimes.
    if !body.images.is_empty() {
        if runtime_for_tab(&s.app, &tab_id) != Some(AgentRuntime::Claude) {
            return Ok(Json(json!({ "status": "unsupported", "reason": "unsupported_runtime",
                "detail": "This agent can't accept images yet." })));
        }
        // foreground_command is Some only for a live ssh/mosh: the temp files are LOCAL paths the
        // remote claude can't see. With a live bridge tunnel we stage the bytes on the remote host
        // over its mux socket and type THOSE paths; no tunnel (mosh, bridge down/disabled) or a
        // failed transfer degrades to the same `unsupported_ssh` the app already renders.
        let is_ssh = crate::pty::get_pty_info(&s.app, &pty)
            .map(|i| i.foreground_command.is_some())
            .unwrap_or(false);
        let paths = if is_ssh {
            let tunnel = {
                let tunnels = s.app.ssh_tunnels.read();
                tunnels
                    .values()
                    .find(|t| t.tab_ids.contains(&tab_id))
                    .map(|t| (t.host_key.clone(), t.ssh_args.clone()))
            };
            let Some((host_key, ssh_args)) = tunnel else {
                return Ok(Json(json!({ "status": "unsupported", "reason": "unsupported_ssh",
                    "detail": "Images need the maiTerm SSH bridge, which isn't connected for this tab." })));
            };
            match stage_images_remote(&host_key, &ssh_args, &body.images).await {
                Ok(paths) => paths,
                Err(e) => {
                    log::warn!("[maiLink] remote image staging failed for tab {tab_id}: {e}");
                    return Ok(Json(json!({ "status": "unsupported", "reason": "unsupported_ssh",
                        "detail": "Couldn't stage the images on the remote host." })));
                }
            }
        } else {
            let mut paths = Vec::with_capacity(body.images.len());
            for img in &body.images {
                paths.push(save_image_temp(&img.data, &img.mime).map_err(|e| {
                    log::warn!("[maiLink] local image staging failed for tab {tab_id}: {e}");
                    StatusCode::INTERNAL_SERVER_ERROR
                })?);
            }
            paths
        };
        inject_image_paths_and_text(&s.app, &pty, &paths, &body)
            .await
            .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
        return Ok(Json(json!({ "status": "delivered", "msg_id": format!("m_{}", now_ms()) })));
    }

    inject_text(&s.app, &pty, &body.text, body.submit)
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    Ok(Json(json!({ "status": "delivered", "msg_id": format!("m_{}", now_ms()) })))
}

/// The persisted runtime for a tab (Claude/Codex/Gemini), or None if it isn't/was never an agent
/// tab. Runtime persists after the agent process dies (see designated_tabs), so this is stable.
fn runtime_for_tab(app: &AppState, tab_id: &str) -> Option<AgentRuntime> {
    let data = app.app_data.read();
    data.windows
        .iter()
        .flat_map(|w| &w.workspaces)
        .flat_map(|ws| &ws.panes)
        .flat_map(|p| &p.tabs)
        .find(|t| t.id == tab_id)
        .and_then(|t| t.runtime)
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
    let (kind, cur_id, runtime) = match current {
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
        // permission menu: a single keystroke selects the option (no bracketed paste);
        // the key is runtime-specific — see permission_key.
        "permission" => {
            let key = permission_key(runtime, body.choice.as_deref().unwrap_or(""));
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
            // Confirm the selector actually submitted before claiming success. The PostToolUse hook
            // clears the tab's pending_question when the AskUserQuestion resolves; if it's still
            // open after a grace window, the keystrokes didn't drive it to Submit (e.g. an unhandled
            // selector variant like multiSelect+Other). Return inject_failed instead of a false
            // ok:true — otherwise the phone optimistically marks it answered while the agent got
            // nothing ("phone says done, agent never advanced"). The app recovers on !ok.
            let mut submitted = false;
            for _ in 0..20 {
                tokio::time::sleep(std::time::Duration::from_millis(100)).await;
                if pending_question_for_tab(&s.app, &tab_id).is_none() {
                    submitted = true;
                    break;
                }
            }
            if !submitted {
                log::warn!("[maiLink] AskUserQuestion still open ~2s after inject — reporting inject_failed (tab {tab_id})");
                return Ok(Json(json!({ "ok": false, "reason": "inject_failed",
                    "detail": "selector still open after injection" })));
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
    // Claude Code does NOT fire its Stop hook on a user interrupt (ESC) — only on a normal turn
    // completion. Our whole chat-state machine is hook-driven, so after an interrupt the session
    // stays `Active` forever and the tab latches "Working" on the phone. We caused the interrupt,
    // so we authoritatively settle the tab's running sessions to `Stopped` (mirroring the Stop
    // hook): the next WS tick / REST poll then reports `idle`. Fixing the SOURCE — not emitting a
    // transient event — is what survives the phone's 2s REST-poll floor; an event would be
    // overwritten within a tick. If the ESC didn't actually land (agent keeps working), the next
    // real PreToolUse hook flips it back to `Active`, so a spurious idle self-heals.
    let settled = settle_tab_interrupt(&s.app, &tab_id);
    Ok(Json(json!({ "ok": true, "settled": settled })))
}

/// Settle a tab's mid-turn agent sessions to `Stopped` after an interrupt, mirroring the Stop
/// hook's reset (state + tool + pending-question cleared). Only touches sessions that are actually
/// running (`Active` / `WaitingPermission`) so an already-idle tab is left alone. Returns true if
/// any session was transitioned — i.e. the interrupt settled a running turn.
fn settle_tab_interrupt(app: &AppState, tab_id: &str) -> bool {
    let mut sessions = app.agent_sessions.write();
    let mut changed = false;
    for sess in sessions.values_mut() {
        if sess.tab_id == tab_id
            && matches!(
                sess.state,
                AgentSessionState::Active | AgentSessionState::WaitingPermission
            )
        {
            sess.state = AgentSessionState::Stopped;
            sess.tool_name = None;
            sess.tool_detail = None;
            sess.pending_question = None;
            sess.pending_question_at = None;
            changed = true;
        }
    }
    changed
}

/// Max tab title accepted from the phone. Titles are one-line chat labels; a runaway string would
/// wreck the desktop tab strip. We normalize rather than reject on length: trim, then cap by CHAR
/// count (not bytes — never split a multibyte grapheme's code point).
const MAX_TAB_TITLE_CHARS: usize = 120;

/// Normalize a phone-supplied tab title: trim surrounding whitespace, then cap by CHAR count so a
/// runaway string can't wreck the desktop tab strip. Returns `None` for empty/whitespace-only
/// input (the caller rejects with 400 — an empty title is a client bug, not a way to clear it).
fn normalize_tab_title(raw: &str) -> Option<String> {
    let t: String = raw.trim().chars().take(MAX_TAB_TITLE_CHARS).collect();
    if t.is_empty() {
        None
    } else {
        Some(t)
    }
}

#[derive(serde::Deserialize)]
struct RenameBody {
    title: String,
}

/// POST /chats/{tabId}/rename — set the tab's title from the phone. Sets `custom_name` so the
/// chosen title pins against later OSC/agent title overrides (same semantics as a desktop rename),
/// persists it (survives resume/restart), and emits `mailink-tab-renamed` so every open desktop
/// window updates the live tab strip. The WS poller carries the new label to the phone as
/// `chats_changed` within one tick. Returns the normalized title actually stored.
async fn post_rename(
    State(s): State<ApiState>,
    headers: HeaderMap,
    Path(tab_id): Path<String>,
    Json(body): Json<RenameBody>,
) -> Result<Json<Value>, StatusCode> {
    authorize(&s, &headers)?;
    if !is_designated(&s.app, &tab_id) {
        return Err(StatusCode::NOT_FOUND);
    }
    let title = normalize_tab_title(&body.title).ok_or(StatusCode::BAD_REQUEST)?;
    if !set_tab_name(&s.app, &tab_id, &title) {
        return Err(StatusCode::NOT_FOUND);
    }
    // Reflect on every open desktop window's tab strip immediately (the frontend store owns its
    // own copy of tab.name and has no other signal that the backend changed it).
    if let Some(h) = &s.app_handle {
        let _ = h.emit("mailink-tab-renamed", json!({ "tabId": tab_id, "name": title }));
    }
    Ok(Json(json!({ "ok": true, "title": title })))
}

/// Find the terminal tab `tab_id` across all windows and set its name + `custom_name`, persisting
/// eagerly (like the frontend `rename_tab` command). Returns false if the tab id isn't found.
fn set_tab_name(app: &AppState, tab_id: &str, name: &str) -> bool {
    let data_clone = {
        let mut data = app.app_data.write();
        let mut found = false;
        'outer: for win in &mut data.windows {
            for ws in &mut win.workspaces {
                for pane in &mut ws.panes {
                    if let Some(tab) = pane.tabs.iter_mut().find(|t| t.id == tab_id) {
                        tab.name = name.to_string();
                        tab.custom_name = true;
                        found = true;
                        break 'outer;
                    }
                }
            }
        }
        if !found {
            return false;
        }
        data.clone()
    };
    let _ = crate::state::save_state(&data_clone);
    true
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
    // Per-tab last-seen title. A rename doesn't move `state`/`prompt` (the attn_key), so it needs
    // its own diff to trigger a `chats_changed` and re-fetch the label on the phone.
    let mut titles: HashMap<String, String> = HashMap::new();
    // Streaming state (mailink-protocol §12): per-tab last-window msg_ids + transcript mtime, so the
    // message ticker diffs cheaply and emits only newly-appended turns.
    let mut seen: HashMap<String, std::collections::HashSet<String>> = HashMap::new();
    let mut mtimes: HashMap<String, u64> = HashMap::new();

    // initial snapshot: one chat_state per chat
    for c in build_chats(&s.app) {
        let tab = c["tabId"].as_str().unwrap_or_default().to_string();
        let key = attn_key(c["state"].as_str().unwrap_or_default(), c["prompt"].as_str());
        if socket.send(Message::Text(chat_state_event(&c).to_string().into())).await.is_err() {
            return;
        }
        titles.insert(tab.clone(), c["title"].as_str().unwrap_or_default().to_string());
        last.insert(tab, key);
    }

    let mut ticker = tokio::time::interval(std::time::Duration::from_millis(1500));
    // A faster, mtime-gated ticker for per-turn message streaming: near-instant delivery without
    // paying the full chat rebuild (build_chats) at this cadence.
    let mut msg_ticker = tokio::time::interval(std::time::Duration::from_millis(400));
    // SSH transcript mirror keep-fresh: hook events drive most fetches, but a long assistant
    // turn appends JSONL with no hook until Stop — while a phone is actually watching, pull
    // the delta on a slow tick too. schedule_fetch coalesces, so ticks over an idle session
    // cost one no-op ssh mux command; non-SSH tabs are filtered out inside.
    let mut mirror_ticker = tokio::time::interval(std::time::Duration::from_secs(5));
    loop {
        tokio::select! {
            _ = msg_ticker.tick() => {
                if stream_new_messages(&mut socket, &s.app, &mut seen, &mut mtimes).await.is_err() {
                    return;
                }
            }
            _ = mirror_ticker.tick() => {
                let tabs: Vec<String> = designated_tabs(&s.app).into_iter().map(|t| t.tab_id).collect();
                mirror::refresh_tabs(&s.app, &tabs);
            }
            _ = ticker.tick() => {
                let chats = build_chats(&s.app);
                let mut current_ids = std::collections::HashSet::new();
                let mut roster_changed = false;
                for c in &chats {
                    let tab = c["tabId"].as_str().unwrap_or_default().to_string();
                    let st = c["state"].as_str().unwrap_or_default().to_string();
                    let key = attn_key(&st, c["prompt"].as_str());
                    current_ids.insert(tab.clone());
                    let prev = last.get(&tab).cloned();
                    if prev.is_none() {
                        roster_changed = true;
                    }
                    // A rename of an already-known tab changes the label but not state/prompt —
                    // fire `chats_changed` so the inbox + open thread re-fetch the new title.
                    let title = c["title"].as_str().unwrap_or_default().to_string();
                    if prev.is_some() && titles.get(&tab).map(String::as_str) != Some(title.as_str()) {
                        roster_changed = true;
                    }
                    titles.insert(tab.clone(), title);
                    if prev.as_deref() != Some(key.as_str()) {
                        if socket.send(Message::Text(chat_state_event(c).to_string().into())).await.is_err() {
                            return;
                        }
                        // Attention only on an OBSERVED transition into an attention state. A tab
                        // that merely APPEARS in the roster already idle (exposure toggled on, a
                        // restore) must not announce "finished" — prev must exist and not already
                        // have been attention-worthy.
                        if prev.as_deref().is_some_and(|p| !is_attn(p)) && is_attn(&key) {
                            let ev = attention_event(&s.app, &tab, &st, c["title"].as_str().unwrap_or_default());
                            if socket.send(Message::Text(ev.to_string().into())).await.is_err() {
                                return;
                            }
                        }
                        last.insert(tab, key);
                    }
                }
                // drop tabs that disappeared from the designated set
                let removed: Vec<String> = last.keys().filter(|k| !current_ids.contains(*k)).cloned().collect();
                if !removed.is_empty() {
                    roster_changed = true;
                    for k in removed { last.remove(&k); titles.remove(&k); }
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
    // Carry the chat's REAL per-tab last-activity (build_chats computed it) as both `ts` and
    // `lastActivityTs`. The initial WS snapshot replays one chat_state per existing chat, so
    // stamping `now_ms()` here made every chat land at the same age and jump to "now" together —
    // the reported lockstep. On a genuine live transition this value ≈ now anyway.
    let ts = c.get("lastActivityTs").cloned().unwrap_or_else(|| json!(now_ms()));
    let mut ev = json!({
        "type": "chat_state",
        "tabId": c["tabId"],
        "state": c["state"],
        "runtime": c["runtime"],
        "ts": ts.clone(),
        "lastActivityTs": ts,
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
        let Some((rt, sid)) = resolved_session_for_tab(app, &t.tab_id) else { continue };
        // mtime gate: an unchanged transcript means no new turns, so skip the tail re-parse.
        if let Some(mt) = transcript::mtime_for(rt, &sid) {
            if mtimes.get(&t.tab_id) == Some(&mt) {
                continue;
            }
            mtimes.insert(t.tab_id.clone(), mt);
        }
        // Same call GET uses (limit 40, Marker) so streamed ids are byte-identical to the REST path.
        let Some(turns) = transcript::turns_for(rt, &sid, 40, transcript::ToolRender::Marker)
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
    // Real per-tab last-activity (from the detail we just built) so the phone can sort a chat that
    // enters attention consistently with the /chats list, rather than by request time.
    let ts = detail
        .as_ref()
        .and_then(|d| d.get("lastActivityTs").cloned())
        .unwrap_or_else(|| json!(now_ms()));
    let mut ev = json!({
        "type": "attention",
        "tabId": tab_id,
        "kind": kind,
        "summary": format!("{title}: {what}"),
        "ts": ts.clone(),
        "lastActivityTs": ts,
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
pub(crate) async fn inject_text(
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

/// Pause between image-path writes (and before the caption/submit) so the Claude Code TUI converts
/// each pasted path into an `[Image #N]` chip before the next path — or the CR — arrives. Mirrors
/// the desktop drag-drop path's 200ms inter-path delay, with a little headroom.
const IMAGE_SETTLE_MS: u64 = 220;

/// Type pre-staged image paths plus an optional caption into a Claude Code TUI, then submit once
/// for the whole batch. The paths must already exist on the host where claude RUNS (local temp
/// files, or remote-staged for SSH tabs — staging happens before any typing so a failed transfer
/// never leaves half a batch in the prompt). Mechanism (matches the desktop drag-drop/clipboard
/// path and CC's image-attach contract): type each BARE ABSOLUTE PATH — raw, NOT wrapped in
/// bracketed paste (wrapping defeats CC's path→image detection) — followed by a space, pausing
/// between images so the TUI attaches each before the next arrives. Then paste the caption via the
/// normal bracketed-paste path and submit with CR. CC reads the files at submit time; we leave the
/// temp files for the OS tmp reaper (small; deleting risks racing that read).
async fn inject_image_paths_and_text(
    app: &Arc<AppState>,
    pty_id: &str,
    paths: &[String],
    body: &MessageBody,
) -> Result<(), String> {
    for (i, path) in paths.iter().enumerate() {
        if i > 0 {
            tokio::time::sleep(std::time::Duration::from_millis(IMAGE_SETTLE_MS)).await;
        }
        let mut buf = path.clone().into_bytes();
        buf.push(b' '); // trailing space delimits the path from the next path / the caption
        crate::pty::write_pty(app, pty_id, &buf)?;
    }
    // Let the TUI finish attaching the final image before the caption + CR land.
    tokio::time::sleep(std::time::Duration::from_millis(IMAGE_SETTLE_MS)).await;
    // Caption may be empty — an empty bracketed paste + CR just submits the images alone.
    inject_text(app, pty_id, &body.text, body.submit).await
}

/// The temp-file extension for an image mime. Claude Code sniffs images BY EXTENSION, so unknown
/// mimes default to a recognized one rather than failing the attach.
fn image_ext(mime: &str) -> &'static str {
    match mime {
        "image/png" => "png",
        "image/jpeg" => "jpg",
        "image/webp" => "webp",
        "image/gif" => "gif",
        _ => "png",
    }
}

fn decode_image(data_base64: &str) -> Result<Vec<u8>, String> {
    base64::engine::general_purpose::STANDARD
        .decode(data_base64.as_bytes())
        .map_err(|e| format!("invalid image base64: {e}"))
}

/// Decode a base64 image and write it to a temp file whose extension Claude Code recognizes as an
/// image. Returns the absolute path. Sibling of commands::editor::save_clipboard_image; kept local
/// so the maiLink path has no UI-command dep.
fn save_image_temp(data_base64: &str, mime: &str) -> Result<String, String> {
    let bytes = decode_image(data_base64)?;
    let path = std::env::temp_dir()
        .join(format!("maiterm-mailink-{}.{}", uuid::Uuid::new_v4(), image_ext(mime)));
    std::fs::write(&path, &bytes).map_err(|e| format!("cannot write temp image: {e}"))?;
    log::info!("[maiLink] staged {} image bytes → {:?}", bytes.len(), path);
    Ok(path.to_string_lossy().to_string())
}

/// Stage a message's images in the SSH tab's REMOTE /tmp, returning the remote paths to type.
/// Bytes stream over `ssh … 'cat > path'` mux'd through the bridge tunnel's ControlMaster
/// socket — ssh (not scp) so the tunnel's recorded ssh_args apply verbatim (scp's -P port flag
/// diverges from ssh's -p). All-or-nothing: any failed transfer fails the batch before a single
/// path is typed. Fixed /tmp (not $TMPDIR) so the typed path is knowable without a round trip;
/// like local temps, the files are left to the remote's tmp reaper. Caveat: staging lands on the
/// tunnel host — if the user ssh'd onward to a third host, claude there can't see the files and
/// renders dead path chips (recoverable; same class of mismatch as the transcript mirror, which
/// simply finds no file).
async fn stage_images_remote(
    host_key: &str,
    ssh_args: &str,
    images: &[ImageInput],
) -> Result<Vec<String>, String> {
    let mut paths = Vec::with_capacity(images.len());
    for img in images {
        let bytes = decode_image(&img.data)?;
        let remote_path =
            format!("/tmp/maiterm-mailink-{}.{}", uuid::Uuid::new_v4(), image_ext(&img.mime));
        push_bytes_remote(host_key, ssh_args, &bytes, &remote_path).await?;
        log::info!(
            "[maiLink] staged {} image bytes → {}:{}",
            bytes.len(), host_key, remote_path
        );
        paths.push(remote_path);
    }
    Ok(paths)
}

/// Write `bytes` to `remote_path` on an SSH bridge host via `cat > path` with the bytes on
/// stdin. ~tens of ms over the mux socket; BatchMode direct fallback when the socket is dead.
/// pub(crate): the comms attachment staging (screenshots → SSH tabs) reuses it.
pub(crate) async fn push_bytes_remote(
    host_key: &str,
    ssh_args: &str,
    bytes: &[u8],
    remote_path: &str,
) -> Result<(), String> {
    let quoted = format!("'{}'", remote_path.replace('\'', "'\\''"));
    let mut cmd_args = crate::commands::ssh_tunnel::mux_client_args(host_key);
    for arg in ssh_args.split_whitespace() {
        cmd_args.push(arg.to_string());
    }
    cmd_args.push(format!("cat > {quoted}"));

    let mut child = tokio::process::Command::new("ssh")
        .args(&cmd_args)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .map_err(|e| format!("ssh spawn failed: {e}"))?;

    let mut stdin = child.stdin.take().ok_or("no ssh stdin")?;
    use tokio::io::AsyncWriteExt;
    stdin
        .write_all(bytes)
        .await
        .map_err(|e| format!("streaming image bytes failed: {e}"))?;
    drop(stdin); // EOF ends the remote `cat`

    let output = tokio::time::timeout(
        tokio::time::Duration::from_secs(30),
        child.wait_with_output(),
    )
    .await
    .map_err(|_| "image transfer timed out (30s)".to_string())?
    .map_err(|e| format!("ssh wait failed: {e}"))?;

    if !output.status.success() {
        return Err(format!(
            "remote write failed (exit {:?}): {}",
            output.status.code(),
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    Ok(())
}

/// Read `remote_path`'s bytes from an SSH bridge host via `cat < path` — the pull mirror
/// of push_bytes_remote, muxing over the same tunnel-owned CM socket. Used by the comms
/// integration to fetch an SSH-tab agent's screenshot before uploading it to the chat.
pub(crate) async fn fetch_bytes_remote(
    host_key: &str,
    ssh_args: &str,
    remote_path: &str,
) -> Result<Vec<u8>, String> {
    let quoted = format!("'{}'", remote_path.replace('\'', "'\\''"));
    let mut cmd_args = crate::commands::ssh_tunnel::mux_client_args(host_key);
    for arg in ssh_args.split_whitespace() {
        cmd_args.push(arg.to_string());
    }
    cmd_args.push(format!("cat < {quoted}"));

    let output = tokio::time::timeout(
        tokio::time::Duration::from_secs(30),
        tokio::process::Command::new("ssh")
            .args(&cmd_args)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .output(),
    )
    .await
    .map_err(|_| "remote read timed out (30s)".to_string())?
    .map_err(|e| format!("ssh spawn failed: {e}"))?;

    if !output.status.success() {
        return Err(format!(
            "remote read failed (exit {:?}): {}",
            output.status.code(),
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    Ok(output.stdout)
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

/// Type free-text into the selector's highlighted "Other" inline input (a live field). Collapsed
/// to a single line so a stray newline can't submit early; settles like a keystroke.
async fn send_text(app: &Arc<AppState>, pty_id: &str, text: &str) -> Result<(), String> {
    let one_line: String = text.split(['\n', '\r']).collect::<Vec<_>>().join(" ");
    crate::pty::write_pty(app, pty_id, one_line.as_bytes())?;
    tokio::time::sleep(std::time::Duration::from_millis(ADVANCE_SETTLE_MS)).await;
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
/// Claude Code 2.1.x (docs §12.3). The form is a row of tabs [Q1..Qn][Submit]:
///   (a) each question's highlight starts at row 0;                                      [VERIFIED]
///   (b) the selector is arrow-only — the shown 1..n are labels, NOT digit-select keys;  [VERIFIED]
///   (c) single-select: ↑/↓ to the row, Enter selects AND advances to the next tab;      [VERIFIED e2e]
///   (d) multiSelect: Space toggles each row (live, no Enter), then → advances the tab;   [VERIFIED e2e]
///   (e) a lone single-select question submits on its own Enter; every other form lands on the
///       "Submit" tab and takes one final Enter.                                          [VERIFIED]
///   (f) the free-text "Other" row (labelled "Type something", at index option_count) is a live
///       inline input — navigate to it and TYPE directly (no Enter-to-open). For a single-select
///       question, Enter then selects+advances just like a listed pick.                    [VERIFIED e2e]
/// BEST-GUESS (pending device validation): a multiSelect question with BOTH checkbox picks and
/// Other free-text — typing checks the Other row, then Enter commits/releases the input before →
/// advances. In probes the raw → was swallowed by the active input; the commit-Enter is the fix,
/// unverified on a real device. All other shapes are verified e2e. All mapping is resolved BEFORE
/// any keystroke is sent, so a bad answer rejects the whole batch rather than half-answering.
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
        option_count: usize,   // # of listed options; the "Type something" (Other) row is at this index
        indices: Vec<usize>,   // listed-option rows to select/toggle
        other: Option<String>, // free-text for the "Other" row, if the phone chose it
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
            return Err(format!("question {qi}: no option selected and no Other text"));
        }
        // single-select accepts exactly one pick total (one listed option XOR Other text).
        if !multi && indices.len() + other.is_some() as usize > 1 {
            return Err(format!("question {qi}: single-select received multiple picks"));
        }
        indices.sort_unstable();
        indices.dedup();
        plans.push(Plan { multi, option_count: labels.len(), indices, other });
    }

    // Inject, question by question. The form is a row of tabs [Q1..Qn][Submit]; ↑/↓ moves within
    // a question, and the single-select Enter / a multiSelect → moves to the next tab.
    let single_q_single_select = plans.len() == 1 && !plans[0].multi;
    for plan in &plans {
        let mut cur = 0usize; // highlight starts at row 0 (a)
        if plan.multi {
            for &idx in &plan.indices {
                cur = nav_to(app, pty_id, cur, idx).await?;
                send_key(app, pty_id, b" ", NAV_SETTLE_MS).await?; // Space toggles the row (live)
            }
            if let Some(text) = &plan.other {
                // The "Type something" row (at option_count) is a live inline input — typing into
                // it fills and checks it.
                cur = nav_to(app, pty_id, cur, plan.option_count).await?;
                send_text(app, pty_id, text).await?;
                // BEST-GUESS (multiSelect+Other, pending device validation): commit the typed text
                // with Enter so the input releases; otherwise the active field swallows the → below.
                // In probes the raw → was captured by the input mid-edit and the form never
                // advanced — see docs §12.3.
                send_key(app, pty_id, b"\r", NAV_SETTLE_MS).await?;
            }
            // multiSelect toggles are live and are NOT confirmed with Enter; → advances to the
            // next tab (the next question, or Submit after the last one).
            send_key(app, pty_id, b"\x1b[C", ADVANCE_SETTLE_MS).await?;
        } else if let Some(text) = &plan.other {
            // single-select via the Other row: navigate to it, type the text, then Enter — which
            // selects it and advances exactly like picking a listed option.
            cur = nav_to(app, pty_id, cur, plan.option_count).await?;
            send_text(app, pty_id, text).await?;
            send_key(app, pty_id, b"\r", ADVANCE_SETTLE_MS).await?;
        } else {
            cur = nav_to(app, pty_id, cur, plan.indices[0]).await?;
            send_key(app, pty_id, b"\r", ADVANCE_SETTLE_MS).await?; // Enter selects AND advances
        }
        let _ = cur;
    }

    // Submit. A lone single-select question submits on its own Enter above (there is no Submit
    // tab). Every other form — multi-question, or any multiSelect — lands on the "Submit" tab,
    // which takes one Enter. (Pinned live — docs §12.3.)
    if !single_q_single_select {
        send_key(app, pty_id, b"\r", ADVANCE_SETTLE_MS).await?;
    }
    Ok(())
}

/// The tab's currently-open prompt, as (kind, prompt_id, runtime). Mirrors what
/// `build_chat_detail` synthesizes, so `/respond`'s stale-guard agrees with what the client was
/// shown; the runtime picks the keystroke dialect for the answer injection.
fn current_prompt(app: &AppState, tab_id: &str) -> Option<(&'static str, String, AgentRuntime)> {
    let states = session_states(app);
    let (st, rt, tool) = states.get(tab_id)?;
    // AskUserQuestion first: it coincides with a permission_prompt state (see build_chat_detail),
    // but the open ask is the structured question — the stale-guard must agree with what was shown.
    if tool.as_deref() == Some("AskUserQuestion") {
        Some(("question", question_prompt_id(app, tab_id), *rt))
    } else if map_state(*st) == "permission" {
        Some(("permission", format!("p_{tab_id}"), *rt))
    } else {
        None
    }
}

/// Per-ASK prompt id for an open AskUserQuestion: `q_<tab>_<asked_at>`. The capture timestamp
/// makes successive asks on one tab distinct, so a late `/respond` against an EXPIRED ask
/// (Claude auto-resolves after ~60s) can never pass the stale-guard and answer a newer
/// question that opened meanwhile. Opaque to the app — it just echoes it.
fn question_prompt_id(app: &AppState, tab_id: &str) -> String {
    let at = pending_question_at_for_tab(app, tab_id).unwrap_or(0);
    format!("q_{tab_id}_{at}")
}

/// Map a permission `choice` to the runtime's TUI keystroke. (Fragile by nature — depends on
/// the runtime's current affordance; the robust path is a free-text /message. See docs §5.)
///
///   * Claude: a fixed numeric menu — 1=yes, 2=yes+don't-ask, 3=no. A bare digit passes
///     through; an unknown label defaults to deny.
///   * Codex: the approval overlay is a VARIABLE-length list (2–5 options: approve /
///     approve-for-prefix / approve-for-session / network-amendment / deny / decline
///     depending on the request), where digit keys select by POSITION — so Claude's "3"
///     could land on a "Yes, and don't ask again…" row. Codex's default keymap letter
///     shortcuts are stable regardless of option count (codex-rs tui/src/keymap.rs):
///     y=approve, a=approve-for-session, n=decline ("No, and tell Codex what to do
///     differently" — the analogue of Claude's option 3). Digits from the phone are
///     translated to those letters, never passed through.
///   * Gemini: no hook registrar yet, so a Gemini session can't reach the permission
///     state — falls to the Claude arm as a placeholder.
fn permission_key(runtime: AgentRuntime, choice: &str) -> String {
    let c = choice.trim();
    if runtime == AgentRuntime::Codex {
        let key = match c {
            "1" => "y",
            "2" => "a",
            "3" => "n",
            _ => match c.to_lowercase().as_str() {
                "yes" | "approve" | "allow" => "y",
                "yes, don't ask again" | "yes, and don't ask again" | "always" => "a",
                _ => "n", // safe default: decline (returns control to the human)
            },
        };
        return key.to_string();
    }
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

pub(crate) fn pty_for_tab(app: &AppState, tab_id: &str) -> Option<String> {
    app.tab_pty_map.read().get(tab_id).cloned()
}

/// The (runtime, session id) whose transcript we read for a tab. If a tab has more than one
/// tracked session (e.g. after a resume minted a new id), prefer the most attention-worthy —
/// consistent with how `session_states` picks the tab's displayed state.
fn live_session_for_tab(app: &AppState, tab_id: &str) -> Option<(AgentRuntime, String)> {
    let sessions = app.agent_sessions.read();
    sessions
        .iter()
        .filter(|(_, s)| s.tab_id == tab_id)
        .max_by_key(|(_, s)| rank(s.state))
        .map(|(id, s)| (s.runtime, id.clone()))
}

/// The tab's persisted resume session id — the runtime's `<runtime>SessionId` trigger variable
/// that the auto-resume command interpolates (`claude --resume %claudeSessionId`,
/// `codex resume %codexSessionId`). Used to resolve a transcript for an agent that has
/// auto-resumed but NOT yet re-registered: in that window `agent_sessions` has no live entry, so
/// without this the phone falls back to a raw terminal scrape (wide, unwrapped) or empty, and
/// the app shows stale/duplicated detail.
fn persisted_session_for_tab(app: &AppState, tab_id: &str) -> Option<(AgentRuntime, String)> {
    let data = app.app_data.read();
    for win in &data.windows {
        for ws in &win.workspaces {
            for pane in &ws.panes {
                for tab in &pane.tabs {
                    if tab.id == tab_id {
                        // None → Claude (matches designated_tabs' default): tabs persisted by
                        // app versions predating Tab.runtime can still carry claudeSessionId.
                        let rt = tab.runtime.unwrap_or_default();
                        let var = crate::state::agent_runtime::descriptor(rt).session_id_var;
                        return tab
                            .trigger_variables
                            .get(var)
                            .cloned()
                            .filter(|s| !s.is_empty())
                            .map(|sid| (rt, sid));
                    }
                }
            }
        }
    }
    None
}

/// (runtime, session id) for reading a tab's transcript: the LIVE session if one is registered,
/// else the PERSISTED resume id (covers the resume-before-initSession window after a relaunch).
fn resolved_session_for_tab(app: &AppState, tab_id: &str) -> Option<(AgentRuntime, String)> {
    live_session_for_tab(app, tab_id).or_else(|| persisted_session_for_tab(app, tab_id))
}

/// The captured AskUserQuestion `tool_input` for a tab (most attention-worthy session), if an
/// elicitation is currently open. Mirrors how `live_session_for_tab` resolves the tab's session.
fn pending_question_for_tab(app: &AppState, tab_id: &str) -> Option<Value> {
    let sessions = app.agent_sessions.read();
    sessions
        .iter()
        .filter(|(_, s)| s.tab_id == tab_id)
        .max_by_key(|(_, s)| rank(s.state))
        .and_then(|(_, s)| s.pending_question.clone())
}

/// Unix-ms when the tab's open AskUserQuestion was captured. Display-only on the phone
/// ("asked 2m ago"); expiry is derived from `question_expires_at`, never from this.
fn pending_question_at_for_tab(app: &AppState, tab_id: &str) -> Option<i64> {
    let sessions = app.agent_sessions.read();
    sessions
        .iter()
        .filter(|(_, s)| s.tab_id == tab_id)
        .max_by_key(|(_, s)| rank(s.state))
        .and_then(|(_, s)| s.pending_question_at)
}

/// Millis until an unanswered AskUserQuestion auto-resolves, given the session's Claude Code
/// version and the user's `askUserQuestionTimeout` setting — `None` when the ask waits
/// indefinitely. The timer's history (verified against the CC changelog AND a sweep of every
/// local ask's resolution latency by version, 2026-07-03):
///   - < 2.1.198: no timer ever (asks observed answered hours later).
///   - 2.1.198–2.1.199: hard-coded 60s auto-resolve, non-configurable.
///   - ≥ 2.1.200: opt-in via `askUserQuestionTimeout` in ~/.claude/settings.json (USER scope
///     only): "never" (default) | "60s" | "5m" | "10m". Parsed generically (`<n>s`/`<n>m`) so a
///     future value like "2m" still works.
/// Unknown version ⇒ None: a missing countdown on an ask that does expire degrades safely
/// (stale-guard + composer fallback), while a false countdown expires a live question — the
/// strictly worse failure.
pub(crate) fn ask_deadline_ms(version: Option<&str>, setting: Option<&str>) -> Option<i64> {
    let v = parse_cc_version(version?)?;
    if v < (2, 1, 198) {
        return None;
    }
    if v < (2, 1, 200) {
        return Some(60_000);
    }
    let s = setting?.trim().to_ascii_lowercase();
    let (num, unit) = s.split_at(s.len().checked_sub(1)?);
    let n: i64 = num.parse().ok().filter(|n| *n > 0)?;
    match unit {
        "s" => Some(n * 1_000),
        "m" => Some(n * 60_000),
        _ => None, // "never" and anything unrecognized
    }
}

/// "2.1.200" → (2, 1, 200). Tolerates a suffix on the patch part ("2.2.0-beta1").
fn parse_cc_version(s: &str) -> Option<(u64, u64, u64)> {
    let mut it = s.split('.').map(|p| {
        p.chars()
            .take_while(|c| c.is_ascii_digit())
            .collect::<String>()
            .parse::<u64>()
            .ok()
    });
    Some((it.next()??, it.next()??, it.next()??))
}

/// The user's `askUserQuestionTimeout` from `~/.claude/settings.json` — the only scope Claude
/// Code reads that key from (not project/local settings). `None` when unset or unreadable.
fn ask_timeout_setting() -> Option<String> {
    let path = dirs::home_dir()?.join(".claude").join("settings.json");
    let v: Value = serde_json::from_str(&std::fs::read_to_string(path).ok()?).ok()?;
    v.get("askUserQuestionTimeout")
        .and_then(|s| s.as_str())
        .map(String::from)
}

/// Absolute unix-ms deadline of the tab's open AskUserQuestion, when one applies to this
/// session's CC build + settings. `None` ⇒ the ask waits indefinitely (the phone must show no
/// countdown). Claude-only: codex/gemini have no AskUserQuestion.
fn question_expires_at(app: &AppState, tab_id: &str) -> Option<i64> {
    let at = pending_question_at_for_tab(app, tab_id)?;
    let (rt, sid) = resolved_session_for_tab(app, tab_id)?;
    if !matches!(rt, AgentRuntime::Claude) {
        return None;
    }
    let ver = transcript::claude_session_version(&sid);
    let deadline = ask_deadline_ms(ver.as_deref(), ask_timeout_setting().as_deref())?;
    Some(at + deadline)
}

/// The compact argument label of the tab's current tool (e.g. the Bash command awaiting
/// approval), resolved like `pending_question_for_tab`. Feeds the permission card text.
fn tool_detail_for_tab(app: &AppState, tab_id: &str) -> Option<String> {
    let sessions = app.agent_sessions.read();
    sessions
        .iter()
        .filter(|(_, s)| s.tab_id == tab_id)
        .max_by_key(|(_, s)| rank(s.state))
        .and_then(|(_, s)| s.tool_detail.clone())
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
                            let mut opt = json!({
                                "label": o.get("label").and_then(|v| v.as_str()).unwrap_or(""),
                                "description": o.get("description").and_then(|v| v.as_str()),
                            });
                            // Newer Claude Code asks attach a per-option `preview` (ASCII
                            // mockup / code snippet). Additive passthrough — the app can
                            // render it in the card whenever it wants to.
                            if let Some(p) = o.get("preview").and_then(|v| v.as_str()) {
                                opt["preview"] = json!(p);
                            }
                            opt
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

/// Build the chat transcript: per-turn source markdown from the session's transcript file
/// (Claude JSONL / Codex rollout) when we can find it, otherwise a single-system-turn scrape of
/// the tab's own live terminal (SSH tabs — whose transcripts live on the remote host — pruned
/// local sessions, Gemini, plain shells).
fn build_transcript(app: &AppState, tab_id: &str, now: u64) -> Vec<Value> {
    // Resolve via the LIVE session, or (post-relaunch, pre-initSession) the persisted resume
    // id — so a dormant/resuming agent still shows its real distilled conversation, keyed to
    // THIS tab, instead of a raw terminal scrape or empty (which the app rendered as
    // stale/duplicated "all agents look the same" detail).
    if let Some((rt, sid)) = resolved_session_for_tab(app, tab_id) {
        if let Some(turns) = transcript::turns_for(rt, &sid, 40, transcript::ToolRender::Marker) {
            if !turns.is_empty() {
                return turns;
            }
        }
    }
    // No transcript file resolvable — Claude included. This is the NORMAL case for every SSH
    // tab (the session runs on the remote host, so its JSONL lives in the REMOTE
    // ~/.claude/projects — most of Darryl's fleet) and for old local sessions Claude Code has
    // pruned (cleanupPeriodDays). Field bug 2026-07-03: Claude used to return empty here "to
    // avoid a scrape being misread", which blanked MOST of the inbox; the scrape is this tab's
    // own live PTY content (keyed via the LIVE tab→pty map, not the stale persisted tab.pty_id),
    // so showing it as a single system turn is strictly better than nothing. Dormant tabs have
    // no live PTY and still come back empty — the app renders its "no messages captured yet"
    // empty-state for those.
    let recent = pty_for_tab(app, tab_id)
        .and_then(|p| crate::commands::terminal::recent_text(app, &p, 40).ok())
        .unwrap_or_default();
    let mut out = Vec::new();
    if !recent.trim().is_empty() {
        // `kind: "terminal_snapshot"` is an explicit signal to the phone renderer: this is a raw
        // scrape of the live terminal grid (newline-delimited, may contain TUI chrome), NOT a
        // distilled conversation turn — render it preformatted (pre-wrap + break-word), badged as
        // a live snapshot, and treat it as a single replaceable block (stable msg_id, re-scraped
        // every GET) rather than appended history. The `ctx_` msg_id prefix carries the same
        // signal for older clients that sniff it.
        out.push(json!({
            "msg_id": format!("ctx_{tab_id}"),
            "role": "system",
            "kind": "terminal_snapshot",
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

/// Normalize a model id to a friendly display string, per runtime. Claude ids go through
/// `display_model` ("claude-opus-4-8[1m]" → "Opus 4.8"); Codex ids just get the family
/// capitalized ("gpt-5.5" → "GPT-5.5", "gpt-5-codex" → "GPT-5-codex"); anything else passes
/// through as-is.
fn display_model_for(rt: AgentRuntime, model_id: &str) -> String {
    match rt {
        AgentRuntime::Claude => display_model(model_id),
        AgentRuntime::Codex | AgentRuntime::Gemini => {
            match model_id.strip_prefix("gpt") {
                Some(rest) => format!("GPT{rest}"),
                None => model_id.to_string(),
            }
        }
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
/// from the session's transcript file — Claude JSONL `message.usage` (the SessionStart hook's
/// model is often null), or a Codex rollout's `token_count`/`turn_context` (which state the
/// window and model directly). Live/persisted session id so it also resolves during the
/// resume-before-init window. `effort` is intentionally omitted — it's only in Claude Code's
/// statusLine payload, which maiTerm doesn't receive. None for Gemini tabs (no transcript
/// source) or before the first assistant turn.
fn build_meta(app: &AppState, tab_id: &str) -> Option<Value> {
    let (rt, sid) = resolved_session_for_tab(app, tab_id)?;
    let meta = transcript::meta_for(rt, &sid)?;
    let model_id = meta.model_id.as_deref().unwrap_or("");
    // Codex rollouts carry the window; Claude's is derived from the model id.
    let limit = meta.context_window.unwrap_or_else(|| context_limit_for(model_id));
    let pct = ((meta.context_tokens as f64 / limit as f64) * 100.0)
        .round()
        .clamp(0.0, 100.0) as u64;
    let mut m = json!({
        "contextUsed": meta.context_tokens,
        "contextLimit": limit,
        "contextPct": pct,
    });
    if !model_id.is_empty() {
        m["model"] = json!(display_model_for(rt, model_id));
    }
    Some(m)
}

/// tab_id → scrollback `updated_at` in unix ms (one DB read). SQLite stores `datetime('now')` as
/// `YYYY-MM-DD HH:MM:SS` UTC; normalize to RFC3339 (`T` + `Z`) for the shared transcript parser.
/// Used as the last-activity fallback for tabs without a Claude transcript (Codex/Gemini, or a
/// Claude tab before its first turn).
fn scrollback_times(app: &AppState) -> HashMap<String, u64> {
    let mut out = HashMap::new();
    if let Ok(rows) = app.scrollback_db.tab_times() {
        for (tab, updated) in rows {
            let ms = transcript::rfc3339_to_ms(&format!("{}Z", updated.replace(' ', "T")));
            if ms > 0 {
                out.insert(tab, ms as u64);
            }
        }
    }
    out
}

/// Per-tab last-activity timestamp (unix ms) that the phone's inbox sorts by. A REAL signal, not
/// request time and not file mtime: the timestamp of the tab's last actual turn (assistant/tool
/// output or a genuine human message) from its Claude transcript, else the persisted scrollback
/// `updated_at` (any runtime), else `now` for a brand-new tab that has neither (legitimately "just
/// now"). We deliberately do NOT use the JSONL mtime here: a resume/replay rewrites the transcript
/// (hook context, `mode`/`last-prompt` metadata, `<system-reminder>`s) WITHOUT adding a real turn,
/// so mtime bumps for every restored tab on a restart and flattens the whole inbox to "now" —
/// exactly the recency clump this signal exists to prevent. The last-real-turn ts does not advance
/// on a pure resume. (mtime is still the right change-gate for WS streaming, where "anything
/// appended → re-scan" is the intended semantics — see stream_new_messages.)
fn last_activity_ts(app: &AppState, tab_id: &str, scrollback: &HashMap<String, u64>, now: u64) -> u64 {
    resolved_session_for_tab(app, tab_id)
        .and_then(|(rt, sid)| transcript::last_turn_ts_for(rt, &sid))
        .or_else(|| scrollback.get(tab_id).copied())
        .unwrap_or(now)
}

fn build_chats(app: &AppState) -> Vec<Value> {
    let t_total = std::time::Instant::now();
    let ph = std::time::Instant::now();
    let tabs = designated_tabs(app);
    let ms_tabs = ph.elapsed().as_millis(); // app_data read lock + workspace scan
    let ph = std::time::Instant::now();
    let states = session_states(app);
    let ms_states = ph.elapsed().as_millis();
    let now = now_ms();
    let ph = std::time::Instant::now();
    let scrollback = scrollback_times(app);
    let ms_scrollback = ph.elapsed().as_millis(); // scrollback_db mutex + one SQLite query
    let tab_count = tabs.len();
    let ph = std::time::Instant::now();
    let chats: Vec<Value> = tabs
        .into_iter()
        .map(|t| {
            let (state, runtime, tool) = match states.get(&t.tab_id) {
                Some((st, rt, tool)) => (map_state(*st), runtime_key(*rt), tool.clone()),
                None => ("dormant", runtime_key(t.runtime), None),
            };
            let ask_open = tool.as_deref() == Some("AskUserQuestion");
            // The kind of prompt currently open, if any. An open AskUserQuestion outranks the
            // (usually coincident) permission state — mirrors build_chat_detail/attention_event.
            let prompt_kind = if ask_open {
                Some("question")
            } else if state == "permission" {
                Some("permission")
            } else {
                None
            };
            let mut chat = json!({
                "tabId": t.tab_id,
                "title": t.title,
                "workspace": t.workspace,
                "runtime": runtime,
                "state": state,
                // Additive field: lets clients (and our own tickers) see prompt-kind changes
                // that don't move `state` — e.g. an AskUserQuestion opening at state=="active".
                "prompt": prompt_kind,
                // ask_open guards the case where a build leaves an open AskUserQuestion at
                // state=="active" — it still needs to surface as unread in the inbox.
                "unread": ask_open || state == "permission" || state == "idle",
                "lastActivityTs": last_activity_ts(app, &t.tab_id, &scrollback, now),
                "preview": preview_for(state, tool.as_deref()),
            });
            if let Some(meta) = build_meta(app, &t.tab_id) {
                chat["meta"] = meta;
            }
            chat
        })
        .collect();
    let ms_loop = ph.elapsed().as_millis(); // per-tab: locate_jsonl + last-turn + meta tail reads
    let ms_total = t_total.elapsed().as_millis();
    if ms_total > SLOW_BUILD_LOG_MS {
        log::warn!(
            "mailink slow chat-list tabs={} total={}ms [tabs(app_data)={} states(sessions)={} scrollback(db)={} per_tab_loop={}]",
            tab_count, ms_total, ms_tabs, ms_states, ms_scrollback, ms_loop,
        );
    }
    chats
}

/// A chat_detail / chat-list build slower than this logs a per-phase WARN breakdown. Opening a
/// thread is a handful of ms of CPU (transcript tail read + parse), so anything past this is
/// almost always LOCK-WAIT — an `app_data.read()` queued behind a `save_state` writer, or the
/// `scrollback_db` mutex behind a scrollback `save()`. The per-phase timings localize which lock.
const SLOW_BUILD_LOG_MS: u128 = 500;

fn build_chat_detail(app: &AppState, tab_id: &str) -> Option<Value> {
    let t_total = std::time::Instant::now();

    let ph = std::time::Instant::now();
    let meta = designated_tabs(app).into_iter().find(|t| t.tab_id == tab_id)?;
    let ms_tabs = ph.elapsed().as_millis(); // app_data read lock + workspace scan

    let ph = std::time::Instant::now();
    let states = session_states(app);
    let ms_states = ph.elapsed().as_millis(); // agent_sessions read lock

    let now = now_ms();

    let ph = std::time::Instant::now();
    let scrollback = scrollback_times(app);
    let ms_scrollback = ph.elapsed().as_millis(); // scrollback_db mutex + one SQLite query

    let ph = std::time::Instant::now();
    let last_activity = last_activity_ts(app, tab_id, &scrollback, now);
    let ms_activity = ph.elapsed().as_millis(); // locate_jsonl + last-turn tail read
    let (state, runtime, tool) = match states.get(tab_id) {
        Some((st, rt, tool)) => (map_state(*st), runtime_key(*rt), tool.clone()),
        None => ("dormant", runtime_key(meta.runtime), None),
    };

    // Per-turn source markdown from the session transcript (Claude) so the phone's GFM renderer
    // lights up; falls back to the distilled terminal scrape for other runtimes / when no
    // transcript is found. See mailink/transcript.rs.
    let ph = std::time::Instant::now();
    let transcript = build_transcript(app, tab_id, now);
    let ms_transcript = ph.elapsed().as_millis(); // 8 MiB tail read + distill

    let mut detail = json!({
        "tabId": meta.tab_id,
        "title": meta.title,
        "workspace": meta.workspace,
        "runtime": runtime,
        "state": state,
        // Same rule as build_chats: an open AskUserQuestion is unread even if a build leaves
        // the session state at "active".
        "unread": tool.as_deref() == Some("AskUserQuestion")
            || state == "permission"
            || state == "idle",
        "lastActivityTs": last_activity,
        "transcript": transcript,
    });

    // Per-agent telemetry strip (model + context gauge). See build_meta.
    let ph = std::time::Instant::now();
    if let Some(agent_meta) = build_meta(app, tab_id) {
        detail["meta"] = agent_meta;
    }
    let ms_meta = ph.elapsed().as_millis(); // locate_jsonl + meta tail read

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
    // arm of post_respond) is enabled: single-select (incl. Other free-text), multiSelect, and
    // mixed multi-question forms are verified end-to-end against the live TUI; the one remaining
    // combo (multiSelect + Other simultaneously) uses a best-guess gesture pending device
    // validation (docs §12.3).
    if tool.as_deref() == Some("AskUserQuestion") {
        let mut pp = json!({
            // Per-ask id (q_<tab>_<asked_at>) — must agree with current_prompt's stale-guard.
            "prompt_id": question_prompt_id(app, tab_id),
            "thread_id": tab_id,
            "kind": "question",
            "respondable": true,
        });
        match pending_question_for_tab(app, tab_id).as_ref().and_then(map_ask_questions) {
            Some(qs) => { pp["questions"] = qs; }
            None => { pp["text"] = json!("The agent is asking a question — see the terminal for details."); }
        }
        // asked_at is display-only on the phone ("asked 2m ago"). expires_at is AUTHORITATIVE
        // for expiry: present only when this session's CC build + settings actually auto-resolve
        // the ask (see ask_deadline_ms); absent ⇒ the app shows NO countdown and the question is
        // answerable until the prompt clears. The real deadline is sent un-buffered — the app
        // closes the tappable window at expires_at − 5s (keystroke-inject headroom) itself.
        if let Some(at) = pending_question_at_for_tab(app, tab_id) {
            pp["asked_at"] = json!(at);
            if let Some(exp) = question_expires_at(app, tab_id) {
                pp["expires_at"] = json!(exp);
            }
        }
        detail["pendingPrompt"] = pp;
    } else if state == "permission" {
        // A real permission prompt (some other tool, e.g. Bash). Synthesized: the hook carries no
        // structured options; that keystroke respond path is proven, so respondable now. The
        // compact tool_detail (captured from the PreToolUse / Codex PermissionRequest tool_input)
        // shows WHAT is being approved, e.g. "Bash(rm -rf ./dist) — approve?".
        let text = match (tool.as_deref(), tool_detail_for_tab(app, tab_id).as_deref()) {
            (Some(t), Some(d)) => format!("{t}({d}) — approve?"),
            (Some(t), None) => format!("{t} — approve?"),
            _ => "Permission requested".to_string(),
        };
        detail["pendingPrompt"] = json!({
            "prompt_id": format!("p_{tab_id}"),
            "thread_id": tab_id,
            "kind": "permission",
            "respondable": true,
            "text": text,
            "options": ["Yes", "Yes, don't ask again", "No"],
        });
    }

    let ms_total = t_total.elapsed().as_millis();
    if ms_total > SLOW_BUILD_LOG_MS {
        // A slow open is near-always lock-wait, not CPU: whichever phase dominates names the
        // contended lock (tabs=app_data, scrollback=scrollback_db) vs real work (transcript/meta).
        log::warn!(
            "mailink slow chat_detail tab={} total={}ms [tabs(app_data)={} states(sessions)={} scrollback(db)={} activity={} transcript={} meta={}]",
            &tab_id[..tab_id.len().min(8)],
            ms_total,
            ms_tabs,
            ms_states,
            ms_scrollback,
            ms_activity,
            ms_transcript,
            ms_meta,
        );
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
    // tab_id → last observed attn_key. Rings only on an OBSERVED transition into attention:
    // the first sighting of a tab (loop start, roster add) just baselines, so an
    // already-idle tab that gets exposed doesn't push a phantom "finished".
    let mut last: HashMap<String, String> = HashMap::new();
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
            let key = attn_key(
                c["state"].as_str().unwrap_or_default(),
                c["prompt"].as_str(),
            );
            let title = c["title"].as_str().unwrap_or_default().to_string();
            current.insert(tab.clone());
            let prev = last.insert(tab.clone(), key.clone());

            // Fire only on an observed transition into attention, while uncovered. First
            // sighting (prev None) baselines silently — see `last` above.
            if covered {
                continue;
            }
            if prev.as_deref().is_some_and(|p| !is_attn(p)) && is_attn(&key) {
                // Distinguish an open AskUserQuestion (state coincides with "permission") from a
                // real approval prompt so the push line/route matches what the card will show.
                let kind = match current_prompt(&app, &tab) {
                    Some(("question", _, _)) => "question",
                    Some(_) => "permission",
                    None => "idle_done",
                };
                ring_devices(&client, &app, &relay_url, &tab, &title, kind).await;
            }
        }
        last.retain(|k, _| current.contains(k));
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

    /// Drive the real router with a JSON body of `body_len` bytes and report the status.
    /// The token is deliberately wrong, so a body that survives the extractor lands on
    /// `authorize` and returns 401 — nothing is ever injected into a PTY. That makes
    /// 413-vs-401 a clean read on "rejected before the handler" vs "reached the handler".
    async fn post_message_status(body_len: usize) -> StatusCode {
        use tower::ServiceExt as _;

        let api = ApiState {
            app: Arc::new(AppState::new()),
            app_handle: None,
            server_name: "maiTerm".to_string(),
            fingerprint: "sha256/test".to_string(),
            dev_token: "correct-token".to_string(),
        };
        // {"text":"<pad>"} padded to exactly body_len bytes.
        let prefix = br#"{"text":""#;
        let suffix = br#""}"#;
        let pad = body_len - prefix.len() - suffix.len();
        let mut body = Vec::with_capacity(body_len);
        body.extend_from_slice(prefix);
        body.resize(prefix.len() + pad, b'x');
        body.extend_from_slice(suffix);
        assert_eq!(body.len(), body_len);

        let req = axum::http::Request::builder()
            .method("POST")
            .uri("/mailink/v1/chats/no-such-tab/message")
            .header("content-type", "application/json")
            .header("authorization", "Bearer wrong-token")
            .body(axum::body::Body::from(body))
            .unwrap();

        build_router(api).oneshot(req).await.unwrap().status()
    }

    /// Regression: the message route carries base64 images inline, so axum's 2 MiB default
    /// body limit rejected a realistic 6-image batch at the extractor — before the handler
    /// ran, hence no log line and a generic "Failed to send" on the phone. Bodies well past
    /// 2 MiB must now reach the handler.
    #[tokio::test]
    async fn message_route_accepts_bodies_past_axum_default_limit() {
        // Control: comfortably under the old 2 MiB default — always reached the handler.
        assert_eq!(post_message_status(1_500_000).await, StatusCode::UNAUTHORIZED);

        // The regression: these are the sizes that used to 413.
        for len in [3 * 1024 * 1024, 8 * 1024 * 1024] {
            assert_eq!(
                post_message_status(len).await,
                StatusCode::UNAUTHORIZED,
                "{len}-byte body should reach the handler, not 413 at the extractor",
            );
        }

        // The new ceiling still holds: past MAX_MESSAGE_BODY_BYTES we do reject.
        assert_eq!(
            post_message_status(MAX_MESSAGE_BODY_BYTES + 1024).await,
            StatusCode::PAYLOAD_TOO_LARGE,
        );
    }

    /// The other routes have no reason to accept large bodies; they keep the tight default.
    #[tokio::test]
    async fn other_routes_keep_the_default_limit() {
        use tower::ServiceExt as _;

        let api = ApiState {
            app: Arc::new(AppState::new()),
            app_handle: None,
            server_name: "maiTerm".to_string(),
            fingerprint: "sha256/test".to_string(),
            dev_token: "correct-token".to_string(),
        };
        let req = axum::http::Request::builder()
            .method("POST")
            .uri("/mailink/v1/chats/no-such-tab/respond")
            .header("content-type", "application/json")
            .header("authorization", "Bearer wrong-token")
            .body(axum::body::Body::from(vec![b'x'; 3 * 1024 * 1024]))
            .unwrap();
        let status = build_router(api).oneshot(req).await.unwrap().status();
        assert_eq!(status, StatusCode::PAYLOAD_TOO_LARGE);
    }

    #[test]
    fn normalize_tab_title_trims_rejects_empty_and_caps_length() {
        // trims surrounding whitespace
        assert_eq!(normalize_tab_title("  deploy box  ").as_deref(), Some("deploy box"));
        // empty / whitespace-only → None (handler turns this into 400)
        assert_eq!(normalize_tab_title(""), None);
        assert_eq!(normalize_tab_title("   \t\n "), None);
        // capped by CHAR count, and the cap never splits a multibyte code point
        let long = "é".repeat(MAX_TAB_TITLE_CHARS + 40);
        let out = normalize_tab_title(&long).unwrap();
        assert_eq!(out.chars().count(), MAX_TAB_TITLE_CHARS);
        assert!(out.chars().all(|c| c == 'é'), "must not split a multibyte grapheme");
    }

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
    fn attn_key_transitions_cover_question_without_state_change() {
        // Plain states.
        assert!(!is_attn(&attn_key("active", None)));
        assert!(!is_attn(&attn_key("dormant", None)));
        assert!(is_attn(&attn_key("permission", Some("permission"))));
        assert!(is_attn(&attn_key("idle", None)));
        // The case the state-only diff missed: an AskUserQuestion opening while the session
        // state stays "active" (no coincident permission notification).
        assert!(is_attn(&attn_key("active", Some("question"))));
        // And the key CHANGES for that transition, so the tickers see it.
        assert_ne!(attn_key("active", None), attn_key("active", Some("question")));
        // permission → permission+question changes the key but stays attention-worthy
        // (no re-ring for the same underlying ask).
        assert_ne!(
            attn_key("permission", Some("permission")),
            attn_key("permission", Some("question"))
        );
        assert!(is_attn(&attn_key("permission", Some("question"))));
    }

    #[test]
    fn permission_key_is_runtime_specific() {
        use AgentRuntime::*;
        // Claude: fixed numeric menu; bare digits pass through; unknown label → deny (3).
        assert_eq!(permission_key(Claude, "Yes"), "1");
        assert_eq!(permission_key(Claude, "yes, don't ask again"), "2");
        assert_eq!(permission_key(Claude, "No"), "3");
        assert_eq!(permission_key(Claude, "2"), "2");
        assert_eq!(permission_key(Claude, "whatever"), "3");
        // Codex: stable letter shortcuts (y/a/n) — digits are POSITIONAL in codex's
        // variable-length overlay and must never pass through raw.
        assert_eq!(permission_key(Codex, "Yes"), "y");
        assert_eq!(permission_key(Codex, "approve"), "y");
        assert_eq!(permission_key(Codex, "Yes, don't ask again"), "a");
        assert_eq!(permission_key(Codex, "always"), "a");
        assert_eq!(permission_key(Codex, "No"), "n");
        assert_eq!(permission_key(Codex, "1"), "y");
        assert_eq!(permission_key(Codex, "2"), "a");
        assert_eq!(permission_key(Codex, "3"), "n");
        assert_eq!(permission_key(Codex, "5"), "n"); // unknown digit → safe decline
        assert_eq!(permission_key(Codex, "whatever"), "n");
    }

    #[test]
    fn ask_deadline_is_version_and_setting_gated() {
        // The 60s timer existed ONLY in CC 2.1.198–2.1.199.
        assert_eq!(ask_deadline_ms(Some("2.1.197"), None), None);
        assert_eq!(ask_deadline_ms(Some("2.1.198"), None), Some(60_000));
        assert_eq!(ask_deadline_ms(Some("2.1.199"), Some("never")), Some(60_000)); // setting didn't exist yet
        // ≥ 2.1.200: opt-in via askUserQuestionTimeout; default (unset/"never") = no deadline.
        assert_eq!(ask_deadline_ms(Some("2.1.200"), None), None);
        assert_eq!(ask_deadline_ms(Some("2.1.200"), Some("never")), None);
        assert_eq!(ask_deadline_ms(Some("2.1.200"), Some("60s")), Some(60_000));
        assert_eq!(ask_deadline_ms(Some("2.1.201"), Some("5m")), Some(300_000));
        assert_eq!(ask_deadline_ms(Some("2.2.0"), Some("10m")), Some(600_000));
        assert_eq!(ask_deadline_ms(Some("2.2.0-beta1"), Some("garbage")), None);
        // Unknown version ⇒ no deadline (a false countdown expires a live question — worse
        // than a missing one, which degrades to stale-guard + composer fallback).
        assert_eq!(ask_deadline_ms(None, Some("60s")), None);
        assert_eq!(ask_deadline_ms(Some("weird"), Some("60s")), None);
    }

    #[test]
    fn interrupt_settles_only_the_targeted_tabs_running_sessions() {
        use crate::state::app_state::AgentSessionInfo;
        let app = AppState::new();
        let mk = |tab: &str, state: AgentSessionState| AgentSessionInfo {
            runtime: AgentRuntime::Claude,
            tab_id: tab.to_string(),
            cwd: None,
            state,
            tool_name: Some("Bash".into()),
            tool_detail: Some("ls".into()),
            pending_question: Some(json!({ "q": 1 })),
            pending_question_at: Some(123),
            model: None,
            transcript_path: None,
            connection_id: None,
        };
        {
            let mut s = app.agent_sessions.write();
            s.insert("a".into(), mk("tab-1", AgentSessionState::Active));
            s.insert("b".into(), mk("tab-1", AgentSessionState::WaitingInput)); // already idle
            s.insert("c".into(), mk("tab-2", AgentSessionState::Active)); // other tab
        }

        // Interrupting tab-1 settles its running session and clears its tool/question, but leaves
        // the already-idle session and the other tab untouched.
        assert!(settle_tab_interrupt(&app, "tab-1"));
        {
            let s = app.agent_sessions.read();
            assert!(matches!(s["a"].state, AgentSessionState::Stopped));
            assert!(s["a"].tool_name.is_none() && s["a"].tool_detail.is_none());
            assert!(s["a"].pending_question.is_none() && s["a"].pending_question_at.is_none());
            assert!(matches!(s["b"].state, AgentSessionState::WaitingInput));
            assert!(s["b"].tool_name.is_some()); // untouched — we only reset running sessions
            assert!(matches!(s["c"].state, AgentSessionState::Active));
        }

        // Nothing left running on tab-1 → no-op, reported as false (nothing was interrupted).
        assert!(!settle_tab_interrupt(&app, "tab-1"));

        // An open permission gate counts as running and settles too (ESC dismisses it).
        app.agent_sessions
            .write()
            .insert("d".into(), mk("tab-3", AgentSessionState::WaitingPermission));
        assert!(settle_tab_interrupt(&app, "tab-3"));
        assert!(matches!(
            app.agent_sessions.read()["d"].state,
            AgentSessionState::Stopped
        ));
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
