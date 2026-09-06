//! Claude Code task-board SHADOW for SSH tabs — what is left of the board reader.
//!
//! Claude Code's session task list (TaskCreate/TaskUpdate — the strip above the prompt in the
//! TUI) persists as one small JSON file per task at `~/.claude/tasks/<session_id>/<id>.json`.
//! Until mailink-protocol v0.5 this module read that board for the phone; maiTerm's own tasks
//! replaced it on the wire (mailink/board.rs — docs/tasks.md demotes the Claude board to an
//! importer input, and the importer already folds it into `Workspace.tasks`).
//!
//! What remains is the SSH half the importer and the transcript mirror still depend on: a remote
//! session's board lives on the remote host, and `mirror.rs` snapshots it into
//! `<data_dir>/<slug>/remote-tasks/<session_id>.json` (a JSON array) in the same ssh round trip
//! it already makes for the transcript delta. `shadow_path` names that file; `parse_concatenated`
//! turns the mirror's separator-less dump into a sorted array.

use serde_json::Value;
use std::path::PathBuf;

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

#[cfg(test)]
mod tests {
    use super::parse_concatenated;

    #[test]
    fn a_concatenated_dump_sorts_numerically_and_stops_at_a_torn_value() {
        // Lexicographic order would put 10 before 2 — the numeric sort must not. No separators
        // between values, as `find -exec cat` emits them; a torn tail is dropped, not fatal.
        let dump = br#"{"id":"10","subject":"ten","status":"pending"}{"id":"2","subject":"two","status":"completed"}{"id":"3","sub"#;
        let tasks = parse_concatenated(dump);
        let ids: Vec<&str> = tasks.iter().filter_map(|t| t["id"].as_str()).collect();
        assert_eq!(ids, vec!["2", "10"]);
        assert_eq!(tasks[0]["status"], "completed");
    }
}
