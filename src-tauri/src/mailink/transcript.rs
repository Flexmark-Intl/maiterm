//! Per-turn source-markdown distillation for the maiLink chat surface (docs/mailink-protocol.md).
//!
//! The old transcript was `recent_text()` — a flattened scrape of the TUI frame (box-drawing,
//! the `❯` prompt, footer; the agent's markdown already rendered-then-ANSI-stripped). That reads
//! as terminal chrome in the phone's GFM renderer. Instead we read each turn's **source markdown**
//! straight from the agent's session transcript (the actual output, pre-TUI-render).
//!
//! Two sources, dispatched by runtime (`turns_for` / `meta_for` / `mtime_for` /
//! `last_turn_ts_for`):
//!   * **Claude** — `~/.claude/projects/*/<session_id>.jsonl`, located by the unique session id.
//!   * **Codex** — `~/.codex/sessions/YYYY/MM/DD/rollout-<ts>-<session_id>.jsonl` (codex-rs
//!     appends to the same rollout on resume, so the path is stable per session id).
//!
//! Each maiLink message is `{msg_id, role, text, ts}` where role is the frozen contract set
//! `agent | user | system | tool`:
//!   - assistant text            → role "agent" (the markdown that lights up code fences/lists)
//!   - tool calls                → role "tool", a slim one-line marker (e.g. `Bash(rm …)`)
//!   - genuine human messages    → role "user"
//! Thinking/reasoning blocks and tool outputs are skipped (private / noisy). Gemini has no
//! transcript source yet; callers fall back to `recent_text()` when nothing resolves.

use serde_json::{json, Value};
use std::collections::HashMap;
use std::path::PathBuf;

use crate::state::AgentRuntime;

// ─── runtime dispatchers ────────────────────────────────────────────────────────────────

/// The last `limit` maiLink messages for a session, per the tab's runtime. `None` when the
/// runtime has no transcript source (Gemini) or the file can't be located/read.
pub fn turns_for(rt: AgentRuntime, session_id: &str, limit: usize, tools: ToolRender) -> Option<Vec<Value>> {
    match rt {
        AgentRuntime::Claude => turns_for_session(session_id, limit, tools),
        AgentRuntime::Codex => codex_turns_for_session(session_id, limit, tools),
        AgentRuntime::Gemini => None,
    }
}

/// Per-agent telemetry (model + context gauge) for a session, per runtime.
pub fn meta_for(rt: AgentRuntime, session_id: &str) -> Option<SessionMeta> {
    match rt {
        AgentRuntime::Claude => session_meta(session_id),
        AgentRuntime::Codex => codex_session_meta(session_id),
        AgentRuntime::Gemini => None,
    }
}

/// Millis-since-epoch mtime of the session's transcript file, if locatable. A cheap change-gate
/// for WS streaming: an unchanged mtime means no new turns, so the tail isn't re-parsed.
pub fn mtime_for(rt: AgentRuntime, session_id: &str) -> Option<u64> {
    let path = match rt {
        AgentRuntime::Claude => locate_jsonl(session_id)?,
        AgentRuntime::Codex => locate_codex_jsonl(session_id)?,
        AgentRuntime::Gemini => return None,
    };
    let modified = std::fs::metadata(&path).ok()?.modified().ok()?;
    modified
        .duration_since(std::time::UNIX_EPOCH)
        .ok()
        .map(|d| d.as_millis() as u64)
}

/// Unix-ms timestamp of the session's last REAL turn, per runtime — see
/// `session_last_turn_ts` for why this is distinct from the file mtime.
pub fn last_turn_ts_for(rt: AgentRuntime, session_id: &str) -> Option<u64> {
    match rt {
        AgentRuntime::Claude => session_last_turn_ts(session_id),
        AgentRuntime::Codex => codex_session_last_turn_ts(session_id),
        AgentRuntime::Gemini => None,
    }
}

/// How tool calls render in the transcript. (b) is the default — assistant prose plus a slim
/// one-line tool marker; raw tool_result dumps are always skipped.
#[derive(Clone, Copy, PartialEq)]
pub enum ToolRender {
    /// (a) assistant + user text only — no tool turns at all.
    None,
    /// (b) a compact one-line `role:"tool"` marker per tool call (default).
    Marker,
}

/// Build the last `limit` maiLink messages for a Claude session, or `None` if its transcript
/// can't be found/read (caller falls back to the terminal scrape).
fn turns_for_session(session_id: &str, limit: usize, tools: ToolRender) -> Option<Vec<Value>> {
    let path = locate_jsonl(session_id)?;
    let body = std::fs::read_to_string(&path).ok()?;
    // Only the tail can hold the last `limit` turns; bound the work regardless of file size.
    // A turn is a handful of lines, so ~12× headroom is plenty.
    let lines: Vec<&str> = body.lines().collect();
    let start = lines.len().saturating_sub(limit * 12 + 64);
    let mut msgs: Vec<Value> = Vec::new();
    for line in &lines[start..] {
        if let Ok(v) = serde_json::from_str::<Value>(line) {
            push_line_messages(&v, tools, &mut msgs);
        }
    }
    if msgs.len() > limit {
        msgs = msgs.split_off(msgs.len() - limit);
    }
    Some(msgs)
}

/// Live per-agent telemetry read from the tail of a Claude session's transcript JSONL: the model id
/// and current context size (prompt tokens). Drives the maiLink per-agent `meta` strip. Sourced
/// from the JSONL (not the SessionStart hook, whose `model` is often null) so it's always available
/// for a Claude tab; naturally Claude-only since it reads `~/.claude` transcripts.
pub struct SessionMeta {
    /// Raw model id from the last assistant turn (e.g. "claude-opus-4-8", "gpt-5.5"). Caller
    /// normalizes for display.
    /// NOTE (Claude): the transcript records only the BARE id — it never carries the 1M-context
    /// variant marker (no "[1m]", no betas field), so the 1M window can't be detected from the id.
    /// maiLink works around this by assuming 1M for Opus 4.8 (see context_limit_for in mod.rs).
    pub model_id: Option<String>,
    /// Tokens currently in the context window. Claude: input + cache_read + cache_creation
    /// (matches the maiTerm statusline). Codex: the last token_count's
    /// `last_token_usage.total_tokens` — the LAST request's size IS the current context; the
    /// `total_token_usage` sibling is a running sum across turns (it exceeds the window on any
    /// long session) and is what codex-rs's own gauge divides by the window only as a
    /// no-window fallback display.
    pub context_tokens: u64,
    /// The model's context window when the transcript states it directly (Codex rollouts carry
    /// `model_context_window`). `None` for Claude — the caller derives it from the model id.
    pub context_window: Option<u64>,
}

/// Read the most recent `message.usage` line from a Claude session's transcript and return its
/// model id + summed context tokens. None if the transcript can't be found/read or has no usage.
fn session_meta(session_id: &str) -> Option<SessionMeta> {
    let path = locate_jsonl(session_id)?;
    // The last usage block is near EOF (the latest assistant turn); scan a bounded tail so this
    // stays cheap when polled per chat_state. A truncated first line just fails to parse → skipped.
    let tail = read_tail(&path, 256 * 1024)?;
    for line in tail.lines().rev() {
        if !line.contains("\"usage\"") {
            continue;
        }
        let Ok(v) = serde_json::from_str::<Value>(line) else { continue };
        let Some(msg) = v.get("message") else { continue };
        let Some(usage) = msg.get("usage") else { continue };
        let tokens = ["input_tokens", "cache_read_input_tokens", "cache_creation_input_tokens"]
            .iter()
            .map(|k| usage.get(k).and_then(|x| x.as_u64()).unwrap_or(0))
            .sum::<u64>();
        if tokens == 0 {
            continue;
        }
        let model_id = msg.get("model").and_then(|m| m.as_str()).map(String::from);
        return Some(SessionMeta { model_id, context_tokens: tokens, context_window: None });
    }
    None
}

