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

pub(crate) mod assets;
pub(crate) mod mirror;
pub(crate) mod models;
pub(crate) mod shells;
pub(crate) mod tasks;
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

/// Heartbeat for a maiLink WS, and the deadline for the answer: one unanswered ping closes it.
///
/// This is a correctness constant, not a hygiene one. A live WS SUPPRESSES the doorbell, on the
/// reasoning that a phone receiving events directly doesn't need a push. That reasoning only holds
/// while "live" is true, and TCP will not tell us: a phone that leaves the network (or whose app
/// iOS suspends) leaves the socket ESTABLISHED, so without a heartbeat the desktop believes a phone
/// is watching when nothing is.
///
/// The consequence was worse than a delay. The doorbell records each attention transition as it
/// observes it, and skips ringing while covered — so a transition that occurred during phantom
/// coverage is consumed, and no push is ever sent for it even after the socket finally errors out
/// minutes later. Agent finishes while the phone is in a pocket → notification lost, silently, in
/// exactly the situation maiLink exists for.
///
/// 20s keeps that window to one interval. Answering costs a phone one frame per 20s; not answering
/// costs it the notification, so the trade is not close.
///
/// The first ping goes out at connect rather than after an interval, and an unanswered ping always
/// closes the socket — there is no "maybe this client can't pong" escape hatch. Verified against
/// both maiLink transports: iOS `URLSessionWebSocketTask` and Android OkHttp `RealWebSocket` both
/// answer at framework level, with control frames never reaching app code.
const WS_PING_INTERVAL: std::time::Duration = std::time::Duration::from_secs(20);

/// Doorbell coverage decision: a phone is receiving events directly (suppress the push) if a WS is
/// live now, OR one disconnected within the grace window (`last_drop_ms == 0` means never dropped).
fn ws_covered(live: bool, last_drop_ms: u64, now_ms: u64) -> bool {
    live || (last_drop_ms != 0 && now_ms.saturating_sub(last_drop_ms) < WS_COVERAGE_GRACE_MS)
}

/// The per-tab attention signature the WS/doorbell tickers diff: chat state PLUS the open-prompt
/// kind (the `prompt` field build_chats computes). Including the prompt kind means an
/// AskUserQuestion that opens without moving `state` (no coincident permission notification)
/// still registers as a transition.
///
/// It does NOT make `permission` → `question` re-fire, and shouldn't: `rings_attention` needs the
/// PREVIOUS key to be outside attention, and `permission|` is already inside it. Both keys are
/// attention, so the key changing is not an edge. That is the wanted behaviour — it is one ask
/// changing shape, and the human was already rung for it (see the `attn_key` test).
fn attn_key(state: &str, prompt: Option<&str>) -> String {
    format!("{state}|{}", prompt.unwrap_or(""))
}

/// Whether an `attn_key` means "the agent wants a human": an open permission/question prompt, or
/// a finished turn waiting on input.
fn is_attn(key: &str) -> bool {
    let (state, prompt) = key.split_once('|').unwrap_or((key, ""));
    state == "permission" || state == "idle" || prompt == "question"
}

/// Whether a tab just crossed INTO attention in a way worth announcing — the shared edge rule for
/// both announcers, the push doorbell and the WS `attention` frame (unit-tested).
///
/// Two things must hold, and the second is the one that cost a push storm. The edge must be an
/// OBSERVED transition into attention, so a first sighting baselines silently. And the tab must
/// already have had a tracked session when we last saw it: a session row APPEARING is a
/// registration edge, not a turn ending.
///
/// A restart walks every tab across that second edge at once. `agent_sessions` is in-memory, so at
/// launch every tab reports `dormant` + `registered:false`; seconds later each agent's SessionStart
/// hook inserts a row, which maps to "idle", which `is_attn` counts as attention. Nothing finished
/// — the roster was merely coming up — but the desktop had been down, so nothing was `covered` and
/// every resumed tab rang "Agent finished".
///
/// Nothing real is suppressed. A genuine finish happens on a tab whose row already existed (the
/// agent had to be running to finish anything), and a compaction writes Active onto an existing
/// row, so it is not a registration edge either. The one case this does swallow is a row dropped
/// and recreated mid-work — which arrives as "the process just came up" and should stay silent.
///
/// Sibling guard: `tab_looks_live_despite_no_session` reports "active" rather than "idle" for the
/// UNregistered case for exactly this reason. This is that same rule for the registered path.
fn rings_attention(prev_key: Option<&str>, prev_registered: bool, key: &str) -> bool {
    prev_key.is_some_and(|p| prev_registered && !is_attn(p)) && is_attn(key)
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
        .route("/mailink/v1/models", get(models_list))
        // Files an agent sent, newest first across every tab — the phone's Files view. Static
        // segment, and `{asset_id}` is a uuid, so neither can shadow the other.
        .route("/mailink/v1/assets", get(assets_list))
        .route("/mailink/v1/assets/{asset_id}", get(asset_bytes))
        .route("/mailink/v1/chats", get(chats_list))
        // Static segment — must be registered before `/chats/{tab_id}` so it isn't shadowed.
        .route("/mailink/v1/chats/archived", get(chats_archived))
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
        .route(
            "/mailink/v1/chats/{tab_id}/shells/{shell_id}/stop",
            post(post_shell_stop),
        )
        .route("/mailink/v1/chats/{tab_id}/wake", post(post_wake))
        .route(
            "/mailink/v1/chats/{tab_id}/queue/cancel",
            post(post_queue_cancel),
        )
        .route("/mailink/v1/chats/{tab_id}/new", post(post_new_conversation))
        .route("/mailink/v1/chats/{tab_id}/rename", post(post_rename))
        .route(
            "/mailink/v1/chats/{tab_id}/resume-workspace",
            post(post_resume_workspace),
        )
        .route(
            "/mailink/v1/chats/{tab_id}/mesh-init",
            post(post_mesh_init),
        )
        .route("/mailink/v1/chats/{tab_id}/archive", post(post_archive))
        .route("/mailink/v1/chats/{tab_id}/close", post(post_close))
        .route("/mailink/v1/chats/{tab_id}/restore", post(post_restore))
        .route("/mailink/v1/ws", get(ws_handler))
        .route("/mailink/v1/pair", post(post_pair))
        .route("/mailink/v1/push-register", post(post_push_register))
        // Outermost, so it sees the final status of every route including the WS upgrade (which
        // authenticates itself and never calls `authorize`).
        .layer(axum::middleware::from_fn_with_state(api.clone(), log_rejections))
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

/// GET /mailink/v1/models — what this machine can switch a Claude tab to.
///
/// Exists so the phone stops hardcoding a list that goes stale on every Claude release. Each entry
/// says where it came from (`source`), because the two sources are not equally trustworthy — see
/// `models.rs`.
async fn models_list(
    State(s): State<ApiState>,
    headers: HeaderMap,
) -> Result<Json<Value>, StatusCode> {
    authorize(&s, &headers)?;
    Ok(Json(json!(models::available())))
}

/// How many assets `GET /assets` returns. The phone's Files view is a browse surface, not an
/// archive; the transcript is where an older file is found in context.
const ASSET_LIST_LIMIT: usize = 200;

/// Bytes per streamed chunk. Large enough that a LAN transfer isn't syscall-bound, small enough
/// that a 1 GB asset never becomes 1 GB of maiTerm RSS.
const ASSET_CHUNK_BYTES: u64 = 256 * 1024;

/// GET /mailink/v1/assets — every file an agent has sent, newest first, across all tabs.
async fn assets_list(
    State(s): State<ApiState>,
    headers: HeaderMap,
) -> Result<Json<Value>, StatusCode> {
    authorize(&s, &headers)?;
    // Filtered to designated tabs. Without this, a file sent from a tab the human had explicitly
    // kept out of maiLink still appeared in the Files view — tagged with a tabId the phone has no
    // chat for. Exposure is a real gate, not a visibility toggle (§ the same rule every tab-scoped
    // endpoint follows), and it has to hold on the cross-chat surface too.
    let designated: std::collections::HashSet<String> =
        designated_tabs(&s.app).into_iter().map(|t| t.tab_id).collect();
    let visible: Vec<assets::AssetRecord> = assets::list(ASSET_LIST_LIMIT)
        .into_iter()
        .filter(|a| designated.contains(&a.tab_id))
        .collect();
    Ok(Json(json!(visible)))
}

/// What a `Range` header asks for, resolved against a known length (unit-tested).
#[derive(Debug, PartialEq)]
enum RangeAsk {
    /// No range, or a form we don't implement — answer 200 with the whole file, which is always
    /// a legal response to a range request.
    Whole,
    /// Inclusive byte offsets, both within the file.
    Part(u64, u64),
    /// Syntactically a byte range, but off the end of the file — 416, never a silent full body.
    /// Answering 200 here corrupts a resume: the client writes file-start bytes at its offset.
    Unsatisfiable,
}

/// Parse a single `bytes=` range. Multi-range (`bytes=0-9,20-29`) is deliberately `Whole`: a
/// multipart/byteranges body is a lot of machinery for something no client here sends, and a full
/// body is a correct answer to any range request.
fn parse_range(header: &str, len: u64) -> RangeAsk {
    let Some(spec) = header.trim().strip_prefix("bytes=") else { return RangeAsk::Whole };
    if spec.contains(',') {
        return RangeAsk::Whole;
    }
    let Some((from, to)) = spec.split_once('-') else { return RangeAsk::Whole };
    let (from, to) = (from.trim(), to.trim());
    if len == 0 {
        return RangeAsk::Unsatisfiable;
    }
    match (from.is_empty(), to.is_empty()) {
        // `bytes=-N` — the LAST n bytes. n > len is not an error; it means the whole file.
        (true, false) => match to.parse::<u64>() {
            Ok(0) => RangeAsk::Unsatisfiable,
            Ok(n) => RangeAsk::Part(len.saturating_sub(n), len - 1),
            Err(_) => RangeAsk::Whole,
        },
        // `bytes=N-` — from n to the end.
        (false, true) => match from.parse::<u64>() {
            Ok(n) if n < len => RangeAsk::Part(n, len - 1),
            Ok(_) => RangeAsk::Unsatisfiable,
            Err(_) => RangeAsk::Whole,
        },
        // `bytes=N-M` — inclusive, clamped to the end (a client may ask past it).
        (false, false) => match (from.parse::<u64>(), to.parse::<u64>()) {
            (Ok(a), Ok(b)) if a < len && a <= b => RangeAsk::Part(a, b.min(len - 1)),
            (Ok(_), Ok(_)) => RangeAsk::Unsatisfiable,
            _ => RangeAsk::Whole,
        },
        (true, true) => RangeAsk::Whole,
    }
}

/// `Content-Disposition` for a downloaded file. The plain `filename=` is ASCII-only and quoted;
/// `filename*=` carries the real name for anything else, so a file called `résumé.pdf` saves under
/// its own name rather than a mangled one.
fn content_disposition(name: &str) -> String {
    let safe: String = name
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() || " ._-()[]".contains(c) { c } else { '_' })
        .collect();
    let encoded: String = name
        .bytes()
        .map(|b| {
            if b.is_ascii_alphanumeric() || b"._-~".contains(&b) {
                (b as char).to_string()
            } else {
                format!("%{b:02X}")
            }
        })
        .collect();
    format!("attachment; filename=\"{safe}\"; filename*=UTF-8''{encoded}")
}

/// GET /mailink/v1/assets/{assetId} — the bytes.
///
/// Streamed in chunks rather than read into memory: the per-file cap is 1 GB, and a phone pulling
/// one must not cost maiTerm the same. Honours `Range`, which matters less than first thought —
/// the phone plays from a downloaded copy in its own container, not from this URL — but is what
/// lets an interrupted 1 GB download resume instead of restarting.
async fn asset_bytes(
    State(s): State<ApiState>,
    headers: HeaderMap,
    Path(asset_id): Path<String>,
) -> Result<Response, StatusCode> {
    authorize(&s, &headers)?;
    // 404 covers both "no such asset" and "evicted": the phone should never be discovering
    // availability here, because the descriptor already told it (`available`).
    let (record, path) = assets::resolve(&asset_id).ok_or(StatusCode::NOT_FOUND)?;
    let len = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(record.bytes);

    let ask = headers
        .get(header::RANGE)
        .and_then(|v| v.to_str().ok())
        .map(|h| parse_range(h, len))
        .unwrap_or(RangeAsk::Whole);

    let (status, start, end) = match ask {
        RangeAsk::Whole => (StatusCode::OK, 0, len.saturating_sub(1)),
        RangeAsk::Part(a, b) => (StatusCode::PARTIAL_CONTENT, a, b),
        RangeAsk::Unsatisfiable => {
            return Ok((
                StatusCode::RANGE_NOT_SATISFIABLE,
                [(header::CONTENT_RANGE, format!("bytes */{len}"))],
            )
                .into_response())
        }
    };

    let mut file = tokio::fs::File::open(&path).await.map_err(|e| {
        log::warn!("[maiLink] asset {asset_id} unreadable: {e}");
        StatusCode::NOT_FOUND
    })?;
    if start > 0 {
        use tokio::io::AsyncSeekExt;
        file.seek(std::io::SeekFrom::Start(start))
            .await
            .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    }
    let count = end.saturating_sub(start).saturating_add(1).min(len);

    let stream = futures_util::stream::try_unfold(
        (file, count),
        |(mut file, remaining)| async move {
            use tokio::io::AsyncReadExt;
            if remaining == 0 {
                return Ok::<_, std::io::Error>(None);
            }
            let want = remaining.min(ASSET_CHUNK_BYTES) as usize;
            let mut buf = vec![0u8; want];
            let read = file.read(&mut buf).await?;
            if read == 0 {
                return Ok(None); // truncated under us — end cleanly rather than hang
            }
            buf.truncate(read);
            Ok(Some((buf, (file, remaining - read as u64))))
        },
    );

    let mut resp = Response::builder()
        .status(status)
        .header(header::CONTENT_TYPE, record.mime.clone())
        .header(header::CONTENT_LENGTH, count.to_string())
        .header(header::ACCEPT_RANGES, "bytes")
        .header(header::CONTENT_DISPOSITION, content_disposition(&record.name))
        .body(axum::body::Body::from_stream(stream))
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    if status == StatusCode::PARTIAL_CONTENT {
        if let Ok(v) = format!("bytes {start}-{end}/{len}").parse() {
            resp.headers_mut().insert(header::CONTENT_RANGE, v);
        }
    }
    Ok(resp)
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

    // Auto-wake. An unregistered tab isn't merely untracked — if its agent EXITED, the PTY is a
    // bash prompt and injecting the message would run it as a shell command. So bring the tab
    // back to a state where typing is safe before typing (no-op for a registered tab, which is
    // the whole point: never touch a live, possibly-mid-turn agent).
    let woke = match wake_tab(&s, &tab_id).await {
        Wake::AlreadyRegistered => None,
        Wake::Woke(action) => Some(action),
        Wake::Unreachable(reason) => {
            return Ok(Json(
                json!({ "status": "unreachable", "reason": reason, "detail": wake_detail(reason) }),
            ))
        }
    };

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
        return Ok(Json(
            json!({ "status": "delivered", "msg_id": format!("m_{}", now_ms()), "woke": woke }),
        ));
    }

    inject_text(&s.app, &pty, &body.text, body.submit)
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    Ok(Json(
        json!({ "status": "delivered", "msg_id": format!("m_{}", now_ms()), "woke": woke }),
    ))
}

/// How long a wake holds its caller. A local `claude --resume` is usually up inside 10 s; an
/// ssh hop plus a remote agent boot can take 20–30 s. Past this we stop waiting and say so —
/// the wake itself keeps going, so a retry a few seconds later normally delivers instantly.
const WAKE_BUDGET_MS: u64 = 45_000;
/// Registration poll interval. Cheap — an in-memory `agent_sessions` scan, no process probe.
const WAKE_POLL_MS: u64 = 400;

