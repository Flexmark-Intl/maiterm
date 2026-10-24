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
/// A worker's task row patched by the store: a note appended, a status, a blocker.
pub(crate) const VERB_TASK: &str = "hive.task";
/// A worker's agent relaunched in its own tab: typed at an empty prompt, or the tab mounted.
pub(crate) const VERB_RELAUNCH: &str = "hive.relaunch";
/// A worker retired (§8.4): its tab dropped (the PTY with it) and its row's status set, the row kept.
pub(crate) const VERB_RETIRE: &str = "hive.retire";
/// A Hive deleted (§11.2): every Hive tab dropped and the rows' tabs cleared, the rows kept.
pub(crate) const VERB_DELETE: &str = "hive.delete";
/// A queued job's hidden tab minted and its line typed through the stack's guard (§6.8),
/// answered once the shell's own B for that line is seen.
pub(crate) const VERB_JOB_START: &str = "stack.job.start";
/// ^C to a job, under the stack's pid guard: a timeout, a release, the human's Stop anyway.
pub(crate) const VERB_JOB_STOP: &str = "stack.job.stop";
/// A worker's finished job tabs closed: its next job is starting, or it retired.
pub(crate) const VERB_JOB_CLEANUP: &str = "stack.job.cleanup";
/// A job start's window waits out the stack's own guard (mount, first prompt, settle, the B)
/// before it answers, so it gets longer than `TIMEOUT`.
pub(crate) const JOB_START_TIMEOUT: Duration = Duration::from_secs(90);

/// The window a Hive lives in, as the tools see it: the app's in production, a stub in tests.
pub(crate) trait HiveWindow: Clone + Send + Sync + 'static {
    fn request(&self, verb: &'static str, args: Value) -> impl Future<Output = Outcome> + Send;
    /// The Hive's workers changed (a spawn, a rollback): its board refetches.
    fn workers_changed(&self, workspace_id: &str);
    /// Types `text` into `tab`'s agent through the delivery gate (`deliver::deliver`).
    fn deliver(&self, tab: &str, text: String, what: &'static str) -> impl Future<Output = Sent> + Send;
    /// What holds `tab`'s terminal now.
    fn front(&self, tab: &str) -> impl Future<Output = Front> + Send;
    /// SIGTERM to `tab`'s agent, and only while it is the terminal's foreground job: never the
    /// shell, never by name. Whether a signal went.
    fn stop_agent(&self, tab: &str) -> impl Future<Output = Result<bool, String>> + Send;
    /// A worker's files may have changed outside its own hooks (an accepted report): its diff
    /// stat recomputes, debounced. A test window has no board to push to.
    fn diff_stat_touch(&self, _caller: &super::tools::Caller, _task_id: &str) {}
    /// Agent requests the Hive's tabs filed and no one carried out end as failed (a deleted Hive
    /// can't land or undo anything). How many. A test window files none.
    fn fail_requests(&self, _tab_ids: &[String], _why: &str) -> usize {
        0
    }
}

pub(crate) use super::deliver::Sent;

/// What holds a tab's terminal (`get_pty_foreground_job`, read fresh).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Front {
    /// No PTY: the tab isn't mounted, or its terminal is gone.
    NoTerminal,
    /// The shell, at its prompt: no agent is running.
    Shell,
    /// Another job: the agent, or whatever replaced it.
    Job,
    /// It can't be told.
    Unknown,
}