/// Claude Code version that wrote the most recent entries of a session's transcript (every JSONL
/// entry carries a `"version"` field). Gates the AskUserQuestion expiry contract: the 60s
/// auto-resolve existed only in specific CC versions (see `ask_deadline_ms` in mod.rs). Read from
/// the newest entry so an in-place CC upgrade mid-session is picked up.
pub fn claude_session_version(session_id: &str) -> Option<String> {
    let path = locate_jsonl(session_id)?;
    let tail = read_tail(&path, 64 * 1024)?;
    for line in tail.lines().rev() {
        let Ok(v) = serde_json::from_str::<Value>(line) else { continue };
        if let Some(ver) = v.get("version").and_then(|x| x.as_str()) {
            return Some(ver.to_string());
        }
    }
    None
}

/// Unix-ms timestamp of the last REAL turn in a Claude session transcript — the last assistant/tool
/// turn or genuine human message — as distinct from the file mtime. A resume/replay appends only
/// scaffolding (SessionStart hook context, `mode`/`last-prompt`/`permission-mode`/`attachment`
/// metadata, `<system-reminder>` blocks, the post-compaction summary); that bumps the JSONL mtime
/// for EVERY restored tab and would clump the whole inbox at "now" on a restart. Sourcing recency
/// from the last real turn keeps a dormant thread at its true age across a restart. `None` if the
/// transcript can't be found/read or no real turn falls within the scanned tail.
fn session_last_turn_ts(session_id: &str) -> Option<u64> {
    let path = locate_jsonl(session_id)?;
    // The last real turn sits just before whatever small scaffolding a resume appends at EOF, so a
    // 256KB tail clears the largest realistic resume dump (hook context + deferred-tool list) easily.
    let tail = read_tail(&path, 256 * 1024)?;
    for line in tail.lines().rev() {
        let Ok(v) = serde_json::from_str::<Value>(line) else { continue };
        if let Some(ts) = real_turn_ts(&v) {
            return Some(ts as u64); // scanning from EOF: the first real turn found IS the latest
        }
    }
    None
}

/// The unix-ms timestamp of `v` iff it is a REAL turn (agent/tool output or a genuine human
/// message), else `None`. Mirrors what `push_line_messages` surfaces so recency tracks exactly the
/// content the phone renders — and, crucially, ignores every entry a resume appends. `ts <= 0`
/// (missing/garbage timestamp) is treated as "no signal" → skip and keep scanning older turns.
fn real_turn_ts(v: &Value) -> Option<i64> {
    let ts = v
        .get("timestamp")
        .and_then(|t| t.as_str())
        .map(rfc3339_to_ms)
        .filter(|&t| t > 0);
    match v.get("type").and_then(|t| t.as_str()) {
        // Any assistant turn (text, tool_use, even thinking-only) exists only from real work — a
        // resume never runs the model, so it never appends one.
        Some("assistant") => ts,
        // A genuine human message is plain-string content that isn't the compaction summary, a
        // tool_result (list content), or injected scaffolding (<system-reminder>, Caveat:, …).
        Some("user") => {
            if v.get("isCompactSummary").and_then(|b| b.as_bool()) == Some(true) {
                return None;
            }
            let text = v.get("message").and_then(|m| m.get("content")).and_then(|c| c.as_str())?;
            if text.trim().is_empty() || is_system_noise(text) {
                return None;
            }
            ts
        }
        // mode / last-prompt / permission-mode / attachment / compact_boundary / other system.
        _ => None,
    }
}

/// Read at most the last `max` bytes of a file as lossy UTF-8 (for tail scans).
fn read_tail(path: &std::path::Path, max: u64) -> Option<String> {
    use std::io::{Read, Seek, SeekFrom};
    let mut f = std::fs::File::open(path).ok()?;
    let len = f.metadata().ok()?.len();
    f.seek(SeekFrom::Start(len.saturating_sub(max))).ok()?;
    let mut buf = Vec::new();
    f.read_to_end(&mut buf).ok()?;
    Some(String::from_utf8_lossy(&buf).into_owned())
}

/// Find `<session_id>.jsonl` under any `~/.claude/projects/*/` dir. The session id is globally
/// unique, so a match is unambiguous.
fn locate_jsonl(session_id: &str) -> Option<PathBuf> {
    let root = dirs::home_dir()?.join(".claude").join("projects");
    let file = format!("{session_id}.jsonl");
    for entry in std::fs::read_dir(&root).ok()? {
        let dir = entry.ok()?.path();
        if dir.is_dir() {
            let candidate = dir.join(&file);
            if candidate.is_file() {
                return Some(candidate);
            }
        }
    }
    None
}

// ─── Codex (rollout JSONL) ──────────────────────────────────────────────────────────────
//
// Codex writes one append-only rollout per session:
// `~/.codex/sessions/YYYY/MM/DD/rollout-<file-ts>-<session_id>.jsonl` (codex-rs resumes append
// to the SAME file, so the path is stable for a session's lifetime). Lines are
// `{timestamp, type, payload}`; the content we distill lives in `response_item` payloads:
//   - `message` role assistant / `output_text` blocks → role "agent"
//   - `message` role user / `input_text` blocks       → role "user" (scaffolding like
//     `<user_instructions>`/`<environment_context>` is `<`-tagged → dropped by is_system_noise)
//   - `function_call` (args = a JSON-encoded string) and `custom_tool_call` → role "tool"
//   - `reasoning`, `*_output`, `web_search_call`, … → skipped
// `event_msg` lines mirror the same content for the TUI (skipped to avoid duplicates), except
// `token_count`, which feeds the context gauge (`total_token_usage.total_tokens` over
// `model_context_window` — exactly what codex-rs's own footer divides). The model id rides
// `turn_context.payload.model`.

/// session_id → located rollout path. The walk is date-dir-shaped (years×months×days), so cache
/// hits skip it; entries re-validate with `is_file()` (safe: resume appends, never moves).
static CODEX_PATHS: std::sync::OnceLock<std::sync::Mutex<HashMap<String, PathBuf>>> =
    std::sync::OnceLock::new();

/// Locate a codex rollout by session-id filename suffix, newest date first (recent sessions
/// resolve after a couple of `read_dir`s).
fn locate_codex_jsonl(session_id: &str) -> Option<PathBuf> {
    let cache = CODEX_PATHS.get_or_init(|| std::sync::Mutex::new(HashMap::new()));
    if let Some(p) = cache.lock().ok()?.get(session_id) {
        if p.is_file() {
            return Some(p.clone());
        }
    }
    let root = dirs::home_dir()?.join(".codex").join("sessions");
    let suffix = format!("-{session_id}.jsonl");
    for year in subdirs_desc(&root) {
        for month in subdirs_desc(&year) {
            for day in subdirs_desc(&month) {
                let Ok(entries) = std::fs::read_dir(&day) else { continue };
                for entry in entries.flatten() {
                    let path = entry.path();
                    let is_match = path
                        .file_name()
                        .and_then(|n| n.to_str())
                        .is_some_and(|n| n.ends_with(&suffix));
                    if is_match && path.is_file() {
                        if let Ok(mut c) = cache.lock() {
                            c.insert(session_id.to_string(), path.clone());
                        }
                        return Some(path);
                    }
                }
            }
        }
    }
    None
}

