//! The land log, `logs/lands.jsonl` (docs/hive.md §8.6): one line per repo per event, append-only.
//! Whether a land was undone, or is suspect, is worked out by folding the lines (step 6e), never
//! by editing one. Every field is a string or a list (§3.2), and a later build's fields survive.

use std::path::Path;
use std::sync::Mutex;

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::hive::paths::HivePaths;

pub const LOG_VERSION: u32 = 1;
pub const KIND_LAND: &str = "land";
/// A land onto `hive/<slug>` by a `--no-ff` merge of the approved sha (§8.3).
pub const KIND_INTEGRATION: &str = "integration";
pub const BY_COORDINATOR: &str = "coordinator";
pub const BY_HUMAN: &str = "human";
pub const REVIEW_PASS: &str = "pass";
pub const REVIEW_ACCEPTED: &str = "accepted";
pub const REVIEW_NONE: &str = "none";
pub const PUSHED_OFF: &str = "off";

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct LandLogEntry {
    pub v: u32,
    /// `<land>/<folder>`: what an undo names.
    pub id: String,
    pub land: String,
    /// `land` | `batch_member` | `integration` | `undo` | `base_check`.
    pub kind: String,
    pub hive_id: String,
    pub task_id: String,
    /// The worker's slug.
    pub worker: String,
    pub title: String,
    pub folder: String,
    /// The branch it landed on: Base, or `hive/<slug>`.
    pub target: String,
    pub reviewed_sha: String,
    pub tested_sha: String,
    /// A batch member's or an integration land's own merge commit.
    pub merge_commit: Option<String>,
    pub base_before: String,
    pub base_after: String,
    pub tree_tested: String,
    pub tree_landed: String,
    /// The coordinator's words, quoted.
    pub summary: String,
    /// `coordinator` | `human`.
    pub by: String,
    /// The agent request a human approved it through.
    pub request: Option<String>,
    /// `pass` | `accepted` | `none`.
    pub review: String,
    pub undoes: Option<String>,
    /// `ok` | `held: <why>` | `off`.
    pub pushed: String,
    pub at: String,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

impl Default for LandLogEntry {
    fn default() -> Self {
        Self {
            v: LOG_VERSION,
            id: String::new(),
            land: String::new(),
            kind: KIND_LAND.to_string(),
            hive_id: String::new(),
            task_id: String::new(),
            worker: String::new(),
            title: String::new(),
            folder: String::new(),
            target: String::new(),
            reviewed_sha: String::new(),
            tested_sha: String::new(),
            merge_commit: None,
            base_before: String::new(),
            base_after: String::new(),
            tree_tested: String::new(),
            tree_landed: String::new(),
            summary: String::new(),
            by: BY_COORDINATOR.to_string(),
            request: None,
            review: REVIEW_NONE.to_string(),
            undoes: None,
            pushed: PUSHED_OFF.to_string(),
            at: String::new(),
            extra: Map::new(),
        }
    }
}

/// One writer at a time in this process; each line one `O_APPEND` write.
static WRITE: Mutex<()> = Mutex::new(());

pub fn append(paths: &HivePaths, entry: &LandLogEntry) -> Result<(), String> {
    let _held = WRITE.lock().unwrap_or_else(|e| e.into_inner());
    let v = serde_json::to_value(entry).map_err(|e| e.to_string())?;
    crate::hive::queue::append_line(&paths.land_log(), &v)
}

/// Every line that parses, oldest first.
pub fn read(paths: &HivePaths) -> Vec<LandLogEntry> {
    read_at(&paths.land_log())
}

fn read_at(path: &Path) -> Vec<LandLogEntry> {
    let Ok(text) = std::fs::read_to_string(path) else { return Vec::new() };
    text.lines().filter_map(|l| serde_json::from_str::<LandLogEntry>(l).ok()).collect()
}

/// Whether the log already has this repo's line for this land (a restart's reconcile wrote it).
pub fn has(paths: &HivePaths, id: &str) -> bool {
    read(paths).iter().any(|e| e.id == id && e.kind != "undo")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lines_append_and_read_back_with_a_later_builds_fields() {
        let base = std::env::temp_dir().join(format!("hive-landlog-{}", uuid::Uuid::new_v4()));
        let paths = HivePaths::new(&base);
        let e = LandLogEntry { id: "L-1/web".into(), land: "L-1".into(), folder: "web".into(), at: "now".into(), ..LandLogEntry::default() };
        append(&paths, &e).unwrap();
        std::fs::write(paths.land_log(), format!("{}not json\n{}\n", std::fs::read_to_string(paths.land_log()).unwrap(), r#"{"v":1,"id":"L-2/api","kind":"land","later":7}"#)).unwrap();
        let all = read(&paths);
        assert_eq!(all.len(), 2, "a line that doesn't parse is skipped");
        assert_eq!(all[0], e);
        assert_eq!(all[1].extra.get("later"), Some(&serde_json::json!(7)));
        assert!(has(&paths, "L-1/web") && !has(&paths, "L-1/api"));
        let line: Value = serde_json::from_str(std::fs::read_to_string(paths.land_log()).unwrap().lines().next().unwrap()).unwrap();
        for k in ["v", "merge_commit", "request", "undoes", "pushed", "tree_landed"] {
            assert!(line.get(k).is_some(), "{k} is written even when empty");
        }
        let _ = std::fs::remove_dir_all(&base);
    }
}
