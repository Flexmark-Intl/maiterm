//! Background-shell roster for the maiLink chat surface (docs/mailink-protocol.md).
//!
//! Claude Code can run a `Bash` call with `run_in_background: true`; the TUI lists these under
//! `/bashes`, but nothing about them reached the phone. This module reconstructs the roster for a
//! session and — crucially — checks it against the OS, so the phone never offers a Stop button for
//! a process that already exited.
//!
//! **Two sources, deliberately split by what each can actually prove:**
//!
//! * **The transcript** is authoritative for IDENTITY and HISTORY — the shell id, the command, the
//!   agent's own description, when it started, and any outcome that was actually observed
//!   (`BashOutput` results carry `<status>`/`<exit_code>`, `KillShell` names a killed id). What it
//!   canNOT tell us is whether a shell that was last seen running is still running: nothing is
//!   appended when a background process exits on its own, so the transcript's "running" is only
//!   ever "was running when last polled".
//! * **The process table** is authoritative for LIVENESS. Claude Code spawns each background shell
//!   as a DIRECT CHILD of the `claude` process:
//!   `/bin/bash -c source …/shell-snapshots/… && eval '<command>' < /dev/null && pwd -P >| …`
//!   so the tab's claude pid plus one cached `ps` sweep answers "still alive?" — and gives the pid
//!   to kill, so Stop is a real signal rather than driving the TUI blind.
//!
//! The join between them is the COMMAND TEXT: the transcript's `command` appears verbatim inside
//! the process's `eval '…'`. Identical concurrent commands are matched newest-first, so a
//! duplicate can at worst attribute one live process to the wrong one of two identical entries —
//! both of which are genuinely running, so the roster stays truthful.
//!
//! Claude runtime only, and only for LOCAL tabs: an SSH tab's shells live on the remote host, out
//! of reach of the local process table. Rather than show a roster whose Stop buttons can't work,
//! SSH tabs report nothing (see `shells_for_tab`).

use serde_json::{json, Value};
use std::collections::HashMap;

/// A background shell as the phone renders it. Field names are the wire contract.
#[derive(Clone)]
pub struct AgentShell {
    pub id: String,
    pub command: String,
    pub description: Option<String>,
    pub status: ShellStatus,
    pub exit_code: Option<i64>,
    pub started_at: u64,
    pub ended_at: Option<u64>,
    pub tail: Option<String>,
    /// Live pid, when a matching process was found. Never sent to the phone — it's what
    /// `stop` signals.
    pub pid: Option<u32>,
}

#[derive(Clone, Copy, PartialEq)]
pub enum ShellStatus {
    Running,
    Completed,
    Failed,
    Killed,
}

impl ShellStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            ShellStatus::Running => "running",
            ShellStatus::Completed => "completed",
            ShellStatus::Failed => "failed",
            ShellStatus::Killed => "killed",
        }
    }
}

impl AgentShell {
    pub fn to_json(&self) -> Value {
        let mut v = json!({
            "id": self.id,
            "command": self.command,
            "status": self.status.as_str(),
            "startedAt": self.started_at,
        });
        if let Some(d) = &self.description {
            v["description"] = json!(d);
        }
        // Absent `exitCode` on a terminal status means "ended, outcome unobserved" — the shell
        // stopped without a final BashOutput poll, so no code was ever recorded. Never guessed.
        if let Some(c) = self.exit_code {
            v["exitCode"] = json!(c);
        }
        if let Some(e) = self.ended_at {
            v["endedAt"] = json!(e);
        }
        if let Some(t) = &self.tail {
            v["tail"] = json!(t);
        }
        v
    }
}

/// The marker Claude Code's background-Bash tool_result opens with. The id follows it.
const BG_ID_MARKER: &str = "Command running in background with ID: ";