/// Subdirectories of `path`, name-sorted descending (the date-dir names sort lexically).
fn subdirs_desc(path: &std::path::Path) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(path) else { return Vec::new() };
    let mut dirs: Vec<PathBuf> = entries
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.is_dir())
        .collect();
    dirs.sort_unstable();
    dirs.reverse();
    dirs
}

/// Build the last `limit` maiLink messages for a Codex session, or `None` if its rollout can't
/// be found/read (caller falls back to the terminal scrape). msg_ids are `cx<line>[:<block>]`
/// keyed on the GLOBAL line number — stable across reads because the rollout is append-only, so
/// the streamed frame and any REST re-fetch dedup to one entry (same guarantee as Claude's
/// uuid-based ids).
fn codex_turns_for_session(session_id: &str, limit: usize, tools: ToolRender) -> Option<Vec<Value>> {
    let path = locate_codex_jsonl(session_id)?;
    let body = std::fs::read_to_string(&path).ok()?;
    let lines: Vec<&str> = body.lines().collect();
    // Only the tail can hold the last `limit` turns; bound the parse like the Claude path.
    let start = lines.len().saturating_sub(limit * 12 + 64);
    let mut msgs: Vec<Value> = Vec::new();
    for (i, line) in lines[start..].iter().enumerate() {
        if let Ok(v) = serde_json::from_str::<Value>(line) {
            push_codex_line_messages(start + i, &v, tools, &mut msgs);
        }
    }
    if msgs.len() > limit {
        msgs = msgs.split_off(msgs.len() - limit);
    }
    Some(msgs)
}

/// Turn one rollout line into zero or more maiLink messages, appended to `out`.
fn push_codex_line_messages(line_no: usize, v: &Value, tools: ToolRender, out: &mut Vec<Value>) {
    if v.get("type").and_then(|t| t.as_str()) != Some("response_item") {
        return; // event_msg mirrors response_item content; session_meta/turn_context are meta
    }
    let Some(p) = v.get("payload") else { return };
    let ts = v
        .get("timestamp")
        .and_then(|t| t.as_str())
        .map(rfc3339_to_ms)
        .unwrap_or(0);

    match p.get("type").and_then(|t| t.as_str()) {
        Some("message") => {
            let role = p.get("role").and_then(|r| r.as_str()).unwrap_or("");
            let Some(blocks) = p.get("content").and_then(|c| c.as_array()) else { return };
            for (bi, b) in blocks.iter().enumerate() {
                let (block_ty, out_role) = match role {
                    "assistant" => ("output_text", "agent"),
                    "user" => ("input_text", "user"),
                    _ => continue,
                };
                if b.get("type").and_then(|t| t.as_str()) != Some(block_ty) {
                    continue;
                }
                let text = b.get("text").and_then(|t| t.as_str()).unwrap_or("");
                if text.trim().is_empty() || (out_role == "user" && is_system_noise(text)) {
                    continue;
                }
                out.push(msg(format!("cx{line_no}:{bi}"), out_role, text, ts));
            }
        }
        // Tool calls: `arguments` is a JSON-ENCODED STRING (e.g. `{"cmd":"pwd",…}`).
        Some("function_call") if tools == ToolRender::Marker => {
            let name = p.get("name").and_then(|n| n.as_str()).unwrap_or("tool");
            let arg = p
                .get("arguments")
                .and_then(|a| a.as_str())
                .and_then(|s| serde_json::from_str::<Value>(s).ok())
                .and_then(|input| compact_tool_arg(&input));
            let text = match arg {
                Some(a) => format!("{name}({a})"),
                None => name.to_string(),
            };
            out.push(msg(format!("cx{line_no}"), "tool", &text, ts));
        }
        // e.g. apply_patch — `input` is the raw patch/text payload.
        Some("custom_tool_call") if tools == ToolRender::Marker => {
            let name = p.get("name").and_then(|n| n.as_str()).unwrap_or("tool");
            let text = match p
                .get("input")
                .and_then(|i| i.as_str())
                .map(one_line_capped)
                .filter(|s| !s.trim().is_empty())
            {
                Some(input) => format!("{name}({input})"),
                None => name.to_string(),
            };
            out.push(msg(format!("cx{line_no}"), "tool", &text, ts));
        }
        _ => {} // reasoning, function_call_output, custom_tool_call_output, web_search_call, …
    }
}

/// Live per-agent telemetry from the tail of a codex rollout: last `token_count` (context used +
/// the window, both stated in the file) + last `turn_context` (model id).
fn codex_session_meta(session_id: &str) -> Option<SessionMeta> {
    let path = locate_codex_jsonl(session_id)?;
    let tail = read_tail(&path, 256 * 1024)?;
    codex_meta_from_tail(&tail)
}

fn codex_meta_from_tail(tail: &str) -> Option<SessionMeta> {
    let mut model_id: Option<String> = None;
    let mut tokens: Option<(u64, Option<u64>)> = None; // (used, window)
    for line in tail.lines().rev() {
        if tokens.is_none() && line.contains("\"token_count\"") {
            if let Ok(v) = serde_json::from_str::<Value>(line) {
                if let Some(info) = v
                    .get("payload")
                    .and_then(|p| p.get("info"))
                    .filter(|i| !i.is_null())
                {
                    // last_token_usage = the latest request = the current context size.
                    // (total_token_usage is a cross-turn running sum — NOT a context measure.)
                    let used = ["last_token_usage", "total_token_usage"]
                        .iter()
                        .find_map(|k| {
                            info.get(k)
                                .and_then(|u| u.get("total_tokens"))
                                .and_then(|t| t.as_u64())
                                .filter(|&t| t > 0)
                        })
                        .unwrap_or(0);
                    if used > 0 {
                        let window = info.get("model_context_window").and_then(|w| w.as_u64());
                        tokens = Some((used, window.filter(|&w| w > 0)));
                    }
                }
            }
        } else if model_id.is_none() && line.contains("\"turn_context\"") {
            if let Ok(v) = serde_json::from_str::<Value>(line) {
                if v.get("type").and_then(|t| t.as_str()) == Some("turn_context") {
                    model_id = v
                        .get("payload")
                        .and_then(|p| p.get("model"))
                        .and_then(|m| m.as_str())
                        .map(String::from);
                }
            }
        }
        if tokens.is_some() && model_id.is_some() {
            break;
        }
    }
    let (context_tokens, context_window) = tokens?;
    Some(SessionMeta { model_id, context_tokens, context_window })
}

/// Unix-ms timestamp of the last REAL turn in a codex rollout — mirrors
/// `session_last_turn_ts`'s rationale (recency from content, not file churn).
fn codex_session_last_turn_ts(session_id: &str) -> Option<u64> {
    let path = locate_codex_jsonl(session_id)?;
    let tail = read_tail(&path, 256 * 1024)?;
    for line in tail.lines().rev() {
        let Ok(v) = serde_json::from_str::<Value>(line) else { continue };
        if let Some(ts) = codex_real_turn_ts(&v) {
            return Some(ts as u64);
        }
    }
    None
}

