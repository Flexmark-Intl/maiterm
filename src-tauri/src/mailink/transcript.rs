//! Per-turn source-markdown distillation for the maiLink chat surface (docs/mailink-protocol.md).
//!
//! The old transcript was `recent_text()` — a flattened scrape of the TUI frame (box-drawing,
//! the `❯` prompt, footer; the agent's markdown already rendered-then-ANSI-stripped). That reads
//! as terminal chrome in the phone's GFM renderer. Instead we read each turn's **source markdown**
//! straight from Claude's session transcript JSONL (the agent's actual output, pre-TUI-render).
//!
//! Located by the unique session id (`~/.claude/projects/*/<session_id>.jsonl`) — no fragile
//! cwd-munging, no hook changes. Each maiLink message is `{msg_id, role, text, ts}` where role is
//! the frozen contract set `agent | user | system | tool`:
//!   - assistant `text` blocks  → role "agent" (the markdown that lights up code fences/lists)
//!   - assistant `tool_use`     → role "tool", a slim one-line marker (e.g. `Bash(rm …)`)
//!   - user string content      → role "user" (the human's actual message)
//! `thinking` blocks and `tool_result` payloads are skipped (private / noisy). Claude-only for now;
//! callers fall back to `recent_text()` for other runtimes or when no transcript is found.

use serde_json::{json, Value};
use std::path::PathBuf;

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
pub fn turns_for_session(session_id: &str, limit: usize, tools: ToolRender) -> Option<Vec<Value>> {
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
    /// Raw model id from the last assistant turn (e.g. "claude-opus-4-8"). Caller normalizes.
    /// NOTE: the transcript records only the BARE id — it never carries the 1M-context variant
    /// marker (no "[1m]", no betas field), so the 1M window can't be detected from the id. maiLink
    /// works around this by assuming 1M for Opus 4.8 (see context_limit_for in mod.rs).
    pub model_id: Option<String>,
    /// input + cache_read + cache_creation tokens — matches the maiTerm statusline's context count.
    pub context_tokens: u64,
}

/// Read the most recent `message.usage` line from a session's transcript and return its model id +
/// summed context tokens. None if the transcript can't be found/read or has no usage yet.
pub fn session_meta(session_id: &str) -> Option<SessionMeta> {
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
        return Some(SessionMeta { model_id, context_tokens: tokens });
    }
    None
}

/// Millis-since-epoch mtime of a session's transcript JSONL, if locatable. A cheap change-gate for
/// WS streaming: an unchanged mtime means no new turns, so the tail isn't re-parsed that tick.
pub fn session_jsonl_mtime(session_id: &str) -> Option<u64> {
    let path = locate_jsonl(session_id)?;
    let modified = std::fs::metadata(&path).ok()?.modified().ok()?;
    modified
        .duration_since(std::time::UNIX_EPOCH)
        .ok()
        .map(|d| d.as_millis() as u64)
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
            // Only a plain string is real human input. List content is tool_result (skip) or the
            // occasional injected "[Request interrupted…]" text (system noise, skip).
            if let Some(text) = content.and_then(|c| c.as_str()) {
                if !text.trim().is_empty() && !is_system_noise(text) {
                    out.push(msg(uuid.to_string(), "user", text, ts));
                }
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
    let input = block.get("input");
    let arg = input.and_then(|inp| {
        for key in ["command", "file_path", "path", "pattern", "query", "url"] {
            if let Some(s) = inp.get(key).and_then(|v| v.as_str()) {
                return Some(s.to_string());
            }
        }
        None
    });
    match arg {
        Some(a) => {
            // Newlines collapsed so the chip stays one line. The UI truncates for display, so we
            // only cap as payload hygiene (a heredoc command can be huge) — normal args pass whole.
            let a = a.replace('\n', " ");
            let a = if a.chars().count() > 160 {
                format!("{} …", a.chars().take(160).collect::<String>())
            } else {
                a
            };
            format!("{name}({a})")
        }
        None => name.to_string(),
    }
}

/// Drop user-string content that is injected system scaffolding, not a human message.
fn is_system_noise(text: &str) -> bool {
    let t = text.trim_start();
    t.starts_with('<')                       // <local-command-…>, <command-name>, <system-reminder>
        || t.starts_with("[Request interrupted")
        || t.starts_with("Caveat:")
}

/// Parse an RFC3339 / ISO-8601 UTC timestamp (`YYYY-MM-DDTHH:MM:SS.sssZ`) to unix ms. Returns 0
/// on any parse miss (the maiLink list orders by array position, so `ts` is display-only). No
/// chrono dependency — the format is fixed, so a tiny civil-days computation suffices.
fn rfc3339_to_ms(s: &str) -> i64 {
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
    fn user_string_kept_but_tool_result_and_noise_skipped() {
        let real = json!({ "type": "user", "uuid": "u2", "timestamp": "2026-06-27T21:25:58Z",
            "message": { "role": "user", "content": "Please fix the bug." } });
        let toolres = json!({ "type": "user", "uuid": "u3", "timestamp": "2026-06-27T21:25:59Z",
            "message": { "role": "user", "content": [ { "type": "tool_result", "content": "output" } ] } });
        let noise = json!({ "type": "user", "uuid": "u4", "timestamp": "2026-06-27T21:26:00Z",
            "message": { "role": "user", "content": "<system-reminder>hi</system-reminder>" } });
        let mut out = Vec::new();
        push_line_messages(&real, ToolRender::Marker, &mut out);
        push_line_messages(&toolres, ToolRender::Marker, &mut out);
        push_line_messages(&noise, ToolRender::Marker, &mut out);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0]["role"], "user");
        assert_eq!(out[0]["text"], "Please fix the bug.");
    }
}
