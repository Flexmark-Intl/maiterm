//! Claude Code task-board reader for the maiLink chat surface.
//!
//! Claude Code's session task list (TaskCreate/TaskUpdate — the strip above the prompt in the
//! TUI) persists as one small JSON file per task at `~/.claude/tasks/<session_id>/<id>.json`:
//! `{id, subject, description, activeForm, status, blocks, blockedBy, owner?}`. maiTerm's
//! terminal shows whatever the TUI renders, but the phone gets structured chat — so the board
//! is invisible there unless we surface it. This module reads a session's board for
//! `chat_detail.tasks` and provides a cheap change key the WS ticker gates its `tasks` event on
//! (docs/mailink-protocol.md).
//!
//! SSH tabs: the board lives on the remote host; the transcript mirror snapshots it into
//! `<data_dir>/<slug>/remote-tasks/<session_id>.json` (a JSON array) in the same ssh round trip
//! it already makes for the transcript delta (see `mirror.rs`). [`tasks_for_session`] checks the
//! local dir first, then that shadow — same layering as the shadow transcripts.
//!
//! Claude runtime only: Codex/Gemini have no task board. Callers gate on runtime.

use serde_json::Value;
use std::path::PathBuf;

/// The local task-board directory for a session, if it exists.
fn local_dir(session_id: &str) -> Option<PathBuf> {
    let dir = dirs::home_dir()?.join(".claude").join("tasks").join(session_id);
    dir.is_dir().then_some(dir)
}

/// Where the SSH transcript mirror shadows a remote session's board: one JSON-array file per
/// session (see mirror.rs). Path only — existence is the caller's concern.
pub fn shadow_path(session_id: &str) -> Option<PathBuf> {
    Some(
        dirs::data_dir()?
            .join(crate::state::persistence::app_data_slug())
            .join("remote-tasks")
            .join(format!("{session_id}.json")),
    )
}

/// The session's task board, sorted by numeric task id, or `None` when the session has no
/// board / an empty one. Each entry is the task file's JSON object passed through verbatim
/// (`{id, subject, description, activeForm, status, blocks, blockedBy, ...}`) so new fields
/// flow to the phone without a code change here.
pub fn tasks_for_session(session_id: &str) -> Option<Vec<Value>> {
    let tasks = match local_dir(session_id) {
        Some(dir) => read_board_dir(&dir),
        // No local board → a remote session mirrored by an SSH tab may have a shadow.
        None => {
            let path = shadow_path(session_id)?;
            let text = std::fs::read_to_string(path).ok()?;
            serde_json::from_str::<Vec<Value>>(&text).ok()?
        }
    };
    (!tasks.is_empty()).then_some(tasks)
}

/// Parse every `*.json` in a board directory (skipping `.lock`/`.highwatermark` and anything
/// unparseable), sorted by numeric task id.
fn read_board_dir(dir: &std::path::Path) -> Vec<Value> {
    let Ok(entries) = std::fs::read_dir(dir) else { return Vec::new() };
    let mut tasks: Vec<Value> = entries
        .flatten()
        .filter(|e| e.path().extension().and_then(|x| x.to_str()) == Some("json"))
        .filter_map(|e| std::fs::read_to_string(e.path()).ok())
        .filter_map(|s| serde_json::from_str::<Value>(&s).ok())
        .filter(|v| v.is_object())
        .collect();
    tasks.sort_by_key(task_order);
    tasks
}

/// Numeric-first ordering on the task's `id` (ids are stringified counters; lexicographic
/// would put "10" before "2").
fn task_order(t: &Value) -> (u64, String) {
    let id = t.get("id").and_then(|v| v.as_str()).unwrap_or_default();
    (id.parse::<u64>().unwrap_or(u64::MAX), id.to_string())
}

/// Parse a remote board dump — the task files' bytes CONCATENATED in arbitrary order (the
/// mirror's `find -exec cat` emits no separators) — into a sorted board. Glob order varies by
/// host, so sorting here (not remotely) is what makes the shadow deterministic. Stops at the
/// first malformed value: a torn read mid-rewrite is possible and a partial board heals on the
/// next fetch.
pub(crate) fn parse_concatenated(bytes: &[u8]) -> Vec<Value> {
    let mut tasks = Vec::new();
    for v in serde_json::Deserializer::from_slice(bytes).into_iter::<Value>() {
        match v {
            Ok(v) if v.is_object() => tasks.push(v),
            _ => break,
        }
    }
    tasks.sort_by_key(task_order);
    tasks
}

/// A cheap change key for the session's board: folds each task file's (name, len, mtime) —
/// or the shadow file's (len, mtime) — into one value. Status flips REWRITE a task file in
/// place (dir mtime unchanged), so per-file mtimes are required; reading the handful of dirents
/// costs microseconds. `None` ⇔ no board, which is itself a distinct state the WS diff must
/// observe (board deleted → emit empty).
pub fn change_key(session_id: &str) -> Option<u64> {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    let mut any = false;
    if let Some(dir) = local_dir(session_id) {
        if let Ok(entries) = std::fs::read_dir(&dir) {
            for e in entries.flatten() {
                let path = e.path();
                if path.extension().and_then(|x| x.to_str()) != Some("json") {
                    continue;
                }
                let Ok(md) = e.metadata() else { continue };
                any = true;
                path.file_name().hash(&mut h);
                md.len().hash(&mut h);
                mtime_ms(&md).hash(&mut h);
            }
        }
    } else if let Some(path) = shadow_path(session_id) {
        if let Ok(md) = std::fs::metadata(&path) {
            any = true;
            md.len().hash(&mut h);
            mtime_ms(&md).hash(&mut h);
        }
    }
    any.then(|| h.finish())
}

fn mtime_ms(md: &std::fs::Metadata) -> u64 {
    md.modified()
        .ok()
        .and_then(|m| m.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::read_board_dir;

    #[test]
    fn board_dir_parses_sorts_numerically_and_skips_noise() {
        let dir = std::env::temp_dir().join(format!("mailink-tasks-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        // Lexicographic order would put 10 before 2 — the numeric sort must not.
        std::fs::write(dir.join("10.json"), r#"{"id":"10","subject":"ten","status":"pending"}"#).unwrap();
        std::fs::write(dir.join("2.json"), r#"{"id":"2","subject":"two","status":"completed"}"#).unwrap();
        // Board-internal bookkeeping and junk must not surface as tasks.
        std::fs::write(dir.join(".lock"), "").unwrap();
        std::fs::write(dir.join(".highwatermark"), "10").unwrap();
        std::fs::write(dir.join("broken.json"), "{nope").unwrap();

        let tasks = read_board_dir(&dir);
        let ids: Vec<&str> = tasks.iter().filter_map(|t| t["id"].as_str()).collect();
        assert_eq!(ids, vec!["2", "10"]);
        assert_eq!(tasks[0]["status"], "completed");

        let _ = std::fs::remove_dir_all(&dir);
    }
}