/// The unix-ms timestamp of `v` iff it is a REAL codex turn (assistant text, a tool call, or a
/// genuine — non-scaffolding — human message), else `None`.
fn codex_real_turn_ts(v: &Value) -> Option<i64> {
    if v.get("type").and_then(|t| t.as_str()) != Some("response_item") {
        return None;
    }
    let p = v.get("payload")?;
    let ts = v
        .get("timestamp")
        .and_then(|t| t.as_str())
        .map(rfc3339_to_ms)
        .filter(|&t| t > 0)?;
    match p.get("type").and_then(|t| t.as_str()) {
        Some("function_call") | Some("custom_tool_call") => Some(ts),
        Some("message") => match p.get("role").and_then(|r| r.as_str()) {
            Some("assistant") => Some(ts),
            Some("user") => {
                let has_real_text = p
                    .get("content")
                    .and_then(|c| c.as_array())
                    .is_some_and(|blocks| {
                        blocks.iter().any(|b| {
                            b.get("type").and_then(|t| t.as_str()) == Some("input_text")
                                && b.get("text").and_then(|t| t.as_str()).is_some_and(|t| {
                                    !t.trim().is_empty() && !is_system_noise(t)
                                })
                        })
                    });
                has_real_text.then_some(ts)
            }
            _ => None,
        },
        _ => None,
    }
}

/// Turn one transcript line into zero or more maiLink messages, appended to `out`.
fn push_line_messages(v: &Value, tools: ToolRender, out: &mut Vec<Value>) {
    let ty = v.get("type").and_then(|t| t.as_str()).unwrap_or("");
    let uuid = v.get("uuid").and_then(|u| u.as_str()).unwrap_or("");
    let ts = v
        .get("timestamp")
        .and_then(|t| t.as_str())
        .map(rfc3339_to_ms)
        .unwrap_or(0);
    let content = v.get("message").and_then(|m| m.get("content"));

    match ty {
        "assistant" => {
            let Some(blocks) = content.and_then(|c| c.as_array()) else { return };
            for (i, b) in blocks.iter().enumerate() {
                match b.get("type").and_then(|t| t.as_str()) {
                    Some("text") => {
                        let text = b.get("text").and_then(|t| t.as_str()).unwrap_or("");
                        if !text.trim().is_empty() {
                            out.push(msg(format!("{uuid}:{i}"), "agent", text, ts));
                        }
                    }
                    Some("tool_use") if tools == ToolRender::Marker => {
                        out.push(msg(format!("{uuid}:{i}"), "tool", &tool_label(b), ts));
                    }
                    _ => {} // thinking, tool_use when None, etc.
                }
            }
        }
        "user" => {
            // The post-compaction summary is injected as a `user` entry (isCompactSummary) but is
            // huge internal scaffolding, not something the human typed. The compact_boundary arm
            // below surfaces the event as a divider, so drop the summary blob itself.
            if v.get("isCompactSummary").and_then(|b| b.as_bool()) == Some(true) {
                return;
            }
            match content {
                // A plain string is ordinary human input. Strip any leading injected image-path
                // prefix first: when CC leaves a maiLink screenshot as a literal `<temp-path>
                // <caption>` user turn (no native chip), the bare path must not survive into the
                // persisted transcript or it renders as a duplicate raw-path bubble on re-open.
                // A no-op for ordinary messages.
                Some(Value::String(text)) => {
                    let cleaned = strip_leading_image_refs(text);
                    if !cleaned.trim().is_empty() && !is_system_noise(cleaned) {
                        out.push(msg(uuid.to_string(), "user", cleaned, ts));
                    }
                }
                // List content is normally a tool_result (skip). The exception is a human message
                // that ATTACHED an image (a maiLink screenshot send, or a desktop paste): its
                // blocks are [text?, image, …]. Surface it — caption only (image bytes aren't
                // re-sent; the phone keeps its own copy) — so the phone's optimistic image bubble
                // reconciles against this GET echo. Non-image lists stay skipped as before.
                Some(Value::Array(blocks)) => {
                    let is_tool_result = blocks
                        .iter()
                        .any(|b| b.get("type").and_then(|t| t.as_str()) == Some("tool_result"));
                    let has_image = blocks
                        .iter()
                        .any(|b| b.get("type").and_then(|t| t.as_str()) == Some("image"));
                    if !is_tool_result && has_image {
                        // Empty caption is legitimate (images with no text) — still emit so the
                        // phone can reconcile by role + msg_id.
                        let caption = image_message_caption(blocks);
                        out.push(msg(uuid.to_string(), "user", &caption, ts));
                    }
                }
                _ => {}
            }
        }
        // A compaction boundary → one `system` turn so maiLink can draw a divider showing how much
        // context was summarized away (pre → post tokens). Its fields are top-level on the entry
        // (no nested `message`); metadata may be absent on odd builds, so degrade gracefully.
        "system" if v.get("subtype").and_then(|s| s.as_str()) == Some("compact_boundary") => {
            let cm = v.get("compactMetadata");
            let auto = cm.and_then(|m| m.get("trigger")).and_then(|t| t.as_str()) == Some("auto");
            let head = if auto { "Auto-compacted" } else { "Context compacted" };
            let pre = cm.and_then(|m| m.get("preTokens")).and_then(|t| t.as_u64()).unwrap_or(0);
            let post = cm.and_then(|m| m.get("postTokens")).and_then(|t| t.as_u64()).unwrap_or(0);
            let text = if pre > 0 {
                format!("{head} · {} → {}", fmt_tokens_k(pre), fmt_tokens_k(post))
            } else {
                head.to_string()
            };
            out.push(msg(uuid.to_string(), "system", &text, ts));
        }
        _ => {} // other system entries, attachment, mode, etc.
    }
}

fn msg(msg_id: String, role: &str, text: &str, ts: i64) -> Value {
    json!({ "msg_id": msg_id, "role": role, "text": text, "ts": ts })
}

/// Caption for a human image-attachment turn: its text blocks joined with spaces, any leading
/// image refs Claude Code may prepend stripped, trimmed. The result equals the caption the phone
/// sent (empty when images were attached with no text), so maiLink's optimistic-bubble reconcile
/// (role=="user" && text==caption) matches on this GET echo.
fn image_message_caption(blocks: &[Value]) -> String {
    let joined = blocks
        .iter()
        .filter(|b| b.get("type").and_then(|t| t.as_str()) == Some("text"))
        .filter_map(|b| b.get("text").and_then(|t| t.as_str()))
        .collect::<Vec<_>>()
        .join(" ");
    strip_leading_image_refs(joined.trim()).trim().to_string()
}

/// Strip a leading run of image references (and surrounding whitespace) from `s`, leaving the bare
/// caption byte-equal to what the phone sent. Two forms are consumed, in any leading order:
///   - `[Image #N]` chips — Claude Code's native composer markers when an image is attached.
///   - `…/maiterm-mailink-<uuid>.<ext>` temp paths — what the maiLink image inject types into the
///     PTY when CC does NOT convert the path to a chip. Left in, they survive as a literal
///     `"<path> <caption>"` user turn → a duplicate raw-path bubble on the phone on thread re-open.
/// A whitespace-delimited leading token is treated as one of ours only if it is an absolute path
/// containing the `maiterm-mailink-` sidecar marker, so ordinary user text is never touched.
/// A no-op when the text starts with neither form.
fn strip_leading_image_refs(s: &str) -> &str {
    let mut s = s.trim_start();
    loop {
        if let Some(rest) = s.strip_prefix("[Image #") {
            if let Some(end) = rest.find(']') {
                if end > 0 && rest[..end].bytes().all(|b| b.is_ascii_digit()) {
                    s = rest[end + 1..].trim_start();
                    continue;
                }
            }
        }
        let token = s.split_whitespace().next().unwrap_or("");
        if !token.is_empty() && token.starts_with('/') && token.contains("maiterm-mailink-") {
            s = s[token.len()..].trim_start();
            continue;
        }
        break;
    }
    s
}