/// Outcome of a wake attempt (see `wake_tab`).
enum Wake {
    /// A tracked session already exists — nothing was done, nothing needed doing.
    AlreadyRegistered,
    /// The tab is now reachable. Carries the remedy that was applied, for the wire.
    Woke(&'static str),
    /// The tab could not be made safe to type into. Carries the wire `reason`.
    Unreachable(&'static str),
}

/// Human-readable companion to an `unreachable` reason. The phone renders these verbatim, so
/// they must read as a state the user can act on — never as an instruction to go to the desktop.
fn wake_detail(reason: &str) -> &'static str {
    match reason {
        "no-pty" => "This tab has no live terminal — resume its workspace first.",
        "no-agent" => "This tab's agent has exited and it has no resume command to restart it.",
        _ => "The agent is still starting up — try again in a moment.",
    }
}

/// Which remedy an unregistered tab needs (unit-tested; `wake_tab` wires the real probes).
///
/// A live agent ALWAYS takes `init`, even when the tab also has a resume command — replaying
/// ssh+resume into a running agent injects junk and nests ssh, and no amount of "it also has a
/// resume stored" makes that the right move. Only a tab whose agent is gone gets `resume`.
fn wake_remedy(agent_up: bool, has_resume: bool) -> Result<&'static str, &'static str> {
    if agent_up {
        Ok("init")
    } else if has_resume {
        Ok("resume")
    } else {
        Err("no-agent")
    }
}

/// Bring an unregistered tab back to a state where a remote message can land in its agent.
///
/// The remedy depends on whether the agent PROCESS is still alive, and getting it wrong is
/// destructive both ways — an ssh+resume replay typed into a running agent injects junk and
/// nests ssh, `/maiterm init` typed at a bare shell just runs as a command — so the choice is
/// made here, from a process-tree probe, and the frontend only executes it (`mailink-wake-tab`
/// → `wakeTab`, which owns the PTY and the dialog-safe delivery).
///
/// Four rules, in order:
///  1. A registered tab is never touched. It may be mid-turn, and typing into a running turn is
///     the one thing guaranteed to corrupt it. This is exactly the `registered:false` condition
///     the phone already renders.
///  2. No live PTY ⇒ unreachable. A suspended workspace has to be resumed first; there is no
///     terminal to type into.
///  3. Agent gone with no auto-resume ⇒ unreachable. There is nothing to restart, and typing
///     into the shell is the bug we're here to prevent.
///  4. Past the budget, fall back to the process probe. Registration is the goal, but delivery
///     only needs a live agent — and for a runtime that never registers, that's the only signal
///     there is. A live agent is safe to type into whether or not it registered.
async fn wake_tab(s: &ApiState, tab_id: &str) -> Wake {
    if tab_registered(&s.app, tab_id) {
        return Wake::AlreadyRegistered;
    }
    let Some(pty) = pty_for_tab(&s.app, tab_id) else {
        return Wake::Unreachable("no-pty");
    };
    let action = match wake_remedy(
        agent_is_up(&s.app, &pty).await,
        tab_has_resume(&s.app, tab_id),
    ) {
        Ok(action) => action,
        Err(reason) => return Wake::Unreachable(reason),
    };
    let Some(h) = s.app_handle.as_ref() else {
        return Wake::Unreachable("no-pty");
    };
    // tab ids are app-unique — the owning window acts, every other window finds no instance.
    // budgetMs travels with the event so the frontend's settle-wait can't outlive our wait and
    // paste `/maiterm init` on top of the message we deliver once the budget is spent.
    let _ = h.emit(
        "mailink-wake-tab",
        json!({ "tabId": tab_id, "action": action, "budgetMs": WAKE_BUDGET_MS }),
    );
    log::info!("[maiLink] waking tab {tab_id} via {action}");
    let deadline = std::time::Instant::now() + std::time::Duration::from_millis(WAKE_BUDGET_MS);
    while std::time::Instant::now() < deadline {
        tokio::time::sleep(std::time::Duration::from_millis(WAKE_POLL_MS)).await;
        if tab_registered(&s.app, tab_id) {
            return Wake::Woke(action);
        }
    }
    if agent_is_up(&s.app, &pty).await {
        log::info!("[maiLink] tab {tab_id} woke but never registered — delivering anyway");
        return Wake::Woke(action);
    }
    Wake::Unreachable("wake-timeout")
}

/// POST /chats/{tabId}/wake — the per-tab Initialize the phone offers on `registered:false`.
/// Same machinery and same rules as the auto-wake on send, exposed on its own because
/// `mesh-init` is workspace-scoped AND mesh-only: a lone unregistered tab had no affordance.
/// Always 200 (`404` only if the tab isn't maiLink-available); `woke` is null when nothing was
/// done, with `reason` saying why.
async fn post_wake(
    State(s): State<ApiState>,
    headers: HeaderMap,
    Path(tab_id): Path<String>,
) -> Result<Json<Value>, StatusCode> {
    authorize(&s, &headers)?;
    if !is_designated(&s.app, &tab_id) {
        return Err(StatusCode::NOT_FOUND);
    }
    Ok(Json(match wake_tab(&s, &tab_id).await {
        Wake::AlreadyRegistered => {
            json!({ "ok": true, "woke": null, "reason": "already-registered" })
        }
        Wake::Woke(action) => json!({ "ok": true, "woke": action }),
        Wake::Unreachable(reason) => {
            json!({ "ok": true, "woke": null, "reason": reason, "detail": wake_detail(reason) })
        }
    }))
}

/// Whether a tab has a tracked agent session — the same predicate that produces the wire's
/// `registered` flag, so "the phone shows Initialize" and "a wake will fire" can't disagree.
fn tab_registered(app: &AppState, tab_id: &str) -> bool {
    app.agent_sessions
        .read()
        .values()
        .any(|s| s.tab_id == tab_id)
}

/// Whether the tab has something to replay when its agent is gone. Mirrors what
/// `replayAutoResume` will actually act on — either arm alone is enough (an ssh command with no
/// agent command still puts the tab back on the remote host).
fn tab_has_resume(app: &AppState, tab_id: &str) -> bool {
    let data = app.app_data.read();
    data.windows
        .iter()
        .flat_map(|w| &w.workspaces)
        .flat_map(|ws| &ws.panes)
        .flat_map(|p| &p.tabs)
        .find(|t| t.id == tab_id)
        .is_some_and(|t| t.auto_resume_command.is_some() || t.auto_resume_ssh_command.is_some())
}

/// Is an agent CLI still alive in this PTY? `ssh_foreground` stands in for a REMOTE agent, whose
/// process isn't in the local tree. Spawns `ps`, so it runs on the blocking pool and is called
/// twice per wake at most — never on a poll path (see the mesh-liveness pinwheel).
async fn agent_is_up(app: &Arc<AppState>, pty_id: &str) -> bool {
    let app = app.clone();
    let pty_id = pty_id.to_string();
    tauri::async_runtime::spawn_blocking(move || crate::pty::get_agent_liveness(&app, &pty_id))
        .await
        .ok()
        .and_then(|r| r.ok())
        .is_some_and(|l| l.agent_running || l.ssh_foreground)
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
pub(crate) struct Answer {
    #[serde(default)]
    pub(crate) selected: Vec<String>,
    #[serde(default)]
    pub(crate) other: Option<String>,
}

/// What is currently blocking a tab, if anything — the read half of the prompt surface.
///
/// Distinguishes the two kinds, which look alike in the agent-state machine but are not the
/// same decision: `permission` is a tool gate ("allow this Bash command?"), `question` is an
/// AskUserQuestion the agent raised. Overlord needs the difference to judge whether it may
/// answer or must escalate, so the caller gets the tool being approved (permission) or the
/// structured questions and options (question) rather than just a state word.
pub(crate) fn tab_prompt_view(app: &AppState, tab_id: &str) -> Option<Value> {
    let (kind, prompt_id, runtime) = current_prompt(app, tab_id)?;
    let mut v = json!({ "kind": kind, "prompt_id": prompt_id, "runtime": runtime.as_key() });
    if kind == "question" {
        if let Some(t) = pending_question_for_tab(app, tab_id) {
            if let Some(q) = t.get("questions") {
                v["questions"] = q.clone();
            }
        }
        if let Some(at) = pending_question_at_for_tab(app, tab_id) {
            v["asked_at"] = json!(at);
        }
    } else {
        let sessions = app.agent_sessions.read();
        if let Some((_, s)) = sessions
            .iter()
            .filter(|(_, s)| s.tab_id == tab_id)
            .max_by_key(|(_, s)| rank(s.state))
        {
            if let Some(t) = &s.tool_name {
                v["tool"] = json!(t);
            }
            if let Some(d) = &s.tool_detail {
                v["detail"] = json!(d);
            }
        }
    }
    Some(v)
}

/// Answer a tab's currently-open prompt.
///
/// Shared by the phone's `POST /chats/{id}/respond` and by Overlord, which supervises the
/// same tabs and hits exactly the same fragile TUI affordances. One implementation on
/// purpose: the runtime-specific keymaps, the one-shot selector guard and the
/// did-it-actually-submit check are all hard-won, and a second copy would drift.
///
/// `prompt_id` is the stale-guard — a mismatch refuses rather than injecting a keystroke
/// into whatever prompt happens to be open NOW.
pub(crate) async fn respond_to_prompt(
    app: &Arc<AppState>,
    tab_id: &str,
    prompt_id: Option<&str>,
    choice: Option<&str>,
    answers: Option<&[Answer]>,
) -> Value {
    let Some((kind, cur_id, runtime)) = current_prompt(app, tab_id) else {
        return json!({ "ok": false, "reason": "stale" });
    };
    if let Some(pid) = prompt_id {
        if pid != cur_id {
            return json!({ "ok": false, "reason": "stale" });
        }
    }
    let Some(pty) = pty_for_tab(app, tab_id) else {
        return json!({ "ok": false, "reason": "no_pty" });
    };
    match kind {
        // permission menu: a single keystroke selects the option (no bracketed paste);
        // the key is runtime-specific — see permission_key.
        "permission" => {
            let key = permission_key(runtime, choice.unwrap_or(""));
            if crate::pty::write_pty(app, &pty, key.as_bytes()).is_err() {
                return json!({ "ok": false, "reason": "inject_failed" });
            }
        }
        // AskUserQuestion: replay per-question answers into the open selector.
        "question" => {
            let Some(tool_input) = pending_question_for_tab(app, tab_id) else {
                return json!({ "ok": false, "reason": "stale" });
            };
            let answers = match answers {
                Some(a) if !a.is_empty() => a,
                _ => return json!({ "ok": false, "reason": "bad_request" }),
            };
            // A SECOND attempt at the same ask is refused, and this is the important guard.
            // Navigation is relative and assumes the highlight starts at row 0, true only for
            // an untouched selector. After a failed attempt the highlight is wherever the
            // keystrokes left it, and it cannot be re-homed (the selector unbinds ↑/↓ while
            // the free-text row holds focus). A retry walks from an unknown origin, and the
            // row it lands on decides what the operator is recorded as having said.
            if !claim_question_inject(tab_id, &cur_id) {
                log::warn!("[maiLink] refusing a second injection into ask {cur_id} (tab {tab_id}): selector position is unknown after the first attempt");
                return json!({ "ok": false, "reason": "selector_dirty",
                    "detail": "this ask was already injected once; its selector position is now unknown, so send the answer as a message instead" });
            }
            if let Err(e) = drive_question_answers(app, &pty, &tool_input, answers).await {
                log::warn!("[maiLink] AskUserQuestion answer injection failed: {e}");
                return json!({ "ok": false, "reason": "inject_failed", "detail": e });
            }
            // Confirm the selector actually submitted before claiming success: the PostToolUse
            // hook clears pending_question when the ask resolves, so if it is still open after
            // a grace window the keystrokes didn't drive it to Submit.
            let mut submitted = false;
            for _ in 0..20 {
                tokio::time::sleep(std::time::Duration::from_millis(100)).await;
                if pending_question_for_tab(app, tab_id).is_none() {
                    submitted = true;
                    break;
                }
            }
            if submitted {
                let used_other = answers.iter().any(|a| a.other.as_deref().is_some_and(|t| !t.trim().is_empty()));
                log::info!("[maiLink] AskUserQuestion answered (tab {tab_id}, {} question(s){})",
                    answers.len(), if used_other { ", via the Other free-text row" } else { "" });
            } else {
                log::warn!("[maiLink] AskUserQuestion still open ~2s after inject — reporting inject_failed (tab {tab_id})");
                return json!({ "ok": false, "reason": "inject_failed",
                    "detail": "selector still open after injection" });
            }
        }
        // free-text fallback: treat choice as a plain message → paste + submit
        _ => {
            if let Some(c) = choice {
                if inject_text(app, &pty, c, true).await.is_err() {
                    return json!({ "ok": false, "reason": "inject_failed" });
                }
            }
        }
    }
    json!({ "ok": true })
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
    // Order matters and is the pre-extraction order: "no prompt open" wins over "no PTY".
    // The phone routinely taps a cached permission card whose tab has since lost its agent
    // (suspended workspace, exited session) — that must stay a graceful 200 `stale` body,
    // not a 409 the app has no branch for.
    if current_prompt(&s.app, &tab_id).is_none() {
        return Ok(Json(json!({ "ok": false, "reason": "stale" })));
    }
    // The tab must still have a PTY — kept here so the phone's HTTP contract still answers
    // 409 for that case rather than the shared function's `{ok:false}`.
    pty_for_tab(&s.app, &tab_id).ok_or(StatusCode::CONFLICT)?;
    Ok(Json(
        respond_to_prompt(
            &s.app,
            &tab_id,
            body.prompt_id.as_deref(),
            body.choice.as_deref(),
            body.answers.as_deref(),
        )
        .await,
    ))
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

    // Cancelling a running turn makes Claude Code RESTORE that turn's prompt into the composer so
    // a human can edit and resubmit it. Our injects are a bracketed paste APPENDED to whatever the
    // composer holds, so the next message from the phone lands on the restored text and the CR
    // submits both as ONE concatenated prompt (observed: "what's that show running?what's that
    // shell running?" as a single user turn). Clear the composer so a remote stop leaves it empty.
    //
    // Only when `settled` — i.e. we actually cancelled a running turn, which is exactly when the
    // restore happens. An already-idle tab is left alone, so a phone Stop can never wipe a draft
    // the desktop operator is typing at an idle prompt. (With a turn running, a draft could still
    // be lost — but today it would instead be merged into the next phone message and submitted,
    // which is worse.) Desktop-side ESC is deliberately untouched: the operator SEES the restored
    // text and can edit it; only a blind remote send has the merge problem.
    // Logged because `settled` is the whole gate, and when a merge is reported afterwards this
    // line is what says whether the gate was even reached. `registered` distinguishes the two
    // ways it can read false: a genuinely idle tab, versus a tab with no tracked session at all
    // (per-turn hooks only mutate an existing row, so an unregistered tab can never go Active and
    // can never settle). Without this, diagnosing a merge means guessing between them.
    log::info!(
        "[maiLink] interrupt {tab_id}: settled={settled} registered={}",
        tab_registered(&s.app, &tab_id)
    );
    let cleared = if settled {
        clear_composer_after_cancel(&s.app, &pty).await
    } else {
        false
    };
    Ok(Json(json!({ "ok": true, "settled": settled, "composerCleared": cleared })))
}

/// Send the composer clear once the cancel's repaint has landed and finished.
///
/// Two phases, both bounded. First wait for the ESC to actually produce output — that repaint IS
/// the restore, and clearing before it arrives is the bug this exists to fix. Then wait for the
/// output to stop, so the clear isn't swallowed mid-redraw. No output at all within the cap means
/// nothing was cancelled and nothing was restored, so we leave the composer alone rather than
/// wipe whatever is in it.
///
/// Returns whether the clear was sent, which the caller reports as `composerCleared` — without it
/// a merge report can't distinguish "we never cleared" from "we cleared and it didn't take".
async fn clear_composer_after_cancel(app: &Arc<AppState>, pty: &str) -> bool {
    let before = crate::pty::last_output_ms(app, pty);
    let start = std::time::Instant::now();
    let cap = std::time::Duration::from_millis(COMPOSER_SETTLE_CAP_MS);
    let poll = std::time::Duration::from_millis(COMPOSER_POLL_MS);

    let mut repainted = false;
    while start.elapsed() < cap {
        tokio::time::sleep(poll).await;
        if crate::pty::last_output_ms(app, pty) != before {
            repainted = true;
            break;
        }
    }
    if !repainted {
        log::info!("[maiLink] interrupt: no repaint after ESC — leaving the composer untouched");
        return false;
    }
    while start.elapsed() < cap {
        tokio::time::sleep(poll).await;
        let quiet_for = crate::pty::last_output_ms(app, pty)
            .map_or(u64::MAX, |last| now_ms().saturating_sub(last));
        if quiet_for >= COMPOSER_QUIET_MS {
            break;
        }
    }
    crate::pty::write_pty(app, pty, COMPOSER_CLEAR).is_ok()
}

/// Tail scanned for the pending input queue. Queue traffic sits near the end of the file, and an
/// entry older than this window has almost certainly been consumed already.
const QUEUE_SCAN_BYTES: u64 = 2 * 1024 * 1024;

/// Cursor-up. In Claude Code's chat input this recalls the input queue when one exists (falling
/// back to prompt history when it doesn't) — see `post_queue_cancel` for why that matters.
const QUEUE_RECALL: &[u8] = b"\x1b[A";
/// How long to wait for the recall to show up in the transcript as the queue emptying.
const QUEUE_RECALL_WAIT_MS: u64 = 2500;
const QUEUE_RECALL_POLL_MS: u64 = 150;

/// POST /chats/{tabId}/queue/cancel — pull back the message waiting in this tab's input queue,
/// before the agent gets to it.
///
/// **Only when EXACTLY ONE message is queued**, and that isn't caution — it's the contract.
/// Reading Claude Code 2.1.220: cursor-up in the chat input calls `popAllEditable`, which recalls
/// the WHOLE queue into the composer as one blob. There is a per-message variant
/// (`popEditableAt`, driven by a `queueEditIndex` the arrow keys move), but it's behind
/// `CLAUDE_CODE_KB_COHESION_FIXES`, which is unset by default. So with two messages queued, one
/// cursor-up recalls BOTH, and clearing the composer would silently destroy the one the user
/// didn't choose. With exactly one, "pop all" and "pop that one" are the same operation.
///
/// It also verifies rather than assumes. We do not depend on knowing which recall variant is
/// live, or on a keystroke landing: after the cursor-up we watch the transcript for the queue to
/// actually empty, and the composer is only cleared once it has. If the recall didn't happen the
/// composer is left untouched and the caller is told `cancelled:false` — never a false success,
/// and never a clear that could wipe something we didn't put there.
async fn post_queue_cancel(
    State(s): State<ApiState>,
    headers: HeaderMap,
    Path(tab_id): Path<String>,
) -> Result<Json<Value>, StatusCode> {
    authorize(&s, &headers)?;
    if !is_designated(&s.app, &tab_id) {
        return Err(StatusCode::NOT_FOUND);
    }
    // Typing into a tab whose agent isn't there is the failure mode the wake work exists to
    // prevent; here there's nothing to cancel anyway, so refuse rather than wake.
    if !tab_registered(&s.app, &tab_id) {
        return Ok(Json(
            json!({ "ok": true, "cancelled": false, "reason": "not-registered" }),
        ));
    }
    let pty = pty_for_tab(&s.app, &tab_id).ok_or(StatusCode::CONFLICT)?;
    let Some((AgentRuntime::Claude, sid)) = resolved_session_for_tab(&s.app, &tab_id) else {
        return Ok(Json(
            json!({ "ok": true, "cancelled": false, "reason": "unsupported_runtime" }),
        ));
    };

    let queued = transcript::pending_queue(&sid, QUEUE_SCAN_BYTES);
    let text = match queued.len() {
        1 => queued[0].0.clone(),
        // Consumed between the phone rendering the affordance and the tap landing. Expected, not
        // an error — the message is already a real turn by now.
        0 => {
            return Ok(Json(
                json!({ "ok": true, "cancelled": false, "reason": "already-consumed" }),
            ))
        }
        n => {
            return Ok(Json(json!({ "ok": true, "cancelled": false,
                "reason": "multiple-queued", "queuedCount": n })))
        }
    };

    send_key(&s.app, &pty, QUEUE_RECALL, 0)
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    // The recall is observable: emptying the queue writes a `popAll` op, which the replay reads.
    let deadline =
        std::time::Instant::now() + std::time::Duration::from_millis(QUEUE_RECALL_WAIT_MS);
    let mut recalled = false;
    while std::time::Instant::now() < deadline {
        tokio::time::sleep(std::time::Duration::from_millis(QUEUE_RECALL_POLL_MS)).await;
        if transcript::pending_queue(&sid, QUEUE_SCAN_BYTES).is_empty() {
            recalled = true;
            break;
        }
    }
    if !recalled {
        log::info!("[maiLink] queue cancel on {tab_id}: queue never emptied — composer untouched");
        return Ok(Json(
            json!({ "ok": true, "cancelled": false, "reason": "not-recalled" }),
        ));
    }

    // It's out of the queue and sitting in the composer now; clearing is what makes it cancelled
    // rather than merely deferred. Same settle-then-clear the interrupt uses, for the same reason.
    let cleared = clear_composer_after_cancel(&s.app, &pty).await;
    Ok(Json(
        json!({ "ok": true, "cancelled": true, "text": text, "composerCleared": cleared }),
    ))
}

/// A tab's background-shell roster (mailink/shells.rs), or empty when it has none / can't be
/// determined. Claude + LOCAL only: an SSH tab's background shells are the REMOTE host's
/// processes, invisible to the local process table, so their liveness can't be confirmed and a
/// Stop button couldn't signal them — a roster we can't stand behind is worse than none, so SSH
/// tabs report nothing (the phone renders no strip).
fn shell_roster(app: &AppState, tab_id: &str) -> Vec<shells::AgentShell> {
    let Some((AgentRuntime::Claude, sid)) = resolved_session_for_tab(app, tab_id) else {
        return Vec::new();
    };
    let Some(pty) = pty_for_tab(app, tab_id) else { return Vec::new() };
    if tab_is_ssh(app, tab_id) {
        return Vec::new();
    }
    shells::roster(&sid, crate::pty::manager::pty_child_pid_of(app, &pty)).unwrap_or_default()
}

/// Whether the tab rides a live SSH/mosh session (its agent runs on another host).
fn tab_is_ssh(app: &AppState, tab_id: &str) -> bool {
    let tunnels = app.ssh_tunnels.read();
    tunnels.values().any(|t| t.tab_ids.contains(tab_id))
}

/// POST /chats/{tabId}/new — start a NEW conversation from this one.
///
/// A LIGHT clone: the source tab is a template for WHERE to run (SSH host + cwd) and nothing
/// else — the new tab gets a fresh agent session, not a copy of this one. Deliberately not the
/// duplicate path, which copies the session-id variable (right for reload/`/branch`, exactly
/// wrong here — the copy would re-render the same conversation instead of starting one).
///
/// The source is never touched and needn't be idle. Creation happens in the owning window (the
/// Svelte store owns tab state), so this emits and then waits for the new tab to appear, and
/// returns its id only once the phone can actually navigate to it.
async fn post_new_conversation(
    State(s): State<ApiState>,
    headers: HeaderMap,
    Path(tab_id): Path<String>,
) -> Result<Json<Value>, StatusCode> {
    authorize(&s, &headers)?;
    if !is_designated(&s.app, &tab_id) {
        return Err(StatusCode::NOT_FOUND);
    }
    // Snapshot the owning pane's tabs so the newcomer can be identified by difference — the id is
    // minted by createTab in the frontend, so there's nothing to pass in.
    let before = sibling_tab_ids(&s.app, &tab_id);
    let h = s.app_handle.as_ref().ok_or(StatusCode::INTERNAL_SERVER_ERROR)?;
    let _ = h.emit("mailink-new-conversation", json!({ "tabId": tab_id }));

    // Spawning involves a round trip through the store and a PTY create; poll briefly rather than
    // returning an id the phone would 404 on. Failure here is a timeout, not an error state — the
    // tab may still appear a moment later and the roster will pick it up via chats_changed.
    //
    // Existing in state is NOT the readiness condition: a tab record with no runtime isn't
    // designated, so GET /chats/{id} 404s on it. Wait for designated — the same predicate the
    // phone's first read will apply — so the id we hand back is addressable when it arrives.
    for _ in 0..NEW_TAB_WAIT_TICKS {
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        let new_id = sibling_tab_ids(&s.app, &tab_id)
            .into_iter()
            .find(|id| !before.contains(id));
        if let Some(new_id) = new_id {
            if is_designated(&s.app, &new_id) {
                return Ok(Json(json!({ "ok": true, "tabId": new_id })));
            }
        }
    }
    log::warn!("[maiLink] new conversation from {tab_id}: no new tab appeared within the wait window");
    Ok(Json(json!({ "ok": false, "reason": "timeout" })))
}

/// How long `post_new_conversation` waits for the frontend to mint the tab (ticks × 100 ms).
const NEW_TAB_WAIT_TICKS: u32 = 50;

/// Every tab id in the pane that owns `tab_id` (including it). Used to spot a newly created tab.
fn sibling_tab_ids(app: &AppState, tab_id: &str) -> Vec<String> {
    let data = app.app_data.read();
    for win in &data.windows {
        for ws in &win.workspaces {
            for pane in &ws.panes {
                if pane.tabs.iter().any(|t| t.id == tab_id) {
                    return pane.tabs.iter().map(|t| t.id.clone()).collect();
                }
            }
        }
    }
    Vec::new()
}

/// POST /chats/{tabId}/shells/{shellId}/stop — terminate one background shell.
///
/// IDEMPOTENT by design: a shell that has already exited (or was never live) returns `ok:true`
/// rather than 404. The phone's roster is up to ~2 s stale, so "stop something that just finished"
/// is the normal race, not a client error — and reporting failure would only invite a retry loop.
/// Signals the shell's own pid (SIGTERM, then SIGKILL if it lingers): a real signal to the process
/// rather than driving the TUI's `/bashes` picker blind. 404 only for an unknown/undesignated tab.
async fn post_shell_stop(
    State(s): State<ApiState>,
    headers: HeaderMap,
    Path((tab_id, shell_id)): Path<(String, String)>,
) -> Result<Json<Value>, StatusCode> {
    authorize(&s, &headers)?;
    if !is_designated(&s.app, &tab_id) {
        return Err(StatusCode::NOT_FOUND);
    }
    let target = shell_roster(&s.app, &tab_id)
        .into_iter()
        .find(|sh| sh.id == shell_id)
        .and_then(|sh| sh.pid);
    let Some(pid) = target else {
        // Unknown id, or known but no longer running — nothing to signal, and that IS the
        // requested end state.
        return Ok(Json(json!({ "ok": true, "stopped": false })));
    };
    let stopped = kill_pid(pid).await;
    Ok(Json(json!({ "ok": true, "stopped": stopped })))
}

/// SIGTERM a pid, escalating to SIGKILL if it's still there shortly after. Returns whether the
/// process is gone by the end. Unix-only signalling; a no-op elsewhere (background shells are a
/// unix construct here).
async fn kill_pid(pid: u32) -> bool {
    #[cfg(unix)]
    {
        let alive = || {
            // signal 0 probes existence without delivering anything.
            unsafe { libc::kill(pid as libc::pid_t, 0) == 0 }
        };
        unsafe { libc::kill(pid as libc::pid_t, libc::SIGTERM) };
        for _ in 0..10 {
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
            if !alive() {
                return true;
            }
        }
        unsafe { libc::kill(pid as libc::pid_t, libc::SIGKILL) };
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        !alive()
    }
    #[cfg(not(unix))]
    {
        let _ = pid;
        false
    }
}

/// Ctrl+L — Claude Code's `chat:clearInput` binding (its Chat keymap is
/// `{escape:"chat:cancel", "ctrl+l":"chat:clearInput", "cmd+k":"chat:clearScreen"}`). A single
/// unambiguous keystroke, unlike the discoverable double-tap-escape, which is overloaded. A no-op
/// on an already-empty composer.
const COMPOSER_CLEAR: &[u8] = b"\x0c";

/// The composer clear has to land AFTER Claude Code has finished cancelling and repainted the
/// restored prompt — clear too early and it no-ops on a still-empty composer, the restore paints
/// afterwards, and the next remote message merges into it.
///
/// This was a fixed 150 ms delay, and in the field it lost the race: three phone sends, each
/// followed by Stop, concatenated into one prompt (`test message onetest message twotest message
/// three`) on a registered tab where `settled` was true and the clear had definitely fired. So
/// don't guess the interval — watch the PTY. Wait for the cancel to actually produce output, then
/// for that output to stop, then clear.
const COMPOSER_QUIET_MS: u64 = 250;
const COMPOSER_POLL_MS: u64 = 50;
const COMPOSER_SETTLE_CAP_MS: u64 = 3000;

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

/// POST /chats/{tabId}/resume-workspace — wake the SUSPENDED workspace that owns this tab so its
/// tabs come back live. Tab-scoped (not workspace-scoped) so the phone keeps addressing everything
/// by tabId; the server resolves tab → workspace. Suspension is a frontend-driven operation (the
/// PTY respawn + agent auto-resume lives in the Svelte store, which the backend can't do), so this
/// emits `mailink-resume-workspace` and the owning window's frontend runs `resumeWorkspace()` —
/// which respawns exactly the tabs that were live at suspend and re-inits their agents. After that
/// the tab is live on its own; the phone does NOT need a separate per-tab Initialize.
///
/// Returns `{ ok, resumed }`: `resumed=false` (200) when the workspace was already awake (nothing
/// to do — the phone can Initialize per-tab as usual); `resumed=true` when a resume was kicked off.
/// `404` if the tab isn't maiLink-available.
async fn post_resume_workspace(
    State(s): State<ApiState>,
    headers: HeaderMap,
    Path(tab_id): Path<String>,
) -> Result<Json<Value>, StatusCode> {
    authorize(&s, &headers)?;
    let meta = designated_tabs(&s.app)
        .into_iter()
        .find(|t| t.tab_id == tab_id)
        .ok_or(StatusCode::NOT_FOUND)?;
    if !meta.workspace_suspended {
        return Ok(Json(json!({ "ok": true, "resumed": false })));
    }
    // Workspace ids are app-unique (uuid), so a global emit reaches exactly one owning window; the
    // store's `resumeWorkspace` guards `!ws || !ws.suspended` → every other window no-ops.
    let h = s.app_handle.as_ref().ok_or(StatusCode::INTERNAL_SERVER_ERROR)?;
    let _ = h.emit(
        "mailink-resume-workspace",
        json!({ "workspaceId": meta.workspace_id }),
    );
    Ok(Json(json!({
        "ok": true,
        "resumed": true,
        "workspaceId": meta.workspace_id,
    })))
}

/// POST /chats/{tabId}/mesh-init — bring the MESH workspace that owns this tab back to ready
/// (the phone's one-tap "Initialize all", typically after a maiTerm restart). Tab-scoped like
/// resume-workspace: the phone addresses by tabId, the server resolves tab → workspace.
///
/// The triage is frontend-owned — per member: a running agent that lost its registration gets
/// `/maiterm init` typed into its PTY (dialog-safe delivery), an exited agent gets its
/// auto-resume replayed, live members are untouched — so this emits `mailink-mesh-init` and the
/// owning window's `agentMeshStore.initializeMesh()` does the work headlessly. Progress is
/// observable on the phone as members re-register: chat_state flips dormant → active/idle.
///
/// Returns `{ ok, initiated }`: `initiated=false` (200, with `reason`) when the workspace isn't
/// a mesh (`"not-mesh"`) or is suspended (`"workspace-suspended"` — resume it first; mesh-init
/// needs live PTYs). `404` if the tab isn't maiLink-available.
async fn post_mesh_init(
    State(s): State<ApiState>,
    headers: HeaderMap,
    Path(tab_id): Path<String>,
) -> Result<Json<Value>, StatusCode> {
    authorize(&s, &headers)?;
    let meta = designated_tabs(&s.app)
        .into_iter()
        .find(|t| t.tab_id == tab_id)
        .ok_or(StatusCode::NOT_FOUND)?;
    if !meta.mesh {
        return Ok(Json(json!({ "ok": true, "initiated": false, "reason": "not-mesh" })));
    }
    if meta.workspace_suspended {
        return Ok(Json(
            json!({ "ok": true, "initiated": false, "reason": "workspace-suspended" }),
        ));
    }
    // Workspace ids are app-unique, so a global emit reaches exactly one owning window; the
    // store's `initializeMesh` guards `!ws?.bridge_all` → every other window no-ops.
    let h = s.app_handle.as_ref().ok_or(StatusCode::INTERNAL_SERVER_ERROR)?;
    let _ = h.emit(
        "mailink-mesh-init",
        json!({ "workspaceId": meta.workspace_id }),
    );
    Ok(Json(json!({
        "ok": true,
        "initiated": true,
        "workspaceId": meta.workspace_id,
    })))
}

/// POST /chats/{tabId}/archive — archive a live tab (RECOVERABLE). The tab leaves the workspace's
/// live tabs into its `archived_tabs`; scrollback + restore context (cwd, ssh) are preserved and
/// it can be restored later. Archiving needs the frontend (serialize the live xterm buffer, kill
/// the PTY, reselect the active tab), so — like resume-workspace — this emits and the owning
/// window's `workspacesStore.archiveTab(...)` does the work. It drops from GET /chats and a
/// `chats_changed` follows on the next WS tick (≤1.5 s).
async fn post_archive(
    State(s): State<ApiState>,
    headers: HeaderMap,
    Path(tab_id): Path<String>,
) -> Result<Json<Value>, StatusCode> {
    authorize(&s, &headers)?;
    // Must be a currently-live maiLink tab (archive/close operate on GET /chats entries).
    if !designated_tabs(&s.app).iter().any(|t| t.tab_id == tab_id) {
        return Err(StatusCode::NOT_FOUND);
    }
    let h = s.app_handle.as_ref().ok_or(StatusCode::INTERNAL_SERVER_ERROR)?;
    // tab ids are app-unique — the owning window resolves ws+pane and acts; others no-op.
    let _ = h.emit("mailink-archive-tab", json!({ "tabId": tab_id }));
    Ok(Json(json!({ "ok": true })))
}

/// POST /chats/{tabId}/close — end a tab permanently (DESTRUCTIVE, NOT recoverable). This is the
/// genuine-close path (desktop Cmd+W / × button): the PTY is killed, the tab is removed from
/// state, its scrollback is deleted, and any agent bridge on it is torn down — there is no
/// archive entry afterward. Frontend-driven for the same reasons as archive; emits
/// `mailink-close-tab`. The phone should confirm before calling this.
async fn post_close(
    State(s): State<ApiState>,
    headers: HeaderMap,
    Path(tab_id): Path<String>,
) -> Result<Json<Value>, StatusCode> {
    authorize(&s, &headers)?;
    if !designated_tabs(&s.app).iter().any(|t| t.tab_id == tab_id) {
        return Err(StatusCode::NOT_FOUND);
    }
    let h = s.app_handle.as_ref().ok_or(StatusCode::INTERNAL_SERVER_ERROR)?;
    let _ = h.emit("mailink-close-tab", json!({ "tabId": tab_id }));
    Ok(Json(json!({ "ok": true })))
}

/// POST /chats/{tabId}/restore — un-archive a tab back into its workspace (reverses archive). The
/// tab id here is an ARCHIVED tab (from GET /chats/archived), not a live one, so it's resolved
/// against `archived_tabs` rather than `designated_tabs`. Restoring respawns the PTY + replays
/// auto-resume (frontend), so this emits `mailink-restore-tab` with the resolved workspace id and
/// the owning window runs `workspacesStore.restoreArchivedTab(...)`. It reappears in GET /chats on
/// the next `chats_changed`.
async fn post_restore(
    State(s): State<ApiState>,
    headers: HeaderMap,
    Path(tab_id): Path<String>,
) -> Result<Json<Value>, StatusCode> {
    authorize(&s, &headers)?;
    let workspace_id = archived_workspace_of(&s.app, &tab_id).ok_or(StatusCode::NOT_FOUND)?;
    let h = s.app_handle.as_ref().ok_or(StatusCode::INTERNAL_SERVER_ERROR)?;
    let _ = h.emit(
        "mailink-restore-tab",
        json!({ "workspaceId": workspace_id, "tabId": tab_id }),
    );
    Ok(Json(json!({ "ok": true, "workspaceId": workspace_id })))
}

/// GET /chats/archived — the flat list of archived tabs across every window/workspace (they're
/// NOT in GET /chats, which only lists live/designated tabs). `workspaceId` on each entry lets the
/// phone group by workspace client-side; the id is what POST /chats/{tabId}/restore takes.
async fn chats_archived(
    State(s): State<ApiState>,
    headers: HeaderMap,
) -> Result<Json<Value>, StatusCode> {
    authorize(&s, &headers)?;
    Ok(Json(json!(archived_chats(&s.app))))
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
    // The WS is the connection a phone holds open for hours, so it's the best liveness evidence
    // there is — and it doesn't go through `authorize`.
    touch_device(&s, if header_ok { bearer_token(&headers) } else { q.token.as_deref().unwrap_or("") });
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
    // Per-tab last-seen workspace-suspended flag. Suspending/resuming a workspace doesn't move any
    // tab's state either (its sessions were already dormant), so it too needs its own diff to fire
    // `chats_changed` → the phone re-GETs /chats and swaps Initialize ⇄ Resume-workspace.
    let mut suspended: HashMap<String, bool> = HashMap::new();
    // Per-tab last-seen mesh flag — enabling/disabling a Mesh Workspace from the desktop must
    // re-badge the phone's inbox group the same way (state/prompt don't move).
    let mut mesh: HashMap<String, bool> = HashMap::new();
    // Per-tab last-seen registration flag. A tab registering (or losing its registration) changes
    // the re-initialize affordance without moving state/prompt, so it needs its own diff.
    let mut registered: HashMap<String, bool> = HashMap::new();
    // Streaming state (mailink-protocol §12): per-tab last-window msg_ids + transcript mtime, so the
    // message ticker diffs cheaply and emits only newly-appended turns.
    let mut seen: HashMap<String, std::collections::HashSet<String>> = HashMap::new();
    let mut mtimes: HashMap<String, u64> = HashMap::new();
    // Per-tab task-board change key (tasks::change_key) — the `tasks` WS event fires only when a
    // board actually changed. Baseline emission on connect is deliberate: the phone gets every
    // existing board without opening threads, and a reconnect catches changes it slept through.
    let mut task_keys: HashMap<String, u64> = HashMap::new();
    // Same discipline for the background-shell roster.
    let mut shell_keys: HashMap<String, u64> = HashMap::new();
    // Asset batches already streamed, per tab, plus the manifest mtime that gates the whole pass.
    // Seeded with everything already sent, right here at connect: the snapshot the phone is about
    // to GET carries that history, and the streamer cannot baseline for itself (see
    // `stream_new_assets`). Seeding at the same instant as the snapshot is what makes "already
    // delivered" and "already shown" the same set.
    let mut asset_seen: HashMap<String, std::collections::HashSet<String>> = HashMap::new();
    let mut asset_rev: u64 = assets::revision();
    {
        // ONE parse for the whole roster. Asking per tab re-read and re-parsed the entire manifest
        // once per designated tab — ~386 of them here — which is the O(tabs x filesystem) shape
        // that produced the chat-list storm.
        let grouped = assets::by_tab();
        for t in designated_tabs(&s.app) {
            if let Some(records) = grouped.get(&t.tab_id) {
                asset_seen.insert(t.tab_id, batch_ids(records));
            }
        }
    }

    // initial snapshot: one chat_state per chat
    for c in build_chats(&s.app) {
        let tab = c["tabId"].as_str().unwrap_or_default().to_string();
        let key = attn_key(c["state"].as_str().unwrap_or_default(), c["prompt"].as_str());
        if socket.send(Message::Text(chat_state_event(&c).to_string().into())).await.is_err() {
            return;
        }
        titles.insert(tab.clone(), c["title"].as_str().unwrap_or_default().to_string());
        suspended.insert(tab.clone(), c["workspaceSuspended"].as_bool().unwrap_or(false));
        mesh.insert(tab.clone(), c["mesh"].as_bool().unwrap_or(false));
        registered.insert(tab.clone(), c["registered"].as_bool().unwrap_or(true));
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
    // Liveness. Everything else in this loop only writes when something CHANGES, so on a quiet
    // desktop a dead peer is never written to and never discovered — and even when there is a
    // write, a half-open TCP socket accepts it into the send buffer and only errors after the
    // retransmit timeout, minutes later. See WS_PING_INTERVAL for why that matters more here than
    // it looks.
    // First tick fires IMMEDIATELY (tokio's default, deliberately not reset here): the client is
    // provably awake at the instant it completes a handshake, so pinging now is the only moment a
    // pong is guaranteed to be answerable. Waiting an interval instead would mean the first ping
    // routinely lands in a suspended app — open maiLink, glance, pocket the phone — and "this
    // client has never ponged" would be a race with iOS rather than a fact about the client.
    let mut ping_ticker = tokio::time::interval(WS_PING_INTERVAL);
    let mut awaiting_pong = false;
    let mut answered_a_ping = false;
    loop {
        tokio::select! {
            _ = ping_ticker.tick() => {
                if awaiting_pong {
                    // Closing is the SAFE direction either way: a phone that isn't answering
                    // isn't receiving events either, so it must go back to being reachable by
                    // doorbell. Both cases below close — only the diagnosis differs.
                    if answered_a_ping {
                        log::info!("[maiLink] ws: no pong within {}s — treating the phone as gone", WS_PING_INTERVAL.as_secs());
                    } else {
                        // Never answered even the connect-time ping. Most likely a client that
                        // went away immediately; possibly one that doesn't implement pong. It is
                        // NOT treated as the latter: an earlier version disabled liveness
                        // detection on this evidence, which restored the very bug the heartbeat
                        // fixes — silently, and in the commonest usage pattern. Absence of a pong
                        // is not proof of a specific cause, and the branch that assumed one
                        // resolved the ambiguity by switching the check off. A reconnect loop is
                        // loud and self-announcing; phantom coverage is silent and eats
                        // notifications. Prefer the loud failure.
                        log::warn!("[maiLink] ws: client never answered a ping (not even at connect) — closing. If a client legitimately cannot pong, that is a client bug: RFC 6455 requires it and both maiLink transports answer at framework level.");
                    }
                    break;
                }
                if socket.send(Message::Ping(Default::default())).await.is_err() {
                    return;
                }
                awaiting_pong = true;
            }
            _ = msg_ticker.tick() => {
                if stream_new_messages(&mut socket, &s.app, &mut seen, &mut mtimes, &mut task_keys, &mut shell_keys).await.is_err() {
                    return;
                }
                if stream_new_assets(&mut socket, &s.app, &mut asset_seen, &mut asset_rev).await.is_err() {
                    return;
                }
            }
            _ = mirror_ticker.tick() => {
                let tabs: Vec<String> = designated_tabs(&s.app).into_iter().map(|t| t.tab_id).collect();
                mirror::refresh_tabs(&s.app, &tabs);
            }
            _ = ticker.tick() => {
                // Summaries, not full chats: this fires forever at 1.5s, and the full build's
                // scrollback + per-tab transcript reads were constant background lock pressure.
                // The rare transitioning tab is enriched below.
                let chats = build_chat_summaries(&s.app);
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
                    // Workspace suspend/resume flips this without touching state/prompt — diff it
                    // so the phone re-fetches and swaps the Initialize ⇄ Resume-workspace control.
                    let ws_susp = c["workspaceSuspended"].as_bool().unwrap_or(false);
                    if prev.is_some() && suspended.get(&tab) != Some(&ws_susp) {
                        roster_changed = true;
                    }
                    suspended.insert(tab.clone(), ws_susp);
                    // Same for the mesh flag (toggled from the desktop's mesh setup modal).
                    let ws_mesh = c["mesh"].as_bool().unwrap_or(false);
                    if prev.is_some() && mesh.get(&tab) != Some(&ws_mesh) {
                        roster_changed = true;
                    }
                    mesh.insert(tab.clone(), ws_mesh);
                    // Registration flips when an agent finally re-registers (or a restart drops
                    // its session entry) — the phone must re-render the re-initialize control.
                    let reg = c["registered"].as_bool().unwrap_or(true);
                    // Captured before the insert: the attention edge below needs to know whether
                    // this tab had a tracked session LAST tick, not this one.
                    let prev_reg = registered.get(&tab).copied().unwrap_or(false);
                    let reg_changed = prev.is_some() && registered.get(&tab) != Some(&reg);
                    if reg_changed {
                        roster_changed = true;
                    }
                    registered.insert(tab.clone(), reg);
                    // `chats_changed` alone is a ROSTER signal — it says "re-GET /chats", which
                    // refreshes the inbox but not an already-open thread. Registration usually
                    // flips with the attention key UNCHANGED (the live-agent fallback already
                    // reported "active", and a registered session reports "active" too), so
                    // without the `|| reg_changed` arm an open thread got no frame at all and
                    // the "Running but not registered" banner survived the very init that fixed
                    // it, until the user backed out and re-opened the thread.
                    if state_frame_needed(prev.as_deref(), &key, reg_changed) {
                        if socket.send(Message::Text(enriched_chat_state_event(&s.app, c).to_string().into())).await.is_err() {
                            return;
                        }
                        // Same edge rule the push doorbell uses — see `rings_attention`. A tab
                        // that merely APPEARS in the roster already idle, or whose session row is
                        // being created for the first time, must not announce "finished".
                        if rings_attention(prev.as_deref(), prev_reg, &key) {
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
                    for k in removed { last.remove(&k); titles.remove(&k); suspended.remove(&k); mesh.remove(&k); registered.remove(&k); }
                }
                if roster_changed {
                    let _ = socket.send(Message::Text(json!({ "type": "chats_changed" }).to_string().into())).await;
                }
            }
            msg = socket.recv() => {
                match msg {
                    Some(Ok(Message::Close(_))) | None => break,
                    Some(Err(_)) => break,
                    // The phone is alive and its app is running. (A suspended app answers
                    // nothing — which is the point; see WS_PING_INTERVAL.)
                    Some(Ok(Message::Pong(_))) => {
                        awaiting_pong = false;
                        answered_a_ping = true;
                    }
                    // inbound client frames are ignored in v1 — the client uses REST for actions
                    Some(Ok(_)) => {}
                }
            }
        }
    }
}

/// `chat_state_event` for a ticker SUMMARY row (build_chat_summaries): fills in the two fields
/// the summary deliberately omits — lastActivityTs and meta — for just this one transitioning
/// tab, so the per-tick cost of the heavy sources is paid only on actual state changes.
fn enriched_chat_state_event(app: &AppState, c: &Value) -> Value {
    let tab_id = c["tabId"].as_str().unwrap_or_default();
    let mut c = c.clone();
    c["lastActivityTs"] = json!(last_activity_ts(
        app,
        tab_id,
        scrollback_time_for(app, tab_id),
        now_ms(),
    ));
    if let Some(meta) = build_meta(app, tab_id) {
        c["meta"] = meta;
    }
    chat_state_event(&c)
}

/// Whether the roster ticker owes a tab a `chat_state` frame this tick (unit-tested; the ticker
/// wires the real diffs).
///
/// The attention key is the usual trigger, but registration flips independently of it: the
/// live-agent fallback already reports "active", and a tab that registers reports "active" too,
/// so a key-only test emitted nothing on the one transition the phone's banner is bound to.
fn state_frame_needed(prev: Option<&str>, key: &str, reg_changed: bool) -> bool {
    prev != Some(key) || reg_changed
}

/// A `chat_state` frame.
///
/// **Every field here is ALWAYS present, `meta` excepted.** That is a contract, not an accident:
/// the client MERGES these frames over rows built by `build_chats`, so it cannot distinguish a
/// field this producer omitted from a field this producer is claiming is empty. Omitting one
/// therefore either strands a stale value (the answered-ask pin, see `prompt` below) or clobbers
/// a good one (absent `registered` read as `true` would have hidden the re-initialize banner on
/// exactly the desktops that still needed it). Send a real `null`, never nothing.
///
/// `meta` is the single deliberate exception: `build_meta` returns None for a non-Claude tab or a
/// transcript it cannot resolve right now, and that is "unknown", not "no telemetry" — a
/// transient miss must not blank a live context gauge. Clients merge it only when present.
///
/// So: adding a field to `build_chats` that the phone RENDERS means adding it here too, or the
/// live path silently disagrees with the REST path. Two producers, one row.
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
        // The banner an unregistered chat renders lives INSIDE the thread, and `state` cannot
        // express registration (a live agent reads "active" whether or not it registered). This
        // is the only live signal an open thread gets, so the phone binds the banner to it and
        // the affordance disappears the moment the init it asked for actually lands.
        "registered": c.get("registered").cloned().unwrap_or_else(|| json!(true)),
        // Explicitly present, and explicitly `null` when there is no open prompt. This frame
        // FIRES on a prompt change — `attn_key` is state+prompt — so omitting the prompt half
        // told the phone "something moved" while withholding what moved. Answering an ask
        // (prompt question → null, state stays "active") emits exactly this frame, raises no
        // `attention` event (that only fires INTO attention) and no `chats_changed` (the roster
        // diff doesn't watch prompt), so nothing made the phone re-GET: the row kept its stale
        // prompt and the answered ask stayed pinned at the top of the inbox.
        "prompt": c.get("prompt").cloned().unwrap_or(Value::Null),
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
    let mut ev = json!({
        "type": "message",
        "tabId": tab_id,
        "role": turn.get("role"),
        "text": turn.get("text"),
        "msg_id": turn.get("msg_id"),
        "ts": turn.get("ts"),
    });
    // Typed turns (`peer_message`, …) must carry their tag on the LIVE path too: a streamed turn
    // that arrives untagged renders as a plain message and then silently changes shape when the
    // next GET returns the tagged version.
    for key in ["kind", "peer", "assets"] {
        if let Some(v) = turn.get(key) {
            ev[key] = v.clone();
        }
    }
    ev
}

/// Emit a `message` frame for any file batch the phone hasn't been sent yet.
///
/// Assets are synthesized turns, so they are invisible to the transcript-mtime path that streams
/// everything else — they need their own diff. Gated on the manifest's mtime so the common case
/// (nobody has sent a file) costs one stat per tick rather than a parse per designated tab; that
/// per-tab shape is what made the chat-list storm. A tab's first observation baselines silently:
/// the phone's GET already carried its history, and replaying it would duplicate every row.
async fn stream_new_assets(
    socket: &mut WebSocket,
    app: &AppState,
    seen: &mut HashMap<String, std::collections::HashSet<String>>,
    last_rev: &mut u64,
) -> Result<(), ()> {
    let rev = assets::revision();
    if rev == *last_rev {
        return Ok(());
    }
    *last_rev = rev;
    let grouped = assets::by_tab();
    for t in designated_tabs(app) {
        let Some(records) = grouped.get(&t.tab_id) else { continue };
        let turns = turns_from_records(records);
        if turns.is_empty() {
            continue;
        }
        let entry = seen.entry(t.tab_id.clone()).or_default();
        // NO first-observation baseline here, unlike the message streamer. That pass runs every
        // tick, so a tab is always baselined on a tick before its first new turn. This one runs
        // ONLY when the manifest changed — which is precisely the tick an asset was added — so
        // baselining on an empty set would swallow the first file ever sent to a tab, every
        // time. History is baselined once at connect instead (`asset_seen` is seeded there),
        // which is the moment the phone's own GET carried it.
        for turn in &turns {
            let Some(id) = turn.get("msg_id").and_then(|v| v.as_str()) else { continue };
            if !entry.insert(id.to_string()) {
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
    }
    Ok(())
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
    task_keys: &mut HashMap<String, u64>,
    shell_keys: &mut HashMap<String, u64>,
) -> Result<(), ()> {
    for t in designated_tabs(app) {
        let Some((rt, sid)) = resolved_session_for_tab(app, &t.tab_id) else { continue };
        // Task-board diff — BEFORE the transcript-mtime gate: a subagent claiming/completing
        // tasks rewrites board files without appending to the MAIN transcript, so the board
        // needs its own change key. Cost per tick per tab is one readdir of a tiny dir (or one
        // ENOENT stat for the no-board majority). Claude only — no board elsewhere.
        if rt == AgentRuntime::Claude {
            stream_tasks_if_changed(socket, &t.tab_id, &sid, task_keys).await?;
            // Background shells: also outside the transcript-mtime gate — a shell EXITING appends
            // nothing to the transcript, and that transition is exactly what the strip must show.
            stream_shells_if_changed(socket, app, &t.tab_id, shell_keys).await?;
        }
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

/// Emit a `shells` WS frame when the tab's background-shell roster changed. Same full-array
/// replace + baseline-on-connect discipline as `tasks`; `[]` clears the strip. The change key
/// folds in each shell's status and pid, so a shell EXITING (which appends nothing to the
/// transcript) still fires — that transition is the whole point of the strip.
async fn stream_shells_if_changed(
    socket: &mut WebSocket,
    app: &AppState,
    tab_id: &str,
    shell_keys: &mut HashMap<String, u64>,
) -> Result<(), ()> {
    use std::hash::{Hash, Hasher};
    let roster = shell_roster(app, tab_id);
    let key = {
        let mut h = std::collections::hash_map::DefaultHasher::new();
        for sh in &roster {
            sh.id.hash(&mut h);
            sh.status.as_str().hash(&mut h);
            sh.pid.hash(&mut h);
            sh.tail.hash(&mut h);
        }
        roster.len().hash(&mut h);
        h.finish()
    };
    if roster.is_empty() {
        // Nothing to report: emit one clearing frame only if this tab previously had a roster.
        if shell_keys.remove(tab_id).is_none() {
            return Ok(());
        }
        let ev = json!({ "type": "shells", "tabId": tab_id, "shells": [], "ts": now_ms() });
        return socket.send(Message::Text(ev.to_string().into())).await.map_err(|_| ());
    }
    if shell_keys.get(tab_id) == Some(&key) {
        return Ok(());
    }
    shell_keys.insert(tab_id.to_string(), key);
    let ev = json!({
        "type": "shells",
        "tabId": tab_id,
        "shells": roster.iter().map(|s| s.to_json()).collect::<Vec<_>>(),
        "ts": now_ms(),
    });
    socket.send(Message::Text(ev.to_string().into())).await.map_err(|_| ())
}

/// Emit a `tasks` WS frame when the tab's Claude task board changed (mailink/tasks.rs).
/// Full-array replace semantics — boards are tiny, so no per-task diffing. A board that
/// disappears (session ended, tasks all deleted) emits one final empty array so the phone
/// clears its strip.
async fn stream_tasks_if_changed(
    socket: &mut WebSocket,
    tab_id: &str,
    session_id: &str,
    task_keys: &mut HashMap<String, u64>,
) -> Result<(), ()> {
    let event = match tasks::change_key(session_id) {
        Some(key) => {
            if task_keys.get(tab_id) == Some(&key) {
                return Ok(());
            }
            task_keys.insert(tab_id.to_string(), key);
            json!({
                "type": "tasks",
                "tabId": tab_id,
                "tasks": tasks::tasks_for_session(session_id).unwrap_or_default(),
                "ts": now_ms(),
            })
        }
        None => {
            if task_keys.remove(tab_id).is_none() {
                return Ok(());
            }
            json!({ "type": "tasks", "tabId": tab_id, "tasks": [], "ts": now_ms() })
        }
    };
    socket
        .send(Message::Text(event.to_string().into()))
        .await
        .map_err(|_| ())
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

/// Claim the one permitted keystroke injection into a given open ask. `true` for the first call
/// per (tab, prompt_id); `false` for every repeat.
///
/// One slot per tab, keyed by the ask's prompt_id, so a new ask replaces the old entry and the map
/// stays bounded by tab count. Deliberately NOT cleared on success: a resolved ask can't be
/// answered twice anyway, and clearing on failure is exactly the case that must stay refused.
fn claim_question_inject(tab_id: &str, prompt_id: &str) -> bool {
    static CLAIMED: std::sync::OnceLock<std::sync::Mutex<HashMap<String, String>>> =
        std::sync::OnceLock::new();
    let map = CLAIMED.get_or_init(|| std::sync::Mutex::new(HashMap::new()));
    let Ok(mut m) = map.lock() else { return true }; // a poisoned lock must not block answering
    match m.get(tab_id) {
        Some(seen) if seen == prompt_id => false,
        _ => {
            m.insert(tab_id.to_string(), prompt_id.to_string());
            true
        }
    }
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
///
/// TYPING INTO THE WRONG ROW IS DESTRUCTIVE, which is why (a) is load-bearing rather than
/// cosmetic. Read from the 2.1.224 bundle:
///   * The container's own onKeyDown calls `onRespondToClaude()` — which DISMISSES the ask and
///     routes the turn to chat, discarding every answer — when the key is the digit
///     `options.length + 2` ("Chat about this"). It is gated on NOT being focused in the Other
///     input. So if the highlight is off by one row when the free text is typed, any digit in the
///     operator's own text that happens to equal `option_count + 2` silently throws the ask away.
///     That is the reported "it acted like I said Chat about this", and its intermittency is
///     explained by needing that one particular digit.
///   * There IS also a literal `{value:"__chat__"}` ROW below Other, but only under
///     `tlt = useContext(InternalAccessibilityContext)` — i.e. `isScreenReaderEnabled`, which
///     defaults false. Do not plan around that row for ordinary terminals; do not assume it is
///     absent for a screen-reader user either.
///   * `select:next`/`select:previous` are unbound while an input row holds focus, so once the
///     highlight reaches Other it CANNOT be moved by arrows. A wrong position is therefore not
///     recoverable by navigating — see `claim_question_inject` for the consequence.
///   (g) multiSelect + Other free-text leaves via the form's OWN Submit/Next button: Down from
///       the last row focuses it, Enter activates it. NOT Enter-then-→.        [UNVERIFIED — see below]
///
/// (g) was carried for months as a best guess (Enter to "commit", then → to advance) and never
/// worked. What a device actually shows: both answers land — the box ticked, the Other row CHECKED
/// with the typed text — and the form just never submits, the highlight still on the Other row.
/// Typing already selects that row, so there was nothing for the Enter to commit; and both the
/// Enter and the → were swallowed by the active text input, so the injector never left it. Nothing
/// was corrupted; the answer was simply complete and unsubmitted, reported as `inject_failed`.
///
/// The exit is the form's own button. Down over Tab: both survive the input-focus filter, but Tab
/// is ALSO the form-level question switcher and the selector does not stop its propagation.
///
/// (g) IS ITSELF STILL UNVERIFIED ON A DEVICE — source-derived from 2.1.240, not guessed, but the
/// swallowed Enter proves the text input can eat a whitelisted key before the select sees it. The
/// [VERIFIED e2e] tag above is aspirational until a real multiSelect+Other answer arrives from a
/// phone. Marking a keystroke verified on reasoning alone is exactly how (g) went wrong the first
/// time.
///
/// All mapping is resolved BEFORE any keystroke is sent, so a bad answer rejects the whole batch
/// rather than half-answering.
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
    // Set when the FINAL question was committed by pressing the form's own Submit/Next button
    // (the multiSelect+Other exit), which already submits — so the trailing Enter below must not
    // fire a second time into whatever the form became.
    let mut submitted_by_button = false;
    let last_qi = plans.len() - 1;
    for (qi, plan) in plans.iter().enumerate() {
        let mut cur = 0usize; // highlight starts at row 0 (a)
        if plan.multi {
            for &idx in &plan.indices {
                cur = nav_to(app, pty_id, cur, idx).await?;
                send_key(app, pty_id, b" ", NAV_SETTLE_MS).await?; // Space toggles the row (live)
            }
            if let Some(text) = &plan.other {
                // The "Type something" row (at option_count) is a live inline input — typing into
                // it fills it AND selects it (updateInputValue adds `__other__` to the selected
                // set on any non-empty value). So there is nothing to "commit".
                cur = nav_to(app, pty_id, cur, plan.option_count).await?;
                send_text(app, pty_id, text).await?;
                // Leave via the form's own Submit/Next button rather than the tab arrow.
                //
                // OBSERVED on a device (2.1.240), from a screenshot of the stuck form: both
                // answers land correctly — the ticked box is ticked and the Other row is CHECKED
                // and holds the typed text — and the form simply never submits, with the
                // highlight still on the Other row. So the old trailing `Enter, →` corrupted
                // nothing; both keys were swallowed by the active text input and we never left
                // it. The answer was complete, and unsubmitted.
                //
                // The select whitelists exactly up/down/escape/tab/return while an input row has
                // focus, and Down from the LAST row focuses Submit/Next, after which Enter calls
                // onSubmit. `__other__` IS last: the row array is [...options, otherInput], and
                // the "Chat about this" line below it is a numeric-shortcut hint (options+2), not
                // a focusable row — so Down cannot land on the dismiss-the-ask affordance. Down
                // over Tab because Tab is also the form-level question switcher and the selector
                // does not stop its propagation.
                //
                // UNVERIFIED ON A DEVICE. The whitelist says these keys reach the select, but the
                // swallowed Enter above is evidence that the text input can consume a whitelisted
                // key first — so this may still fail to release. It is source-derived rather than
                // guessed, and a failure is now non-destructive (one attempt per ask, and the
                // phone keeps the text). Do NOT mark it verified without a real multiSelect+Other
                // answer sent from a phone.
                send_key(app, pty_id, b"\x1b[B", NAV_SETTLE_MS).await?; // Down → focus Submit/Next
                send_key(app, pty_id, b"\r", ADVANCE_SETTLE_MS).await?; // activate it
                // That button IS this question's advance ("Next") or the form's submit ("Submit"
                // on the last one), so the tab arrow below must not also fire.
                if qi == last_qi {
                    submitted_by_button = true;
                }
                continue;
            }
            // multiSelect toggles are live and are NOT confirmed with Enter; → advances to the
            // next tab (the next question, or Submit after the last one). Only reachable when the
            // highlight is on a listed row — an input row would swallow it.
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
    if !single_q_single_select && !submitted_by_button {
        send_key(app, pty_id, b"\r", ADVANCE_SETTLE_MS).await?;
    }
    Ok(())
}

/// The tab's currently-open prompt, as (kind, prompt_id, runtime). Mirrors what
/// `build_chat_detail` synthesizes, so `/respond`'s stale-guard agrees with what the client was
/// shown; the runtime picks the keystroke dialect for the answer injection.
fn current_prompt(app: &AppState, tab_id: &str) -> Option<(&'static str, String, AgentRuntime)> {
    let states = session_states(app);
    let (st, rt, tool, _) = states.get(tab_id)?;
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
/// makes successive asks on one tab distinct, so a late `/respond` against an ask that is no
/// longer the open one can never pass the stale-guard and answer a newer question that opened
/// meanwhile. Opaque to the app — it just echoes it.
///
/// The guard does NOT depend on asks expiring, and must not be justified by a timer: whether an
/// unanswered ask auto-resolves at all is version- and setting-gated (`ask_deadline_ms` — never
/// before CC 2.1.198, a hard 60s in 2.1.198–2.1.199 only, opt-in and defaulting to NEVER from
/// 2.1.200). Superseding is enough on its own — the human answering in the TUI, or a second ask
/// opening, both move `asked_at` with no timer involved.
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
    let token = bearer_token(headers);
    if token_valid(s, token) {
        touch_device(s, token);
        Ok(())
    } else {
        Err(StatusCode::UNAUTHORIZED)
    }
}

/// How often a device's `last_seen_at` is allowed to cost a state write. The phone polls every
/// couple of seconds, so this is purely about not saving state on every request.
const LAST_SEEN_THROTTLE_MS: u64 = 5 * 60 * 1000;

/// Record that a paired device just made an authenticated request.
///
/// Until this existed, `last_seen_at` was written ONLY when a device paired or re-registered its
/// push token — so a phone that had been talking to the desktop all morning could show a
/// `last_seen_at` two days old, in Preferences and to anything else reading it. A field named
/// "last seen" that doesn't track being seen isn't a stale value, it's a wrong one: it reads as
/// "this device is gone" at exactly the moment the device is busiest.
///
/// No-op for the dev token, which has no device record.
fn touch_device(s: &ApiState, token: &str) {
    if token.is_empty() || token == s.dev_token {
        return;
    }
    let hash = sha256_hex(token.as_bytes());
    let now = now_ms() as i64;
    // Read first: the common case is "seen recently", which must not take the write lock or
    // rewrite state on a 2-second poll.
    {
        let data = s.app.app_data.read();
        match data.preferences.mailink_devices.iter().find(|d| d.token_hash == hash) {
            Some(d) if now - d.last_seen_at < LAST_SEEN_THROTTLE_MS as i64 => return,
            Some(_) => {}
            None => return,
        }
    }
    let data_clone = {
        let mut data = s.app.app_data.write();
        let Some(d) =
            data.preferences.mailink_devices.iter_mut().find(|d| d.token_hash == hash)
        else {
            return;
        };
        d.last_seen_at = now;
        data.clone()
    };
    let _ = crate::state::save_state(&data_clone);
}

/// Log every request maiLink turns away.
///
/// A rejected maiLink client was previously invisible: no request logging, and `authorize` returned
/// a bare 401. An expired or rotated device token therefore produced a phone stuck on "no
/// conversations" and a desktop log with nothing in it at all — the one failure shape you cannot
/// diagnose from the phone, and the one most likely to happen (tokens are the only thing that can
/// silently stop matching). Successful requests stay unlogged; the phone polls every couple of
/// seconds and would drown the log.
async fn log_rejections(
    State(s): State<ApiState>,
    req: axum::extract::Request,
    next: axum::middleware::Next,
) -> Response {
    let method = req.method().clone();
    let path = req.uri().path().to_string();
    let who = describe_caller(&s, req.headers());
    let res = next.run(req).await;
    if res.status().as_u16() >= 400 {
        log::warn!("[maiLink] {} {} → {} ({})", method, path, res.status().as_u16(), who);
    }
    res
}

/// Who a request claims to be, safe to log: never the token itself, only a short hash prefix and
/// whether anything on this desktop matches it.
fn describe_caller(s: &ApiState, headers: &HeaderMap) -> String {
    let Some(raw) = headers.get(header::AUTHORIZATION).and_then(|v| v.to_str().ok()) else {
        return "no Authorization header".to_string();
    };
    let Some(token) = raw.strip_prefix("Bearer ") else {
        return "Authorization header is not a Bearer token".to_string();
    };
    if token.is_empty() {
        return "empty bearer token".to_string();
    }
    if token == s.dev_token && !s.dev_token.is_empty() {
        return "dev token".to_string();
    }
    let hash = sha256_hex(token.as_bytes());
    let short = &hash[..8.min(hash.len())];
    let data = s.app.app_data.read();
    match data.preferences.mailink_devices.iter().find(|d| d.token_hash == hash) {
        Some(d) => format!("device \"{}\" (token {short}…)", d.name),
        // The actionable one: the phone holds a token this desktop has no record of, so the fix
        // is re-pairing, not restarting anything.
        None => format!(
            "token {short}… matches none of the {} paired device(s) — re-pair the phone",
            data.preferences.mailink_devices.len()
        ),
    }
}

/// Owned snapshot of a maiLink-native tab (taken under the app_data lock, then released).
struct TabMeta {
    tab_id: String,
    title: String,
    workspace: String,
    /// Owning workspace id — lets the phone resolve tab → workspace for the resume flow without
    /// having to model workspace ids itself (it resumes by tabId; the server does the lookup).
    workspace_id: String,
    /// Whether the owning workspace is suspended (PTYs killed, tabs restore-on-demand). A tab in a
    /// suspended workspace can't be Initialized per-tab — the workspace must be resumed first — so
    /// the phone shows a "Resume workspace" affordance instead of a dead-end Initialize button.
    workspace_suspended: bool,
    /// Whether the owning workspace is a Mesh Workspace (`Workspace.bridge_all`). The phone
    /// badges the workspace group and offers the mesh Initialize-all action on it.
    mesh: bool,
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
                        workspace_id: ws.id.clone(),
                        workspace_suspended: ws.suspended,
                        mesh: ws.bridge_all,
                        runtime: tab.runtime.unwrap_or_default(),
                    });
                }
            }
        }
    }
    out
}

/// Resolve an ARCHIVED tab id → its owning workspace id (searching every window/workspace's
/// `archived_tabs`). `None` if no archived tab has that id. Used by POST /chats/{tabId}/restore.
fn archived_workspace_of(app: &AppState, tab_id: &str) -> Option<String> {
    let data = app.app_data.read();
    for win in &data.windows {
        for ws in &win.workspaces {
            if ws.archived_tabs.iter().any(|t| t.id == tab_id) {
                return Some(ws.id.clone());
            }
        }
    }
    None
}

/// The flat archived-tab list for GET /chats/archived — every workspace's `archived_tabs` across
/// all windows, newest-archived first. Only terminal tabs are exposed (editor/diff archives aren't
/// maiLink chats). `runtime` is best-effort (persisted from when it was live); `cwd` is the
/// restore cwd captured at archive time.
fn archived_chats(app: &AppState) -> Vec<Value> {
    let data = app.app_data.read();
    let mut out = Vec::new();
    for win in &data.windows {
        for ws in &win.workspaces {
            for tab in &ws.archived_tabs {
                if !matches!(tab.tab_type, TabType::Terminal) {
                    continue;
                }
                out.push(json!({
                    "tabId": tab.id,
                    // The resolved name shown in the archive list (falls back to the raw tab name).
                    "name": tab.archived_name.clone().unwrap_or_else(|| tab.name.clone()),
                    "workspace": ws.name,
                    "workspaceId": ws.id,
                    "runtime": tab.runtime.map(runtime_key),
                    "archivedAt": tab.archived_at,
                    "cwd": tab.restore_cwd,
                }));
            }
        }
    }
    // Newest archived first; entries without a timestamp sort last.
    out.sort_by(|a, b| b["archivedAt"].as_str().cmp(&a["archivedAt"].as_str()));
    out
}

/// True if `tab_id` is currently available to maiLink — the same gate that governs the chat
/// list, WS stream, and doorbell. The write + context endpoints call this so a tab held back
/// via the exposure settings (designate-only mode, or `mailink_excluded` in expose-all mode)
/// is genuinely unreachable, not merely hidden from discovery. Returns NOT_FOUND-worthy false
/// for unknown or non-designated tab_ids alike.
pub(crate) fn is_designated(app: &AppState, tab_id: &str) -> bool {
    designated_tabs(app).iter().any(|t| t.tab_id == tab_id)
}

/// tab_id → (state, runtime, current tool, has this session ever finished a turn), choosing the
/// most attention-worthy session if a tab somehow has more than one tracked session.
///
/// The last field is what `unread` keys on. `state` can't answer it: `"idle"` covers both
/// "finished, go read it" and "alive at an empty prompt", and the second is the resting state of
/// every tab after a restart. See `AgentSessionInfo::finished_a_turn`.
type SessionState = (AgentSessionState, AgentRuntime, Option<String>, bool);

fn session_states(app: &AppState) -> HashMap<String, SessionState> {
    let sessions = app.agent_sessions.read();
    let mut map: HashMap<String, SessionState> = HashMap::new();
    for sess in sessions.values() {
        let candidate = (sess.state, sess.runtime, sess.tool_name.clone(), sess.finished_a_turn);
        map.entry(sess.tab_id.clone())
            .and_modify(|cur| {
                if rank(sess.state) > rank(cur.0) {
                    *cur = (sess.state, sess.runtime, sess.tool_name.clone(), sess.finished_a_turn);
                }
            })
            .or_insert(candidate);
    }
    map
}

/// How recently a tab's transcript must have produced a REAL turn for the live-agent fallback to
/// override "dormant". Long enough to bridge a slow tool call / thinking gap, short enough that a
/// genuinely finished-and-quiet agent reverts to dormant so the operator sees the real state.
const LIVE_STATE_FALLBACK_MS: u64 = 10 * 60 * 1000;

/// Pure decision for the dormant→live fallback (unit-tested; the impl below wires the real reads).
fn live_fallback_decision(has_live_pty: bool, last_turn_ts: Option<u64>, now: u64) -> bool {
    has_live_pty && last_turn_ts.is_some_and(|ts| now.saturating_sub(ts) <= LIVE_STATE_FALLBACK_MS)
}

/// Self-correcting liveness fallback for a designated tab that has NO tracked `agent_sessions`
/// entry (would report "dormant"). Such a tab can still be a fully-live, producing agent whose
/// session entry was simply never created: on a mesh/SSH resume, SessionStart buffers to `pending`
/// (Claude http hooks carry no tab_id to insert directly) and initSession may catch neither a
/// sessionId nor a still-unclaimed pending entry (the pending pool is shared across all tabs, so a
/// sibling's init can consume it) — and once no entry exists, the running agent's per-turn hooks
/// only MUTATE an existing entry, so they can never heal it. When such a tab has a LIVE PTY and its
/// transcript produced a real turn within LIVE_STATE_FALLBACK_MS, report "active" (NOT "idle":
/// active is not an attention state, so this drops the phone's dormant/Initialize banner without
/// firing a phantom "done" doorbell). Self-correcting: PTY death or a transcript that stops
/// advancing reverts to "dormant". Cheap: an in-memory PTY-map lookup + a bounded transcript tail
/// read (the same read `last_activity_ts` already does per tab) — no process scan.
fn tab_looks_live_despite_no_session(app: &AppState, tab_id: &str, now: u64) -> bool {
    // Order matters, and it is not style. Rust evaluates both arguments before the call, so
    // passing the transcript read positionally paid for it on every dormant tab — the ones where
    // the PTY check has already decided the answer is false. That was ~206 pointless transcript
    // lookups per tick once every agent tab became designated. Check the cheap in-memory half
    // first and return.
    if pty_for_tab(app, tab_id).is_none() {
        return false;
    }
    live_fallback_decision(
        true,
        resolved_session_for_tab(app, tab_id).and_then(|(rt, sid)| transcript::last_turn_ts_for(rt, &sid)),
        now,
    )
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
    // Tab ids are app-unique, so the first match is the only match — stop there. This is called
    // 2–3× per designated tab per tick by three loops; walking all ~490 tabs to the end each
    // time was hundreds of thousands of pointless comparisons a second once every agent tab
    // became designated.
    let found: Option<(AgentRuntime, String)> = data
        .windows
        .iter()
        .flat_map(|w| &w.workspaces)
        .flat_map(|ws| &ws.panes)
        .flat_map(|p| &p.tabs)
        .find(|tab| tab.id == tab_id)
        .and_then(|tab| {
            // None → Claude (matches designated_tabs' default): tabs persisted by app versions
            // predating Tab.runtime can still carry claudeSessionId.
            let rt = tab.runtime.unwrap_or_default();
            let var = crate::state::agent_runtime::descriptor(rt).session_id_var;
            tab.trigger_variables
                .get(var)
                .cloned()
                .filter(|s| !s.is_empty())
                .map(|sid| (rt, sid))
        });
    let (rt, sid) = found?;
    // Contested-sid resolution: tab duplication copies the session-id var ON PURPOSE (reload =
    // duplicate + close original; fork = duplicate + branch), so two tabs claiming one sid is a
    // legitimate transient state — but only ONE of them may render the conversation, or both
    // tabs show the same (possibly someone else's) transcript. The transcript itself names its
    // rightful renderer: the SessionStart hook echoes the hosting tab id into the JSONL on
    // every start/resume/compact, so the last marker follows actual usage — the original keeps
    // rendering until the duplicate actually resumes the session, then it flips. Unknown host
    // (unlocatable transcript, marker out of tail range, non-Claude runtime) → None for every
    // claimant: the snapshot fallback beats rendering someone else's conversation. A LIVE
    // registration (live_session_for_tab, tried before this fn) is unaffected.
    if data.session_id_claimants(&sid) > 1 {
        let owns = rt == AgentRuntime::Claude
            && transcript::claude_session_host_tab(&sid).as_deref() == Some(tab_id);
        if !owns {
            return None;
        }
    }
    Some((rt, sid))
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
/// One synthesized transcript turn per `sendFilesToPhone` call, oldest first.
///
/// Synthesized, not injected: nothing is written into Claude's JSONL — the same move
/// `goal_status` and `terminal_snapshot` make. `text` is a real human sentence rather than a
/// placeholder, because a client that doesn't know `kind: "asset"` falls through to rendering it,
/// and "Sent 2 files: report.pdf, clip.mov" degrades usefully where an empty string would not.
/// The `msg_id`s a tab's asset records render as — the streamer's dedup key.
fn batch_ids(records: &[assets::AssetRecord]) -> std::collections::HashSet<String> {
    records.iter().map(|r| format!("asset_{}", r.batch_id)).collect()
}

fn asset_turns(tab_id: &str) -> Vec<Value> {
    turns_from_records(&assets::for_tab(tab_id))
}

/// Group already-loaded records into turns. Split from `asset_turns` so a caller walking the whole
/// roster parses the manifest once (`assets::by_tab`) instead of once per tab.
fn turns_from_records(records: &[assets::AssetRecord]) -> Vec<Value> {
    let mut by_batch: Vec<(String, Vec<assets::AssetRecord>)> = Vec::new();
    for record in records.iter().cloned() {
        match by_batch.iter_mut().find(|(b, _)| *b == record.batch_id) {
            Some((_, group)) => group.push(record),
            None => by_batch.push((record.batch_id.clone(), vec![record])),
        }
    }
    by_batch
        .into_iter()
        .filter_map(|(batch_id, group)| {
            let first = group.first()?;
            let names: Vec<&str> = group.iter().map(|r| r.name.as_str()).collect();
            let text = match (group.len(), first.caption.as_deref()) {
                (_, Some(c)) => format!("{c} ({})", names.join(", ")),
                (1, None) => format!("Sent {}", names[0]),
                (n, None) => format!("Sent {n} files: {}", names.join(", ")),
            };
            Some(json!({
                "msg_id": format!("asset_{batch_id}"),
                "role": "system",
                "kind": "asset",
                "text": text,
                "ts": first.ts,
                "assets": group,
            }))
        })
        .collect()
}

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

/// Model families whose 1M-context variant is the one in use here, keyed by the id as it appears
/// in the transcript. Needed only because of the exposure gap below; entries are deliberately
/// specific (`opus-5`, not `opus`) — a blanket family rule would also claim 1M for older Opus
/// releases that don't have it, and being wrong in that direction UNDERSTATES context usage,
/// which is the harmful direction (no warning before a surprise compaction).
///
/// `fable-5` covers `claude-fable-5` and `claude-fable-5-1`, which is how live transcripts spell
/// it (29k+ lines across this machine's sessions, never once with `[1m]`). A handful say bare
/// `fable`; those fall to the `observed_tokens` backstop, which is the right place for a spelling
/// we have seen eight times.
///
/// **The residual risk is per-ACCOUNT, not per-version, and there is no backstop for it.** These
/// entries assert an entitlement, and entitlements differ between maiLink users — a 1M grant on
/// this account says nothing about anyone else's. Guessing 1M for an account that only has 200k
/// OVERSTATES the window, the gauge reads a fifth of the truth, and unlike the understating
/// direction nothing self-corrects it. Add a family here only when that tier has no 200k variant
/// to be wrong about.
const ASSUMED_1M_MODELS: [&str; 3] = ["opus-4-8", "opus-5", "fable-5"];

/// The context window for a model id: 1M-context variants vs the 200k default.
///
/// The `[1m]` variant marker exists in Claude Code — its statusLine input carries it on
/// `.model.id` — but NOT in the transcript's `message.model`, which reports a bare
/// `claude-opus-5` for 1M and 200k sessions alike (verified across live transcripts). maiTerm
/// never receives the statusLine, so the transcript is all we have and the variant has to be
/// inferred. Hence [`ASSUMED_1M_MODELS`].
///
/// `observed_tokens` is the session's current context usage and acts as a self-correcting
/// backstop: a session that has already exceeded 200k is definitively on a larger window,
/// whatever its id says. That keeps an unrecognized future 1M model merely wrong-until-200k
/// rather than permanently pegged at 100%, without needing a code change per model.
fn context_limit_for(model_id: &str, observed_tokens: u64) -> u64 {
    if model_id.contains("[1m]") || model_id.contains("-1m") {
        return 1_000_000;
    }
    if ASSUMED_1M_MODELS.iter().any(|m| model_id.contains(m)) {
        return 1_000_000;
    }
    if observed_tokens > 200_000 {
        return 1_000_000;
    }
    200_000
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
/// resume-before-init window. `effort` (Claude-only reasoning level) rides the same assistant
/// JSONL line as the usage block. None for Gemini tabs (no transcript source) or before the
/// first assistant turn.
fn build_meta(app: &AppState, tab_id: &str) -> Option<Value> {
    let (rt, sid) = resolved_session_for_tab(app, tab_id)?;
    let meta = transcript::meta_for(rt, &sid)?;
    let model_id = meta.model_id.as_deref().unwrap_or("");
    // Codex rollouts carry the window; Claude's is derived from the model id.
    let limit = meta
        .context_window
        .unwrap_or_else(|| context_limit_for(model_id, meta.context_tokens));
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
    // Reasoning-effort level (Claude-only; low/medium/high/xhigh/max) read from the transcript's
    // top-level `effort` field. Optional — omitted for effort-less models and non-Claude runtimes.
    if let Some(effort) = meta.effort {
        m["effort"] = json!(effort);
    }
    Some(m)
}

/// Everything the tab's agent has SAID since `since_ms`, joined oldest-first.
///
/// This is the tail Overlord reads to harvest an answer to a directive it typed. Overlord
/// injects raw text with no envelope (docs/overlord.md §3), so the agent on the other end
/// answers in its own terminal exactly as it would answer the human — it has no reason to
/// call `replyToOverlord` unless the directive asked it to. Without this, a directive that
/// asks a question gets answered into a void.
///
/// `since_ms` must come from the same clock as the transcript (pass the tab's previous
/// `last_turn_ts`, not the local wall clock): an SSH tab's transcript is written on the
/// REMOTE host, so comparing its timestamps against this machine's clock skews.
pub(crate) fn agent_reply_since(app: &AppState, tab_id: &str, since_ms: i64) -> Option<String> {
    let (rt, sid) = resolved_session_for_tab(app, tab_id)?;
    let turns = transcript::turns_for(rt, &sid, 40, transcript::ToolRender::Marker)?;
    let mut parts: Vec<String> = Vec::new();
    for t in turns.iter() {
        if t.get("role").and_then(|r| r.as_str()) != Some("agent") {
            continue;
        }
        if t.get("ts").and_then(|v| v.as_i64()).unwrap_or(0) <= since_ms {
            continue;
        }
        if let Some(text) = t.get("text").and_then(|v| v.as_str()) {
            if !text.trim().is_empty() {
                parts.push(text.to_string());
            }
        }
    }
    if parts.is_empty() {
        return None;
    }
    Some(parts.join("\n\n"))
}

/// Per-tab agent facts for the Overlord engine (docs/overlord.md §5 detection table) — the
/// cheap, cached signals the frontend rules engine polls: context gauge, last-real-turn
/// timestamp, runtime, session id. Everything comes from the (mtime,len)-gated tail-facts
/// cache in `transcript.rs`, so steady-state cost per tab is one stat. snake_case keys —
/// this is a maiTerm frontend surface, not the mailink phone protocol.
///
/// NOTE `last_turn_ts` is absent for a tab whose transcript this machine cannot read.
/// Callers must treat its absence as "unknown", never as "no turns". For an SSH tab the
/// JSONL lives on the remote host, and the mirror shadows it whenever Overlord or maiLink is
/// enabled — but only for Claude, and only while the tab's bridge tunnel is up. A Codex or
/// Gemini SSH tab, or one whose bridge is down, still has no facts at all.
pub(crate) fn overlord_tab_facts(app: &AppState, tab_id: &str) -> Option<Value> {
    let (rt, sid) = resolved_session_for_tab(app, tab_id)?;
    let mut v = json!({
        "runtime": rt.as_key(),
        "session_id": sid,
    });
    if let Some(meta) = transcript::meta_for(rt, &sid) {
        let model_id = meta.model_id.as_deref().unwrap_or("");
        let limit = meta
            .context_window
            .unwrap_or_else(|| context_limit_for(model_id, meta.context_tokens));
        let pct = ((meta.context_tokens as f64 / limit as f64) * 100.0)
            .round()
            .clamp(0.0, 100.0) as u64;
        v["context_used"] = json!(meta.context_tokens);
        v["context_limit"] = json!(limit);
        v["context_pct"] = json!(pct);
        if !model_id.is_empty() {
            v["model"] = json!(display_model_for(rt, model_id));
        }
    }
    if let Some(ts) = transcript::last_turn_ts_for(rt, &sid) {
        v["last_turn_ts"] = json!(ts);
    }
    if let Some(of) = transcript::overlord_facts_for(rt, &sid) {
        if let Some(ts) = of.last_commit_ts {
            v["last_commit_ts"] = json!(ts);
        }
        if let Some(ts) = of.todos_ts {
            v["todos_ts"] = json!(ts);
        }
        if let Some(ts) = of.last_compact_ts {
            v["last_compact_ts"] = json!(ts);
        }
        // Task lists, best source first. Claude Code's own store is authoritative and
        // COMPLETE; the transcript tail only ever saw whatever fit in its window. An SSH
        // session's store lives on the remote host, so `claude_task_store` reads the
        // mirror's shadow of it — the tail is the floor for those tabs now, not the norm.
        //
        // `tracked` is the fact the tail cannot supply: whether this session has a task
        // list AT ALL. Claude Code deletes the task files once every task is completed, so
        // an empty-but-present store means "finished everything", not "never tracked" —
        // conflating those makes the board freeze finished rows and makes Overlord nudge
        // an agent to start a list at the moment it completed one.
        let store = if rt == AgentRuntime::Claude {
            transcript::claude_task_store(&sid).or_else(|| transcript::claude_todo_store(&sid))
        } else {
            None
        };
        match store {
            Some(todos) => {
                v["tracked"] = json!(true);
                v["todos"] = todos;
                v["todos_source"] = json!("store");
            }
            None => {
                if let Some(todos) = of.todos {
                    v["tracked"] = json!(true);
                    v["todos"] = todos;
                    v["todos_source"] = json!("transcript");
                }
            }
        }
    }
    Some(v)
}

/// tab_id → scrollback `updated_at` in unix ms (one DB read). SQLite stores `datetime('now')` as
/// `YYYY-MM-DD HH:MM:SS` UTC; normalize to RFC3339 (`T` + `Z`) for the shared transcript parser.
/// Used as the last-activity fallback for tabs without a Claude transcript (Codex/Gemini, or a
/// Claude tab before its first turn).
fn scrollback_times(app: &AppState) -> HashMap<String, u64> {
    let mut out = HashMap::new();
    if let Ok(rows) = app.scrollback_db.tab_times() {
        for (tab, updated) in rows {
            let ms = scrollback_ts_to_ms(&updated);
            if ms > 0 {
                out.insert(tab, ms as u64);
            }
        }
    }
    out
}

/// SQLite `datetime('now')` (`YYYY-MM-DD HH:MM:SS` UTC) → unix ms via the shared RFC3339 parser.
fn scrollback_ts_to_ms(updated: &str) -> i64 {
    transcript::rfc3339_to_ms(&format!("{}Z", updated.replace(' ', "T")))
}

/// Single-tab scrollback `updated_at` in unix ms — chat_detail's counterpart to
/// `scrollback_times`, so a one-tab build doesn't scan the whole roster.
fn scrollback_time_for(app: &AppState, tab_id: &str) -> Option<u64> {
    let updated = app.scrollback_db.tab_time(tab_id).ok().flatten()?;
    let ms = scrollback_ts_to_ms(&updated);
    (ms > 0).then_some(ms as u64)
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
fn last_activity_ts(app: &AppState, tab_id: &str, scrollback_ts: Option<u64>, now: u64) -> u64 {
    resolved_session_for_tab(app, tab_id)
        .and_then(|(rt, sid)| transcript::last_turn_ts_for(rt, &sid))
        .or(scrollback_ts)
        .unwrap_or(now)
}

/// The in-memory-only slice of `build_chats` that the periodic tickers (WS diff loop, doorbell)
/// actually consume: identity + the diffed flags + state/prompt. Deliberately NO lastActivityTs,
/// meta, or preview — those need the scrollback DB and transcript tails, and rebuilding them for
/// every tab every ~2s was the permanent background load that kept the scrollback mutex ~40%
/// held and made human-initiated fetches queue for seconds (the "chat-list storm" was maiTerm's
/// own tickers, not the phone). The one per-tab I/O left is the dormant fallback's stat-gated
/// tail read, and only for tabs with no session entry. Tabs that actually transition get
/// enriched on demand (enriched_chat_state_event); full builds remain for REST + the
/// once-per-connection WS snapshot.
fn build_chat_summaries(app: &AppState) -> Vec<Value> {
    let states = session_states(app);
    let now = now_ms();
    designated_tabs(app)
        .into_iter()
        .map(|t| {
            let (state, runtime, tool, registered, _finished) = match states.get(&t.tab_id) {
                Some((st, rt, tool, fin)) => (map_state(*st), runtime_key(*rt), tool.clone(), true, *fin),
                None => {
                    let st = if tab_looks_live_despite_no_session(app, &t.tab_id, now) {
                        "active"
                    } else {
                        "dormant"
                    };
                    // An unregistered tab has no session that could have finished anything.
                    (st, runtime_key(t.runtime), None, false, false)
                }
            };
            // Same prompt-kind rule as build_chats: an open AskUserQuestion outranks permission.
            let prompt_kind = if tool.as_deref() == Some("AskUserQuestion") {
                Some("question")
            } else if state == "permission" {
                Some("permission")
            } else {
                None
            };
            json!({
                "tabId": t.tab_id,
                "title": t.title,
                "workspaceSuspended": t.workspace_suspended,
                "mesh": t.mesh,
                "runtime": runtime,
                "state": state,
                // Diffed by the WS ticker like the other flags, so a tab that registers (or
                // loses its registration) re-renders the re-initialize affordance promptly.
                "registered": registered,
                "prompt": prompt_kind,
            })
        })
        .collect()
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
            let (state, runtime, tool, registered, finished) = match states.get(&t.tab_id) {
                Some((st, rt, tool, fin)) => (map_state(*st), runtime_key(*rt), tool.clone(), true, *fin),
                None => {
                    let st = if tab_looks_live_despite_no_session(app, &t.tab_id, now) {
                        "active"
                    } else {
                        "dormant"
                    };
                    // An unregistered tab has no session that could have finished anything.
                    (st, runtime_key(t.runtime), None, false, false)
                }
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
                "workspaceId": t.workspace_id,
                // Surfaced so the phone shows "Resume workspace" instead of a dead-end Initialize.
                "workspaceSuspended": t.workspace_suspended,
                // Mesh Workspace flag — the phone badges the group and offers Initialize-all.
                "mesh": t.mesh,
                "runtime": runtime,
                "state": state,
                // Additive field: lets clients (and our own tickers) see prompt-kind changes
                // that don't move `state` — e.g. an AskUserQuestion opening at state=="active".
                "prompt": prompt_kind,
                // ask_open guards the case where a build leaves an open AskUserQuestion at
                // state=="active" — it still needs to surface as unread in the inbox.
                // NOT plain `state == "idle"`. Since a starting session registers as idle, that
                // word also means "alive at an empty prompt" — the resting state of every tab
                // after a maiTerm restart — which made every chat permanently unread and the
                // phone's Focus filter identical to All. `finished` is the direct answer to the
                // question `unread` is actually asking: has a turn ENDED here.
                "unread": ask_open || state == "permission" || (state == "idle" && finished),
                // `state` alone can't express "a live agent that never registered": the fallback
                // has to pick a word, and both are wrong — it isn't dormant (there's a live agent)
                // and it isn't working (it's sitting at a prompt). Reporting it as active also hid
                // the Initialize affordance, which is the fix for exactly this state. So the claim
                // is split out: `registered:false` means the state came from the liveness fallback
                // rather than a tracked session, and the client should offer re-initialize.
                "registered": registered,
                "lastActivityTs": last_activity_ts(app, &t.tab_id, scrollback.get(&t.tab_id).copied(), now),
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
    // Single-row lookup — a one-tab build has no business scanning the whole roster's times.
    let scrollback_ts = scrollback_time_for(app, tab_id);
    let ms_scrollback = ph.elapsed().as_millis(); // scrollback_db read conn + one indexed row

    let ph = std::time::Instant::now();
    let last_activity = last_activity_ts(app, tab_id, scrollback_ts, now);
    let ms_activity = ph.elapsed().as_millis(); // locate_jsonl + last-turn tail read
    let (state, runtime, tool, registered, finished) = match states.get(tab_id) {
        Some((st, rt, tool, fin)) => (map_state(*st), runtime_key(*rt), tool.clone(), true, *fin),
        None => {
            let st = if tab_looks_live_despite_no_session(app, tab_id, now) {
                "active"
            } else {
                "dormant"
            };
            // An unregistered tab has no session that could have finished anything.
            (st, runtime_key(meta.runtime), None, false, false)
        }
    };

    // Per-turn source markdown from the session transcript (Claude) so the phone's GFM renderer
    // lights up; falls back to the distilled terminal scrape for other runtimes / when no
    // transcript is found. See mailink/transcript.rs.
    let ph = std::time::Instant::now();
    let mut transcript = build_transcript(app, tab_id, now);
    // Files an agent sent belong where it sent them, so merge by ts rather than appending. The
    // live terminal_snapshot turn carries `now`, so it stays last on its own.
    let sent = asset_turns(tab_id);
    if !sent.is_empty() {
        transcript.extend(sent);
        transcript.sort_by_key(|t| t.get("ts").and_then(|v| v.as_u64()).unwrap_or(0));
    }
    let ms_transcript = ph.elapsed().as_millis(); // 8 MiB tail read + distill

    let mut detail = json!({
        "tabId": meta.tab_id,
        "title": meta.title,
        "workspace": meta.workspace,
        "workspaceId": meta.workspace_id,
        "workspaceSuspended": meta.workspace_suspended,
        "mesh": meta.mesh,
        "runtime": runtime,
        "state": state,
        // Same rule as build_chats, both halves: an open AskUserQuestion is unread even if a
        // build leaves the session state at "active", and a bare "idle" is not a result.
        "unread": tool.as_deref() == Some("AskUserQuestion")
            || state == "permission"
            || (state == "idle" && finished),
        "registered": registered,
        "lastActivityTs": last_activity,
        "transcript": transcript,
    });

    // Per-agent telemetry strip (model + context gauge). See build_meta.
    let ph = std::time::Instant::now();
    if let Some(agent_meta) = build_meta(app, tab_id) {
        detail["meta"] = agent_meta;
    }
    let ms_meta = ph.elapsed().as_millis(); // locate_jsonl + meta tail read

    // The session's Claude Code task board (TaskCreate/TaskUpdate — the strip above the prompt
    // in the TUI), invisible in structured chat without this. Present only when non-empty;
    // live updates ride the WS `tasks` event (see stream_new_messages). mailink/tasks.rs.
    if let Some((AgentRuntime::Claude, sid)) = resolved_session_for_tab(app, tab_id) {
        if let Some(board) = tasks::tasks_for_session(&sid) {
            detail["tasks"] = json!(board);
        }
    }

    // Messages typed while the agent was busy and NOT yet consumed. The phone renders these as
    // genuinely "queued" rather than a spinner, and it's the precondition for offering to pull one
    // back — an already-consumed message can't be recalled. Same text the queued turn will echo.
    if let Some((AgentRuntime::Claude, sid)) = resolved_session_for_tab(app, tab_id) {
        let queued: Vec<Value> = transcript::pending_queue(&sid, QUEUE_SCAN_BYTES)
            .into_iter()
            .map(|(text, ts)| json!({ "text": text, "queuedAt": ts }))
            .collect();
        if !queued.is_empty() {
            detail["queued"] = json!(queued);
        }
    }

    // The `/goal` condition the agent is being held to, if any — the answer to "is it actually
    // going to finish, and what's left" from a phone. Detail-only on purpose: it costs a tail read
    // per call, which is fine for one open thread and is exactly what must never go near the
    // roster tickers. See transcript::goal_for_session.
    if let Some((AgentRuntime::Claude, sid)) = resolved_session_for_tab(app, tab_id) {
        if let Some(goal) = transcript::goal_for_session(&sid) {
            detail["goal"] = json!(goal);
        }
    }

    // Background shells (`Bash run_in_background` — the TUI's /bashes list), with liveness settled
    // against the process table so no Stop button is offered for a dead process. mailink/shells.rs.
    let ph = std::time::Instant::now();
    let shells = shell_roster(app, tab_id);
    if !shells.is_empty() {
        detail["shells"] = json!(shells.iter().map(|s| s.to_json()).collect::<Vec<_>>());
    }
    let ms_shells = ph.elapsed().as_millis(); // transcript scan + cached ps sweep

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
            "mailink slow chat_detail tab={} total={}ms [tabs(app_data)={} states(sessions)={} scrollback(db)={} activity={} transcript={} meta={} shells={}]",
            &tab_id[..tab_id.len().min(8)],
            ms_total,
            ms_tabs,
            ms_states,
            ms_scrollback,
            ms_activity,
            ms_transcript,
            ms_meta,
            ms_shells,
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
    // tab_id → (last observed attn_key, whether it had a tracked session then). Both halves are
    // load-bearing — see `rings_attention`: the first sighting of a tab baselines silently, and so
    // does the tab's session row first appearing, which is a registration edge rather than a
    // finished turn.
    let mut last: HashMap<String, (String, bool)> = HashMap::new();
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

        // Summaries only — the doorbell consumes tabId/title/state/prompt and nothing else, and
        // this loop runs forever at 2s whether or not a phone exists (being UNcovered is exactly
        // when it must ring). The full build_chats here was the fixed-interval "chat-list storm":
        // scrollback + per-tab transcript reads for 100 tabs, every 2s, holding the scrollback
        // mutex ~40% of wall-clock so every human-initiated fetch queued behind it.
        let chats = build_chat_summaries(&app);
        let mut current = std::collections::HashSet::new();
        for c in &chats {
            let tab = c["tabId"].as_str().unwrap_or_default().to_string();
            let key = attn_key(
                c["state"].as_str().unwrap_or_default(),
                c["prompt"].as_str(),
            );
            let title = c["title"].as_str().unwrap_or_default().to_string();
            let reg = c["registered"].as_bool().unwrap_or(true);
            current.insert(tab.clone());
            let prev = last.insert(tab.clone(), (key.clone(), reg));

            // Fire only on an announceable edge, while uncovered — see `rings_attention`.
            if covered {
                continue;
            }
            let (prev_key, prev_reg) = match &prev {
                Some((k, r)) => (Some(k.as_str()), *r),
                None => (None, false),
            };
            if rings_attention(prev_key, prev_reg, &key) {
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
        assert_eq!(display_model("claude-opus-5[1m]"), "Opus 5");
        assert_eq!(display_model("claude-opus-5"), "Opus 5");
        // 1M-context variants vs the 200k default. The marker resolves 1M outright…
        assert_eq!(context_limit_for("claude-opus-4-8[1m]", 0), 1_000_000);
        assert_eq!(context_limit_for("claude-opus-4-8-1m", 0), 1_000_000);
        // …and the assumed-1M ids resolve without one, since the transcript id never carries it.
        assert_eq!(context_limit_for("claude-opus-4-8", 0), 1_000_000);
        assert_eq!(context_limit_for("claude-opus-5", 0), 1_000_000);
        // Both spellings Fable actually uses in live transcripts. Missing these read a 200k gauge
        // on a 1M session, pegging it at ~100% until the observed-tokens backstop tripped.
        assert_eq!(context_limit_for("claude-fable-5", 0), 1_000_000);
        assert_eq!(context_limit_for("claude-fable-5-1", 0), 1_000_000);
        // Everything else defaults to 200k — including other Opus releases, which a blanket
        // "any opus is 1M" rule would have wrongly promoted.
        assert_eq!(context_limit_for("claude-sonnet-4-5", 0), 200_000);
        assert_eq!(context_limit_for("claude-opus-4-7", 0), 200_000);
        // Backstop: an unrecognized model already past 200k is definitively on a bigger window,
        // so the gauge self-corrects instead of pegging at 100% forever.
        assert_eq!(context_limit_for("claude-something-new", 200_000), 200_000);
        assert_eq!(context_limit_for("claude-something-new", 200_001), 1_000_000);
    }

    #[test]
    fn a_tab_coming_up_after_a_restart_must_not_ring_finished() {
        let dormant = attn_key("dormant", None);
        let active = attn_key("active", None);
        let idle = attn_key("idle", None);

        // THE REPORTED STORM. `agent_sessions` is in-memory, so at launch every tab baselines
        // dormant + unregistered; seconds later its SessionStart hook inserts a row that maps to
        // "idle". That is a textbook transition into attention, and the desktop having been down
        // means nothing is `covered` — so every resumed tab rang "Agent finished" at once.
        assert!(!rings_attention(Some(&dormant), false, &idle));
        // Same edge from the live-agent fallback's "active" — also a registration edge.
        assert!(!rings_attention(Some(&active), false, &idle));

        // What must still ring: a real turn ending on a tab whose session row already existed.
        assert!(rings_attention(Some(&active), true, &idle));
        // ...and a prompt opening on one.
        assert!(rings_attention(Some(&active), true, &attn_key("permission", Some("permission"))));
        assert!(rings_attention(Some(&active), true, &attn_key("active", Some("question"))));

        // Unchanged guards: a first sighting baselines silently whatever its registration...
        assert!(!rings_attention(None, true, &idle));
        // ...and a tab that was ALREADY wanting a human doesn't re-ring.
        assert!(!rings_attention(Some(&idle), true, &idle));
        assert!(!rings_attention(Some(&idle), true, &attn_key("permission", Some("permission"))));
        // Leaving attention is not an edge into it.
        assert!(!rings_attention(Some(&idle), true, &active));
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
    fn a_resumed_agent_is_not_unread_until_it_finishes_something() {
        use crate::state::app_state::AgentSessionInfo;
        use crate::state::workspace::{WindowData, Workspace};
        let app = AppState::new();
        let (fresh_tab, done_tab) = {
            let mut data = app.app_data.write();
            data.preferences.mailink_expose_all = true;
            let mut win = WindowData::new("main".into());
            let mut ws = Workspace::new("ENAGIC".into());
            ws.panes[0].tabs.push(crate::state::workspace::Tab::new("agent-2".into()));
            for t in &mut ws.panes[0].tabs {
                t.runtime = Some(AgentRuntime::Claude);
            }
            let ids = (ws.panes[0].tabs[0].id.clone(), ws.panes[0].tabs[1].id.clone());
            win.workspaces.push(ws);
            data.windows.push(win);
            ids
        };
        let mk = |tab: &str, state: AgentSessionState, finished: bool| AgentSessionInfo {
            runtime: AgentRuntime::Claude,
            tab_id: tab.to_string(),
            cwd: None,
            state,
            tool_name: None,
            tool_detail: None,
            pending_question: None,
            pending_question_at: None,
            model: None,
            transcript_path: None,
            finished_a_turn: finished,
            connection_id: None,
        };
        {
            let mut s = app.agent_sessions.write();
            // What a maiTerm restart produces: the SessionStart hook registers the agent, and
            // nothing has happened since.
            s.insert("s1".into(), mk(&fresh_tab, AgentSessionState::WaitingInput, false));
            // A real finished turn. Note it is ALSO reported as state "idle" — which is exactly
            // why `state` alone could not tell these two apart, and why every chat became a
            // permanent member of the phone's Focus list.
            s.insert("s2".into(), mk(&done_tab, AgentSessionState::Stopped, true));
        }
        let chats = build_chats(&app);
        let unread = |tab: &str| {
            chats
                .iter()
                .find(|c| c["tabId"] == json!(tab))
                .expect("tab listed")["unread"]
                .as_bool()
                .expect("unread present")
        };
        assert_eq!(unread(&done_tab), true, "a finished turn is a result to read");
        assert_eq!(unread(&fresh_tab), false, "coming up is not a result");
        // Both still report the same wire state — the fix is NOT a state change.
        for t in [&fresh_tab, &done_tab] {
            let c = chats.iter().find(|c| c["tabId"] == json!(t)).unwrap();
            assert_eq!(c["state"], json!("idle"));
        }
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
            finished_a_turn: false,
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
    fn designated_tabs_surface_workspace_id_and_suspension() {
        use crate::state::workspace::{WindowData, Workspace};
        let app = AppState::new();
        let (ws_id, tab_id) = {
            let mut data = app.app_data.write();
            data.preferences.mailink_expose_all = true;
            let mut win = WindowData::new("main".into());
            let mut ws = Workspace::new("ENAGIC".into());
            ws.suspended = true;
            ws.bridge_all = true;
            // The auto-created Terminal tab needs a detected runtime to be exposed in expose-all.
            ws.panes[0].tabs[0].runtime = Some(AgentRuntime::Claude);
            let ids = (ws.id.clone(), ws.panes[0].tabs[0].id.clone());
            win.workspaces.push(ws);
            data.windows.push(win);
            ids
        };
        let metas = designated_tabs(&app);
        let m = metas
            .iter()
            .find(|m| m.tab_id == tab_id)
            .expect("suspended-workspace tab is still exposed to maiLink");
        assert!(m.workspace_suspended);
        assert_eq!(m.workspace_id, ws_id);
        assert!(m.mesh, "bridge_all surfaces as the mesh flag");
    }

    #[test]
    fn contested_session_id_resolves_only_to_the_transcript_named_host() {
        // Tab duplication copies the session-id var on purpose (reload / fork workflows), so
        // two claimants is a legitimate transient state — but only the transcript-named host
        // may render the conversation. Unknown host (no locatable transcript) → None for BOTH:
        // rendering nothing beats rendering someone else's conversation. Unique sids resolve
        // as before.
        use crate::state::workspace::{WindowData, Workspace};
        let app = AppState::new();
        let sid = "contested-0000-4000-8000-aiterm-test000";
        let (dup_a, dup_b, solo) = {
            let mut data = app.app_data.write();
            let mut win = WindowData::new("main".into());
            let mut ws = Workspace::new("ENAGIC".into());
            while ws.panes[0].tabs.len() < 3 {
                let n = ws.panes[0].tabs.len();
                ws.panes[0].tabs.push(crate::state::workspace::Tab::new(format!("t{n}")));
            }
            for tab in &mut ws.panes[0].tabs {
                tab.runtime = Some(AgentRuntime::Claude);
            }
            ws.panes[0].tabs[0].trigger_variables.insert("claudeSessionId".into(), sid.into());
            ws.panes[0].tabs[1].trigger_variables.insert("claudeSessionId".into(), sid.into());
            ws.panes[0].tabs[2].trigger_variables.insert("claudeSessionId".into(), "sid-solo".into());
            let ids = (
                ws.panes[0].tabs[0].id.clone(),
                ws.panes[0].tabs[1].id.clone(),
                ws.panes[0].tabs[2].id.clone(),
            );
            win.workspaces.push(ws);
            data.windows.push(win);
            ids
        };

        // Phase 1 — no transcript anywhere: unknown host, both claimants refuse.
        assert_eq!(persisted_session_for_tab(&app, &dup_a), None);
        assert_eq!(persisted_session_for_tab(&app, &dup_b), None);
        assert_eq!(
            persisted_session_for_tab(&app, &solo),
            Some((AgentRuntime::Claude, "sid-solo".to_string()))
        );

        // Phase 2 — a transcript whose SessionStart hook line names dup_b as the most recent
        // host: dup_b resolves, dup_a still refuses. (Shadow-mirror dir, same resolution path
        // as the locate_jsonl test.)
        let dir = super::mirror::shadow_dir().expect("data dir resolvable");
        std::fs::create_dir_all(&dir).expect("create shadow dir");
        let path = dir.join(format!("{sid}.jsonl"));
        let line = json!({
            "type": "user",
            "message": { "content": format!(
                "SessionStart hook success: Your maiTerm tab ID is {dup_b}. Your session ID is {sid}."
            )}
        });
        std::fs::write(&path, format!("{line}\n")).expect("write shadow transcript");
        // Phase 1's lookups failed and locate_jsonl remembers that for LOCATE_MISS_TTL. Writing
        // the shadow file by hand is standing in for the SSH mirror's fetch, which is the only
        // real producer of a transcript that did not exist a moment ago — so do what it does.
        transcript::forget_locate_miss(sid);
        let resolved_a = persisted_session_for_tab(&app, &dup_a);
        let resolved_b = persisted_session_for_tab(&app, &dup_b);
        let _ = std::fs::remove_file(&path); // clean up before asserting
        assert_eq!(resolved_a, None, "non-host claimant refuses");
        assert_eq!(
            resolved_b,
            Some((AgentRuntime::Claude, sid.to_string())),
            "transcript-named host renders"
        );
    }

    #[test]
    fn chat_summaries_carry_every_field_the_tickers_diff() {
        // The WS ticker diffs title/workspaceSuspended/mesh/registered and keys on attn_key(state,
        // prompt);
        // the doorbell needs tabId/title/state/prompt; chat_state events need runtime. If a field
        // the tickers consume ever drops out of the summary shape, the diff silently degrades
        // (e.g. every tick looks like a rename) — pin the shape here.
        use crate::state::workspace::{WindowData, Workspace};
        let app = AppState::new();
        let tab_id = {
            let mut data = app.app_data.write();
            data.preferences.mailink_expose_all = true;
            let mut win = WindowData::new("main".into());
            let mut ws = Workspace::new("ENAGIC".into());
            ws.suspended = true;
            ws.bridge_all = true;
            ws.panes[0].tabs[0].runtime = Some(AgentRuntime::Claude);
            let id = ws.panes[0].tabs[0].id.clone();
            win.workspaces.push(ws);
            data.windows.push(win);
            id
        };
        let summaries = build_chat_summaries(&app);
        let s = summaries
            .iter()
            .find(|c| c["tabId"] == json!(tab_id))
            .expect("designated tab summarized");
        assert!(s["title"].is_string());
        assert_eq!(s["workspaceSuspended"], json!(true));
        assert_eq!(s["mesh"], json!(true));
        assert_eq!(s["runtime"], json!("claude"));
        assert_eq!(s["state"], json!("dormant"), "no session entry, no live PTY");
        assert_eq!(
            s["registered"], json!(false),
            "no tracked session ⇒ the state came from the fallback, so the phone must be able to \
             offer re-initialize instead of trusting the word in `state`"
        );
        assert!(s["prompt"].is_null());
        // And the heavy fields must NOT be here — their absence is the whole point.
        assert!(s.get("lastActivityTs").is_none() && s.get("meta").is_none());
    }

    #[test]
    fn archived_chats_lists_and_resolves_by_tab_id() {
        use crate::state::workspace::{Tab, WindowData, Workspace};
        let app = AppState::new();
        let (ws_id, older_id, newer_id) = {
            let mut data = app.app_data.write();
            let mut win = WindowData::new("main".into());
            let mut ws = Workspace::new("ENAGIC".into());
            let mut older = Tab::new("agent-old".into());
            older.runtime = Some(AgentRuntime::Claude);
            older.archived_name = Some("Old Agent".into());
            older.archived_at = Some("2026-07-20T10:00:00Z".into());
            older.restore_cwd = Some("/tmp/old".into());
            let mut newer = Tab::new("agent-new".into());
            newer.archived_at = Some("2026-07-22T10:00:00Z".into());
            let ids = (ws.id.clone(), older.id.clone(), newer.id.clone());
            ws.archived_tabs.push(older);
            ws.archived_tabs.push(newer);
            win.workspaces.push(ws);
            data.windows.push(win);
            ids
        };

        let list = archived_chats(&app);
        assert_eq!(list.len(), 2);
        // Newest-archived first.
        assert_eq!(list[0]["tabId"].as_str(), Some(newer_id.as_str()));
        assert_eq!(list[1]["tabId"].as_str(), Some(older_id.as_str()));
        // Resolved name + workspace + captured cwd surface on the older entry.
        assert_eq!(list[1]["name"].as_str(), Some("Old Agent"));
        assert_eq!(list[1]["workspaceId"].as_str(), Some(ws_id.as_str()));
        assert_eq!(list[1]["runtime"].as_str(), Some("claude"));
        assert_eq!(list[1]["cwd"].as_str(), Some("/tmp/old"));

        // restore resolves an archived tab id → its owning workspace; unknown ids → None.
        assert_eq!(archived_workspace_of(&app, &older_id).as_deref(), Some(ws_id.as_str()));
        assert_eq!(archived_workspace_of(&app, "nope"), None);
    }

    #[test]
    fn live_fallback_flips_dormant_only_for_a_live_pty_with_a_recent_turn() {
        let now = 10_000_000u64;
        let recent = now - 1000; // 1s ago
        let stale = now - (LIVE_STATE_FALLBACK_MS + 1); // just past the window
        // Live PTY + a recent real turn → treat as active (drops the dormant banner).
        assert!(live_fallback_decision(true, Some(recent), now));
        // No live PTY → genuinely dormant regardless of transcript recency (e.g. suspended).
        assert!(!live_fallback_decision(false, Some(recent), now));
        // Live PTY but the last real turn aged out → revert to dormant (self-correcting).
        assert!(!live_fallback_decision(true, Some(stale), now));
        // Live PTY but no resolvable transcript turn at all → dormant.
        assert!(!live_fallback_decision(true, None, now));
        // A turn timestamp slightly in the future (clock skew) is still "recent", not underflow.
        assert!(live_fallback_decision(true, Some(now + 5000), now));
    }

    #[test]
    fn registering_reaches_an_open_thread_even_when_the_state_word_never_moves() {
        // The reported failure: a live-but-unregistered tab already reads "active" (the liveness
        // fallback), so the init that registers it moves nothing the attention key can see. A
        // key-only test emitted no frame, `chats_changed` only refreshes the ROSTER, and the
        // "Running but not registered" banner outlived the init the operator asked for.
        assert!(
            state_frame_needed(Some("active"), "active", true),
            "a registration flip must reach the open thread on its own"
        );
        // And the flip must be the only extra reason — a settled tab still stays quiet.
        assert!(!state_frame_needed(Some("active"), "active", false));
        // The ordinary trigger is untouched.
        assert!(state_frame_needed(Some("active"), "idle_done", false));
        assert!(state_frame_needed(None, "active", false), "a tab we've never seen");
    }

    #[test]
    fn a_byte_range_is_resolved_or_refused_but_never_guessed() {
        use RangeAsk::*;
        let len = 1000;
        // The ordinary forms.
        assert_eq!(parse_range("bytes=0-99", len), Part(0, 99));
        assert_eq!(parse_range("bytes=500-", len), Part(500, 999), "resume from an offset");
        assert_eq!(parse_range("bytes=-100", len), Part(900, 999), "the last N bytes");
        assert_eq!(parse_range(" bytes=0-0 ", len), Part(0, 0), "one byte, whitespace tolerated");
        // A client may ask past the end; that is a clamp, not an error.
        assert_eq!(parse_range("bytes=900-99999", len), Part(900, 999));
        assert_eq!(parse_range("bytes=-99999", len), Part(0, 999), "suffix longer than the file");

        // THE ONE THAT MATTERS. A start past the end must be 416, never a silent 200 with the
        // whole file: a resuming client writes those bytes at ITS offset, so answering from the
        // beginning corrupts the download rather than failing it.
        assert_eq!(parse_range("bytes=1000-", len), Unsatisfiable);
        assert_eq!(parse_range("bytes=1200-1300", len), Unsatisfiable);
        assert_eq!(parse_range("bytes=50-40", len), Unsatisfiable, "backwards");
        assert_eq!(parse_range("bytes=-0", len), Unsatisfiable, "zero-length suffix");
        assert_eq!(parse_range("bytes=0-0", 0), Unsatisfiable, "empty file");

        // Anything we don't implement or can't parse falls back to the whole file, which is a
        // legal answer to any range request.
        assert_eq!(parse_range("bytes=0-9,20-29", len), Whole, "multi-range not implemented");
        assert_eq!(parse_range("items=0-99", len), Whole, "not a byte range");
        assert_eq!(parse_range("bytes=abc-def", len), Whole);
        assert_eq!(parse_range("garbage", len), Whole);
    }

    #[test]
    fn a_download_keeps_its_real_filename() {
        let d = content_disposition("report.pdf");
        assert!(d.contains("filename=\"report.pdf\""));
        // Non-ASCII survives via filename*, and the plain form stays ASCII-safe rather than
        // emitting raw bytes a header parser may reject.
        let d = content_disposition("résumé.pdf");
        assert!(d.contains("filename*=UTF-8''r%C3%A9sum%C3%A9.pdf"), "{d}");
        assert!(d.contains("filename=\"r_sum_.pdf\""), "{d}");
        // A quote in the name must not escape the quoted string.
        assert!(!content_disposition("a\"b.txt").contains("\"a\"b.txt\""));
    }

    #[test]
    fn a_state_frame_never_omits_what_the_phone_merges() {
        // The frame is merged over a row the REST build produced, so a field it leaves out is
        // indistinguishable from a field it is claiming is empty. Every field but `meta` must
        // therefore be present on every frame, carrying a real null where there is nothing.
        let ev = chat_state_event(&json!({
            "tabId": "t1", "state": "active", "runtime": "claude",
            "registered": true, "prompt": Value::Null,
        }));
        for f in ["type", "tabId", "state", "runtime", "registered", "prompt", "ts", "lastActivityTs"] {
            assert!(ev.get(f).is_some(), "`{f}` must be present on every chat_state frame");
        }
        // `meta` is the one deliberate omission: absent means "unknown", so a transient failure
        // to resolve a transcript can't blank a live context gauge.
        assert!(ev.get("meta").is_none());
    }

    #[test]
    fn answering_an_ask_tells_the_phone_the_prompt_is_gone() {
        // This frame is what an answered AskUserQuestion produces: attn_key moves from
        // "active|question" to "active|", so a frame fires — but no `attention` event (those
        // only fire INTO attention) and no `chats_changed` (the roster diff doesn't watch
        // prompt). If the frame withholds `prompt`, nothing ever tells the phone the ask is
        // over and it stays pinned on a stale value.
        let ev = chat_state_event(&json!({
            "tabId": "t1", "state": "active", "runtime": "claude", "registered": true,
            "prompt": Value::Null,
        }));
        assert_eq!(ev["prompt"], Value::Null, "an explicit null, not an omission");
        // And the opening edge still carries the kind, so the pin can be raised from the frame.
        let ev = chat_state_event(&json!({
            "tabId": "t1", "state": "active", "runtime": "claude", "registered": true,
            "prompt": "question",
        }));
        assert_eq!(ev["prompt"], json!("question"));
    }

    #[test]
    fn a_state_frame_says_whether_the_tab_is_registered() {
        // `state` cannot carry this: a live agent reads "active" registered or not, which is why
        // the banner needs its own field on the live path rather than a re-GET of the thread.
        let ev = chat_state_event(&json!({
            "tabId": "t1", "state": "active", "runtime": "claude", "registered": false,
        }));
        assert_eq!(ev["registered"], json!(false));
        // Absent (a caller that never computed it) must not read as "needs re-initialize".
        let ev = chat_state_event(&json!({ "tabId": "t1", "state": "active", "runtime": "claude" }));
        assert_eq!(ev["registered"], json!(true));
    }

    #[test]
    fn an_ask_may_be_injected_once_and_a_retry_is_refused() {
        let (tab, ask) = ("tab-inject-test", "q_tab-inject-test_1");
        assert!(claim_question_inject(tab, ask), "the first attempt must run");
        // The selector cannot be re-homed after a failed attempt (↑/↓ are unbound while the
        // free-text row holds focus), so a retry navigates from an unknown origin and can type
        // the operator's answer into a row that discards it.
        assert!(!claim_question_inject(tab, ask), "a retry on the same ask must be refused");
        assert!(!claim_question_inject(tab, ask));
        // A NEW ask on that tab starts a fresh untouched selector.
        assert!(claim_question_inject(tab, "q_tab-inject-test_2"));
        // Tabs don't interfere: one tab's spent ask says nothing about another's.
        assert!(claim_question_inject("other-tab", ask));
    }

    #[test]
    fn wake_never_replays_a_resume_into_a_running_agent() {
        // Agent alive → re-register it. Never a resume replay, even with one stored: that would
        // type an ssh+resume line into the running agent's composer and nest ssh.
        assert_eq!(wake_remedy(true, true), Ok("init"));
        assert_eq!(wake_remedy(true, false), Ok("init"));
        // Agent gone → restart it. `/maiterm init` here would just be run by the shell.
        assert_eq!(wake_remedy(false, true), Ok("resume"));
        // Agent gone with nothing to restart → refuse. This is the case that used to type the
        // user's message straight into bash, which then executed it.
        assert_eq!(wake_remedy(false, false), Err("no-agent"));
    }

    #[test]
    fn every_unreachable_reason_explains_itself_without_deferring_to_the_desktop() {
        let details: Vec<&str> = ["no-pty", "no-agent", "wake-timeout"]
            .iter()
            .map(|r| wake_detail(r))
            .collect();
        for d in &details {
            assert!(!d.is_empty());
            // maiLink's standing rule: a phone-reachable flow never tells the user to go to the
            // desktop — that defeats the point of the app.
            assert!(!d.to_lowercase().contains("desktop"), "{d}");
        }
        // Distinct reasons must not collapse to the same sentence — the fallback arm is
        // wake-timeout's, and a new reason added without a match arm would silently inherit it.
        let unique: std::collections::HashSet<&&str> = details.iter().collect();
        assert_eq!(unique.len(), details.len());
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
