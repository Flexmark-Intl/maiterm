//! Agent requests that only the human can approve (docs/agent-requests.md).
//!
//! Some MCP tools change things an agent must not change on its own say-so: which tabs share a
//! mesh (who can message whom), and restarting or updating maiTerm itself. Those tools do not act.
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
/// Pending requests one tab may hold: enough for a real reorganisation, not a flood of cards.
const MAX_PENDING_PER_TAB: usize = 5;

/// What a request asks for. A string on the wire (never a serde enum: an unknown variant in
/// stored state wipes it on a downgrade), checked against this list when filed.
pub const KINDS: [&str; 7] = [
    "mesh_enable",
    "mesh_disable",
    "mesh_link",
    "mesh_unlink",
    "tab_move",
    "workspace_move",
    "restart",
];

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
    pub requested_at: u64,
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

/// Lapse what waited too long, forget what settled long ago.
fn sweep(list: &mut Vec<AgentRequest>, now: u64) {
    for r in list.iter_mut() {
        if r.status == "pending" && now.saturating_sub(r.requested_at) >= PENDING_TTL_MS {
            r.status = "expired".into();
            r.decided_at = Some(now);
        }
        // Approved but never reported back: the window that was to carry it out closed or
        // reloaded. Said so, rather than showing "approved" for good.
        if r.status == "approved" && now.saturating_sub(r.decided_at.unwrap_or(now)) >= APPROVED_TTL_MS {
            r.status = "failed".into();
            r.outcome = Some("approved, but the window that was to carry it out never reported back".into());
        }
    }
    list.retain(|r| {
        r.status == "pending"
            || r.status == "approved"
            || now.saturating_sub(r.decided_at.unwrap_or(r.requested_at)) < SETTLED_TTL_MS
    });
}

fn changed(app: &AppHandle) {
    let _ = app.emit("agent-requests-changed", ());
}

/// File a request (the tool side). An identical one already pending is returned instead of a
/// second card. `Err` when the kind is unknown or the tab already has its fill of pending ones.
pub fn file(
    app: &AppHandle,
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
    let now = now_ms();
    let mut list = REQUESTS.lock();
    sweep(&mut list, now);
    if let Some(same) = list
        .iter()
        .find(|r| r.status == "pending" && r.tab_id == tab_id && r.kind == kind && r.args == args)
    {
        return Ok(same.clone());
    }
    if list.iter().filter(|r| r.status == "pending" && r.tab_id == tab_id).count() >= MAX_PENDING_PER_TAB {
        return Err(format!(
            "this tab already has {MAX_PENDING_PER_TAB} requests waiting for the human; wait for those to be answered"
        ));
    }
    let r = AgentRequest {
        id: format!("r_{}", uuid::Uuid::new_v4().simple()),
        tab_id: tab_id.to_string(),
        window_label: window_label.to_string(),
        kind: kind.to_string(),
        args,
        summary,
        effects,
        reason: reason.filter(|s| !s.trim().is_empty()),
        requested_at: now,
        status: "pending".into(),
        decided_at: None,
        decided_by: None,
        outcome: None,
    };
    list.push(r.clone());
    drop(list);
    log::info!("[requests] {} filed by tab {}: {}", r.kind, &r.tab_id[..r.tab_id.len().min(8)], r.summary);
    changed(app);
    Ok(r)
}

/// Every pending request, then the settled ones still kept, oldest first.
pub fn list() -> Vec<AgentRequest> {
    let mut list = REQUESTS.lock();
    sweep(&mut list, now_ms());
    list.clone()
}

pub fn get(id: &str) -> Option<AgentRequest> {
    let mut list = REQUESTS.lock();
    sweep(&mut list, now_ms());
    list.iter().find(|r| r.id == id).cloned()
}

/// The HUMAN's answer (a click, or the phone). Only a pending request moves; anything else
/// returns `None` (already answered, lapsed, unknown). An approval is handed to the window that
/// filed it, which carries it out and reports back through `settle`.
pub fn decide(app: &AppHandle, id: &str, approve: bool, by: &str) -> Option<AgentRequest> {
    let now = now_ms();
    let r = {
        let mut list = REQUESTS.lock();
        sweep(&mut list, now);
        let r = list.iter_mut().find(|r| r.id == id && r.status == "pending")?;
        r.status = if approve { "approved" } else { "denied" }.into();
        r.decided_at = Some(now);
        r.decided_by = Some(by.to_string());
        r.clone()
    };
    log::info!("[requests] {} {} ({}) by {by}", r.kind, r.status, &r.id);
    if approve {
        let _ = app.emit_to(r.window_label.as_str(), "agent-request-approved", &r);
    } else {
        let _ = app.emit("agent-request-settled", &r);
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

// ── Tauri commands ───────────────────────────────────────────────────────────────────────────
// `file_agent_request` is called by the frontend's tool handler; the agent's words reach it only
// as `reason`. `decide_agent_request` is the human's click. No MCP tool maps to either.

#[tauri::command]
#[allow(clippy::too_many_arguments)]
pub fn file_agent_request(
    app: AppHandle,
    window: tauri::Window,
    tab_id: String,
    kind: String,
    args: Value,
    summary: String,
    effects: Vec<String>,
    reason: Option<String>,
) -> Result<AgentRequest, String> {
    file(&app, &tab_id, window.label(), &kind, args, summary, effects, reason)
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
            requested_at,
            status: status.into(),
            decided_at,
            decided_by: None,
            outcome: None,
        }
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
        let mut l = vec![req("approved", 0, Some(0))];
        sweep(&mut l, APPROVED_TTL_MS - 1);
        assert_eq!(l[0].status, "approved");
        sweep(&mut l, APPROVED_TTL_MS);
        assert_eq!(l[0].status, "failed");
    }
}
