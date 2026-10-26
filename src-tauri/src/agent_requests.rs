//! Agent requests that only the human can approve (docs/agent-requests.md).
//!
//! Some MCP tools change things an agent must not change on its own say-so: which tabs share a
//! mesh or a bridge (who can message whom), starting a new agent tab, and restarting or updating maiTerm. Those tools do not act.
//! They file a request here and answer "awaiting approval"; the human approves or denies it on a
//! card (the Loom's Decisions, a toast, the phone), and only then does the window that owns the
//! target carry it out, from the arguments stored HERE — never from anything the agent sends
//! later.
//!
//! The approval is decided only through `decide` — the Tauri command a click invokes and the
//! phone's authenticated endpoint. Nothing on the MCP side can reach it. An agent's own
//! AskUserQuestion is not evidence (its answer is whatever the agent says it was), and Claude's
//! tool-permission dialog is skipped by an allowlist or auto mode, so neither counts.
//!
//! In memory only: a request does not survive a restart, and a restart is one of the things that
//! can be asked for.

use serde::Serialize;
use serde_json::Value;
use std::sync::LazyLock;
use tauri::{AppHandle, Emitter};

/// How long a request waits for the human before it lapses.
const PENDING_TTL_MS: u64 = 30 * 60 * 1000;
/// How long a settled request is kept for `getRequestStatus` and the card's outcome line.
const SETTLED_TTL_MS: u64 = 60 * 60 * 1000;
/// How long an approved request may take to be carried out before it is reported failed.
const APPROVED_TTL_MS: u64 = 5 * 60 * 1000;
const RESTART_APPROVED_TTL_MS: u64 = 30 * 60 * 1000;
/// Pending requests one tab may hold: enough for a real reorganisation, not a flood of cards.
const MAX_PENDING_PER_TAB: usize = 5;
/// A Hive's land waits on the human a day (plan D14): a land is work already done and reviewed,
/// and the human may be away for the evening. Re-filed from `worker.json` after a restart.
const HIVE_PENDING_TTL_MS: u64 = 24 * 60 * 60 * 1000;
/// Rust carries an approved Hive land out itself and settles it once it is under way; a lock
/// wait or a merge in a big repo can take a while before that.
const HIVE_APPROVED_TTL_MS: u64 = 30 * 60 * 1000;

/// What a request asks for. A string on the wire (never a serde enum: an unknown variant in
/// stored state wipes it on a downgrade), checked against this list when filed.
pub const KINDS: [&str; 17] = [
    "mesh_enable",
    "mesh_disable",
    "mesh_link",
    "mesh_unlink",
    "tab_move",
    "workspace_move",
    "tab_create",
    "restart",
    // Reloading another agent's tab interrupts it; reloading a window drops its interface state
    // (editor buffers). An agent's reload of its OWN tab is not asked (it is the one interrupted).
    "tab_reload",
    "window_reload",
    // A 1:1 Agent Bridge (bridgeWith / disconnectBridge): the other way two agents come to
    // message each other. The phone draws a kind from its text fields, so new kinds need no
    // protocol change (docs/mailink-protocol.md §4.4).
    "bridge_connect",
    "bridge_fork",
    "bridge_disconnect",
    // A remote MCP server maiTerm signs in to and serves to every agent (docs/mcp-proxy.md §7).
    // Asked like a new agent: it changes what every agent session in its workspaces can reach.
    "mcp_add",
    // A Hive's land the human approves (docs/hive.md §8.1's config rule, Landing = Ask me, §8.3's
    // integration mode), and the coordinator's undo of one (§8.6). Filed and carried out by Rust
    // alone (`hive::land::approval`), never by a window.
    "hive_land",
    "hive_undo",
    // The coordinator's change of its Hive's settings (docs/hive.md §3.4): workers at once,
    // models, the hosts workers reach. Filed and carried out by Rust (`hive::change`).
    "hive_settings",
];

/// The kinds Rust files and carries out itself: no window files one, and an approval runs in
/// Rust (`hive::land::approval::on_approved`) instead of being handed to a window.
pub fn is_hive_kind(kind: &str) -> bool {
    matches!(kind, "hive_land" | "hive_undo" | "hive_settings")
}