/// Parse a session transcript into the shell roster, keyed by shell id and ordered by start.
/// Pure function over transcript lines — no I/O, no process access — so it is directly testable.
pub fn shells_from_lines(lines: &[Value]) -> Vec<AgentShell> {
    // tool_use id -> (command, description, ts) for background Bash calls, pending the
    // tool_result that reveals the shell id.
    let mut pending: HashMap<String, (String, Option<String>, u64)> = HashMap::new();
    let mut shells: Vec<AgentShell> = Vec::new();
    let mut index: HashMap<String, usize> = HashMap::new();

    for v in lines {
        let ts = v
            .get("timestamp")
            .and_then(|t| t.as_str())
            .map(super::transcript::rfc3339_to_ms)
            .unwrap_or(0)
            .max(0) as u64;
        let Some(blocks) = v.get("message").and_then(|m| m.get("content")).and_then(|c| c.as_array())
        else {
            continue;
        };
        for b in blocks {
            match b.get("type").and_then(|t| t.as_str()) {
                Some("tool_use") => {
                    let name = b.get("name").and_then(|n| n.as_str()).unwrap_or("");
                    let input = b.get("input");
                    let field = |k: &str| input.and_then(|i| i.get(k)).and_then(|x| x.as_str());
                    match name {
                        "Bash"
                            if input
                                .and_then(|i| i.get("run_in_background"))
                                .and_then(|r| r.as_bool())
                                == Some(true) =>
                        {
                            if let (Some(id), Some(cmd)) = (
                                b.get("id").and_then(|i| i.as_str()),
                                field("command"),
                            ) {
                                pending.insert(
                                    id.to_string(),
                                    (
                                        cmd.to_string(),
                                        field("description").map(str::to_string),
                                        ts,
                                    ),
                                );
                            }
                        }
                        // An explicit kill is authoritative — the roster must not keep offering
                        // Stop for something already stopped.
                        "KillShell" => {
                            if let Some(sid) = field("shell_id") {
                                if let Some(&i) = index.get(sid) {
                                    shells[i].status = ShellStatus::Killed;
                                    shells[i].ended_at = Some(ts);
                                }
                            }
                        }
                        _ => {}
                    }
                }
                Some("tool_result") => {
                    let text = tool_result_text(b);
                    // Start: the fixed "running in background with ID: <id>." sentence, tied back
                    // to its Bash call by tool_use_id.
                    if let Some(rest) = text.split(BG_ID_MARKER).nth(1) {
                        let id: String =
                            rest.chars().take_while(|c| c.is_ascii_alphanumeric()).collect();
                        let use_id = b.get("tool_use_id").and_then(|i| i.as_str()).unwrap_or("");
                        if let (false, Some((cmd, desc, started))) =
                            (id.is_empty(), pending.remove(use_id))
                        {
                            index.insert(id.clone(), shells.len());
                            shells.push(AgentShell {
                                id,
                                command: cmd,
                                description: desc,
                                status: ShellStatus::Running,
                                exit_code: None,
                                started_at: started,
                                ended_at: None,
                                tail: None,
                                pid: None,
                            });
                        }
                    }
                    // Poll: a BashOutput result states the observed status/exit code directly.
                    if let Some(obs) = parse_bash_output(&text) {
                        if let Some(&i) = index.get(&obs.task_id) {
                            apply_observation(&mut shells[i], obs, ts);
                        }
                    }
                }
                _ => {}
            }
        }
    }
    shells
}

/// A `BashOutput` tool_result, which reports on a shell by id.
struct Observation {
    task_id: String,
    status: Option<String>,
    exit_code: Option<i64>,
    tail: Option<String>,
}

fn apply_observation(shell: &mut AgentShell, obs: Observation, ts: u64) {
    if let Some(t) = obs.tail {
        shell.tail = Some(t);
    }
    let Some(status) = obs.status.as_deref() else { return };
    if status == "running" {
        return; // still running as of this poll; liveness is settled against the OS later
    }
    // A kill already recorded is more specific than "completed" — don't overwrite it.
    if shell.status != ShellStatus::Killed {
        shell.status = match obs.exit_code {
            Some(0) | None => ShellStatus::Completed,
            Some(_) => ShellStatus::Failed,
        };
    }
    shell.exit_code = obs.exit_code;
    shell.ended_at = Some(ts);
}

/// Extract the XML-ish fields Claude Code wraps a `BashOutput` result in. Returns `None` for any
/// other tool_result, and for non-`local_bash` tasks — the same `tasks/` namespace also holds
/// background SUBAGENTS, which are a different thing and not this roster's business.
fn parse_bash_output(text: &str) -> Option<Observation> {
    let task_id = tag(text, "task_id")?;
    if tag(text, "task_type").is_some_and(|t| t != "local_bash") {
        return None;
    }
    Some(Observation {
        task_id,
        status: tag(text, "status"),
        exit_code: tag(text, "exit_code").and_then(|c| c.parse().ok()),
        // A progress hint only — the LAST non-empty output line, never the log.
        tail: tag(text, "output").and_then(|o| {
            o.lines().rev().map(str::trim).find(|l| !l.is_empty()).map(|l| {
                let capped: String = l.chars().take(200).collect();
                capped
            })
        }),
    })
}

fn tag(text: &str, name: &str) -> Option<String> {
    let open = format!("<{name}>");
    let close = format!("</{name}>");
    let start = text.find(&open)? + open.len();
    let end = text[start..].find(&close)? + start;
    Some(text[start..end].trim().to_string())
}

