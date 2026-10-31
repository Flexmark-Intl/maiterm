//! What a worker's own hooks tell maiTerm (docs/hive.md §6.7, §11.3), off the hook's path.
//!
//! A crash: the agent left the foreground with its shell at its prompt, or the tab's terminal
//! died, while the worker was running with no report since its launch. It goes `failed` and the
//! coordinator is told; a stop is marked before its signal, a suspend or reload kills the PTY
//! (never read as a death), and a report moves the phase off `running`, so none of those count.
//!
//! A sandbox refusal: a call (failed, or a shell call that exited 0) whose output names a host or
//! path the sandbox refused becomes an `action` blocker on the worker's task, once per target. It
//! routes to the queen, who grants a host (`change::change_tool`), refuses it with new
//! instructions, or raises her own blocker to the human (Darryl, 2026-10-31).
//!
//! A prompt: a dialog or question opening is routed to one asker, and one closing drops its route
//! (`prompts`, §7.4).

use std::sync::Arc;
use std::time::Duration;

use serde_json::{json, Value};
use tauri::AppHandle;

use super::deliver::one_line;
use super::rpc::{self, Front, HiveWindow};
use super::sandbox_events::{self, Violation};
use super::tools::{blocking, worker_label, Caller, Ctx};
use super::worker::{self, WorkerRecord};
use crate::state::AppState;

/// The phases a crash moves to `failed`: a launch whose agent never reported.
const CRASHABLE: [&str; 2] = [worker::PHASE_LAUNCHING, worker::PHASE_RUNNING];
/// A SessionEnd comes as the agent exits: the shell has its prompt back a moment later.
const SETTLE: Duration = Duration::from_secs(2);

/// What a worker's hook sets off.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Action {
    /// Its agent may have exited: a crash?
    SessionEnded,
    /// A failed call: a sandbox refusal?
    ToolFailed,
    /// Its files may have changed: the diff stat, debounced (§4.4).
    DiffStat,
    /// Its model or context may have moved: the card's meta, debounced (§4.4).
    Meta,
    /// A permission dialog or a question may be open: route it once the screen settles (§7.4).
    PromptMaybe,
    /// A routed prompt may have closed: drop what isn't open any more.
    PromptGone,
}

/// The tools that change files: a shell command can as much as an edit.
const EDITING_TOOLS: [&str; 6] = ["Edit", "Write", "MultiEdit", "NotebookEdit", "Bash", "PowerShell"];
/// The tools the sandbox holds: a successful call of one may still carry its note.
const SHELL_TOOLS: [&str; 3] = ["Bash", "PowerShell", "Monitor"];

/// The routing table of `observed`. A failed shell command may still have written files, so it
/// touches the diff stat too. SessionStart covers a compaction (`source: compact`). Any tool's
/// end, a turn's end, a new prompt and the session's end can each close a prompt.
pub(crate) fn feed_actions(event: &str, tool: Option<&str>) -> &'static [Action] {
    let edits = tool.is_some_and(|t| EDITING_TOOLS.contains(&t));
    let shell = tool.is_some_and(|t| SHELL_TOOLS.contains(&t));
    match event {
        "SessionEnd" => &[Action::SessionEnded, Action::PromptGone],
        "PostToolUseFailure" if edits => &[Action::ToolFailed, Action::DiffStat, Action::PromptGone],
        "PostToolUseFailure" => &[Action::ToolFailed, Action::PromptGone],
        // A command that exits 0 can carry the sandbox's note too (`observed` reads it only then).
        "PostToolUse" if shell => &[Action::ToolFailed, Action::DiffStat, Action::PromptGone],
        "PostToolUse" if edits => &[Action::DiffStat, Action::PromptGone],
        "PostToolUse" => &[Action::PromptGone],
        "Stop" => &[Action::DiffStat, Action::Meta, Action::PromptGone],
        // A turn that failed has ended as one that finished has, its edits so far included.
        "StopFailure" => &[Action::DiffStat, Action::PromptGone],
        "SessionStart" => &[Action::Meta, Action::DiffStat],
        "UserPromptSubmit" => &[Action::PromptGone],
        "PermissionRequest" | "Notification" => &[Action::PromptMaybe],
        "PreToolUse" if tool == Some("AskUserQuestion") => &[Action::PromptMaybe],
        _ => &[],
    }
}

