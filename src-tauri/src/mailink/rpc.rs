//! A phone action that must run INSIDE a desktop window's webview (docs/mailink-protocol.md §13).
//!
//! The Overlord engine's state exists only in the frontend store, so dismissing an escalation,
//! approving a proposal, or driving a tab through the engine has to be done by that window's
//! JavaScript. This is the same request/response the MCP server already uses for frontend-
//! handled tools — a oneshot parked in `AppState.ide_pending`, answered by the frontend through
//! `claude_code_respond` — but on its OWN event, `mailink-frontend-request`. It must not ride
//! `claude-code-tool`: `tools/call` does not validate tool names, so any verb dispatched from
//! that switch is reachable by every agent over MCP, and these carry the human's authority.
//!
//! The webview may be asleep. That is why every answer here carries `{accepted, confirmed}`
//! rather than a bare ok: `confirmed:false` means the desktop did not answer in time and the
//! action may still land when it wakes. The phone renders that as "sent, not confirmed" and
//! lets the next snapshot settle it — never as success, never as failure (the recoverTab
//! lesson: sent ≠ done). Task writes deliberately do NOT come through here (mailink/board.rs).

use crate::state::AppState;
use serde_json::{json, Value};
use std::sync::Arc;
use tauri::{AppHandle, Emitter};

pub(crate) const EVENT: &str = "mailink-frontend-request";

/// Shorter than the MCP path's 120 s: a human is holding a phone. Long enough for a webview
/// that is merely throttled (not suspended) to get a timer slot.
const TIMEOUT: std::time::Duration = std::time::Duration::from_secs(15);

pub(crate) enum Outcome {
    /// The window answered. `{error}` inside is a REFUSAL, which is still a confirmed answer.
    Answered(Value),
    Timeout,
    /// The frontend dropped the request without answering (its handler threw before responding).
    Disconnected,
    /// No window with that label — a stale `windowLabel` from an old snapshot.
    NoWindow,
    /// Unit-test router: no Tauri handle to emit through.
    NoHandle,
}

/// Ask `window_label`'s webview to run `verb` with `args`, and wait.
pub(crate) async fn request(
    app: &Arc<AppState>,
    handle: Option<&AppHandle>,
    window_label: &str,
    verb: &str,
    args: Value,
) -> Outcome {
    let Some(handle) = handle else { return Outcome::NoHandle };
    if !app.app_data.read().windows.iter().any(|w| w.label == window_label) {
        return Outcome::NoWindow;
    }
    let request_id = uuid::Uuid::new_v4().to_string();
    let (tx, rx) = tokio::sync::oneshot::channel::<Value>();
    app.ide_pending.write().insert(request_id.clone(), tx);
    let payload = json!({ "request_id": request_id, "verb": verb, "args": args });
    if handle.emit_to(window_label, EVENT, payload).is_err() {
        app.ide_pending.write().remove(&request_id);
        return Outcome::NoWindow;
    }
    match tokio::time::timeout(TIMEOUT, rx).await {
        Ok(Ok(v)) => Outcome::Answered(v),
        Ok(Err(_)) => {
            app.ide_pending.write().remove(&request_id);
            Outcome::Disconnected
        }
        Err(_) => {
            app.ide_pending.write().remove(&request_id);
            Outcome::Timeout
        }
    }
}

/// The wire shape of every Overlord action's answer. `accepted` = the action will or did take
/// effect; `confirmed` = the desktop actually answered. A refusal is `accepted:false,
/// confirmed:true, reason`. A timeout is `accepted:true, confirmed:false` — it may still land.
pub(crate) fn response(outcome: Outcome) -> Value {
    match outcome {
        Outcome::Answered(v) => match v.get("error").and_then(Value::as_str) {
            Some(err) => json!({ "accepted": false, "confirmed": true, "reason": err }),
            None => json!({ "accepted": true, "confirmed": true, "result": v }),
        },
        Outcome::Timeout => json!({
            "accepted": true, "confirmed": false,
            "reason": "the desktop did not answer in time — its screen may be asleep; the action may still land, watch the next snapshot"
        }),
        Outcome::Disconnected => json!({ "accepted": false, "confirmed": false, "reason": "the desktop dropped the request" }),
        Outcome::NoWindow => json!({ "accepted": false, "confirmed": true, "reason": "no such window — refresh the Overlord view" }),
        Outcome::NoHandle => json!({ "accepted": false, "confirmed": false, "reason": "no desktop handle" }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_refusal_is_confirmed_and_a_timeout_is_not_a_failure() {
        let r = response(Outcome::Answered(json!({ "error": "that tab is exempt" })));
        assert_eq!(r["accepted"], false);
        assert_eq!(r["confirmed"], true, "the desktop answered — it said no");
        assert_eq!(r["reason"], "that tab is exempt");

        let r = response(Outcome::Answered(json!({ "outcome": "started" })));
        assert_eq!(r["accepted"], true);
        assert_eq!(r["confirmed"], true);
        assert_eq!(r["result"]["outcome"], "started");

        let r = response(Outcome::Timeout);
        assert_eq!(r["accepted"], true, "it may still land when the webview wakes");
        assert_eq!(r["confirmed"], false);
        assert!(r["reason"].as_str().unwrap().contains("asleep"));

        let r = response(Outcome::NoWindow);
        assert_eq!((r["accepted"].as_bool(), r["confirmed"].as_bool()), (Some(false), Some(true)));
    }
}