/// A tool_result's text, whether it is a bare string or a list of content blocks.
fn tool_result_text(block: &Value) -> String {
    match block.get("content") {
        Some(Value::String(s)) => s.clone(),
        Some(Value::Array(items)) => items
            .iter()
            .filter_map(|i| i.get("text").and_then(|t| t.as_str()))
            .collect::<Vec<_>>()
            .join("\n"),
        _ => String::new(),
    }
}

/// Bytes of transcript scanned for the shell roster. Larger than the turn distiller's window: a
/// long-lived watcher can have been started far above the last few turns, and missing its start
/// line would drop a RUNNING shell from the roster entirely.
const SHELL_TAIL_BYTES: u64 = 32 * 1024 * 1024;

/// The roster for a Claude session id: transcript reconstruction settled against the live process
/// tree under `shell_pid`. `None` for a session with no locatable transcript.
pub fn roster(session_id: &str, shell_pid: Option<u32>) -> Option<Vec<AgentShell>> {
    let lines = super::transcript::claude_lines(session_id, SHELL_TAIL_BYTES)?;
    let mut shells = shells_from_lines(&lines);
    if shells.is_empty() {
        return Some(shells);
    }
    // No live agent (dormant tab) ⇒ no children ⇒ every "running" shell settles to ended, which is
    // correct: the shells were the agent's children and died with it.
    let children = shell_pid.map(crate::pty::manager::agent_background_children).unwrap_or_default();
    settle_liveness(&mut shells, &children);
    Some(shells)
}