/// Every event a Hive tab's own command hook forwards (`via=hive`). Called before the hooks
/// handler's mod dedup, which would drop this copy for the mod's: only this one says it is a
/// Hive tab's, and the tab is checked against the state here.
pub fn observed(state: &Arc<AppState>, handle: &AppHandle, tab_id: &str, event_name: &str, event: &Value) {
    // Any Hive tab's write into the memory folder, the coordinator's too (§9 provenance).
    if let Some(path) = super::memory::tool_write_target(event_name, event) {
        super::memory::attribute_tool_write(state, handle, tab_id, path);
    }
    // Any Hive tab's turn that ended well puts a failed one behind it (`turn_failure`).
    if event_name == "Stop" {
        super::turn_failure::turn_completed(state, handle, tab_id);
    }
    let actions = feed_actions(event_name, event.get("tool_name").and_then(Value::as_str));
    if actions.is_empty() {
        return;
    }
    let Some(caller) = worker_caller(state, tab_id) else { return };
    for action in actions {
        match action {
            Action::DiffStat => super::diffstat::touch_tab(handle, &caller, tab_id),
            Action::Meta => super::feed::meta_touch(state, handle, &caller, tab_id),
            Action::SessionEnded => {
                // A `/clear` ends one session and starts the next in the same process.
                if event.get("reason").and_then(Value::as_str) == Some("clear") {
                    continue;
                }
                let (state, handle, caller, tab) = (state.clone(), handle.clone(), caller.clone(), tab_id.to_string());
                tauri::async_runtime::spawn(async move {
                    let ctx = super::tools::app_ctx(&state, &handle, &caller);
                    tokio::time::sleep(SETTLE).await;
                    if session_ended(&ctx, &caller, &tab).await {
                        super::waiters::poll_soon(state.clone(), handle.clone(), caller.workspace_id.clone());
                    }
                });
            }
            // Every successful shell call comes here: read on only when the sandbox left its note.
            Action::ToolFailed if event_name == "PostToolUse" && !sandbox_events::has_note(event) => {}
            Action::ToolFailed => {
                let (state, handle, caller, tab, event) = (state.clone(), handle.clone(), caller.clone(), tab_id.to_string(), event.clone());
                tauri::async_runtime::spawn(async move {
                    let ctx = super::tools::app_ctx(&state, &handle, &caller);
                    tool_failed(&ctx, &caller, &tab, &event).await;
                });
            }
            Action::PromptMaybe => {
                let (state, handle, caller, tab) = (state.clone(), handle.clone(), caller.clone(), tab_id.to_string());
                tauri::async_runtime::spawn(async move {
                    tokio::time::sleep(super::prompts::SETTLE).await;
                    let ctx = super::tools::app_ctx(&state, &handle, &caller);
                    super::prompts::maybe_route(&ctx, &caller, &tab).await;
                });
            }
            // Every tool's end comes here: nothing to do unless one of its prompts is routed.
            Action::PromptGone if !super::prompts::has_screen_routes(tab_id) => {}
            Action::PromptGone => {
                let (state, handle, caller, tab)= (state.clone(), handle.clone(), caller.clone(), tab_id.to_string());
                tauri::async_runtime::spawn(async move {
                    let ctx = super::tools::app_ctx(&state, &handle, &caller);
                    super::prompts::gone(&ctx, &caller, &tab).await;
                });
            }
        }
    }
}