/// An approved Hive request, carried out in Rust by the module that filed it.
fn hive_approved(app: &AppHandle, r: &AgentRequest) {
    if r.kind == crate::hive::change::KIND {
        crate::hive::change::on_approved(app, r);
    } else {
        crate::hive::land::approval::on_approved(app, r);
    }
}

/// A Hive request denied or lapsed: the module that filed it tells the coordinator.
fn hive_closed(app: &AppHandle, r: &AgentRequest) {
    if r.kind == crate::hive::change::KIND {
        crate::hive::change::on_closed(app, r);
    } else {
        crate::hive::land::approval::on_closed(app, r);
    }
}

fn pending_ttl(kind: &str) -> u64 {
    if is_hive_kind(kind) { HIVE_PENDING_TTL_MS } else { PENDING_TTL_MS }
}

#[derive(Debug, Clone, Serialize)]
pub struct AgentRequest {
    pub id: String,
    /// The tab whose agent asked (a STATED identity — see the tools' refusal lists).
    pub tab_id: String,
    /// The window that filed it, which owns the target and carries it out.
    pub window_label: String,
    pub kind: String,
    /// Exactly what runs on approval. Built by maiTerm from the tool call, never edited after.
    pub args: Value,
    /// What the card says will happen, written by maiTerm (not the agent), one line.
    pub summary: String,
    /// Consequences the human should weigh, one line each.
    pub effects: Vec<String>,
    /// The agent's stated reason, shown as the agent's words.
    pub reason: Option<String>,
    /// Who asked, as the tab read when it asked: a request can outlive the tab's name, or the
    /// tab (maiLink 0.23 shows it to approve against).
    pub asker_title: String,
    pub asker_workspace: String,
    pub requested_at: u64,
    /// When a pending request lapses.
    pub expires_at: u64,
    /// pending → approved → done | failed; pending → denied | expired.
    pub status: String,
    pub decided_at: Option<u64>,
    /// "desktop" | "phone".
    pub decided_by: Option<String>,
    pub outcome: Option<String>,
}

static REQUESTS: LazyLock<parking_lot::Mutex<Vec<AgentRequest>>> = LazyLock::new(Default::default);

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as u64)
}

/// For the events a lapse owes: set by the first filing, which is the only way a request (and so
/// a lapse) comes to exist.
static APP: std::sync::OnceLock<AppHandle> = std::sync::OnceLock::new();
/// Requests a sweep lapsed, waiting to be announced once the lock that found them is released.
static LAPSED: LazyLock<parking_lot::Mutex<Vec<AgentRequest>>> = LazyLock::new(Default::default);

/// Announce what a sweep lapsed, like any other settle: the card's toast goes, and the asking
/// agent is told (`agent-request-settled`). Call with the REQUESTS lock released.
fn flush_lapsed() {
    let lapsed = std::mem::take(&mut *LAPSED.lock());
    if lapsed.is_empty() {
        return;
    }
    if let Some(app) = APP.get() {
        for r in &lapsed {
            log::info!("[requests] {} {} ({})", r.kind, r.status, r.id);
            let _ = app.emit("agent-request-settled", r);
            // A Hive's land that lapsed puts its worker back; Rust tells the coordinator.
            if is_hive_kind(&r.kind) {
                hive_closed(app, r);
            }
        }
        let _ = app.emit("agent-requests-changed", ());
    }
}

