//! Delegation roster for the maiLink chat surface (docs/mailink-protocol.md).
//!
//! A subagent — Claude Code's `Agent` tool — is the most common thing an agent does that takes
//! minutes, and until now it reached the phone as a single tool chip at launch and nothing after.
//! A five-minute review rendered as one line that never changed. This module reconstructs the
//! roster the same way `shells.rs` does for background shells, and the phone renders it the same
//! way: a strip with elapsed time.
//!
//! **The lifecycle is NOT "a tool_use with no matching tool_result".** That is the natural guess
//! and it is wrong here, in the direction that silently breaks the feature: the `Agent` tool
//! returns within ~2s with an *acknowledgement*, not a result —
//!
//! ```text
//! tool_use   { name: "Agent", input: { description, subagent_type, prompt } }
//! tool_result "Async agent launched successfully. … agentId: ad45d0719f4187070 …
//!              output_file: …/tasks/ad45d0719f4187070.output"
//! ```
//!
//! so pairing use with result would mark every delegation done two seconds after it started.
//! Completion arrives much later as its own synthetic user turn:
//!
//! ```text
//! <task-notification>
//! <task-id>ad45d0719f4187070</task-id> <tool-use-id>toolu_…</tool-use-id>
//! <status>completed</status> <summary>Agent "…" finished</summary> <result>…</result>
//! ```
//!
//! Three ids are in play and only one is the join. The `tool_use` id ties the launch to its ack;
//! the ack reveals the **agentId**, which is what the notification names and therefore what the
//! roster is keyed by. The `<summary>` is a fixed template (`Agent "<description>" finished`) and
//! carries no information the launch didn't — `<result>` is where the agent's actual answer is.
//!
//! **Two sources, split by what each can prove** — the same division `shells.rs` draws:
//!
//! * **The parent transcript** is authoritative for IDENTITY and OUTCOME: the agentId, the
//!   description, the subagent type, when it started, and — via the task-notification — that it
//!   finished and how. This half works for SSH tabs too, since the mirror shadows the parent
//!   JSONL (mirror.rs).
//! * **The per-agent sidecar** (`<projects>/<slug>/<session>/subagents/agent-<agentId>.jsonl`,
//!   which the ack's `output_file` symlinks to) is where the subagent's OWN turns are — the only
//!   place its progress can be read from. Its path is derived from the located parent transcript
//!   rather than from `output_file`, whose `/private/tmp/…` value is meaningful only on the
//!   machine that wrote it.
//!
//! Note what is NOT a source: `isSidechain`. Subagent turns used to be interleaved into the
//! parent JSONL under that flag; in this Claude Code they are not (every line of a real 8177-line
//! session reads `isSidechain: false`), and they live in the sidecar instead.

use serde_json::{json, Value};
use std::collections::HashMap;
use std::path::PathBuf;

/// A delegation as the phone renders it. Field names are the wire contract.
#[derive(Clone)]
pub struct Subagent {
    /// The agentId from the launch ack — the id the completion notification names.
    pub id: String,
    /// The agent's own 3-5 word label for the job ("Review the start-verb wiring"). The whole
    /// point of the strip: "a subagent is running" without saying which is barely better than
    /// "working".
    pub description: String,
    pub agent_type: Option<String>,
    pub status: SubagentStatus,
    pub started_at: u64,
    pub ended_at: Option<u64>,
    /// The subagent's most recent words about its own progress — its last assistant text while
    /// running, or the opening of its `<result>` once done. Always paired with `last_line_ts`:
    /// it is a claim the subagent made at a moment, and it goes stale.
    pub last_line: Option<String>,
    pub last_line_ts: Option<u64>,
}

#[derive(Clone, Copy, PartialEq)]
pub enum SubagentStatus {
    Running,
    Done,
    Failed,
}

impl SubagentStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            SubagentStatus::Running => "running",
            SubagentStatus::Done => "done",
            SubagentStatus::Failed => "failed",
        }
    }
}

impl Subagent {
    pub fn to_json(&self) -> Value {
        let mut v = json!({
            "id": self.id,
            "description": self.description,
            "status": self.status.as_str(),
            "startedAt": self.started_at,
        });
        if let Some(t) = &self.agent_type {
            v["agentType"] = json!(t);
        }
        if let Some(e) = self.ended_at {
            v["endedAt"] = json!(e);
        }
        // Sent as a pair or not at all: a progress line with no time on it invites the phone to
        // present a nine-minute-old claim as current. Same reason the Overlord card's age line is
        // load-bearing.
        if let (Some(l), Some(ts)) = (&self.last_line, self.last_line_ts) {
            v["lastLine"] = json!(l);
            v["lastLineTs"] = json!(ts);
        }
        v
    }
}

/// The launch ack's marker. The agentId follows it, and its presence is what distinguishes a real
/// launch from an `Agent` call that errored before starting anything.
const AGENT_ID_MARKER: &str = "agentId: ";

