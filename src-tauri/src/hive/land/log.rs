//! The land log, `logs/lands.jsonl` (docs/hive.md §8.6): one line per repo per event, append-only.
//! Whether a land was undone, or is suspect, is worked out by folding the lines (`fold`), never
//! by editing one. Every field is a string or a list (§3.2), and a later build's fields survive.

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::Mutex;

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::hive::paths::HivePaths;

pub const LOG_VERSION: u32 = 1;
pub const KIND_LAND: &str = "land";
/// A land onto `hive/<slug>` by a `--no-ff` merge of the approved sha (§8.3).
pub const KIND_INTEGRATION: &str = "integration";
/// One member of a batched land, with its own merge commit (§6.8).
pub const KIND_BATCH_MEMBER: &str = "batch_member";
/// A revert commit landed by Undo (§8.6): `undoes` names the line it reverts.
pub const KIND_UNDO: &str = "undo";
/// The post-land check (§8.6): the land check on the target's tip alone, `check` its result.
pub const KIND_BASE_CHECK: &str = "base_check";
pub const CHECK_PASSED: &str = "passed";
pub const CHECK_FAILED: &str = "failed";
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
    /// The coordinator's decision-log decision that landed it (§7.6): what the human's Undo
    /// overrides. A land resumed on a new pin keeps its id under a new decision.
    pub decision: Option<String>,
    /// `pass` | `accepted` | `none`.
    pub review: String,
    pub undoes: Option<String>,
    /// `ok` | `held: <why>` | `off`.
    pub pushed: String,
    /// `passed` when a land check passed on exactly what landed (a land, an undo), `passed` |
    /// `failed` on a base check, empty when no check ran.
    pub check: String,
    pub at: String,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

impl LandLogEntry {
    /// A line that moved its target: a land, a batch member, an integration land or an undo.
    pub fn moved_target(&self) -> bool {
        matches!(self.kind.as_str(), KIND_LAND | KIND_INTEGRATION | KIND_BATCH_MEMBER | KIND_UNDO)
    }

    /// Its target, once it landed, is a tree its land check passed: the land check ran on exactly
    /// the tree that landed (§8.6: re-running it there proves nothing new).
    fn proves_target(&self) -> bool {
        self.moved_target() && self.check == CHECK_PASSED && !self.tree_landed.is_empty() && self.tree_landed == self.tree_tested
    }
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
            decision: None,
            review: REVIEW_NONE.to_string(),
            undoes: None,
            pushed: PUSHED_OFF.to_string(),
            check: String::new(),
            at: String::new(),
            extra: Map::new(),
        }
    }
}

/// A line as the land log view shows it: undone by which undo, and whether it is suspect.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct FoldedEntry {
    #[serde(flatten)]
    pub entry: LandLogEntry,
    /// The undo line that reverted it.
    pub undone_by: Option<String>,
    /// A base check failed after it landed, and nothing in its repo has passed since (§8.6).
    pub suspect: bool,
}

/// A repo's standing after the fold: suspect while its target last failed its land check alone
/// and no land, undo or base check there has passed since.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct RepoStanding {
    pub folder: String,
    pub target: String,
    pub suspect: bool,
    /// The failing base check's line, and when.
    pub failed_check: Option<String>,
    pub failed_at: Option<String>,
    /// The lines suspect in it, oldest first.
    pub suspects: Vec<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct Folded {
    /// Oldest first.
    pub entries: Vec<FoldedEntry>,
    /// By folder.
    pub repos: BTreeMap<String, RepoStanding>,
}

impl Folded {
    pub fn suspect_folders(&self) -> Vec<String> {
        self.repos.values().filter(|r| r.suspect).map(|r| r.folder.clone()).collect()
    }

    pub fn get(&self, id: &str) -> Option<&FoldedEntry> {
        self.entries.iter().find(|e| e.entry.id == id)
    }
}