/// Lapse what waited too long, forget what settled long ago.
fn sweep(list: &mut Vec<AgentRequest>, now: u64) {
    let mut moved = Vec::new();
    for r in list.iter_mut() {
        // By its own deadline: a Hive land re-filed after a restart keeps the one it had.
        if r.status == "pending" && now >= r.expires_at {
            r.status = "expired".into();
            r.decided_at = Some(now);
            moved.push(r.clone());
        }
        // Approved but never reported back: the window that was to carry it out closed or
        // reloaded. Said so, rather than showing "approved" for good.
        // A restart that installs an update first downloads it, which can take longer than any
        // other kind runs; reporting it failed meanwhile told the agent so while it went ahead.
        let ttl = match r.kind.as_str() {
            "restart" => RESTART_APPROVED_TTL_MS,
            k if is_hive_kind(k) => HIVE_APPROVED_TTL_MS,
            _ => APPROVED_TTL_MS,
        };
        if r.status == "approved" && now.saturating_sub(r.decided_at.unwrap_or(now)) >= ttl {
            r.status = "failed".into();
            r.outcome = Some("approved, but the window that was to carry it out never reported back".into());
            moved.push(r.clone());
        }
    }
    let moved = !moved.is_empty() && {
        LAPSED.lock().extend(moved);
        true
    };
    // Lapsing is found here, on a read, so it moves the revision itself: the phone's socket sends
    // the list only when that moves, and would otherwise keep showing a lapsed card.
    if moved {
        REVISION.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    }
    list.retain(|r| {
        r.status == "pending"
            || r.status == "approved"
            || now.saturating_sub(r.decided_at.unwrap_or(r.requested_at)) < SETTLED_TTL_MS
    });
}

/// Moves on every change, so the phone's socket sends the list only when it moved.
static REVISION: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);

/// The current revision, after lapsing anything due: a poller's check is what finds a lapse.
pub fn revision() -> u64 {
    sweep(&mut REQUESTS.lock(), now_ms());
    flush_lapsed();
    REVISION.load(std::sync::atomic::Ordering::Relaxed)
}

fn changed(app: &AppHandle) {
    REVISION.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let _ = app.emit("agent-requests-changed", ());
}

/// Pending requests per asking tab (`Chat.requestsWaiting`, maiLink 0.23).
pub fn waiting_by_tab() -> std::collections::HashMap<String, u64> {
    let mut out = std::collections::HashMap::new();
    for r in list().iter().filter(|r| r.status == "pending") {
        *out.entry(r.tab_id.clone()).or_insert(0) += 1;
    }
    out
}

/// A request as the phone draws it (maiLink protocol 0.23 `AgentRequest`). Every word on the card
/// is maiTerm's except `reason`.
pub fn wire(r: &AgentRequest) -> Value {
    serde_json::json!({
        "id": r.id,
        "tabId": r.tab_id,
        "asker": { "tabId": r.tab_id, "title": r.asker_title, "workspace": r.asker_workspace },
        "kind": r.kind,
        "summary": r.summary,
        "effects": r.effects,
        "reason": r.reason,
        "onDeny": "The agent is told you denied it.",
        "status": r.status,
        "requestedAt": r.requested_at,
        "expiresAt": r.expires_at,
    })
}

/// File a request (the tool side). An identical one already pending is returned instead of a
/// second card. `Err` when the kind is unknown or the tab already has its fill of pending ones.
#[allow(clippy::too_many_arguments)]
pub fn file(
    app: &AppHandle,
    state: &crate::state::AppState,
    tab_id: &str,
    window_label: &str,
    kind: &str,
    args: Value,
    summary: String,
    effects: Vec<String>,
    reason: Option<String>,
) -> Result<AgentRequest, String> {
    if !KINDS.contains(&kind) {
        return Err(format!("unknown request kind {kind}"));
    }
    // Its card's words and arguments are Rust's, from the Hive's own records: a window (and so
    // an agent's tool call through one) never files one.
    if is_hive_kind(kind) {
        return Err("maiTerm files a Hive's land and undo requests itself".to_string());
    }
    crate::hive::refuse_request(&state.app_data.read(), kind, &args)?;
    file_inner(app, state, tab_id, window_label, kind, args, summary, effects, reason, None)
}