/// Format a token count for the compaction divider: 775801 → "776k", 14384 → "14k", 1_250_000 →
/// "1.2M". Mirrors the phone's context-gauge rounding so the numbers read consistently.
fn fmt_tokens_k(n: u64) -> String {
    if n >= 1_000_000 {
        format!("{:.1}M", n as f64 / 1_000_000.0)
    } else if n >= 1_000 {
        format!("{}k", ((n as f64) / 1000.0).round() as u64)
    } else {
        n.to_string()
    }
}

/// A slim one-line label for a tool call, e.g. `Bash(rm -rf …)`, `Edit(src/lib.rs)`.
fn tool_label(block: &Value) -> String {
    let name = block.get("name").and_then(|n| n.as_str()).unwrap_or("tool");
    match block.get("input").and_then(compact_tool_arg) {
        Some(a) => format!("{name}({a})"),
        None => name.to_string(),
    }
}

/// The compact primary argument of a tool call's input object — the one line that tells a human
/// what the call actually does (`rm -rf ./dist`, `src/lib.rs`, …). Checked keys cover Claude's
/// tools plus Codex's (`cmd` for exec_command; a `command` may be an argv ARRAY there). Newlines
/// collapsed so it stays one line; capped as payload hygiene (a heredoc command can be huge) —
/// the UI truncates for display. Shared by the transcript tool chips and the session's
/// `tool_detail` (the maiLink permission card).
pub(crate) fn compact_tool_arg(input: &Value) -> Option<String> {
    let arg = ["command", "cmd", "file_path", "path", "pattern", "query", "url"]
        .iter()
        .find_map(|key| match input.get(key) {
            Some(Value::String(s)) => Some(s.clone()),
            Some(Value::Array(items)) => {
                let parts: Vec<&str> = items.iter().filter_map(|v| v.as_str()).collect();
                (!parts.is_empty()).then(|| parts.join(" "))
            }
            _ => None,
        })
        // AskUserQuestion: the first question's text — so the transcript chip reads
        // `AskUserQuestion(Which migration strategy?)` instead of a bare tool name (with the
        // 60s auto-resolve, that chip is often all that remains of an unanswered ask).
        .or_else(|| {
            input
                .get("questions")
                .and_then(|q| q.as_array())
                .and_then(|a| a.first())
                .and_then(|q| q.get("question"))
                .and_then(|s| s.as_str())
                .map(String::from)
        })
        // Task/Agent subagent spawns carry no command/path — their meaning lives in `description`
        // (Claude's 3-5 word label) or, failing that, the full `prompt`. Without this a fan-out of
        // subagents renders as a run of identical bare `Agent` chips — pure repeated noise. Checked
        // last so a tool with a real primary key (Bash's `command`, etc.) never lands here.
        .or_else(|| {
            ["description", "prompt"]
                .iter()
                .find_map(|key| input.get(key).and_then(|v| v.as_str()).map(String::from))
        })?;
    let a = one_line_capped(&arg);
    (!a.trim().is_empty()).then_some(a)
}

/// Collapse to one line and cap at 160 chars (payload hygiene — the UI truncates for display).
fn one_line_capped(s: &str) -> String {
    let a = s.replace('\n', " ");
    if a.chars().count() > 160 {
        format!("{} …", a.chars().take(160).collect::<String>())
    } else {
        a
    }
}

/// Drop user-string content that is injected scaffolding, not a human message.
fn is_system_noise(text: &str) -> bool {
    let t = text.trim_start();
    t.starts_with('<')                       // <local-command-…>, <command-name>, <system-reminder>
        // maiTerm's agent-to-agent injections: ⟦AGENT-BRIDGE⟧ / ⟦MESH⟧ / ⟦TOPIC COMPLETE⟧
        // envelopes are delivered as real user prompts, so without this they render as giant
        // fake "user" messages that flood every mesh participant's maiLink thread.
        || t.starts_with('⟦')
        || t.starts_with("[Request interrupted")
        || t.starts_with("Caveat:")
}

/// Parse an RFC3339 / ISO-8601 UTC timestamp (`YYYY-MM-DDTHH:MM:SS.sssZ`) to unix ms. Returns 0
/// on any parse miss (the maiLink list orders by array position, so `ts` is display-only). No
/// chrono dependency — the format is fixed, so a tiny civil-days computation suffices.
pub(crate) fn rfc3339_to_ms(s: &str) -> i64 {
    let bytes = s.as_bytes();
    if bytes.len() < 19 {
        return 0;
    }
    let num = |a: usize, b: usize| -> i64 { s[a..b].parse::<i64>().unwrap_or(0) };
    let (y, mo, d) = (num(0, 4), num(5, 7), num(8, 10));
    let (h, mi, se) = (num(11, 13), num(14, 16), num(17, 19));
    // Optional ".sss" fraction after the seconds.
    let millis = if bytes.len() > 19 && bytes[19] == b'.' {
        let frac: String = s[20..].chars().take_while(|c| c.is_ascii_digit()).collect();
        let mut frac = frac;
        frac.truncate(3);
        while frac.len() < 3 {
            frac.push('0');
        }
        frac.parse::<i64>().unwrap_or(0)
    } else {
        0
    };
    let days = days_from_civil(y, mo, d);
    ((days * 24 + h) * 3600 + mi * 60 + se) * 1000 + millis
}