/// Parse a session transcript into the delegation roster, keyed by agentId and ordered by launch.
/// Pure function over transcript lines — no I/O — so it is directly testable.
pub fn subagents_from_lines(lines: &[Value]) -> Vec<Subagent> {
    // tool_use id -> (description, agent_type, ts), pending the ack that reveals the agentId.
    let mut pending: HashMap<String, (String, Option<String>, u64)> = HashMap::new();
    let mut agents: Vec<Subagent> = Vec::new();
    let mut index: HashMap<String, usize> = HashMap::new();

    for v in lines {
        let ts = line_ts(v);
        let content = v.get("message").and_then(|m| m.get("content"));

        // The completion notification is a synthetic USER turn whose content is a bare string,
        // not the block array every other path here reads — so it must be looked for in both
        // shapes or completion is never seen at all and every delegation shows as running.
        if let Some(text) = plain_text(content) {
            if let Some(note) = parse_task_notification(&text) {
                if let Some(&i) = index.get(&note.task_id) {
                    apply_completion(&mut agents[i], note, ts);
                }
            }
        }

        let Some(blocks) = content.and_then(|c| c.as_array()) else { continue };
        for b in blocks {
            match b.get("type").and_then(|t| t.as_str()) {
                // A finished agent can be sent BACK to work: the launch ack advertises
                // `SendMessage with to: '<agentId>'`, and the CLI then emits a fresh
                // task-notification only when the resumed run STOPS. Without this the roster
                // reports the earlier outcome for the whole re-run — observed on real data as
                // 9 minutes of "finished 54 minutes ago" while it was demonstrably working, with
                // a frozen `lastLine` to match, because `attach_sidecar` skips non-running
                // entries. The same shape `shells.rs` handles by watching for `KillShell`.
                Some("tool_use")
                    if b.get("name").and_then(|n| n.as_str()) == Some("SendMessage") =>
                {
                    let to = b.get("input").and_then(|i| i.get("to")).and_then(|t| t.as_str());
                    if let Some(&i) = to.and_then(|t| index.get(t)) {
                        // Only a TERMINAL entry is being restarted. The ack advertises
                        // SendMessage as the way to "continue this agent" and does not restrict
                        // it to finished ones, so a message to one still working is ordinary —
                        // and resetting its clock would rewind the phone's elapsed timer to zero
                        // for a delegation that never stopped.
                        if agents[i].status != SubagentStatus::Running {
                            agents[i].status = SubagentStatus::Running;
                            agents[i].ended_at = None;
                            agents[i].started_at = ts;
                        }
                    }
                }
                Some("tool_use") if b.get("name").and_then(|n| n.as_str()) == Some("Agent") => {
                    let input = b.get("input");
                    let field = |k: &str| input.and_then(|i| i.get(k)).and_then(|x| x.as_str());
                    // No description ⇒ nothing worth a chip. Fall back to the prompt's opening
                    // rather than showing an anonymous entry.
                    let label = field("description")
                        .or_else(|| field("prompt"))
                        .map(|s| capped(s, 160));
                    if let (Some(id), Some(label)) =
                        (b.get("id").and_then(|i| i.as_str()), label)
                    {
                        pending.insert(
                            id.to_string(),
                            (label, field("subagent_type").map(str::to_string), ts),
                        );
                    }
                }
                Some("tool_result") => {
                    let text = tool_result_text(b);
                    let Some(rest) = text.split(AGENT_ID_MARKER).nth(1) else { continue };
                    let id: String =
                        rest.chars().take_while(|c| c.is_ascii_alphanumeric()).collect();
                    let use_id = b.get("tool_use_id").and_then(|i| i.as_str()).unwrap_or("");
                    let Some((description, agent_type, started_at)) = pending.remove(use_id)
                    else {
                        continue;
                    };
                    if id.is_empty() {
                        continue;
                    }
                    // A resumed session REPLAYS earlier lines, so the same launch and ack can
                    // appear twice in one transcript — verified on disk: four agentIds, each
                    // acked twice under a duplicated tool_use id. Pushing again would emit two
                    // roster rows sharing one `id` into a keyed list on the phone, and would also
                    // resurrect an entry the later notification had already completed, since the
                    // replay re-registers it as Running. The replay is the same delegation, so
                    // the first entry stands.
                    if index.contains_key(&id) {
                        continue;
                    }
                    index.insert(id.clone(), agents.len());
                    agents.push(Subagent {
                        id,
                        description,
                        agent_type,
                        status: SubagentStatus::Running,
                        started_at,
                        ended_at: None,
                        last_line: None,
                        last_line_ts: None,
                    });
                }
                _ => {}
            }
        }
        // A notification can also arrive as a text block inside an array-shaped user turn.
        for b in blocks {
            if b.get("type").and_then(|t| t.as_str()) != Some("text") {
                continue;
            }
            let Some(text) = b.get("text").and_then(|t| t.as_str()) else { continue };
            if let Some(note) = parse_task_notification(text) {
                if let Some(&i) = index.get(&note.task_id) {
                    apply_completion(&mut agents[i], note, ts);
                }
            }
        }
    }
    agents
}