/// A Hive's land or undo card (docs/hive.md §8.1, §8.3), filed by Rust from the coordinator's
/// tab. One per land (`args.land_id`): the same land again is the same card, and a newer land of
/// the same task supersedes an older card. `keep` is the id and filing time a restart re-files
/// it under, so it keeps its place and its deadline.
#[allow(clippy::too_many_arguments)]
pub fn file_hive(
    app: &AppHandle,
    state: &crate::state::AppState,
    tab_id: &str,
    window_label: &str,
    kind: &str,
    args: Value,
    summary: String,
    effects: Vec<String>,
    reason: Option<String>,
    keep: Option<(String, u64)>,
) -> Result<AgentRequest, String> {
    if !is_hive_kind(kind) {
        return Err(format!("{kind} isn't a Hive request"));
    }
    let superseded = supersede(&mut REQUESTS.lock(), kind, &args, now_ms());
    for r in &superseded {
        log::info!("[requests] {} failed ({}): superseded", r.kind, r.id);
        let _ = app.emit("agent-request-settled", r);
    }
    file_inner(app, state, tab_id, window_label, kind, args, summary, effects, reason, keep)
}

/// Pending Hive requests of the same task for another land end as failed: the task's newer
/// land is the one the human is asked about.
fn supersede(list: &mut [AgentRequest], kind: &str, args: &Value, now: u64) -> Vec<AgentRequest> {
    let key = |a: &Value, k: &str| a.get(k).and_then(Value::as_str).map(str::to_string);
    let (hive, task, land) = (key(args, "hive_id"), key(args, "task_id"), key(args, "land_id"));
    if kind == crate::hive::change::KIND {
        return list
            .iter_mut()
            .filter(|r| r.status == "pending" && r.kind == kind && key(&r.args, "hive_id") == hive)
            .map(|r| {
                r.status = "failed".into();
                r.decided_at = Some(now);
                r.outcome = Some("superseded by a newer settings change".to_string());
                r.clone()
            })
            .collect();
    }
    if task.is_none() {
        return Vec::new();
    }
    list.iter_mut()
        .filter(|r| r.status == "pending" && r.kind == kind && key(&r.args, "hive_id") == hive && key(&r.args, "task_id") == task && key(&r.args, "land_id") != land)
        .map(|r| {
            r.status = "failed".into();
            r.decided_at = Some(now);
            r.outcome = Some("superseded by a newer land of the same work".to_string());
            r.clone()
        })
        .collect()
}

/// The pending one this would duplicate: the same land for a Hive kind, else the same arguments
/// from the same tab.
fn same_pending<'a>(list: &'a [AgentRequest], tab_id: &str, kind: &str, args: &Value) -> Option<&'a AgentRequest> {
    let land = |a: &Value| a.get("land_id").and_then(Value::as_str).map(str::to_string);
    list.iter().find(|r| {
        r.status == "pending"
            && r.kind == kind
            && if is_hive_kind(kind) { land(&r.args).is_some() && land(&r.args) == land(args) } else { r.tab_id == tab_id && r.args == *args }
    })
}

