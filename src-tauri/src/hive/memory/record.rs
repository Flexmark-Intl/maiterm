//! Rust's record of the memory folder, `logs/memory.json` (docs/hive.md §9): who wrote each
//! file, the human's own kept block, and which files are maiTerm's. Under `logs/`, which no Hive
//! agent may write, because frontmatter is agent-editable and proves nothing.
//!
//! Every kind is a String and every field defaults (an older build that meets an unknown enum
//! variant loads nothing); fields a later build adds survive in `extra`.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::hive::paths::HivePaths;

pub const RECORD_VERSION: u32 = 1;

pub const BY_COORDINATOR: &str = "coordinator";
pub const BY_HUMAN: &str = "human";
pub const BY_UNKNOWN: &str = "unknown";
pub const BY_MAITERM: &str = "maiTerm";

pub const KIND_COORDINATOR: &str = "coordinator";
pub const KIND_WORKER: &str = "worker";
pub const KIND_HUMAN: &str = "human";
pub const KIND_UNKNOWN: &str = "unknown";
pub const KIND_MAITERM: &str = "maiterm";

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct MemoryRecord {
    pub v: u32,
    /// The kept block as the human last set it (plan D4): a rebuild never rewrites the block on
    /// disk, and a block that differs from this was changed by someone else.
    pub kept: Vec<String>,
    /// By file name (top level of `memory/`).
    pub files: BTreeMap<String, FileProv>,
    /// The `archive-*.md` folds maiTerm wrote: never indexed, removed once nothing links them.
    pub maiterm_files: Vec<String>,
    /// Why the last rebuild didn't write MEMORY.md, for the Memory view.
    pub status: Option<String>,
    /// The index lines the last rebuild wrote: such a line found anywhere outside the kept block
    /// is maiTerm's own, regenerated, never "unfiled" (a deleted file's line included).
    pub generated: Vec<String>,
    /// Unfiled lines the human chose to leave in `unfiled.md`: no longer offered.
    pub unfiled_kept: Vec<String>,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct FileProv {
    /// `coordinator` | `#2 Session store` | `human` | `unknown` | `maiTerm`.
    pub by: String,
    /// `coordinator` | `worker` | `human` | `unknown` | `maiterm`.
    pub by_kind: String,
    pub task_id: Option<String>,
    /// The first writer seen: never changed after.
    pub created_by: String,
    /// What the file held when `by` wrote it (plan D7): a change nothing explains reads `unknown`.
    pub sha256: String,
    pub at: String,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

impl FileProv {
    pub fn new(by: &str, by_kind: &str, task_id: Option<String>, sha256: String, at: &str) -> Self {
        Self { by: by.into(), by_kind: by_kind.into(), task_id, created_by: by.into(), sha256, at: at.into(), extra: Map::new() }
    }

    /// A new writer; the first one stays `created_by`.
    pub fn rewritten(&mut self, by: &str, by_kind: &str, task_id: Option<String>, sha256: String, at: &str) {
        self.by = by.into();
        self.by_kind = by_kind.into();
        self.task_id = task_id;
        self.sha256 = sha256;
        self.at = at.into();
    }
}

/// None when there is none yet: a Hive made before step 7a, whose first rebuild adopts the kept
/// block on disk. A record that doesn't parse is also None, logged: adopting is the safe reading.
pub fn load(paths: &HivePaths) -> Option<MemoryRecord> {
    let p = paths.memory_record();
    let bytes = std::fs::read(&p).ok()?;
    match serde_json::from_slice(&bytes) {
        Ok(r) => Some(r),
        Err(e) => {
            log::warn!("hive memory: {} unreadable, adopting the folder as it is: {e}", p.display());
            None
        }
    }
}

pub fn save(paths: &HivePaths, rec: &MemoryRecord) -> Result<(), String> {
    crate::hive::launch::write_json(&paths.memory_record(), &serde_json::to_value(rec).map_err(|e| e.to_string())?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unknown_fields_survive_and_missing_ones_default() {
        let raw = serde_json::json!({ "v": 1, "kept": ["- Goal: x"], "future": 3, "files": { "a.md": { "by": "human", "later": true } } });
        let r: MemoryRecord = serde_json::from_value(raw).unwrap();
        assert_eq!(r.kept, vec!["- Goal: x"]);
        assert_eq!(r.files["a.md"].by, "human");
        assert!(r.maiterm_files.is_empty() && r.status.is_none());
        let back = serde_json::to_value(&r).unwrap();
        assert_eq!(back["future"], 3);
        assert_eq!(back["files"]["a.md"]["later"], true);
    }
}