/// A `<task-notification>` turn: the subagent stopped, and this says how.
struct Notification {
    task_id: String,
    status: Option<String>,
    result: Option<String>,
}

/// Apply a completion. Re-openable on purpose: the notification's own `<note>` states that an
/// agent can be resumed and "the same task-id may notify more than once", so this must be a
/// last-writer-wins stamp rather than a one-way latch that ignores the second outcome.
fn apply_completion(agent: &mut Subagent, note: Notification, ts: u64) {
    agent.status = match note.status.as_deref() {
        // Anything that is not an outright success is reported as a failure rather than
        // guessed at — `cancelled`, `error` and an absent status all mean "did not complete".
        Some("completed") | Some("success") => SubagentStatus::Done,
        _ => SubagentStatus::Failed,
    };
    agent.ended_at = Some(ts);
    // The subagent's actual answer. `<summary>` is skipped deliberately: it is the fixed template
    // `Agent "<description>" finished`, so surfacing it would restate the chip's own label back
    // at the reader as if it were news.
    if let Some(r) = note.result {
        if let Some(line) = first_meaningful_line(&r) {
            agent.last_line = Some(line);
            agent.last_line_ts = Some(ts);
        }
    }
}

fn parse_task_notification(text: &str) -> Option<Notification> {
    if !text.trim_start().starts_with("<task-notification>") {
        return None;
    }
    Some(Notification {
        task_id: tag(text, "task-id")?,
        status: tag(text, "status"),
        result: tag(text, "result"),
    })
}

fn tag(text: &str, name: &str) -> Option<String> {
    let open = format!("<{name}>");
    let close = format!("</{name}>");
    let start = text.find(&open)? + open.len();
    let end = text[start..].find(&close)? + start;
    Some(text[start..end].trim().to_string())
}

/// The first line that makes a STATEMENT — skipping blanks, markdown headings and horizontal
/// rules, which are structure and say nothing on their own.
///
/// A heading has to be skipped rather than stripped: an agent's answer routinely opens
/// `## Findings`, and stripping the hashes yields the progress line "Findings", which is the
/// same non-information one glyph shorter. Falling back to the first non-empty line keeps a
/// result that is ONLY a heading (`## No issues found`) from coming back empty.
fn first_meaningful_line(s: &str) -> Option<String> {
    let is_rule = |l: &str| l.chars().all(|c| matches!(c, '-' | '=' | '*' | '_' | ' '));
    // `[harness: subagent output matched instruction-shaped pattern(s): …]` — an annotation the
    // CLI wraps around a subagent's answer, not the subagent's words. Observed on 2 of the 10
    // real delegations in this session's transcript, where it displaced the actual finding.
    let clean = |l: &str| capped(l.trim_start_matches(['-', '*', '+', '#', ' ']), 200);
    s.lines()
        .map(str::trim)
        .find(|l| {
            !l.is_empty() && !l.starts_with('#') && !l.starts_with("[harness:") && !is_rule(l)
        })
        .map(clean)
        .or_else(|| s.lines().map(str::trim).find(|l| !l.is_empty()).map(clean))
        .filter(|l| !l.trim().is_empty())
}

fn capped(s: &str, max: usize) -> String {
    let one = s.replace('\n', " ");
    if one.chars().count() > max {
        format!("{} …", one.chars().take(max).collect::<String>())
    } else {
        one
    }
}

fn line_ts(v: &Value) -> u64 {
    v.get("timestamp")
        .and_then(|t| t.as_str())
        .map(super::transcript::rfc3339_to_ms)
        .unwrap_or(0)
        .max(0) as u64
}