#[allow(clippy::too_many_arguments)]
fn file_inner(
    app: &AppHandle,
    state: &crate::state::AppState,
    tab_id: &str,
    window_label: &str,
    kind: &str,
    args: Value,
    summary: String,
    mut effects: Vec<String>,
    reason: Option<String>,
    keep: Option<(String, u64)>,
) -> Result<AgentRequest, String> {
    let _ = APP.set(app.clone());
    flush_lapsed();
    let now = now_ms();
    let mut list = REQUESTS.lock();
    sweep(&mut list, now);
    if let Some(same) = keep.as_ref().and_then(|(id, _)| list.iter().find(|r| &r.id == id)).or_else(|| same_pending(&list, tab_id, kind, &args)) {
        return Ok(same.clone());
    }
    // A Hive's lands are one per worker, each already through its coordinator: not a flood.
    if !is_hive_kind(kind) && list.iter().filter(|r| r.status == "pending" && r.tab_id == tab_id).count() >= MAX_PENDING_PER_TAB {
        return Err(format!(
            "this tab already has {MAX_PENDING_PER_TAB} requests waiting for the human; wait for those to be answered"
        ));
    }
    drop(list);
    let (asker_title, asker_workspace) = asker(state, tab_id);
    if kind == "restart" {
        // App-wide, so counted here rather than by the filing window, which sees only its own.
        let working = {
            let sessions = state.agent_sessions.read();
            let mut tabs: Vec<&str> = sessions
                .values()
                // Not the asker: it is mid-turn filing this, and restarting it is the point.
                .filter(|s| s.state == crate::state::app_state::AgentSessionState::Active && s.tab_id != tab_id)
                .map(|s| s.tab_id.as_str())
                .collect();
            tabs.sort_unstable();
            tabs.dedup();
            tabs.len()
        };
        effects.insert(0, match working {
            0 => "No other agent is in the middle of a turn right now.".to_string(),
            1 => "1 other agent is in the middle of a turn right now and will be interrupted.".to_string(),
            n => format!("{n} other agents are in the middle of a turn right now and will be interrupted."),
        });
    }
    let mut list = REQUESTS.lock();
    let refiled = keep.is_some();
    let (id, requested_at) = keep.unwrap_or_else(|| (format!("r_{}", uuid::Uuid::new_v4().simple()), now));
    let r = AgentRequest {
        id,
        tab_id: tab_id.to_string(),
        window_label: window_label.to_string(),
        kind: kind.to_string(),
        args,
        summary,
        effects,
        reason: reason.filter(|s| !s.trim().is_empty()),
        asker_title,
        asker_workspace,
        requested_at,
        expires_at: requested_at + pending_ttl(kind),
        status: "pending".into(),
        decided_at: None,
        decided_by: None,
        outcome: None,
    };
    list.push(r.clone());
    drop(list);
    log::info!("[requests] {} {} by tab {}: {}", r.kind, if refiled { "re-filed" } else { "filed" }, &r.tab_id[..r.tab_id.len().min(8)], r.summary);
    // The phone's doorbell (kind `request`), drained by its loop; a tap opens the asking chat.
    // Only for a tab exposed to maiLink: an excluded tab's title must not reach the relay. A card
    // re-filed after a restart rang when it was first filed.
    if !refiled && state.mailink_info.read().is_some() && crate::mailink::is_designated(state, tab_id) {
        let mut q = state.mailink_pending_rings.lock();
        q.push((r.tab_id.clone(), r.asker_title.clone(), "request"));
        if q.len() > crate::mailink::overlord::MAX_PENDING_RINGS {
            let excess = q.len() - crate::mailink::overlord::MAX_PENDING_RINGS;
            q.drain(..excess);
        }
    }
    changed(app);
    Ok(r)
}

/// The tab's name and its workspace's, as they read now.
fn asker(state: &crate::state::AppState, tab_id: &str) -> (String, String) {
    let data = state.app_data.read();
    for ws in data.windows.iter().flat_map(|w| w.workspaces.iter()) {
        if let Some(t) = ws.panes.iter().flat_map(|p| p.tabs.iter()).find(|t| t.id == tab_id) {
            return (t.name.clone(), ws.name.clone());
        }
    }
    ("an agent".to_string(), String::new())
}

/// Every pending request, then the settled ones still kept, oldest first.
pub fn list() -> Vec<AgentRequest> {
    let out = {
        let mut list = REQUESTS.lock();
        sweep(&mut list, now_ms());
        list.clone()
    };
    flush_lapsed();
    out
}

pub fn get(id: &str) -> Option<AgentRequest> {
    let out = {
        let mut list = REQUESTS.lock();
        sweep(&mut list, now_ms());
        list.iter().find(|r| r.id == id).cloned()
    };
    flush_lapsed();
    out
}

/// The HUMAN's answer (a click, or the phone). Only a pending request moves; anything else
/// returns `None` (already answered, lapsed, unknown). An approval is handed to the window that
/// filed it, which carries it out and reports back through `settle`.
pub fn decide(app: &AppHandle, id: &str, approve: bool, by: &str) -> Option<AgentRequest> {
    let now = now_ms();
    let r = {
        let mut list = REQUESTS.lock();
        sweep(&mut list, now);
        list.iter_mut().find(|r| r.id == id && r.status == "pending").map(|r| {
            r.status = if approve { "approved" } else { "denied" }.into();
            r.decided_at = Some(now);
            r.decided_by = Some(by.to_string());
            r.clone()
        })
    };
    flush_lapsed();
    let r = r?;
    log::info!("[requests] {} {} ({}) by {by}", r.kind, r.status, &r.id);
    if approve && is_hive_kind(&r.kind) {
        // Carried out in Rust from the stored arguments; it settles the request itself.
        hive_approved(app, &r);
    } else if approve {
        let _ = app.emit_to(r.window_label.as_str(), "agent-request-approved", &r);
    } else {
        let _ = app.emit("agent-request-settled", &r);
        if is_hive_kind(&r.kind) {
            hive_closed(app, &r);
        }
    }
    changed(app);
    Some(r)
}