/// Settle each shell's liveness against the OS, and attach the pid Stop will signal.
///
/// `children` is `(pid, command line)` for the direct children of the tab's `claude` process. A
/// shell the transcript last saw running is confirmed by finding its command inside a live child's
/// `eval '…'`; unmatched ones DID exit — reported terminal with no `exitCode`, since nothing ever
/// observed how they ended. Matched newest-first so identical concurrent commands each claim a
/// distinct pid.
pub fn settle_liveness(shells: &mut [AgentShell], children: &[(u32, String)]) {
    let mut taken: Vec<u32> = Vec::new();
    for shell in shells.iter_mut().rev() {
        if shell.status != ShellStatus::Running {
            continue;
        }
        let hit = children
            .iter()
            .find(|(pid, cmd)| !taken.contains(pid) && cmd.contains(shell.command.as_str()));
        match hit {
            Some((pid, _)) => {
                taken.push(*pid);
                shell.pid = Some(*pid);
            }
            None => {
                // Ended while nobody was watching. Status is terminal so the phone shows no Stop;
                // exit_code stays None, which the contract defines as "outcome unobserved".
                shell.status = ShellStatus::Completed;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Verbatim shapes from a real session: the background-Bash start pair, a BashOutput poll,
    /// and a KillShell.
    fn start(use_id: &str, cmd: &str, desc: &str, ts: &str) -> Value {
        json!({ "type": "assistant", "timestamp": ts, "message": { "content": [
            { "type": "tool_use", "id": use_id, "name": "Bash",
              "input": { "command": cmd, "description": desc, "run_in_background": true } }
        ] } })
    }
    fn started_result(use_id: &str, id: &str, ts: &str) -> Value {
        json!({ "type": "user", "timestamp": ts, "message": { "content": [
            { "type": "tool_result", "tool_use_id": use_id, "content": format!(
                "Command running in background with ID: {id}. Output is being written to: /tmp/x/tasks/{id}.output.") }
        ] } })
    }
    fn poll(id: &str, status: &str, exit: &str, output: &str, ts: &str) -> Value {
        let body = format!(
            "<task_id>{id}</task_id>\n<task_type>local_bash</task_type>\n<status>{status}</status>\n<exit_code>{exit}</exit_code>\n<output>{output}</output>");
        json!({ "type": "user", "timestamp": ts, "message": { "content": [
            { "type": "tool_result", "tool_use_id": "tr", "content": body } ] } })
    }

    #[test]
    fn reconstructs_roster_from_start_poll_and_kill() {
        let lines = vec![
            start("u1", "npm run dev", "Dev server", "2026-07-26T10:00:00Z"),
            started_result("u1", "bkbod6zxj", "2026-07-26T10:00:01Z"),
            start("u2", "sleep 900", "Waiter", "2026-07-26T10:00:02Z"),
            started_result("u2", "b5tx1yzxh", "2026-07-26T10:00:03Z"),
            // A poll that reports still-running must NOT mark it terminal, but does set tail.
            poll("bkbod6zxj", "running", "", "compiling\n  ready on :3000\n", "2026-07-26T10:01:00Z"),
            // A terminal poll with a non-zero code is a failure, not a completion.
            poll("b5tx1yzxh", "completed", "1", "boom\n", "2026-07-26T10:02:00Z"),
        ];
        let shells = shells_from_lines(&lines);
        assert_eq!(shells.len(), 2);

        assert_eq!(shells[0].id, "bkbod6zxj");
        assert_eq!(shells[0].command, "npm run dev");
        assert_eq!(shells[0].description.as_deref(), Some("Dev server"));
        assert!(shells[0].status == ShellStatus::Running);
        // tail is the LAST non-empty output line, not the whole capture.
        assert_eq!(shells[0].tail.as_deref(), Some("ready on :3000"));
        assert!(shells[0].ended_at.is_none());

        assert!(shells[1].status == ShellStatus::Failed, "exit 1 is a failure");
        assert_eq!(shells[1].exit_code, Some(1));
        assert!(shells[1].ended_at.is_some());
    }

    #[test]
    fn kill_wins_over_a_later_completed_poll() {
        let lines = vec![
            start("u1", "tail -f log", "Watcher", "2026-07-26T10:00:00Z"),
            started_result("u1", "bzz111aaa", "2026-07-26T10:00:01Z"),
            json!({ "type": "assistant", "timestamp": "2026-07-26T10:03:00Z", "message": { "content": [
                { "type": "tool_use", "id": "u9", "name": "KillShell", "input": { "shell_id": "bzz111aaa" } } ] } }),
            poll("bzz111aaa", "completed", "0", "bye\n", "2026-07-26T10:04:00Z"),
        ];
        let shells = shells_from_lines(&lines);
        assert!(shells[0].status == ShellStatus::Killed, "an explicit kill is more specific");
    }

    #[test]
    fn background_subagents_are_not_shells() {
        // The same tasks/ namespace holds background SUBAGENTS; a non-local_bash task_type must
        // not touch the roster.
        let lines = vec![
            start("u1", "npm test", "Tests", "2026-07-26T10:00:00Z"),
            started_result("u1", "bqqq222", "2026-07-26T10:00:01Z"),
            json!({ "type": "user", "timestamp": "2026-07-26T10:01:00Z", "message": { "content": [
                { "type": "tool_result", "tool_use_id": "tr", "content":
                  "<task_id>bqqq222</task_id>\n<task_type>subagent</task_type>\n<status>completed</status>\n<exit_code>0</exit_code>" } ] } }),
        ];
        let shells = shells_from_lines(&lines);
        assert!(shells[0].status == ShellStatus::Running, "subagent result must be ignored");
    }

    #[test]
    fn liveness_confirms_running_shells_and_retires_vanished_ones() {
        let lines = vec![
            start("u1", "npm run dev", "Dev", "2026-07-26T10:00:00Z"),
            started_result("u1", "alive1", "2026-07-26T10:00:01Z"),
            start("u2", "python3 -m http.server 9999", "Static", "2026-07-26T10:00:02Z"),
            started_result("u2", "gone1", "2026-07-26T10:00:03Z"),
        ];
        let mut shells = shells_from_lines(&lines);
        // Real shape of a Claude background child: the command inside `eval '…'`.
        let children = vec![(
            4242u32,
            "/bin/bash -c source /Users/x/.claude/shell-snapshots/snapshot-bash-1.sh 2>/dev/null || true && eval 'npm run dev' < /dev/null && pwd -P >| /tmp/claude-1234-cwd".to_string(),
        )];
        settle_liveness(&mut shells, &children);

        assert!(shells[0].status == ShellStatus::Running);
        assert_eq!(shells[0].pid, Some(4242), "pid is what Stop signals");
        // No live process ⇒ it ended unobserved: terminal (so no Stop button) with NO exit code.
        assert!(shells[1].status == ShellStatus::Completed);
        assert_eq!(shells[1].pid, None);
        assert_eq!(shells[1].exit_code, None, "an unobserved outcome is never guessed");
        assert!(shells[1].to_json().get("exitCode").is_none());
    }

    #[test]
    fn identical_commands_claim_distinct_pids() {
        let lines = vec![
            start("u1", "sleep 60", "A", "2026-07-26T10:00:00Z"),
            started_result("u1", "one", "2026-07-26T10:00:01Z"),
            start("u2", "sleep 60", "B", "2026-07-26T10:00:02Z"),
            started_result("u2", "two", "2026-07-26T10:00:03Z"),
        ];
        let mut shells = shells_from_lines(&lines);
        let children = vec![
            (11u32, "/bin/bash -c … eval 'sleep 60' < /dev/null".to_string()),
            (22u32, "/bin/bash -c … eval 'sleep 60' < /dev/null".to_string()),
        ];
        settle_liveness(&mut shells, &children);
        let pids: Vec<Option<u32>> = shells.iter().map(|s| s.pid).collect();
        assert!(shells.iter().all(|s| s.status == ShellStatus::Running));
        assert_ne!(pids[0], pids[1], "each entry must claim its own pid, not share one");
    }
}