/// A message content that is a bare string, which is how the task-notification turn arrives.
fn plain_text(content: Option<&Value>) -> Option<String> {
    match content {
        Some(Value::String(s)) => Some(s.clone()),
        _ => None,
    }
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

/// Bytes of parent transcript scanned. Matches the shell roster's window and for the same reason:
/// a delegation can have been launched far above the last few turns, and missing its launch line
/// would drop a RUNNING entry from the roster entirely.
const SUBAGENT_TAIL_BYTES: u64 = 32 * 1024 * 1024;

/// Bytes of a sidecar scanned for the progress line. Only the end is ever wanted, and a sidecar
/// runs to hundreds of KB.
const SIDECAR_TAIL_BYTES: u64 = 64 * 1024;

/// How long a running entry's sidecar may go untouched before it is treated as ended.
///
/// Very generous on purpose, and the asymmetry is the whole design. The case this exists for is
/// not a slow agent but a DEAD PARENT: the CLI fires a notification whenever a subagent stops, so
/// the only way to strand a `running` entry is for the session to be killed mid-delegation. A
/// stale entry lingering costs one wrong chip on a screen nobody is watching; retiring an agent
/// that is still working reproduces the exact "it says finished but it isn't" defect this module
/// exists to end — and it self-corrects noisily, because the settle is an inference re-derived
/// each tick, so the entry flips back to `running` the moment the parent transcript next moves.
///
/// A subagent is silent for as long as its current tool call takes, and a real review here sat
/// 9 minutes inside a single `Bash`. 30 minutes was the first value and it is not enough headroom
/// over that: one long build or test run crosses it.
const SIDECAR_STALE_MS: u64 = 2 * 60 * 60 * 1000;

/// How long since LAUNCH before a running entry with no readable sidecar is treated as ended.
///
/// Without a sidecar there is no liveness evidence at all, so elapsed time is the only bound
/// available — hence a much larger one than the sidecar rule. It is not an edge case: an SSH
/// tab's sidecar lives on the remote host and is never mirrored, and a session RESUME moves new
/// sidecars under the new session id while a tab may still resolve to the old one. Both leave
/// entries that only a task-notification on the parent transcript can retire, and if the mirror
/// stops (a dropped tunnel) or the notification landed in the resumed transcript, nothing ever
/// does. Real instance on this machine before the rule: two delegations reading `running` with a
/// growing elapsed timer for six months.
const NO_SIDECAR_STALE_MS: u64 = 12 * 60 * 60 * 1000;

/// The roster for a Claude session id: transcript reconstruction plus each entry's progress line.
/// `None` for a session with no locatable transcript. The REST path, which pays both halves.
pub fn roster(session_id: &str) -> Option<Vec<Subagent>> {
    let mut agents = roster_from_transcript(session_id)?;
    refresh_progress(&mut agents, session_id);
    Some(agents)
}

/// The EXPENSIVE half — a 32 MB tail read and a JSON parse per line of a transcript that runs to
/// tens of MB here. Split out so the live ticker can gate it on the parent transcript's mtime,
/// which is sound for delegations and would not be for shells: a shell exiting appends nothing,
/// but a subagent's launch AND its completion notification are both written to the parent.
pub fn roster_from_transcript(session_id: &str) -> Option<Vec<Subagent>> {
    let lines = super::transcript::claude_lines(session_id, SUBAGENT_TAIL_BYTES)?;
    Some(subagents_from_lines(&lines))
}

/// The CHEAP half — a 64 KB tail per RUNNING entry, so it costs nothing on the overwhelmingly
/// common tick where a tab has no delegation in flight. Must run every tick even when the parent
/// transcript hasn't moved: a subagent's progress goes to its own sidecar, so between launch and
/// completion the parent is silent and the mtime gate would freeze the strip's status line for
/// the entire run — the exact "one line that never changes" this feature exists to end.
pub fn refresh_progress(agents: &mut [Subagent], session_id: &str) {
    if !agents.iter().any(|a| a.status == SubagentStatus::Running) {
        return;
    }
    let Some(transcript) = super::transcript::claude_transcript_path(session_id) else { return };
    let dir = sidecar_dir(&transcript, session_id);
    let now = super::now_ms();
    for a in agents.iter_mut() {
        attach_sidecar(a, &dir, now);
    }
}

/// `<projects>/<slug>/<session_id>/subagents/`, derived from the located parent transcript.
///
/// Deliberately not the ack's `output_file`: that names a `/private/tmp/claude-<uid>/…` path on
/// the machine that ran the agent, which for an SSH tab is another host entirely. Deriving it
/// means a mirrored SSH tab simply finds no sidecar and degrades to lifecycle-only, rather than
/// reading some local path that happens to collide.
fn sidecar_dir(transcript: &PathBuf, session_id: &str) -> PathBuf {
    transcript
        .parent()
        .map(|p| p.join(session_id).join("subagents"))
        .unwrap_or_else(|| PathBuf::from(session_id))
}

/// What a sidecar's mtime says about a RUNNING entry. Pure decision (unit-tested; `attach_sidecar`
/// wires the real file reads), because every interesting case here is a clock relationship and
/// none of them are worth a temp file with a forged mtime to express.
#[derive(Debug, PartialEq)]
enum SidecarVerdict {
    /// It is this run's, and recent: read the progress line, leave the status alone.
    Live,
    /// Nothing can be concluded yet. Touch nothing.
    NoEvidence,
    /// Silent long enough to call it ended. `Some(ms)` when we know when — an mtime is a real
    /// upper bound; `None` when there is nothing to date it by, and it is never guessed.
    Ended(Option<u64>),
}

fn sidecar_verdict(started_at: u64, mtime: Option<u64>, now: u64) -> SidecarVerdict {
    // A file last written BEFORE this run began belongs to the PREVIOUS one — a resumed agent
    // inherits its predecessor's file at the same path. Judging this run by it settled the entry
    // instantly with an `ended_at` earlier than its `started_at`, and it must not supply a
    // progress line either, which would show the last run's sign-off as this run's status.
    //
    // So such a file is treated as ABSENT rather than as its own case. That is not tidiness: a
    // dedicated no-evidence branch here would never settle at all, and a resume whose subagent
    // then never writes — the parent dies right after handing off — would strand `running`
    // forever, since the elapsed-since-launch rule below only applies when there is no file.
    // Which is the same class of permanent-`running` bug that the six-month entries were.
    let usable = mtime.filter(|&m| m > 0 && m >= started_at);
    let Some(mtime) = usable else {
        // `started_at > 0` is not a formality. `line_ts` yields 0 for a timestamp it could not
        // read, which means "I don't know when", and without the guard this reads it as "launched
        // at the epoch, therefore ancient" and retires the entry hardest of all. That is this
        // codebase's most repeated defect — a default read as a positive claim — and a missing
        // launch time is LESS evidence than a missing sidecar, not more.
        let old = started_at > 0 && now.saturating_sub(started_at) > NO_SIDECAR_STALE_MS;
        return if old { SidecarVerdict::Ended(None) } else { SidecarVerdict::NoEvidence };
    };
    if now.saturating_sub(mtime) > SIDECAR_STALE_MS {
        SidecarVerdict::Ended(Some(mtime))
    } else {
        SidecarVerdict::Live
    }
}

/// Read one entry's sidecar for its latest progress line, and settle a stale `running`.
///
/// A missing sidecar means no evidence, not "it stopped": an SSH tab's sidecar lives on the remote
/// host and is never mirrored. Those settle by elapsed time since launch instead, and the
/// transcript's own notification still retires them normally — it rides the mirrored parent JSONL
/// — so only a parent killed mid-delegation ever reaches the timeout.
fn attach_sidecar(agent: &mut Subagent, dir: &PathBuf, now: u64) {
    // A finished agent already carries its `<result>` opening as `last_line`, which is a better
    // answer than its last in-flight remark — nothing here can improve on it, so not even the
    // stat is worth paying. Most entries on a settled roster are in this state.
    if agent.status != SubagentStatus::Running {
        return;
    }
    let path = dir.join(format!("agent-{}.jsonl", agent.id));
    let mtime = std::fs::metadata(&path).ok().and_then(|m| {
        m.modified()
            .ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|d| d.as_millis() as u64)
    });
    match sidecar_verdict(agent.started_at, mtime, now) {
        SidecarVerdict::NoEvidence => {}
        SidecarVerdict::Live => {
            if let Some((line, ts)) = last_assistant_line(&path) {
                agent.last_line = Some(line);
                agent.last_line_ts = Some(ts);
            }
        }
        SidecarVerdict::Ended(at) => {
            // Ended while nobody was watching. `done` with no observed outcome — the same
            // convention `shells.rs` uses for a shell that vanished without a final poll: render
            // "ended", never "succeeded".
            agent.status = SubagentStatus::Done;
            agent.ended_at = at;
        }
    }
}