/// The executing window's report. Only an approved request settles, once.
pub fn settle(app: &AppHandle, id: &str, ok: bool, outcome: String) -> Option<AgentRequest> {
    let r = {
        let mut list = REQUESTS.lock();
        let r = list.iter_mut().find(|r| r.id == id && r.status == "approved")?;
        r.status = if ok { "done" } else { "failed" }.into();
        r.outcome = Some(outcome);
        r.clone()
    };
    log::info!("[requests] {} {}: {}", r.kind, r.status, r.outcome.as_deref().unwrap_or(""));
    let _ = app.emit("agent-request-settled", &r);
    changed(app);
    Some(r)
}

/// Every request those tabs filed that hasn't settled ends as failed, with `why` (their Hive was
/// deleted, docs/hive.md §11.2): nothing is left on a card for an approval no one can carry out.
pub fn fail_for_tabs(app: &AppHandle, tab_ids: &[String], why: &str) -> usize {
    let failed = fail_open(&mut REQUESTS.lock(), tab_ids, why, now_ms());
    for r in &failed {
        log::info!("[requests] {} failed ({}): {why}", r.kind, r.id);
        let _ = app.emit("agent-request-settled", r);
    }
    if !failed.is_empty() {
        changed(app);
    }
    failed.len()
}

/// One request withdrawn, with `why`, if it hasn't settled: a Hive land whose worker was stopped,
/// retired or reported again while its card waited.
pub fn fail_request(app: &AppHandle, id: &str, why: &str) -> bool {
    let failed = {
        let mut list = REQUESTS.lock();
        let ids = [id.to_string()];
        fail_open_where(&mut list, |r| ids.contains(&r.id), why, now_ms())
    };
    for r in &failed {
        log::info!("[requests] {} failed ({}): {why}", r.kind, r.id);
        let _ = app.emit("agent-request-settled", r);
    }
    if !failed.is_empty() {
        changed(app);
    }
    !failed.is_empty()
}

fn fail_open(list: &mut [AgentRequest], tab_ids: &[String], why: &str, now: u64) -> Vec<AgentRequest> {
    fail_open_where(list, |r| tab_ids.contains(&r.tab_id), why, now)
}

fn fail_open_where(list: &mut [AgentRequest], pick: impl Fn(&AgentRequest) -> bool, why: &str, now: u64) -> Vec<AgentRequest> {
    list.iter_mut()
        .filter(|r| pick(r) && (r.status == "pending" || r.status == "approved"))
        .map(|r| {
            r.status = "failed".into();
            r.decided_at.get_or_insert(now);
            r.outcome = Some(why.to_string());
            r.clone()
        })
        .collect()
}

// ── Tauri commands ───────────────────────────────────────────────────────────────────────────
// `file_agent_request` is called by the frontend's tool handler; the agent's words reach it only
// as `reason`. `decide_agent_request` is the human's click. No MCP tool maps to either.

#[tauri::command]
#[allow(clippy::too_many_arguments)]
pub fn file_agent_request(
    app: AppHandle,
    state: tauri::State<'_, std::sync::Arc<crate::state::AppState>>,
    window: tauri::Window,
    tab_id: String,
    kind: String,
    args: Value,
    summary: String,
    effects: Vec<String>,
    reason: Option<String>,
) -> Result<AgentRequest, String> {
    file(&app, &state, &tab_id, window.label(), &kind, args, summary, effects, reason)
}

#[tauri::command]
pub fn list_agent_requests() -> Vec<AgentRequest> {
    list()
}

#[tauri::command]
pub fn decide_agent_request(app: AppHandle, id: String, approve: bool) -> Option<AgentRequest> {
    decide(&app, &id, approve, "desktop")
}