/// Days since 1970-01-01 for a civil (proleptic Gregorian) date — Howard Hinnant's algorithm.
fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = y - if m <= 2 { 1 } else { 0 };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let mp = (m + if m > 2 { -3 } else { 9 }) as i64;
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146097 + doe - 719468
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rfc3339_parses_to_known_epoch_ms() {
        // 2026-06-27T21:25:57.904Z — verified against the unix epoch.
        assert_eq!(rfc3339_to_ms("2026-06-27T21:25:57.904Z"), 1782595557904);
        // Epoch itself.
        assert_eq!(rfc3339_to_ms("1970-01-01T00:00:00.000Z"), 0);
        // No fraction.
        assert_eq!(rfc3339_to_ms("2000-01-01T00:00:00Z"), 946684800000);
        // Garbage → 0, never panics.
        assert_eq!(rfc3339_to_ms("not-a-date"), 0);
    }

    #[test]
    fn parses_assistant_text_and_tool_marker_skips_thinking() {
        let line = json!({
            "type": "assistant",
            "uuid": "u1",
            "timestamp": "2026-06-27T21:25:57.904Z",
            "message": { "role": "assistant", "content": [
                { "type": "thinking", "thinking": "secret" },
                { "type": "text", "text": "Here is **markdown**." },
                { "type": "tool_use", "name": "Bash", "input": { "command": "rm -f /tmp/x" } }
            ]}
        });
        let mut out = Vec::new();
        push_line_messages(&line, ToolRender::Marker, &mut out);
        assert_eq!(out.len(), 2); // thinking skipped
        assert_eq!(out[0]["role"], "agent");
        assert_eq!(out[0]["text"], "Here is **markdown**.");
        assert_eq!(out[0]["ts"], 1782595557904i64);
        assert_eq!(out[1]["role"], "tool");
        assert_eq!(out[1]["text"], "Bash(rm -f /tmp/x)");

        // ToolRender::None drops the tool marker entirely.
        let mut out2 = Vec::new();
        push_line_messages(&line, ToolRender::None, &mut out2);
        assert_eq!(out2.len(), 1);
        assert_eq!(out2[0]["role"], "agent");
    }

    #[test]
    fn compact_boundary_becomes_system_divider_and_summary_is_skipped() {
        // The boundary entry → one `system` turn with a pre→post token delta (top-level fields).
        let boundary = json!({
            "type": "system", "subtype": "compact_boundary", "uuid": "cb1",
            "timestamp": "2026-06-27T21:25:57.904Z", "content": "Conversation compacted",
            "compactMetadata": { "trigger": "manual", "preTokens": 775801, "postTokens": 14384 }
        });
        let mut out = Vec::new();
        push_line_messages(&boundary, ToolRender::Marker, &mut out);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0]["role"], "system");
        assert_eq!(out[0]["msg_id"], "cb1");
        assert_eq!(out[0]["text"], "Context compacted · 776k → 14k");
        assert_eq!(out[0]["ts"], 1782595557904i64);

        // trigger:"auto" swaps the label prefix and M-formats large counts.
        let auto = json!({
            "type": "system", "subtype": "compact_boundary", "uuid": "cb2",
            "timestamp": "2026-06-27T21:25:57Z",
            "compactMetadata": { "trigger": "auto", "preTokens": 1_250_000u64, "postTokens": 20000 }
        });
        let mut out2 = Vec::new();
        push_line_messages(&auto, ToolRender::Marker, &mut out2);
        assert_eq!(out2[0]["text"], "Auto-compacted · 1.2M → 20k");

        // The injected compaction summary (isCompactSummary) is NOT surfaced as a user turn.
        let summary = json!({
            "type": "user", "uuid": "cs1", "timestamp": "2026-06-27T21:25:58Z",
            "isCompactSummary": true, "isVisibleInTranscriptOnly": true,
            "message": { "role": "user", "content": "This session is being continued..." }
        });
        let mut out3 = Vec::new();
        push_line_messages(&summary, ToolRender::Marker, &mut out3);
        assert!(out3.is_empty());

        // A non-compaction system entry is still ignored.
        let other = json!({ "type": "system", "subtype": "other", "uuid": "s9",
            "timestamp": "2026-06-27T21:25:58Z" });
        let mut out4 = Vec::new();
        push_line_messages(&other, ToolRender::Marker, &mut out4);
        assert!(out4.is_empty());
    }

    #[test]
    fn real_turn_ts_counts_activity_but_ignores_resume_scaffolding() {
        let ts = 1782595557904i64; // 2026-06-27T21:25:57.904Z
        let at = "2026-06-27T21:25:57.904Z";

        // Real activity → the entry's timestamp.
        let asst = json!({ "type": "assistant", "timestamp": at,
            "message": { "role": "assistant", "content": [ { "type": "text", "text": "hi" } ] } });
        assert_eq!(real_turn_ts(&asst), Some(ts));
        let tool = json!({ "type": "assistant", "timestamp": at,
            "message": { "role": "assistant", "content": [ { "type": "tool_use", "name": "Bash" } ] } });
        assert_eq!(real_turn_ts(&tool), Some(ts));
        let human = json!({ "type": "user", "timestamp": at,
            "message": { "role": "user", "content": "please fix it" } });
        assert_eq!(real_turn_ts(&human), Some(ts));

        // Resume/replay scaffolding and non-turns → None (must NOT advance recency on restart).
        for scaffold in [
            json!({ "type": "mode", "timestamp": at }),
            json!({ "type": "last-prompt", "timestamp": at }),
            json!({ "type": "permission-mode", "timestamp": at }),
            json!({ "type": "attachment", "timestamp": at }),
            json!({ "type": "system", "subtype": "compact_boundary", "timestamp": at }),
            // tool_result (list content), injected reminder, and the compaction summary blob:
            json!({ "type": "user", "timestamp": at,
                "message": { "role": "user", "content": [ { "type": "tool_result", "content": "x" } ] } }),
            json!({ "type": "user", "timestamp": at,
                "message": { "role": "user", "content": "<system-reminder>init</system-reminder>" } }),
            json!({ "type": "user", "timestamp": at, "isCompactSummary": true,
                "message": { "role": "user", "content": "This session is being continued..." } }),
        ] {
            assert_eq!(real_turn_ts(&scaffold), None, "should ignore: {scaffold}");
        }

        // A real turn with a missing/garbage timestamp yields None so the scan falls back to an
        // older turn (and ultimately scrollback) rather than emitting a 0 age.
        let no_ts = json!({ "type": "assistant",
            "message": { "role": "assistant", "content": [ { "type": "text", "text": "hi" } ] } });
        assert_eq!(real_turn_ts(&no_ts), None);
    }

    #[test]
    fn image_attachment_user_turn_surfaced_with_caption_only() {
        let ts = 1782595557904i64;
        let at = "2026-06-27T21:25:57.904Z";

        // Caption + one image block → one user turn carrying only the caption (no bytes echoed).
        let with_caption = json!({ "type": "user", "uuid": "iu1", "timestamp": at,
            "message": { "role": "user", "content": [
                { "type": "text", "text": "look at this" },
                { "type": "image", "source": { "type": "base64", "media_type": "image/png", "data": "AAAA" } } ] } });
        let mut out = Vec::new();
        push_line_messages(&with_caption, ToolRender::Marker, &mut out);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0]["role"], "user");
        assert_eq!(out[0]["text"], "look at this");
        assert_eq!(out[0]["msg_id"], "iu1");
        assert_eq!(out[0]["ts"], ts);

        // Images with NO caption → still surfaced with empty text so the phone can reconcile.
        let no_caption = json!({ "type": "user", "uuid": "iu2", "timestamp": at,
            "message": { "role": "user", "content": [
                { "type": "image", "source": { "media_type": "image/jpeg" } } ] } });
        let mut out2 = Vec::new();
        push_line_messages(&no_caption, ToolRender::Marker, &mut out2);
        assert_eq!(out2.len(), 1);
        assert_eq!(out2[0]["text"], "");

        // Leading [Image #N] chips Claude Code may inject are stripped → echo == caption.
        let chipped = json!({ "type": "user", "uuid": "iu3", "timestamp": at,
            "message": { "role": "user", "content": [
                { "type": "text", "text": "[Image #1] [Image #2] my caption" },
                { "type": "image", "source": {} } ] } });
        let mut out3 = Vec::new();
        push_line_messages(&chipped, ToolRender::Marker, &mut out3);
        assert_eq!(out3[0]["text"], "my caption");

        // A tool_result that CONTAINS an image is still skipped (not a human turn).
        let tool_img = json!({ "type": "user", "uuid": "iu4", "timestamp": at,
            "message": { "role": "user", "content": [
                { "type": "tool_result", "content": [ { "type": "image", "source": {} } ] } ] } });
        let mut out4 = Vec::new();
        push_line_messages(&tool_img, ToolRender::Marker, &mut out4);
        assert!(out4.is_empty());

        // Literal-path inject: CC did NOT convert the maiLink screenshot to a native chip, so the
        // turn is a plain string `<temp-path> <caption>`. The raw path must be stripped so the
        // persisted echo == caption (else a duplicate raw-path bubble on thread re-open).
        let literal = json!({ "type": "user", "uuid": "iu5", "timestamp": at,
            "message": { "role": "user",
                "content": "/var/folders/xy/T/maiterm-mailink-9f2c.png what is this error" } });
        let mut out5 = Vec::new();
        push_line_messages(&literal, ToolRender::Marker, &mut out5);
        assert_eq!(out5.len(), 1);
        assert_eq!(out5[0]["text"], "what is this error");

        // Multiple images → multiple leading temp paths, all stripped down to the caption.
        let multi = json!({ "type": "user", "uuid": "iu6", "timestamp": at,
            "message": { "role": "user", "content":
                "/tmp/maiterm-mailink-a.png /tmp/maiterm-mailink-b.jpg compare these" } });
        let mut out6 = Vec::new();
        push_line_messages(&multi, ToolRender::Marker, &mut out6);
        assert_eq!(out6[0]["text"], "compare these");

        // An ordinary message that merely mentions a path mid-sentence is untouched.
        let ordinary = json!({ "type": "user", "uuid": "iu7", "timestamp": at,
            "message": { "role": "user", "content": "check /var/log/app.png for the crash" } });
        let mut out7 = Vec::new();
        push_line_messages(&ordinary, ToolRender::Marker, &mut out7);
        assert_eq!(out7[0]["text"], "check /var/log/app.png for the crash");
    }

    #[test]
    fn ask_user_question_chip_carries_the_first_question() {
        let line = json!({
            "type": "assistant", "uuid": "q1", "timestamp": "2026-07-02T19:40:18.530Z",
            "message": { "role": "assistant", "content": [ { "type": "tool_use",
                "name": "AskUserQuestion", "input": { "questions": [
                    { "question": "Which migration strategy?", "header": "Approach",
                      "multiSelect": false, "options": [ { "label": "Big bang" } ] } ] } } ] }
        });
        let mut out = Vec::new();
        push_line_messages(&line, ToolRender::Marker, &mut out);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0]["role"], "tool");
        assert_eq!(out[0]["text"], "AskUserQuestion(Which migration strategy?)");
    }

    #[test]
    fn subagent_spawn_chip_carries_description_not_bare_name() {
        // A Task/Agent spawn has no command/path key; its label is the `description`. Before the
        // fix a fan-out rendered as identical bare `Agent` chips (repeated noise on the phone).
        let line = json!({
            "type": "assistant", "uuid": "a1", "timestamp": "2026-07-02T19:40:18.530Z",
            "message": { "role": "assistant", "content": [ { "type": "tool_use",
                "name": "Agent", "input": {
                    "description": "Investigate the parser",
                    "subagent_type": "general-purpose",
                    "prompt": "Read the whole parser module and report every panic path." } } ] }
        });
        let mut out = Vec::new();
        push_line_messages(&line, ToolRender::Marker, &mut out);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0]["text"], "Agent(Investigate the parser)");

        // Falls back to the full prompt when no description is present.
        let no_desc = json!({ "name": "Task", "input": { "prompt": "Do the thing." } });
        assert_eq!(tool_label(&no_desc), "Task(Do the thing.)");
    }

    #[test]
    fn codex_message_lines_parse_and_scaffolding_skipped() {
        let at = "2026-06-16T03:45:02.460Z";
        let ts = rfc3339_to_ms(at);

        // Assistant output_text → role "agent" (shape verbatim from a real rollout).
        let agent = json!({ "timestamp": at, "type": "response_item", "payload": {
            "type": "message", "role": "assistant",
            "content": [ { "type": "output_text", "text": "Hi. What would you like to work on?" } ],
            "phase": "final_answer" } });
        // Genuine user input_text → role "user".
        let user = json!({ "timestamp": at, "type": "response_item", "payload": {
            "type": "message", "role": "user",
            "content": [ { "type": "input_text", "text": "hi" } ] } });
        // Codex scaffolding rides <tagged> user messages → skipped.
        let scaffold = json!({ "timestamp": at, "type": "response_item", "payload": {
            "type": "message", "role": "user",
            "content": [ { "type": "input_text", "text": "<user_instructions>…</user_instructions>" } ] } });
        // Reasoning and event_msg mirrors are skipped (event_msg would duplicate response_item).
        let reasoning = json!({ "timestamp": at, "type": "response_item",
            "payload": { "type": "reasoning", "summary": [] } });
        let event_dup = json!({ "timestamp": at, "type": "event_msg", "payload": {
            "type": "agent_message", "message": "Hi. What would you like to work on?" } });

        let mut out = Vec::new();
        for (i, line) in [agent, user, scaffold, reasoning, event_dup].iter().enumerate() {
            push_codex_line_messages(i, line, ToolRender::Marker, &mut out);
        }
        assert_eq!(out.len(), 2);
        assert_eq!(out[0]["role"], "agent");
        assert_eq!(out[0]["text"], "Hi. What would you like to work on?");
        assert_eq!(out[0]["msg_id"], "cx0:0");
        assert_eq!(out[0]["ts"], ts);
        assert_eq!(out[1]["role"], "user");
        assert_eq!(out[1]["text"], "hi");
        assert_eq!(out[1]["msg_id"], "cx1:0");
    }

    #[test]
    fn codex_tool_calls_become_markers() {
        let at = "2026-06-16T03:45:02.460Z";
        // function_call: `arguments` is a JSON-ENCODED STRING; `cmd` is codex's exec key.
        let exec = json!({ "timestamp": at, "type": "response_item", "payload": {
            "type": "function_call", "name": "exec_command",
            "arguments": "{\"cmd\":\"pwd\",\"workdir\":\"/tmp\"}", "call_id": "c1" } });
        // Older shell tool shape: `command` as an argv ARRAY.
        let argv = json!({ "timestamp": at, "type": "response_item", "payload": {
            "type": "function_call", "name": "shell",
            "arguments": "{\"command\":[\"bash\",\"-lc\",\"ls -la\"]}", "call_id": "c2" } });
        // custom_tool_call: `input` is the raw payload (capped to one line).
        let patch = json!({ "timestamp": at, "type": "response_item", "payload": {
            "type": "custom_tool_call", "status": "completed", "call_id": "c3",
            "name": "apply_patch", "input": "*** Begin Patch\n*** Add File: a.txt\n+hello" } });
        // Tool OUTPUT lines are never surfaced.
        let output = json!({ "timestamp": at, "type": "response_item", "payload": {
            "type": "function_call_output", "call_id": "c1", "output": "…" } });

        let mut out = Vec::new();
        for (i, line) in [exec, argv, patch, output].iter().enumerate() {
            push_codex_line_messages(i, line, ToolRender::Marker, &mut out);
        }
        assert_eq!(out.len(), 3);
        assert_eq!(out[0]["role"], "tool");
        assert_eq!(out[0]["text"], "exec_command(pwd)");
        assert_eq!(out[1]["text"], "shell(bash -lc ls -la)");
        assert_eq!(out[2]["text"], "apply_patch(*** Begin Patch *** Add File: a.txt +hello)");

        // ToolRender::None drops tool markers but keeps messages.
        let mut none = Vec::new();
        push_codex_line_messages(
            0,
            &json!({ "timestamp": at, "type": "response_item", "payload": {
                "type": "function_call", "name": "exec_command", "arguments": "{}" } }),
            ToolRender::None,
            &mut none,
        );
        assert!(none.is_empty());
    }

    #[test]
    fn codex_meta_parses_token_count_and_turn_context() {
        // Shapes verbatim from a real rollout (trimmed).
        let tail = concat!(
            r#"{"timestamp":"2026-06-16T03:44:50.000Z","type":"turn_context","payload":{"cwd":"/tmp","approval_policy":"on-request","model":"gpt-5.5","summary":"auto"}}"#, "\n",
            r#"{"timestamp":"2026-06-16T03:45:02.517Z","type":"event_msg","payload":{"type":"token_count","info":{"total_token_usage":{"input_tokens":16257,"cached_input_tokens":2432,"output_tokens":14,"reasoning_output_tokens":0,"total_tokens":16271},"last_token_usage":{"input_tokens":16257,"cached_input_tokens":2432,"output_tokens":14,"reasoning_output_tokens":0,"total_tokens":16271},"model_context_window":258400},"rate_limits":{}}}"#, "\n",
        );
        let meta = codex_meta_from_tail(tail).expect("meta parses");
        assert_eq!(meta.model_id.as_deref(), Some("gpt-5.5"));
        assert_eq!(meta.context_tokens, 16271);
        assert_eq!(meta.context_window, Some(258400));

        // Multi-turn: last_token_usage (current context) wins over the cross-turn running sum,
        // which exceeds the window on any long session.
        let multi = r#"{"timestamp":"t","type":"event_msg","payload":{"type":"token_count","info":{"total_token_usage":{"total_tokens":494942},"last_token_usage":{"total_tokens":83000},"model_context_window":258400}}}"#;
        let meta_multi = codex_meta_from_tail(multi).expect("parses");
        assert_eq!(meta_multi.context_tokens, 83000);

        // A null info (some builds emit rate-limit-only token_counts) is skipped, older one wins.
        let tail2 = concat!(
            r#"{"timestamp":"t","type":"event_msg","payload":{"type":"token_count","info":{"total_token_usage":{"total_tokens":100},"model_context_window":1000}}}"#, "\n",
            r#"{"timestamp":"t","type":"event_msg","payload":{"type":"token_count","info":null,"rate_limits":{}}}"#, "\n",
        );
        let meta2 = codex_meta_from_tail(tail2).expect("falls back past null info");
        assert_eq!(meta2.context_tokens, 100);
        assert_eq!(meta2.context_window, Some(1000));
        assert_eq!(meta2.model_id, None);

        // No usable token_count at all → None (no gauge is better than a wrong gauge).
        assert!(codex_meta_from_tail(r#"{"type":"turn_context","payload":{"model":"gpt-5.5"}}"#).is_none());
    }

    #[test]
    fn codex_real_turn_ts_counts_content_not_scaffolding() {
        let at = "2026-06-16T03:45:02.460Z";
        let ts = rfc3339_to_ms(at);
        let real = [
            json!({ "timestamp": at, "type": "response_item", "payload": { "type": "message",
                "role": "assistant", "content": [ { "type": "output_text", "text": "done" } ] } }),
            json!({ "timestamp": at, "type": "response_item", "payload": {
                "type": "function_call", "name": "exec_command", "arguments": "{}" } }),
            json!({ "timestamp": at, "type": "response_item", "payload": { "type": "message",
                "role": "user", "content": [ { "type": "input_text", "text": "do it" } ] } }),
        ];
        for v in &real {
            assert_eq!(codex_real_turn_ts(v), Some(ts), "should count: {v}");
        }
        let not_real = [
            json!({ "timestamp": at, "type": "response_item", "payload": { "type": "message",
                "role": "user", "content": [ { "type": "input_text", "text": "<environment_context>…" } ] } }),
            json!({ "timestamp": at, "type": "response_item", "payload": { "type": "reasoning" } }),
            json!({ "timestamp": at, "type": "event_msg", "payload": { "type": "agent_message", "message": "x" } }),
            json!({ "timestamp": at, "type": "turn_context", "payload": {} }),
            json!({ "type": "response_item", "payload": { "type": "message", "role": "assistant",
                "content": [ { "type": "output_text", "text": "no ts" } ] } }),
        ];
        for v in &not_real {
            assert_eq!(codex_real_turn_ts(v), None, "should ignore: {v}");
        }
    }

    /// Machine-conditional smoke: when a real ~/.codex/sessions exists, the newest sizeable
    /// rollout must parse into at least one turn and a usable meta. Skips silently elsewhere
    /// (same pattern as the openssl DER cross-check above).
    #[test]
    fn codex_real_rollout_smoke() {
        let Some(root) = dirs::home_dir().map(|h| h.join(".codex").join("sessions")) else {
            return;
        };
        if !root.is_dir() {
            eprintln!("[codex smoke] no ~/.codex/sessions — skipped");
            return;
        }
        // Newest-first walk; pick the first file big enough to hold real turns.
        let mut picked: Option<(String, PathBuf)> = None;
        'walk: for y in subdirs_desc(&root) {
            for m in subdirs_desc(&y) {
                for d in subdirs_desc(&m) {
                    let Ok(entries) = std::fs::read_dir(&d) else { continue };
                    for e in entries.flatten() {
                        let p = e.path();
                        let name = p.file_name().and_then(|n| n.to_str()).unwrap_or("");
                        let big = std::fs::metadata(&p).map(|md| md.len() > 20_000).unwrap_or(false);
                        if name.starts_with("rollout-") && name.ends_with(".jsonl") && big {
                            // session id = the last 36 chars before ".jsonl" (uuid).
                            let stem = name.trim_end_matches(".jsonl");
                            if stem.len() > 36 {
                                picked = Some((stem[stem.len() - 36..].to_string(), p));
                                break 'walk;
                            }
                        }
                    }
                }
            }
        }
        let Some((sid, path)) = picked else {
            eprintln!("[codex smoke] no sizeable rollout — skipped");
            return;
        };
        let located = locate_codex_jsonl(&sid).expect("locates by session id");
        assert_eq!(located, path, "locator must resolve the same file");
        let turns = codex_turns_for_session(&sid, 40, ToolRender::Marker).expect("parses");
        assert!(!turns.is_empty(), "a >20KB rollout distills to at least one turn");
        let meta = codex_session_meta(&sid).expect("meta parses");
        assert!(meta.context_tokens > 0);
        assert!(codex_session_last_turn_ts(&sid).is_some());
        eprintln!(
            "[codex smoke] {} → {} turns, model={:?}, ctx={}/{:?}",
            path.display(),
            turns.len(),
            meta.model_id,
            meta.context_tokens,
            meta.context_window
        );
    }

    #[test]
    fn user_string_kept_but_tool_result_and_noise_skipped() {
        let real = json!({ "type": "user", "uuid": "u2", "timestamp": "2026-06-27T21:25:58Z",
            "message": { "role": "user", "content": "Please fix the bug." } });
        let toolres = json!({ "type": "user", "uuid": "u3", "timestamp": "2026-06-27T21:25:59Z",
            "message": { "role": "user", "content": [ { "type": "tool_result", "content": "output" } ] } });
        let noise = json!({ "type": "user", "uuid": "u4", "timestamp": "2026-06-27T21:26:00Z",
            "message": { "role": "user", "content": "<system-reminder>hi</system-reminder>" } });
        // Agent-to-agent injections (bridge/mesh envelopes) are not human messages.
        let bridge = json!({ "type": "user", "uuid": "u5", "timestamp": "2026-06-27T21:26:01Z",
            "message": { "role": "user",
                "content": "⟦AGENT-BRIDGE⟧ Message from \"peer\" — a peer AI agent…" } });
        let mesh = json!({ "type": "user", "uuid": "u6", "timestamp": "2026-06-27T21:26:02Z",
            "message": { "role": "user",
                "content": "⟦MESH⟧ Message from \"reviewer\" [topic: api] [turn 3]…" } });
        let mut out = Vec::new();
        push_line_messages(&real, ToolRender::Marker, &mut out);
        push_line_messages(&toolres, ToolRender::Marker, &mut out);
        push_line_messages(&noise, ToolRender::Marker, &mut out);
        push_line_messages(&bridge, ToolRender::Marker, &mut out);
        push_line_messages(&mesh, ToolRender::Marker, &mut out);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0]["role"], "user");
        assert_eq!(out[0]["text"], "Please fix the bug.");
    }
}