/// A worker tab's terminal died on its own (not killed by maiTerm: a suspend, close or reload
/// sends its own kill, which never comes here).
///
/// The tab may be gone already: the window closes a tab whose terminal closed, and its delete
/// can land before the PTY's thread sees the exit. So a tab the state no longer has is looked
/// up in the Hives' worker records, which name it until a restart gives the worker another.
pub fn pty_exited(state: &Arc<AppState>, handle: &AppHandle, tab_id: &str) {
    let candidates = {
        let data = state.app_data.read();
        if data.windows.iter().flat_map(|w| w.workspaces.iter()).flat_map(|ws| ws.panes.iter()).any(|p| p.tabs.iter().any(|t| t.id == tab_id)) {
            match worker_caller(state, tab_id) {
                Some(c) => vec![c],
                None => return,
            }
        } else {
            super::tools::hive_callers(&data)
        }
    };
    if candidates.is_empty() {
        return;
    }
    let (state, handle, tab) = (state.clone(), handle.clone(), tab_id.to_string());
    tauri::async_runtime::spawn(async move {
        let t = tab.clone();
        let Ok(Some(caller)) = blocking(move || Ok(owner_of(candidates, &t))).await else { return };
        let ctx = super::tools::app_ctx(&state, &handle, &caller);
        if mark_failed(&ctx, &caller, &tab, "its terminal closed").await {
            super::waiters::poll_soon(state.clone(), handle.clone(), caller.workspace_id.clone());
        }
    });
}

/// The Hive among `candidates` whose worker records name `tab_id`. Blocks.
pub(crate) fn owner_of(candidates: Vec<Caller>, tab_id: &str) -> Option<Caller> {
    candidates.into_iter().find(|c| worker::worker_for_tab(&c.paths, tab_id).is_some())
}

fn worker_caller(state: &Arc<AppState>, tab_id: &str) -> Option<Caller> {
    let data = state.app_data.read();
    let (_, tab) = super::find_hive_tab(&data, tab_id)?;
    if tab.hive_role.as_deref() != Some(super::ROLE_WORKER) {
        return None;
    }
    super::tools::caller_for_tab(&data, tab_id)
}

/// The agent's session ended: a crash only if its shell is back at its prompt. A terminal that's
/// gone was killed (a suspend, a reload) or died, which `pty_exited` hears; another job in front
/// is something else's. True when the worker went `failed`.
pub(crate) async fn session_ended<W: HiveWindow>(ctx: &Ctx<'_, W>, caller: &Caller, tab: &str) -> bool {
    if ctx.window.front(tab).await != Front::Shell {
        return false;
    }
    mark_failed(ctx, caller, tab, "its agent exited").await
}

/// `failed`, when the worker is still running with no report since its launch, and the
/// coordinator told. True when it moved.
pub(crate) async fn mark_failed<W: HiveWindow>(ctx: &Ctx<'_, W>, caller: &Caller, tab: &str, how: &str) -> bool {
    let Some((w, given)) = move_to_failed(ctx, caller, tab, how).await else { return false };
    if given {
        return true;
    }
    notify(
        ctx,
        caller,
        format!(
            "[maiTerm] Worker {} stopped without reporting: {how}. Its worktrees and branch are kept. hiveRestartWorker resumes \
             it (fresh: true starts a new session that reads its brief again); hiveStopWorker retires it.",
            worker_label(&w)
        ),
    )
    .await;
    true
}