/// The log folded (§8.6). Per repo, every land since the target last passed its land check
/// (a base check that passed, or a land or undo whose check passed on exactly what landed) is
/// suspect once a base check fails; anything passing there later clears the repo and its lands.
/// An undo line marks the line it names as undone.
pub fn fold(lines: Vec<LandLogEntry>) -> Folded {
    #[derive(Default)]
    struct Repo {
        since: Vec<usize>,
        suspect: Vec<usize>,
        failed: Option<usize>,
        target: String,
    }
    let mut repos: BTreeMap<String, Repo> = BTreeMap::new();
    let mut undone: BTreeMap<String, String> = BTreeMap::new();
    for (i, e) in lines.iter().enumerate() {
        let r = repos.entry(e.folder.clone()).or_default();
        if !e.target.is_empty() {
            r.target = e.target.clone();
        }
        if e.kind == KIND_UNDO {
            if let Some(of) = &e.undoes {
                undone.entry(of.clone()).or_insert_with(|| e.id.clone());
            }
        }
        let passed = e.proves_target() || (e.kind == KIND_BASE_CHECK && e.check == CHECK_PASSED);
        if passed {
            r.since.clear();
            r.suspect.clear();
            r.failed = None;
        } else if e.moved_target() {
            r.since.push(i);
        } else if e.kind == KIND_BASE_CHECK && e.check == CHECK_FAILED {
            r.failed = Some(i);
            for k in r.since.clone() {
                if !r.suspect.contains(&k) {
                    r.suspect.push(k);
                }
            }
        }
    }
    let suspect: Vec<usize> = repos.values().flat_map(|r| r.suspect.iter().copied()).collect();
    let standings = repos
        .iter()
        .map(|(folder, r)| {
            let failed = r.failed.map(|k| &lines[k]);
            (
                folder.clone(),
                RepoStanding {
                    folder: folder.clone(),
                    target: r.target.clone(),
                    suspect: failed.is_some(),
                    failed_check: failed.map(|e| e.id.clone()),
                    failed_at: failed.map(|e| e.at.clone()),
                    suspects: r.suspect.iter().map(|k| lines[*k].id.clone()).collect(),
                },
            )
        })
        .collect();
    let entries = lines
        .into_iter()
        .enumerate()
        .map(|(i, e)| FoldedEntry { undone_by: undone.get(&e.id).cloned(), suspect: suspect.contains(&i), entry: e })
        .collect();
    Folded { entries, repos: standings }
}

/// The log as it stands, folded. Blocks on the disk.
pub fn folded(paths: &HivePaths) -> Folded {
    fold(read(paths))
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
    read(paths).iter().any(|e| e.id == id)
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
        for k in ["v", "merge_commit", "request", "undoes", "pushed", "tree_landed", "check"] {
            assert!(line.get(k).is_some(), "{k} is written even when empty");
        }
        let _ = std::fs::remove_dir_all(&base);
    }

    fn line(id: &str, kind: &str, folder: &str, check: &str) -> LandLogEntry {
        let (land, _) = id.split_once('/').unwrap();
        LandLogEntry { id: id.into(), land: land.into(), kind: kind.into(), folder: folder.into(), target: "main".into(), tree_tested: "t".into(), tree_landed: "t".into(), check: check.into(), ..LandLogEntry::default() }
    }

    #[test]
    fn the_fold_marks_undone_lines_and_the_lands_since_base_last_passed_as_suspect() {
        let mut undo = line("U-1/web", KIND_UNDO, "web", "");
        undo.undoes = Some("L-2/web".into());
        let mut raced = line("L-4/web", KIND_LAND, "web", CHECK_PASSED);
        raced.tree_landed = "other".into();
        let f = fold(vec![
            line("L-1/web", KIND_LAND, "web", CHECK_PASSED),
            line("L-2/web", KIND_LAND, "web", ""),
            line("L-3/api", KIND_LAND, "api", ""),
            undo,
            raced,
            line("B-1/web", KIND_BASE_CHECK, "web", CHECK_FAILED),
        ]);
        // L-1's check passed on what landed: the lands after it are the suspects.
        assert_eq!(f.repos["web"].suspects, vec!["L-2/web", "U-1/web", "L-4/web"], "a check that passed on another tree proves nothing");
        assert!(f.repos["web"].suspect && !f.repos["api"].suspect);
        assert_eq!(f.repos["web"].failed_check.as_deref(), Some("B-1/web"));
        assert_eq!(f.suspect_folders(), vec!["web"]);
        assert_eq!(f.get("L-2/web").unwrap().undone_by.as_deref(), Some("U-1/web"));
        assert!(!f.get("L-1/web").unwrap().suspect && f.get("L-2/web").unwrap().suspect && !f.get("L-3/api").unwrap().suspect);

        // A fix forward whose check passed on what landed clears the repo and its suspects; so
        // does a base check that passes.
        for clear in [line("L-5/web", KIND_LAND, "web", CHECK_PASSED), line("B-2/web", KIND_BASE_CHECK, "web", CHECK_PASSED), line("U-2/web", KIND_UNDO, "web", CHECK_PASSED)] {
            let mut lines: Vec<LandLogEntry> = f.entries.iter().map(|e| e.entry.clone()).collect();
            lines.push(clear.clone());
            let g = fold(lines);
            assert!(!g.repos["web"].suspect && g.entries.iter().all(|e| !e.suspect), "{}", clear.id);
        }
        // A land without a check after the failure joins the suspects only when Base fails again.
        let mut lines: Vec<LandLogEntry> = f.entries.iter().map(|e| e.entry.clone()).collect();
        lines.push(line("L-6/web", KIND_LAND, "web", ""));
        let g = fold(lines.clone());
        assert!(g.repos["web"].suspect && !g.get("L-6/web").unwrap().suspect);
        lines.push(line("B-3/web", KIND_BASE_CHECK, "web", CHECK_FAILED));
        assert!(fold(lines).get("L-6/web").unwrap().suspect);
    }
}