/// The subagent's most recent assistant TEXT, with the timestamp of the line that carried it.
///
/// `thinking` blocks are skipped: they are the model reasoning to itself, not a progress report,
/// and are the wrong thing to put on a phone as a status line.
fn last_assistant_line(path: &PathBuf) -> Option<(String, u64)> {
    let body = super::transcript::read_tail(path, SIDECAR_TAIL_BYTES)?;
    for line in body.lines().rev() {
        let Ok(v) = serde_json::from_str::<Value>(line) else { continue };
        if v.get("type").and_then(|t| t.as_str()) != Some("assistant") {
            continue;
        }
        let Some(blocks) = v.get("message").and_then(|m| m.get("content")).and_then(|c| c.as_array())
        else {
            continue;
        };
        let text = blocks
            .iter()
            .filter(|b| b.get("type").and_then(|t| t.as_str()) == Some("text"))
            .filter_map(|b| b.get("text").and_then(|t| t.as_str()))
            .find_map(first_meaningful_line);
        if let Some(t) = text {
            return Some((t, line_ts(&v)));
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Verbatim shapes from a real session: the launch pair and the completion notification.
    fn launch(use_id: &str, desc: &str, kind: &str, ts: &str) -> Value {
        json!({ "type": "assistant", "timestamp": ts, "message": { "content": [
            { "type": "tool_use", "id": use_id, "name": "Agent",
              "input": { "description": desc, "subagent_type": kind, "prompt": "Review it." } }
        ] } })
    }
    fn ack(use_id: &str, agent_id: &str, ts: &str) -> Value {
        json!({ "type": "user", "timestamp": ts, "message": { "content": [
            { "type": "tool_result", "tool_use_id": use_id, "content": [ { "type": "text", "text": format!(
                "Async agent launched successfully. (This tool result is internal metadata …)\nagentId: {agent_id} (internal ID - do not mention to user.)\nThe agent is working in the background.\noutput_file: /private/tmp/claude-503/x/y/tasks/{agent_id}.output") } ] }
        ] } })
    }
    fn notify(agent_id: &str, status: &str, result: &str, ts: &str) -> Value {
        json!({ "type": "user", "timestamp": ts, "message": { "content": format!(
            "<task-notification>\n<task-id>{agent_id}</task-id>\n<tool-use-id>toolu_x</tool-use-id>\n<status>{status}</status>\n<summary>Agent \"Review the wiring\" finished</summary>\n<result>{result}</result>\n</task-notification>") } })
    }

    #[test]
    fn launch_ack_is_not_completion() {
        // The regression this whole module exists to avoid: the ack arrives seconds after the
        // launch, so pairing tool_use with tool_result marks every delegation instantly done.
        let lines = vec![
            launch("u1", "Review the start-verb wiring", "code-reviewer", "2026-09-08T10:00:00Z"),
            ack("u1", "ad45d0719f4187070", "2026-09-08T10:00:02Z"),
        ];
        let agents = subagents_from_lines(&lines);
        assert_eq!(agents.len(), 1);
        assert_eq!(agents[0].id, "ad45d0719f4187070", "keyed by agentId, not the tool_use id");
        assert_eq!(agents[0].description, "Review the start-verb wiring");
        assert_eq!(agents[0].agent_type.as_deref(), Some("code-reviewer"));
        assert!(agents[0].status == SubagentStatus::Running, "the ack means launched, not done");
        assert!(agents[0].ended_at.is_none());
    }

    #[test]
    fn notification_completes_and_carries_the_result_not_the_summary() {
        let lines = vec![
            launch("u1", "Review the wiring", "code-reviewer", "2026-09-08T10:00:00Z"),
            ack("u1", "aaa111", "2026-09-08T10:00:02Z"),
            notify("aaa111", "completed", "## Findings\n\nThe busy map is never cleared on the 404 path.", "2026-09-08T10:09:00Z"),
        ];
        let agents = subagents_from_lines(&lines);
        assert!(agents[0].status == SubagentStatus::Done);
        assert!(agents[0].ended_at.is_some());
        // `<summary>` is the template `Agent "…" finished` — restating the chip's own label.
        assert_eq!(
            agents[0].last_line.as_deref(),
            Some("The busy map is never cleared on the 404 path."),
            "a markdown heading is structure, not the finding"
        );
        assert!(agents[0].last_line_ts.is_some(), "a progress line is never sent undated");
    }

    #[test]
    fn a_result_that_is_only_a_heading_still_says_something() {
        // The skip-headings rule must not be able to empty a short result out entirely.
        let lines = vec![
            launch("u1", "Review", "code-reviewer", "2026-09-08T10:00:00Z"),
            ack("u1", "ddd444", "2026-09-08T10:00:02Z"),
            notify("ddd444", "completed", "## No issues found", "2026-09-08T10:05:00Z"),
        ];
        let agents = subagents_from_lines(&lines);
        assert_eq!(agents[0].last_line.as_deref(), Some("No issues found"));
    }

    #[test]
    fn harness_scaffolding_is_not_the_agents_words() {
        let lines = vec![
            launch("u1", "Review", "code-reviewer", "2026-09-08T10:00:00Z"),
            ack("u1", "fff666", "2026-09-08T10:00:02Z"),
            notify(
                "fff666",
                "completed",
                "[harness: subagent output matched instruction-shaped pattern(s): settings-json.]\nThe retry loop never resets its counter.",
                "2026-09-08T10:05:00Z",
            ),
        ];
        let agents = subagents_from_lines(&lines);
        assert_eq!(
            agents[0].last_line.as_deref(),
            Some("The retry loop never resets its counter.")
        );
    }

    #[test]
    fn a_bullet_result_keeps_its_text_and_loses_its_marker() {
        let lines = vec![
            launch("u1", "Review", "code-reviewer", "2026-09-08T10:00:00Z"),
            ack("u1", "eee555", "2026-09-08T10:00:02Z"),
            notify("eee555", "completed", "---\n\n- The 404 path leaks a lock.", "2026-09-08T10:05:00Z"),
        ];
        let agents = subagents_from_lines(&lines);
        assert_eq!(agents[0].last_line.as_deref(), Some("The 404 path leaks a lock."));
    }

    #[test]
    fn a_non_success_status_is_a_failure_never_a_completion() {
        let lines = vec![
            launch("u1", "Do a thing", "general-purpose", "2026-09-08T10:00:00Z"),
            ack("u1", "bbb222", "2026-09-08T10:00:02Z"),
            notify("bbb222", "cancelled", "", "2026-09-08T10:01:00Z"),
        ];
        let agents = subagents_from_lines(&lines);
        assert!(agents[0].status == SubagentStatus::Failed);
    }

    #[test]
    fn a_resumed_agent_reopens_rather_than_latching() {
        // The notification's own note: an agent can be sent another message and resume, so the
        // same task-id notifies more than once. Last outcome wins.
        let lines = vec![
            launch("u1", "Review", "code-reviewer", "2026-09-08T10:00:00Z"),
            ack("u1", "ccc333", "2026-09-08T10:00:02Z"),
            notify("ccc333", "completed", "First pass done.", "2026-09-08T10:05:00Z"),
            notify("ccc333", "cancelled", "", "2026-09-08T10:20:00Z"),
        ];
        let agents = subagents_from_lines(&lines);
        assert!(agents[0].status == SubagentStatus::Failed, "the later outcome is the outcome");
        let expected = crate::mailink::transcript::rfc3339_to_ms("2026-09-08T10:20:00Z") as u64;
        assert_eq!(agents[0].ended_at, Some(expected));
    }

    #[test]
    fn an_agent_call_that_never_launched_is_not_in_the_roster() {
        // A tool_result with no `agentId:` marker is an error, not a launch. Without the marker
        // check it would become a roster entry with an empty id that nothing can ever complete.
        let lines = vec![
            launch("u1", "Review", "code-reviewer", "2026-09-08T10:00:00Z"),
            json!({ "type": "user", "timestamp": "2026-09-08T10:00:01Z", "message": { "content": [
                { "type": "tool_result", "tool_use_id": "u1", "content": "Error: unknown subagent_type" } ] } }),
        ];
        assert!(subagents_from_lines(&lines).is_empty());
    }

    #[test]
    fn concurrent_delegations_stay_distinct() {
        let lines = vec![
            launch("u1", "Review the wiring", "code-reviewer", "2026-09-08T10:00:00Z"),
            launch("u2", "Explore the callers", "Explore", "2026-09-08T10:00:01Z"),
            ack("u1", "aaa111", "2026-09-08T10:00:03Z"),
            ack("u2", "bbb222", "2026-09-08T10:00:04Z"),
            notify("bbb222", "completed", "Found four call sites.", "2026-09-08T10:02:00Z"),
        ];
        let agents = subagents_from_lines(&lines);
        assert_eq!(agents.len(), 2);
        // The acks arrive out of launch order relative to nothing in particular — the join is
        // tool_use_id, so each entry must keep its own label.
        assert_eq!(agents[0].description, "Review the wiring");
        assert_eq!(agents[1].description, "Explore the callers");
        assert!(agents[0].status == SubagentStatus::Running);
        assert!(agents[1].status == SubagentStatus::Done);
        assert!(agents[0].to_json().get("lastLine").is_none(), "no progress read yet");
    }

    fn send_message(to: &str, ts: &str) -> Value {
        json!({ "type": "assistant", "timestamp": ts, "message": { "content": [
            { "type": "tool_use", "id": "u9", "name": "SendMessage",
              "input": { "to": to, "message": "Also check the 404 path." } } ] } })
    }

    #[test]
    fn resuming_a_finished_agent_puts_it_back_to_running() {
        // Observed on real data: a completed agent was resumed twice via SendMessage and the
        // roster reported "finished 54 minutes ago" for the whole 9-minute re-run, because
        // nothing ever moved an entry back out of a terminal status.
        let lines = vec![
            launch("u1", "Review", "code-reviewer", "2026-09-08T10:00:00Z"),
            ack("u1", "ggg777", "2026-09-08T10:00:02Z"),
            notify("ggg777", "completed", "First pass done.", "2026-09-08T10:05:00Z"),
            send_message("ggg777", "2026-09-08T11:00:00Z"),
        ];
        let agents = subagents_from_lines(&lines);
        assert!(agents[0].status == SubagentStatus::Running, "a resumed agent is working again");
        assert!(agents[0].ended_at.is_none(), "it has not ended — the old end time is a lie");
        // Elapsed must count THIS run, not the original launch an hour ago: the strip's job is
        // "is something happening, and for how long".
        let resumed_at = crate::mailink::transcript::rfc3339_to_ms("2026-09-08T11:00:00Z") as u64;
        assert_eq!(agents[0].started_at, resumed_at);
    }

    #[test]
    fn a_send_message_to_a_still_running_agent_does_not_rewind_its_clock() {
        // The ack advertises SendMessage as the way to "continue this agent" without restricting
        // it to finished ones, so this is ordinary traffic — and resetting `started_at` here
        // would send the phone's elapsed timer back to zero for a run that never stopped.
        let lines = vec![
            launch("u1", "Review", "code-reviewer", "2026-09-08T10:00:00Z"),
            ack("u1", "jjj111", "2026-09-08T10:00:02Z"),
            send_message("jjj111", "2026-09-08T10:04:00Z"),
        ];
        let agents = subagents_from_lines(&lines);
        assert!(agents[0].status == SubagentStatus::Running);
        let launched = crate::mailink::transcript::rfc3339_to_ms("2026-09-08T10:00:00Z") as u64;
        assert_eq!(agents[0].started_at, launched, "still the same run");
    }

    const HOUR: u64 = 60 * 60 * 1000;

    #[test]
    fn a_resumed_run_is_not_judged_by_the_previous_runs_sidecar() {
        // Demonstrated by review: the sidecar's last write belongs to the run that already
        // finished, so judging liveness by it settled the NEW run on the same pass — stamping an
        // `endedAt` five hours BEFORE its `startedAt`, and then latching there, because a settled
        // entry is skipped on every later tick. The resume fix's own defect, on the path it added.
        assert_eq!(
            sidecar_verdict(12 * HOUR, Some(7 * HOUR), 12 * HOUR + 1000),
            SidecarVerdict::NoEvidence,
        );
    }

    #[test]
    fn a_resume_whose_subagent_never_writes_still_settles_eventually() {
        // The trap in the fix above: treating "sidecar predates this run" as its own no-evidence
        // case never settles, so a resume whose parent dies before the subagent writes anything
        // would strand `running` forever — the same permanent-running bug as the six-month
        // entries, reached by a different road. A stale file is treated as ABSENT, so the
        // elapsed-since-launch rule still applies to it.
        assert_eq!(
            sidecar_verdict(12 * HOUR, Some(7 * HOUR), 12 * HOUR + NO_SIDECAR_STALE_MS + 1),
            SidecarVerdict::Ended(None),
            "no end time is invented — the old mtime is not this run's",
        );
    }

    #[test]
    fn a_sidecar_silent_since_this_run_began_still_settles() {
        // The guard above must not disarm the settle in the case it exists for — a parent killed
        // mid-delegation, where the sidecar IS this run's and simply stopped.
        assert_eq!(
            sidecar_verdict(HOUR, Some(2 * HOUR), 24 * HOUR),
            SidecarVerdict::Ended(Some(2 * HOUR)),
            "the mtime is a real upper bound on when it stopped",
        );
        // …and a sidecar written since, within the window, is just a working agent.
        assert_eq!(sidecar_verdict(HOUR, Some(2 * HOUR), 2 * HOUR + 1000), SidecarVerdict::Live);
    }

    #[test]
    fn an_unreadable_launch_time_is_not_read_as_ancient() {
        // `line_ts` yields 0 for a timestamp it could not parse — "I don't know when", which the
        // no-sidecar settle would otherwise read as "launched at the epoch, therefore ancient".
        // A missing launch time is LESS evidence than a missing sidecar, not more.
        assert_eq!(sidecar_verdict(0, None, 99 * NO_SIDECAR_STALE_MS), SidecarVerdict::NoEvidence);
        // A real launch time with no sidecar still settles, with no end time invented for it.
        assert_eq!(
            sidecar_verdict(HOUR, None, HOUR + NO_SIDECAR_STALE_MS + 1),
            SidecarVerdict::Ended(None),
        );
        assert_eq!(sidecar_verdict(HOUR, None, HOUR + 1000), SidecarVerdict::NoEvidence);
    }

    #[test]
    fn a_send_message_to_something_that_is_not_a_subagent_changes_nothing() {
        // SendMessage also addresses bridged peers and other sessions by name; only ids that are
        // actually in the roster may re-open an entry.
        let lines = vec![
            launch("u1", "Review", "code-reviewer", "2026-09-08T10:00:00Z"),
            ack("u1", "hhh888", "2026-09-08T10:00:02Z"),
            notify("hhh888", "completed", "Done.", "2026-09-08T10:05:00Z"),
            send_message("maiLink App", "2026-09-08T11:00:00Z"),
        ];
        let agents = subagents_from_lines(&lines);
        assert!(agents[0].status == SubagentStatus::Done);
    }

    #[test]
    fn a_replayed_launch_does_not_duplicate_or_resurrect_the_entry() {
        // A resumed session replays earlier lines. Real transcript on disk: four agentIds each
        // acked twice under a duplicated tool_use id. Two rows with one id break a keyed list,
        // and a replay landing after the completion would report a finished agent as running.
        let lines = vec![
            launch("u1", "Review", "code-reviewer", "2026-09-08T10:00:00Z"),
            ack("u1", "iii999", "2026-09-08T10:00:02Z"),
            notify("iii999", "completed", "All clear.", "2026-09-08T10:05:00Z"),
            launch("u1", "Review", "code-reviewer", "2026-09-08T10:00:00Z"),
            ack("u1", "iii999", "2026-09-08T10:00:02Z"),
        ];
        let agents = subagents_from_lines(&lines);
        assert_eq!(agents.len(), 1, "the replay is the same delegation, not a second one");
        assert!(agents[0].status == SubagentStatus::Done, "and it is still finished");
    }

    #[test]
    fn a_notification_for_an_unknown_agent_is_ignored() {
        // The tail window can start after a launch; its completion must not mint a phantom row.
        let lines = vec![notify("zzz999", "completed", "done", "2026-09-08T10:00:00Z")];
        assert!(subagents_from_lines(&lines).is_empty());
    }

    #[test]
    fn json_pairs_last_line_with_its_timestamp() {
        let a = Subagent {
            id: "a1".into(),
            description: "Review".into(),
            agent_type: None,
            status: SubagentStatus::Running,
            started_at: 1,
            ended_at: None,
            last_line: Some("checking the busy map".into()),
            last_line_ts: None,
        };
        assert!(
            a.to_json().get("lastLine").is_none(),
            "an undated progress line must not ship — the phone would read it as current"
        );
    }
}