/// `mark_failed` but for telling the coordinator, which the caller does in its own words (a
/// failed turn, `turn_failure`). The record as it now is, and whether it was a reviewer that had
/// given its verdict (nothing to tell). None when it didn't move.
pub(crate) async fn move_to_failed<W: HiveWindow>(ctx: &Ctx<'_, W>, caller: &Caller, tab: &str, how: &str) -> Option<(WorkerRecord, bool)> {
    let (paths, tab_id) = (caller.paths.clone(), tab.to_string());
    let moved = blocking(move || {
        let Some(w) = worker::worker_for_tab(&paths, &tab_id) else { return Ok(None) };
        let dir = worker::task_dir(&paths, &w.task_id)?;
        let mut moved = false;
        let after = worker::update_worker(&dir, |w| {
            moved = CRASHABLE.contains(&w.phase.as_str()) && w.tab_id.as_deref() == Some(tab_id.as_str());
            if moved {
                w.phase = worker::PHASE_FAILED.to_string();
            }
            moved
        })?;
        // A reviewer has no report, only a verdict: one that gave it just ended early.
        let given = after.reviews.as_deref().filter(|_| after.is_reviewer()).and_then(|id| worker::task_dir(&paths, id).ok()).and_then(|d| worker::read_worker(&d).ok());
        let given = given.is_some_and(|b| super::review::verdict_given(&b, &after).is_some());
        Ok(moved.then_some((after, given)))
    })
    .await;
    let (w, given) = match moved {
        Ok(Some(w)) => w,
        Ok(None) => return None,
        Err(e) => {
            log::warn!("hive {}: tab {tab}: crash not recorded: {e}", caller.hive.id);
            return None;
        }
    };
    if given {
        // Not news to the coordinator, which has its verdict: its retirement is owed (the caller polls).
        log::info!("hive {}: reviewer {} ended after its verdict: {how}", caller.hive.id, worker_label(&w));
        super::queue::release_worker(&w.task_id, "your review is done");
        ctx.window.workers_changed(&caller.workspace_id);
        return Some((w, true));
    }
    log::warn!("hive {}: worker {} failed: {how} without a report", caller.hive.id, worker_label(&w));
    super::queue::release_worker(&w.task_id, &format!("you stopped without reporting ({how})"));
    ctx.window.workers_changed(&caller.workspace_id);
    let note = format!("Failed: {how} without a report");
    if let Err(e) = super::control::task_note(ctx, caller, &w.task_id, &note, None).await {
        log::info!("hive {}: worker {} failure note not written: {e}", caller.hive.id, worker_label(&w));
    }
    Some((w, false))
}

/// A worker's failed call: each target the sandbox refused that it hasn't been told about yet.
pub(crate) async fn tool_failed<W: HiveWindow>(ctx: &Ctx<'_, W>, caller: &Caller, tab: &str, event: &Value) {
    let (paths, tab_id, ev) = (caller.paths.clone(), tab.to_string(), event.clone());
    let found = blocking(move || {
        let Some(w) = worker::worker_for_tab(&paths, &tab_id) else { return Ok(None) };
        let dir = worker::task_dir(&paths, &w.task_id)?;
        let settings = super::launch::read_record(&dir)
            .ok()
            .and_then(|r| std::fs::read(&r.settings_path).ok())
            .and_then(|b| serde_json::from_slice::<Value>(&b).ok())
            .unwrap_or(Value::Null);
        let writable = sandbox_events::Writable::from_settings(&settings, &w.cwd(&paths));
        let all = sandbox_events::parse_violations(&ev, &writable);
        if all.is_empty() {
            return Ok(None);
        }
        Ok(Some((w, dir, all)))
    })
    .await;
    let (w, dir, all) = match found {
        Ok(Some(f)) => f,
        Ok(None) => return,
        Err(e) => {
            log::warn!("hive {}: tab {tab}: sandbox refusal not read: {e}", caller.hive.id);
            return;
        }
    };
    // Recorded before it is filed, under the record's lock: two hooks reporting one refusal
    // file one blocker.
    let fresh = blocking(move || {
        let mut fresh: Vec<Violation> = Vec::new();
        worker::update_worker(&dir, |w| {
            fresh = all.into_iter().filter(|v| !w.violations_seen.contains(&v.key())).collect();
            w.violations_seen.extend(fresh.iter().map(Violation::key));
            !fresh.is_empty()
        })?;
        Ok(fresh)
    })
    .await
    .unwrap_or_default();
    if fresh.is_empty() {
        return;
    }
    let command = event.get("tool_input").and_then(|i| i.get("command")).and_then(Value::as_str).map(|c| one_line(c, 300));
    file_sandbox_blocker(ctx, caller, &w, &fresh, command.as_deref()).await;
}

