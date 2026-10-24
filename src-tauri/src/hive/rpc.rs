//! What a Hive tool asks of its window (docs/hive.md §7.2): the task list, which the window's
//! tasks store owns and writes whole, and a tab's PTY, which exists only once that webview
//! mounts it. Rust validates and keeps what must be atomic; the window does its part and answers.
//!
//! Its own event, `hive-frontend-request`, answered by `src/lib/hive/rpc.ts` through the same
//! oneshot map as mailink's (`mailink::rpc::request_on`). Never a case in `claude-code-tool`:
//! `tools/call` doesn't validate names, so a verb there is any agent's to call over MCP.

use std::future::Future;
use std::sync::Arc;
use std::time::Duration;

use serde_json::Value;
use tauri::AppHandle;

pub(crate) use crate::mailink::rpc::Outcome;
use crate::state::AppState;

pub(crate) const EVENT: &str = "hive-frontend-request";
/// A spawn's window has this long to confirm; its reservation (120 s) outlives it.
pub(crate) const TIMEOUT: Duration = Duration::from_secs(20);

pub(crate) const VERB_SPAWN: &str = "hive.spawn";
pub(crate) const VERB_UNSPAWN: &str = "hive.unspawn";

/// The window a Hive lives in, as the tools see it: the app's in production, a stub in tests.
pub(crate) trait HiveWindow: Clone + Send + Sync + 'static {
    fn request(&self, verb: &'static str, args: Value) -> impl Future<Output = Outcome> + Send;
    /// The Hive's workers changed (a spawn, a rollback): its board refetches.
    fn workers_changed(&self, workspace_id: &str);
}

#[derive(Clone)]
pub(crate) struct AppWindow {
    pub app: Arc<AppState>,
    pub handle: AppHandle,
    pub label: String,
}

impl HiveWindow for AppWindow {
    fn request(&self, verb: &'static str, args: Value) -> impl Future<Output = Outcome> + Send {
        let this = self.clone();
        async move { crate::mailink::rpc::request_on(&this.app, Some(&this.handle), EVENT, &this.label, verb, args, TIMEOUT).await }
    }

    fn workers_changed(&self, workspace_id: &str) {
        use tauri::Emitter;
        // To the Hive's window only: `emit` would reach every window's board.
        let payload = serde_json::json!({ "workspace_id": workspace_id });
        if let Err(e) = self.handle.emit_to(self.label.as_str(), super::tools::WORKERS_CHANGED_EVENT, payload) {
            log::warn!("hive: workers-changed not sent to window {}: {e}", self.label);
        }
    }
}

/// The window's answer as the spawn reads it: done, or why not. A timeout is not done: the
/// spawn is rolled back, and the window, if it wakes, undoes its half on the unspawn after it.
pub(crate) fn accepted(outcome: Outcome) -> Result<Value, String> {
    match outcome {
        Outcome::Answered(v) => match v.get("error").and_then(Value::as_str) {
            Some(err) => Err(format!("maiTerm's window refused it: {err}")),
            None => Ok(v),
        },
        Outcome::Timeout => Err(format!(
            "maiTerm's window didn't confirm within {} s (its screen may be asleep), so nothing was started.",
            TIMEOUT.as_secs()
        )),
        Outcome::Disconnected => Err("maiTerm's window dropped the request, so nothing was started.".to_string()),
        Outcome::NoWindow => Err("This Hive's window isn't open, so nothing was started.".to_string()),
        Outcome::NoHandle => Err("No maiTerm window to ask, so nothing was started.".to_string()),
    }
}

/// Whether the window is known to be listening: a refusal or a failed save after its answer is
/// worth waiting on an unspawn for; a timeout isn't.
pub(crate) fn window_answered(outcome: &Outcome) -> bool {
    matches!(outcome, Outcome::Answered(_))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn only_an_answer_without_an_error_is_done() {
        assert!(accepted(Outcome::Answered(json!({ "ok": true }))).is_ok());
        let e = accepted(Outcome::Answered(json!({ "error": "not a Hive" }))).unwrap_err();
        assert!(e.contains("refused it: not a Hive"), "{e}");
        for o in [Outcome::Timeout, Outcome::Disconnected, Outcome::NoWindow, Outcome::NoHandle] {
            let e = accepted(o).unwrap_err();
            assert!(e.ends_with("nothing was started."), "{e}");
        }
        assert!(window_answered(&Outcome::Answered(json!({}))) && !window_answered(&Outcome::Timeout));
    }
}