/// Tells the Hive in `workspace_id`, in window `label`, that its workers changed: its board
/// refetches. For the paths that change a worker outside a tool call (a session start, a reload,
/// the breaker).
pub(crate) fn emit_workers_changed(handle: &AppHandle, label: &str, workspace_id: &str) {
    use tauri::Emitter;
    // To the Hive's window only: `emit` would reach every window's board.
    let payload = serde_json::json!({ "workspace_id": workspace_id });
    if let Err(e) = handle.emit_to(label, super::tools::WORKERS_CHANGED_EVENT, payload) {
        log::warn!("hive: workers-changed not sent to window {label}: {e}");
    }
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
        emit_workers_changed(&self.handle, &self.label, workspace_id);
    }

    fn deliver(&self, tab: &str, text: String, what: &'static str) -> impl Future<Output = Sent> + Send {
        let (this, tab) = (self.clone(), tab.to_string());
        async move { super::deliver::deliver(&this.app, &this.handle, &tab, &text, what).await }
    }

    fn front(&self, tab: &str) -> impl Future<Output = Front> + Send {
        let (app, pty) = (self.app.clone(), crate::mailink::pty_for_tab(&self.app, tab));
        async move {
            let Some(pty) = pty else { return Front::NoTerminal };
            let fg = tokio::task::spawn_blocking(move || crate::pty::get_pty_foreground_job(&app, &pty, true)).await;
            match fg {
                Ok(Ok(f)) => match f.shell_at_prompt {
                    Some(true) => Front::Shell,
                    Some(false) => Front::Job,
                    None => Front::Unknown,
                },
                // Gone between the lookup and the read.
                Ok(Err(e)) if e == "PTY not found" => Front::NoTerminal,
                _ => Front::Unknown,
            }
        }
    }

    fn stop_agent(&self, tab: &str) -> impl Future<Output = Result<bool, String>> + Send {
        let (app, pty) = (self.app.clone(), crate::mailink::pty_for_tab(&self.app, tab));
        async move {
            let Some(pty) = pty else { return Ok(false) };
            tokio::task::spawn_blocking(move || stop_foreground_agent(&app, &pty))
                .await
                .map_err(|e| format!("maiTerm's worker thread failed: {e}"))?
        }
    }

    fn diff_stat_touch(&self, caller: &super::tools::Caller, task_id: &str) {
        super::diffstat::touch(&self.handle, caller, task_id);
    }

    fn fail_requests(&self, tab_ids: &[String], why: &str) -> usize {
        crate::agent_requests::fail_for_tabs(&self.handle, tab_ids, why)
    }
}

/// The foreground job's pid, read fresh, and signalled through `kill_pty_foreground_job`, which
/// reads it again and sends nothing if another process took the terminal meanwhile. Only a job
/// that is `claude` (its program, or the script a runtime runs it as) is signalled. Ok(false) is
/// only "the shell has its prompt": a front that can't be read is an error, never "no agent".
fn stop_foreground_agent(app: &Arc<AppState>, pty: &str) -> Result<bool, String> {
    let fg = crate::pty::get_pty_foreground_job(app, pty, true)?;
    match fg.shell_at_prompt {
        Some(true) => return Ok(false),
        Some(false) => {}
        None => return Err("maiTerm can't tell what holds its terminal, so nothing was signalled".to_string()),
    }
    let Some(pid) = fg.pid else { return Err("the program in front of its terminal has no pid maiTerm can read, so nothing was signalled".to_string()) };
    if !fg.command.as_deref().is_some_and(names_claude) {
        return Err(format!(
            "the program in front of its terminal ({}) isn't its agent, so nothing was signalled",
            fg.executable.as_deref().unwrap_or("unknown")
        ));
    }
    crate::pty::kill_pty_foreground_job(app, pty, pid, false)
}

/// A command line whose program, or whose script under an interpreter (`node …/claude`), is claude.
fn names_claude(cmd: &str) -> bool {
    cmd.split_whitespace().take(2).any(|w| std::path::Path::new(w).file_name().is_some_and(|n| n == "claude"))
}

/// The window's answer as the spawn reads it: done, or why not. A timeout is not done: the
/// spawn is rolled back, and the window, if it wakes, undoes its half on the unspawn after it.
pub(crate) fn accepted(outcome: Outcome) -> Result<Value, String> {
    answer(outcome, "nothing was started")
}

/// As `accepted`, for a change to a worker that exists (its task row, its relaunch).
pub(crate) fn applied(outcome: Outcome) -> Result<Value, String> {
    answer(outcome, "nothing was changed")
}

fn answer(outcome: Outcome, so: &str) -> Result<Value, String> {
    match outcome {
        Outcome::Answered(v) => match v.get("error").and_then(Value::as_str) {
            Some(err) => Err(format!("maiTerm's window refused it: {err}")),
            None => Ok(v),
        },
        Outcome::Timeout => Err(format!(
            "maiTerm's window didn't confirm within {} s (its screen may be asleep), so {so}.",
            TIMEOUT.as_secs()
        )),
        Outcome::Disconnected => Err(format!("maiTerm's window dropped the request, so {so}.")),
        Outcome::NoWindow => Err(format!("This Hive's window isn't open, so {so}.")),
        Outcome::NoHandle => Err(format!("No maiTerm window to ask, so {so}.")),
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

    #[test]
    fn only_claude_in_front_is_its_agent() {
        assert!(names_claude("claude --resume s1 --settings /x"));
        assert!(names_claude("/Users/x/.local/bin/claude --model sonnet"));
        assert!(names_claude("node /usr/local/lib/node_modules/.bin/claude --model sonnet"));
        for other in ["vim notes.md", "-zsh", "node server.js claude", "python3 claude.py", "sleep 100"] {
            assert!(!names_claude(other), "{other}");
        }
    }
}