fn describe(v: &Violation) -> String {
    format!("{} {}", v.kind, one_line(&v.target, 200))
}

/// How Rust's sandbox questions start: `"The sandbox refused host a.com, path /x"`.
pub(crate) const SANDBOX_QUESTION: &str = "The sandbox refused ";

/// The hosts a sandbox question of Rust's names, when every refusal in it is a host. None for
/// anything else (a path among them, a worker's own words).
pub(crate) fn sandbox_question_hosts(question: &str) -> Option<Vec<String>> {
    let rest = question.strip_prefix(SANDBOX_QUESTION)?;
    rest.split(", ").map(|n| n.strip_prefix(&format!("{} ", sandbox_events::KIND_HOST)).map(str::to_string)).collect::<Option<Vec<_>>>().filter(|h| !h.is_empty())
}

/// What the queen may do about one refused host, in maiTerm's words: grant it, or why it can't be
/// granted whoever asks (§6.7). `remotes` are the Hive's repo remote hosts.
fn host_remedy(host: &str, remotes: &[String]) -> String {
    use super::gallery::budget::{normalize_host, reaches_remote};
    let local = host.parse::<std::net::IpAddr>().map(|ip| crate::host_guard::is_home(&ip));
    match normalize_host(host) {
        _ if local == Ok(true) => format!(
            "{host} is this computer or its network: a worker\u{2019}s localhost stays closed. To run the app and look at it, a \
             worker uses hiveRun with serve: true, hiveScreenshot and hiveFetch."
        ),
        Err(_) => format!("{host} can\u{2019}t be allowed for workers (only public host names can)."),
        // The same rule a grant meets, so she's never told to grant what will be refused.
        Ok(h) if reaches_remote(&h, remotes).is_some() => {
            format!("{host} reaches where this Hive\u{2019}s repos push: it can\u{2019}t be granted, so refuse it with what the worker does instead.")
        }
        Ok(h) => format!(
            "If the task needs {h} (a package registry, docs, an API the work talks to), allow it: hiveChangeSettings \
             {{add_hosts: [\"{h}\"], reason: \"\u{2026}\"}} applies at once and maiTerm answers this blocker. If not, refuse it with \
             what to do instead."
        ),
    }
}

async fn file_sandbox_blocker<W: HiveWindow>(ctx: &Ctx<'_, W>, caller: &Caller, w: &WorkerRecord, fresh: &[Violation], command: Option<&str>) {
    let names: Vec<String> = fresh.iter().map(describe).collect();
    let now = crate::commands::workspace::iso_now();
    // git and `ssh -G`: off the async thread.
    let h = caller.hive.clone();
    let remotes = blocking(move || Ok(super::settings::hive_remote_hosts(&h))).await.unwrap_or_default();
    let mut context: Vec<String> = Vec::new();
    if let Some(c) = command {
        context.push(format!("While running: `{c}`"));
    }
    for v in fresh {
        if v.kind == sandbox_events::KIND_HOST {
            context.push(host_remedy(&v.target, &remotes));
        } else {
            context.push(format!(
                "{} is outside the folders this worker may write, and no one widens that: tell it how to do without (its \
                 worktree, its scratch folder, or mktemp -d -p \"$TMPDIR\"), or raise your own blocker.",
                one_line(&v.target, 200)
            ));
        }
    }
    // An action, so it goes to the queen first (`prompts::classify`), and she grants, refuses or
    // escalates (§7.4). The route's frame tells her; nothing else does.
    let blocker = json!({
        "kind": "action",
        "question": format!("{SANDBOX_QUESTION}{}", names.join(", ")),
        "context": context.join("\n"),
        "asked_at": now,
        "asked_by": "agent",
    });
    let args = json!({
        "workspace_id": caller.workspace_id,
        "task_id": w.task_id,
        "status": "blocked",
        "blocker": blocker,
        "note": format!("The sandbox refused {}", names.join(", ")),
    });
    if let Err(e) = rpc::applied(ctx.window.request(rpc::VERB_TASK, args).await) {
        log::warn!("hive {}: worker {} sandbox blocker not filed: {e}", caller.hive.id, worker_label(w));
    }
    log::info!("hive {}: worker {}: the sandbox refused {}", caller.hive.id, worker_label(w), names.join(", "));
    ctx.window.workers_changed(&caller.workspace_id);
}