#[tauri::command]
pub fn settle_agent_request(app: AppHandle, id: String, ok: bool, outcome: String) -> Option<AgentRequest> {
    settle(&app, &id, ok, outcome)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn req(status: &str, requested_at: u64, decided_at: Option<u64>) -> AgentRequest {
        AgentRequest {
            id: "r".into(),
            tab_id: "t".into(),
            window_label: "main".into(),
            kind: "restart".into(),
            args: Value::Null,
            summary: String::new(),
            effects: vec![],
            reason: None,
            asker_title: String::new(),
            asker_workspace: String::new(),
            requested_at,
            expires_at: requested_at + PENDING_TTL_MS,
            status: status.into(),
            decided_at,
            decided_by: None,
            outcome: None,
        }
    }

    #[test]
    fn a_lapse_is_queued_once_to_be_announced() {
        // Its own id: tests run in parallel, and every sweep feeds the one shared queue.
        let mut l = vec![AgentRequest { id: "lapse-once".into(), ..req("pending", 0, None) }];
        sweep(&mut l, PENDING_TTL_MS);
        sweep(&mut l, PENDING_TTL_MS + 1);
        assert_eq!(LAPSED.lock().iter().filter(|r| r.id == "lapse-once").count(), 1);
    }

    #[test]
    fn a_pending_request_lapses_and_is_kept_a_while_after() {
        let mut l = vec![req("pending", 0, None)];
        sweep(&mut l, PENDING_TTL_MS - 1);
        assert_eq!(l[0].status, "pending");
        sweep(&mut l, PENDING_TTL_MS);
        assert_eq!(l[0].status, "expired");
        sweep(&mut l, PENDING_TTL_MS + SETTLED_TTL_MS - 1);
        assert_eq!(l.len(), 1);
        sweep(&mut l, PENDING_TTL_MS + SETTLED_TTL_MS);
        assert!(l.is_empty());
    }

    #[test]
    fn an_approved_request_nobody_carries_out_is_reported_failed() {
        let mut l = vec![AgentRequest { id: "move".into(), kind: "tab_move".into(), ..req("approved", 0, Some(0)) }];
        sweep(&mut l, APPROVED_TTL_MS - 1);
        assert_eq!(l[0].status, "approved");
        sweep(&mut l, APPROVED_TTL_MS);
        assert_eq!(l[0].status, "failed");
    }

    #[test]
    fn a_deleted_hives_open_requests_fail_and_nothing_else_moves() {
        let mut l = vec![
            AgentRequest { id: "a".into(), ..req("pending", 0, None) },
            AgentRequest { id: "b".into(), ..req("approved", 0, Some(5)) },
            AgentRequest { id: "c".into(), ..req("denied", 0, Some(5)) },
            AgentRequest { id: "d".into(), tab_id: "other".into(), ..req("pending", 0, None) },
        ];
        let failed = fail_open(&mut l, &["t".to_string()], "its Hive was deleted", 9);
        assert_eq!(failed.iter().map(|r| r.id.as_str()).collect::<Vec<_>>(), vec!["a", "b"]);
        assert_eq!(l.iter().map(|r| r.status.as_str()).collect::<Vec<_>>(), vec!["failed", "failed", "denied", "pending"]);
        assert_eq!((l[0].decided_at, l[1].decided_at), (Some(9), Some(5)));
        assert_eq!(l[0].outcome.as_deref(), Some("its Hive was deleted"));
    }

    fn hive(id: &str, task: &str, land: &str) -> AgentRequest {
        AgentRequest {
            id: id.into(),
            kind: "hive_land".into(),
            args: serde_json::json!({ "hive_id": "h1", "task_id": task, "land_id": land }),
            expires_at: HIVE_PENDING_TTL_MS,
            ..req("pending", 0, None)
        }
    }

    #[test]
    fn a_hive_land_waits_a_day_and_its_approval_has_time_to_run() {
        let mut l = vec![hive("hive-ttl", "t1", "L-1")];
        sweep(&mut l, PENDING_TTL_MS);
        assert_eq!(l[0].status, "pending", "not the 30 minutes of the others");
        sweep(&mut l, HIVE_PENDING_TTL_MS);
        assert_eq!(l[0].status, "expired");
        let mut l = vec![AgentRequest { id: "hive-approved".into(), ..hive("x", "t1", "L-1") }];
        l[0].status = "approved".into();
        l[0].decided_at = Some(0);
        sweep(&mut l, APPROVED_TTL_MS);
        assert_eq!(l[0].status, "approved");
        sweep(&mut l, HIVE_APPROVED_TTL_MS);
        assert_eq!(l[0].status, "failed");
    }

    #[test]
    fn a_hive_land_is_one_card_per_land_and_a_newer_land_supersedes_it() {
        let mut l = vec![hive("a", "t1", "L-1"), hive("b", "t2", "L-2"), AgentRequest { kind: "restart".into(), ..hive("c", "t1", "L-9") }];
        // The same land again, whatever tab or arguments, is the same card.
        let again = serde_json::json!({ "hive_id": "h1", "task_id": "t1", "land_id": "L-1", "why": "config" });
        assert_eq!(same_pending(&l, "other-tab", "hive_land", &again).map(|r| r.id.as_str()), Some("a"));
        assert!(same_pending(&l, "t", "hive_land", &serde_json::json!({ "task_id": "t1" })).is_none(), "no land id, no match");
        // Its task's newer land ends the older card; another task's and other kinds stay.
        let newer = serde_json::json!({ "hive_id": "h1", "task_id": "t1", "land_id": "L-3" });
        let gone = supersede(&mut l, "hive_land", &newer, 7);
        assert_eq!(gone.iter().map(|r| r.id.as_str()).collect::<Vec<_>>(), vec!["a"]);
        assert_eq!(l.iter().map(|r| r.status.as_str()).collect::<Vec<_>>(), vec!["failed", "pending", "pending"]);
        assert!(supersede(&mut l, "hive_land", &serde_json::json!({ "task_id": "t2", "hive_id": "h1", "land_id": "L-2" }), 8).is_empty(), "its own land");
    }

    /// Only the human approves (docs/agent-requests.md): `decide` is called by the click's Tauri
    /// command and the phone's authenticated endpoint, nowhere else, so no Hive path (an agent's
    /// tool call, a land) can approve its own card.
    #[test]
    fn decide_is_reached_only_from_the_click_and_the_phone() {
        fn walk(dir: &std::path::Path, out: &mut Vec<std::path::PathBuf>) {
            for e in std::fs::read_dir(dir).unwrap().flatten() {
                let p = e.path();
                if p.is_dir() {
                    walk(&p, out);
                } else if p.extension().is_some_and(|x| x == "rs") {
                    out.push(p);
                }
            }
        }
        let src = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        let mut files = Vec::new();
        walk(&src, &mut files);
        // Split, so this test's own lines don't match.
        let (path_call, local_call) = (concat!("agent_requests::", "decide("), concat!(" decide", "(&app"));
        let mut callers = Vec::new();
        for f in files {
            let text = std::fs::read_to_string(&f).unwrap();
            let rel = f.strip_prefix(&src).unwrap().to_string_lossy().replace('\\', "/");
            for (i, line) in text.lines().enumerate() {
                let code = line.split("//").next().unwrap_or_default();
                if code.contains(path_call) || (rel == "agent_requests.rs" && code.contains(local_call)) {
                    callers.push(format!("{rel}:{}", i + 1));
                }
            }
        }
        let mut files: Vec<&str> = callers.iter().map(|c| c.split(':').next().unwrap()).collect();
        files.sort_unstable();
        assert_eq!(files, vec!["agent_requests.rs", "mailink/mod.rs"], "{callers:?}");
    }

    #[test]
    fn an_approved_restart_has_time_to_download_its_update() {
        let mut l = vec![AgentRequest { id: "restart-ttl".into(), ..req("approved", 0, Some(0)) }];
        sweep(&mut l, APPROVED_TTL_MS);
        assert_eq!(l[0].status, "approved");
        sweep(&mut l, RESTART_APPROVED_TTL_MS);
        assert_eq!(l[0].status, "failed");
    }
}