pub(crate) async fn notify<W: HiveWindow>(ctx: &Ctx<'_, W>, caller: &Caller, text: String) {
    let Some(c) = super::tools::coordinator_tab(ctx.data, caller) else { return };
    let sent = ctx.window.deliver(&c, text, "Hive notice").await;
    log::info!("hive {}: notice to the coordinator: {}", caller.hive.id, sent.describe());
}

#[cfg(test)]
mod tests {
    use super::*;
    use Action::*;

    /// The regression review: the remedy uses the grant's rule, so the queen is never told to
    /// grant what `hiveChangeSettings` would refuse.
    #[test]
    fn the_remedy_never_offers_a_host_the_grant_refuses() {
        let remotes = vec!["github.com".to_string()];
        for h in ["api.github.com", "ssh.github.com", "github.com"] {
            let r = host_remedy(h, &remotes);
            assert!(r.contains("can\u{2019}t be granted") && !r.contains("add_hosts"), "{h}: {r}");
        }
        assert!(host_remedy("registry.example.org", &remotes).contains("add_hosts"));
    }

    #[test]
    fn hooks_route_to_their_actions() {
        for tool in EDITING_TOOLS {
            let after = if SHELL_TOOLS.contains(&tool) { &[ToolFailed, DiffStat, PromptGone][..] } else { &[DiffStat, PromptGone][..] };
            assert_eq!(feed_actions("PostToolUse", Some(tool)), after, "{tool}: a shell call that exited 0 can carry the sandbox's note");
            assert_eq!(feed_actions("PostToolUseFailure", Some(tool)), &[ToolFailed, DiffStat, PromptGone], "{tool}");
        }
        assert_eq!(feed_actions("PostToolUse", Some("Monitor")), &[ToolFailed, DiffStat, PromptGone]);
        for tool in [Some("Read"), Some("Grep"), Some("mcp__maiterm__hiveReport"), Some("edit"), None] {
            assert_eq!(feed_actions("PostToolUse", tool), &[PromptGone], "{tool:?}: changes no file");
            assert_eq!(feed_actions("PostToolUseFailure", tool), &[ToolFailed, PromptGone], "{tool:?}");
        }
        assert_eq!(feed_actions("Stop", None), &[DiffStat, Meta, PromptGone]);
        assert_eq!(feed_actions("StopFailure", None), &[DiffStat, PromptGone], "a failed turn ended too");
        assert_eq!(feed_actions("SessionStart", None), &[Meta, DiffStat]);
        assert_eq!(feed_actions("SessionEnd", None), &[SessionEnded, PromptGone]);
        assert_eq!(feed_actions("UserPromptSubmit", None), &[PromptGone]);
        // A dialog opening: its permission hook, Claude's notice, or the question being asked.
        assert_eq!(feed_actions("PermissionRequest", Some("Bash")), &[PromptMaybe]);
        assert_eq!(feed_actions("Notification", None), &[PromptMaybe]);
        assert_eq!(feed_actions("PreToolUse", Some("AskUserQuestion")), &[PromptMaybe]);
        for quiet in ["PreToolUse", "PreCompact", "SubagentStop", ""] {
            assert!(feed_actions(quiet, Some("Edit")).is_empty(), "{quiet}");
        }
    }
}
