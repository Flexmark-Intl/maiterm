//! A worker's record, `tasks/<task-id>/worker.json` (docs/hive.md §6.2, §11.1): which task it
//! works, its folders and branches, where it is in its life, and the tab it runs in.
//!
//! The tab → task map lives here, never on a `Tab` field, so a downgrade that drops unknown tab
//! fields can't strand a worker. Every kind is a String and every field defaults (an older build
//! that meets an unknown enum variant loads nothing); fields a later build adds survive in `extra`.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use super::paths::{is_component, HivePaths, COORDINATOR_SLUG, RESERVED_SLUGS};

pub const WORKER_VERSION: u32 = 1;
pub const WORKER_FILE: &str = "worker.json";

pub const PHASE_WAITING: &str = "waiting";
pub const PHASE_LAUNCHING: &str = "launching";
pub const PHASE_RUNNING: &str = "running";
pub const PHASE_BLOCKED: &str = "blocked";
pub const PHASE_REPORTED: &str = "reported";
pub const PHASE_FAILED: &str = "failed";
pub const PHASE_STOPPED: &str = "stopped";
/// Its work is being merged into its landing target, held, or waiting on the human (§8.1).
pub const PHASE_LANDING: &str = "landing";
/// Merged and retired: its agent exited, its chat stays readable (§8.4).
pub const PHASE_LANDED: &str = "landed";
/// Retired without landing: abandoned, its branches kept (§8.4).
pub const PHASE_RETIRED: &str = "retired";

pub const CARGO_TARGET_OWN: &str = "own";
pub const CARGO_TARGET_NONE: &str = "none";
/// The phases that hold a slot under the cap (§3.1). Reported counts until the worker lands or
/// is stopped, and Landing until it has landed; waiting holds none.
pub const LIVE_PHASES: [&str; 5] = [PHASE_LAUNCHING, PHASE_RUNNING, PHASE_BLOCKED, PHASE_REPORTED, PHASE_LANDING];

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct WorkerRecord {
    pub version: u32,
    /// The task it works; its folder is `tasks/<task_id>/`.
    pub task_id: String,
    /// Names its folder under `wt.noindex/` and its branches.
    pub slug: String,
    pub title: String,
    /// A role from the catalogue (`roles`).
    pub role: String,
    /// Its number on the board, never reused within the Hive (§4.6).
    pub badge: u32,
    /// `"waiting"` | `"launching"` | `"running"` | `"blocked"` | `"reported"` | `"failed"` | `"stopped"`
    /// | `"landing"` | `"landed"` | `"retired"`.
    pub phase: String,
    pub tab_id: Option<String>,
    pub folders: Vec<WorkerFolder>,
    pub model: String,
    pub effort: String,
    /// Task ids of the workers whose work this one waits on (§3.1).
    pub depends_on: Vec<String>,
    /// Why a waiting worker hasn't launched: a dependency, the cap, a fuse, the breaker.
    pub wait_reason: Option<String>,
    /// The setup commands ran (on its first session start): never again on a relaunch.
    pub setup_done: bool,
    /// The next launch starts a new session instead of resuming (`hiveRestartWorker` fresh).
    pub fresh_next: bool,
    /// `"own"` (its `CARGO_TARGET_DIR` under `wt.noindex/`) | `"none"` | `""` (not decided yet).
    /// Decided once, at its first launch: re-probed each time, a human's `cargo build` or
    /// `cargo clean` would move its builds between relaunches and lose the warm target (§6.5).
    pub cargo_target: String,
    pub last_report: Option<WorkerReport>,
    /// Sandbox refusals already turned into a blocker, so a retry doesn't file another.
    pub violations_seen: Vec<String>,
    /// Its queued runs not yet answered, and `⟦HIVE RUN⟧` results not yet delivered (§6.8):
    /// Rust's, keyed by job, so a reload or a restart still reaches it exactly once.
    pub pending_runs: Vec<PendingRun>,
    pub created_at: String,
    pub launched_at: Option<String>,
    /// Set once it left for good, landed or not (§8.4): why, and what of its disk was kept.
    pub retired: Option<Retired>,
    /// A land of its was undone (§8.6): its waiters block, its card says so.
    pub undone: bool,
    /// A reviewer's builder, by task id (§7.7). A reviewer keeps `hive_role = "worker"`: this is
    /// what tells it apart (plan D2).
    pub reviews: Option<String>,
    /// A reviewer's pinned commits, folder → sha: its detached worktrees sit there, and its
    /// verdict is about exactly these.
    pub reviewed: BTreeMap<String, String>,
    /// A builder's reviews: every verdict, the attempt count and any escalation (§7.7).
    pub review: Option<Review>,
    /// Its land while one is under way, held or waiting (§8.1): persisted before every
    /// irreversible step, so a restart knows how far it got (`land::reconcile`).
    pub land: Option<super::land::LandState>,
    /// Gallery asset ids the coordinator gave it to start from (§7.2): their paths go in its
    /// brief at every launch.
    pub inputs: Vec<String>,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

/// A builder's review record (§7.7): three failed attempts escalate, and only `hiveResolve`
/// settles an escalation.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Review {
    /// Failed verdicts since the brief was last revised.
    pub attempts: u32,
    pub verdicts: Vec<Verdict>,
    pub escalated: bool,
    pub resolution: Option<Resolution>,
    /// Briefs the coordinator revised after an escalation, oldest first.
    pub brief_revisions: Vec<BriefRevision>,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

impl Review {
    /// Escalated, and no resolution has settled it: no new reviewer, no land.
    pub fn unresolved(&self) -> bool {
        self.escalated && self.resolution.is_none()
    }

    pub fn latest(&self) -> Option<&Verdict> {
        self.verdicts.last()
    }
}

/// One `hiveVerdict`, as maiTerm checked it.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Verdict {
    pub reviewer_task: String,
    /// The review round it closed, from 1.
    pub attempt: u32,
    /// `"pass"` | `"fail"`.
    pub verdict: String,
    /// The commits reviewed, folder → sha.
    pub shas: BTreeMap<String, String>,
    pub summary: String,
    pub issues: Vec<ReviewIssue>,
    pub at: String,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

/// One issue in a fail (§7.7's fail-report shape).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ReviewIssue {
    pub expected: String,
    pub actual: String,
    pub evidence: String,
    pub fix: String,
    pub files: Vec<String>,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

/// How an escalation was settled: `"reassign"` | `"decompose"` | `"revise"` | `"accept"` | `"defer"`.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Resolution {
    pub kind: String,
    pub reason: String,
    pub at: String,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct BriefRevision {
    pub brief: String,
    pub reason: String,
    pub at: String,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

/// How a worker left (§8.4).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Retired {
    pub reason: String,
    pub at: String,
    /// What stayed on disk, one line each: a worktree with uncommitted or untracked work, one
    /// whose `.git` file maiTerm didn't make, its `scratch/`. Its branches always stay.
    pub kept: Vec<String>,
    /// The commit its land left its target at, for the column's "landed a1b2c3d".
    pub landed: Option<String>,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

impl Default for WorkerRecord {
    fn default() -> Self {
        Self {
            version: WORKER_VERSION,
            task_id: String::new(),
            slug: String::new(),
            title: String::new(),
            role: String::new(),
            badge: 0,
            phase: PHASE_WAITING.to_string(),
            tab_id: None,
            folders: Vec::new(),
            model: String::new(),
            effort: String::new(),
            depends_on: Vec::new(),
            wait_reason: None,
            setup_done: false,
            fresh_next: false,
            cargo_target: String::new(),
            last_report: None,
            violations_seen: Vec::new(),
            pending_runs: Vec::new(),
            created_at: String::new(),
            launched_at: None,
            retired: None,
            undone: false,
            reviews: None,
            reviewed: BTreeMap::new(),
            review: None,
            land: None,
            inputs: Vec::new(),
            extra: Map::new(),
        }
    }
}

impl WorkerRecord {
    pub fn is_live(&self) -> bool {
        LIVE_PHASES.contains(&self.phase.as_str())
    }

    pub fn is_reviewer(&self) -> bool {
        self.reviews.as_deref().is_some_and(|t| !t.is_empty())
    }

    pub fn git_folders(&self) -> impl Iterator<Item = &WorkerFolder> {
        self.folders.iter().filter(|f| f.worktree.is_some())
    }

    /// Where it works (§6.3): the worktree of its first git folder, or its `scratch/`. A
    /// reviewer's is always its `scratch/`: under `--setting-sources project` the cwd is the one
    /// project whose settings load, and the reviewed branch's must not (plan D1).
    pub fn cwd(&self, paths: &HivePaths) -> PathBuf {
        if self.is_reviewer() {
            return paths.worker_scratch(&self.slug);
        }
        self.git_folders().find_map(|f| f.worktree.as_ref()).map(PathBuf::from).unwrap_or_else(|| paths.worker_scratch(&self.slug))
    }
}

/// One of the Hive's folders as this worker has it: a worktree of a git folder on its own
/// branch, or a plain folder (read-only, no worktree).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct WorkerFolder {
    /// The Hive folder's name.
    pub name: String,
    /// `"git"` | `"plain"`.
    pub kind: String,
    pub worktree: Option<String>,
    pub branch: Option<String>,
    /// The landing target: Base, or the integration branch (§6.4). None when Base was a
    /// detached HEAD.
    pub target: Option<String>,
    /// The target's tip the branch was made at: a rollback deletes the branch only while it's here.
    pub base_sha: Option<String>,
    /// The repo's common git dir and this worktree's admin dir, both canonical: the worker's
    /// sandbox allows exactly its own corner of them (§6.7).
    pub common_dir: Option<String>,
    pub admin_dir: Option<String>,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

/// The worker's last `hiveReport`, as maiTerm verified it.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct WorkerReport {
    /// `"done"` | `"blocked"` | `"failed"`.
    pub status: String,
    pub summary: String,
    pub receipts: WorkerReceipts,
    pub notes: Option<String>,
    pub at: String,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct WorkerReceipts {
    pub commits: Vec<String>,
    pub files: Vec<String>,
    pub assets: Vec<String>,
    /// `hiveRun` jobs maiTerm checked against its run log (§7.7).
    pub checks: Vec<CheckReceipt>,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct CheckReceipt {
    pub job: String,
    pub summary: String,
}

/// A run still open: queued or running.
pub const RUN_OPEN: &str = "open";
/// A run's `⟦HIVE RUN⟧` result, waiting to be delivered.
pub const RUN_RESULT: &str = "result";

/// One entry of `pending_runs`.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct PendingRun {
    pub job: String,
    /// `"open"` | `"result"`.
    pub state: String,
    /// The command while it is open; the message to type once it is a result.
    pub text: String,
    pub at: String,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

/// A worker's task folder. Task ids come from the task list, which is hand-editable, and this
/// path gets written and removed.
pub fn task_dir(paths: &HivePaths, task_id: &str) -> Result<PathBuf, String> {
    if !is_component(task_id) || task_id == COORDINATOR_SLUG {
        return Err(format!("\"{task_id}\" can't name a task folder"));
    }
    Ok(paths.task(task_id))
}

pub fn read_worker(task_dir: &Path) -> Result<WorkerRecord, String> {
    let p = task_dir.join(WORKER_FILE);
    let bytes = fs::read(&p).map_err(|e| format!("{}: {e}", p.display()))?;
    serde_json::from_slice(&bytes).map_err(|e| format!("{}: {e}", p.display()))
}

pub fn write_worker(task_dir: &Path, w: &WorkerRecord) -> Result<(), String> {
    super::launch::write_json(&task_dir.join(WORKER_FILE), &serde_json::to_value(w).map_err(|e| e.to_string())?)
}

static UPDATE_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// Read, change and write one record under one lock, so a session start and a spawn or report
/// never write over each other's change. `f` answers whether it changed anything; nothing is
/// written when it didn't. Returns the record as it now is.
pub fn update_worker(task_dir: &Path, f: impl FnOnce(&mut WorkerRecord) -> bool) -> Result<WorkerRecord, String> {
    let _held = UPDATE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let mut w = read_worker(task_dir)?;
    if f(&mut w) {
        write_worker(task_dir, &w)?;
    }
    Ok(w)
}

/// Every worker of the Hive, by badge. A record that doesn't load, or claims a task other than
/// its folder's, is skipped with a warning: one bad file must not hide the rest.
pub fn list_workers(paths: &HivePaths) -> Vec<WorkerRecord> {
    let Ok(entries) = fs::read_dir(paths.tasks()) else { return Vec::new() };
    let mut out: Vec<WorkerRecord> = Vec::new();
    for e in entries.flatten() {
        let name = e.file_name().to_string_lossy().to_string();
        // `file_type` doesn't follow a symlink: a linked task folder isn't one maiTerm made.
        if name == COORDINATOR_SLUG || !e.file_type().is_ok_and(|t| t.is_dir()) {
            continue;
        }
        let dir = e.path();
        if !dir.join(WORKER_FILE).exists() {
            continue;
        }
        match read_worker(&dir) {
            Ok(mut w) => {
                if w.task_id.is_empty() {
                    w.task_id = name;
                } else if w.task_id != name {
                    log::warn!("hive: {} names task {}, not its folder's; skipped", dir.display(), w.task_id);
                    continue;
                }
                out.push(w);
            }
            Err(e) => log::warn!("hive: worker record skipped: {e}"),
        }
    }
    out.sort_by(|a, b| a.badge.cmp(&b.badge).then_with(|| a.created_at.cmp(&b.created_at)));
    out
}

/// The worker a task row is: the task it was given, or a sub-task it made on its own tab (workers
/// split their work, and block those rows too).
pub fn worker_for_row<'a>(workers: &'a [WorkerRecord], task_id: &str, task_tab: Option<&str>) -> Option<&'a WorkerRecord> {
    workers.iter().find(|w| w.task_id == task_id).or_else(|| {
        let tab = task_tab.filter(|t| !t.is_empty())?;
        workers.iter().find(|w| w.tab_id.as_deref() == Some(tab))
    })
}

pub fn worker_for_tab(paths: &HivePaths, tab_id: &str) -> Option<WorkerRecord> {
    if tab_id.is_empty() {
        return None;
    }
    list_workers(paths).into_iter().find(|w| w.tab_id.as_deref() == Some(tab_id))
}

/// The slug a worker's title gives it: what names its folder and its branches.
pub fn worker_slug(title: &str) -> Result<String, String> {
    let slug = super::create::slugify(title);
    if slug.is_empty() {
        return Err("The title needs at least one letter or digit: it names the worker's branch.".to_string());
    }
    if RESERVED_SLUGS.contains(&slug.as_str()) || !is_component(&slug) {
        return Err(format!("\"{slug}\" is reserved: give the worker another title."));
    }
    Ok(slug)
}

/// One past the highest badge any record holds: a number stays its worker's for the Hive's
/// life. The caller takes it under the spawn lock, so two spawns never share one.
pub fn next_badge(workers: &[WorkerRecord]) -> u32 {
    workers.iter().map(|w| w.badge).max().unwrap_or(0).saturating_add(1)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch() -> HivePaths {
        let t = std::env::temp_dir().join(format!("hive-worker-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&t).unwrap();
        HivePaths::new(t)
    }

    fn worker(task: &str, badge: u32, tab: Option<&str>) -> WorkerRecord {
        WorkerRecord {
            task_id: task.into(),
            slug: format!("w-{badge}"),
            title: format!("Worker {badge}"),
            role: "backend".into(),
            badge,
            phase: PHASE_RUNNING.into(),
            tab_id: tab.map(Into::into),
            ..WorkerRecord::default()
        }
    }

    #[test]
    fn a_record_round_trips_with_fields_a_later_build_added() {
        let p = scratch();
        let dir = task_dir(&p, "t1").unwrap();
        let text = r#"{
            "task_id": "t1", "slug": "api", "badge": 3, "phase": "reviewing",
            "future": {"x": 1},
            "folders": [{"name": "web", "kind": "git", "worktree": "/w", "branch": "b", "folder_future": true}],
            "last_report": {"status": "done", "receipts": {"commits": ["abc"], "checks_future": [1]}, "report_future": 2}
        }"#;
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join(WORKER_FILE), text).unwrap();
        let w = read_worker(&dir).unwrap();
        assert_eq!(w.phase, "reviewing", "an unknown phase still loads");
        assert!(!w.is_live());
        assert_eq!(w.version, WORKER_VERSION, "a missing field takes its default");
        assert_eq!(w.extra.get("future"), Some(&serde_json::json!({"x": 1})));
        assert_eq!(w.folders[0].extra.get("folder_future"), Some(&serde_json::json!(true)));
        let report = w.last_report.as_ref().unwrap();
        assert_eq!(report.receipts.commits, vec!["abc"]);

        write_worker(&dir, &w).unwrap();
        let again = read_worker(&dir).unwrap();
        assert_eq!(again, w);
        let saved = fs::read_to_string(dir.join(WORKER_FILE)).unwrap();
        for kept in ["future", "folder_future", "report_future", "checks_future"] {
            assert!(saved.contains(kept), "{kept} survives a save");
        }
        let _ = fs::remove_dir_all(&p.root);
    }

    #[test]
    fn list_and_find_by_tab_skip_what_isnt_a_worker() {
        let p = scratch();
        for (task, badge, tab) in [("t2", 2, Some("tab-b")), ("t1", 1, Some("tab-a")), ("t3", 3, None)] {
            write_worker(&task_dir(&p, task).unwrap(), &worker(task, badge, tab)).unwrap();
        }
        // The coordinator's folder, a folder with no record, a record naming another task, junk.
        fs::create_dir_all(p.coordinator_task()).unwrap();
        fs::write(p.coordinator_task().join(WORKER_FILE), "{}").unwrap();
        fs::create_dir_all(p.task("empty")).unwrap();
        write_worker(&p.task("liar"), &worker("t1", 9, Some("tab-x"))).unwrap();
        fs::create_dir_all(p.task("junk")).unwrap();
        fs::write(p.task("junk").join(WORKER_FILE), "not json").unwrap();

        let all = list_workers(&p);
        assert_eq!(all.iter().map(|w| w.task_id.as_str()).collect::<Vec<_>>(), vec!["t1", "t2", "t3"], "by badge");
        assert_eq!(worker_for_tab(&p, "tab-b").map(|w| w.task_id), Some("t2".into()));
        assert!(worker_for_tab(&p, "tab-x").is_none(), "a record naming another task is no worker");
        assert!(worker_for_tab(&p, "").is_none());
        let _ = fs::remove_dir_all(&p.root);
    }

    #[test]
    fn badges_are_never_reused() {
        assert_eq!(next_badge(&[]), 1);
        let ws = vec![worker("a", 1, None), worker("c", 3, None)];
        assert_eq!(next_badge(&ws), 4, "a gap stays a gap: badge 2 was someone's");
        let mut stopped = worker("d", 7, None);
        stopped.phase = PHASE_STOPPED.into();
        assert_eq!(next_badge(&[ws, vec![stopped]].concat()), 8, "a stopped worker keeps its number");
    }

    #[test]
    fn slugs_come_from_the_title_and_refuse_the_reserved_ones() {
        assert_eq!(worker_slug("API: auth tokens").unwrap(), "api-auth-tokens");
        for bad in ["", "!!!", "Coordinator", "land"] {
            assert!(worker_slug(bad).is_err(), "{bad:?}");
        }
        assert!(worker_slug("Landing page").is_ok());
    }

    #[test]
    fn task_folders_refuse_unsafe_ids() {
        let p = HivePaths::new("/x/hive/h");
        assert_eq!(task_dir(&p, "t-1").unwrap(), Path::new("/x/hive/h/tasks/t-1"));
        for bad in ["", "..", "a/b", ".h", "coordinator"] {
            assert!(task_dir(&p, bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn live_phases_are_what_the_cap_counts() {
        let mut w = WorkerRecord::default();
        assert_eq!(w.phase, PHASE_WAITING);
        assert!(!w.is_live(), "a waiting worker holds no slot");
        for p in LIVE_PHASES {
            w.phase = p.into();
            assert!(w.is_live());
        }
        assert!(LIVE_PHASES.contains(&PHASE_LANDING), "§3.1 counts Landing");
        for p in [PHASE_FAILED, PHASE_STOPPED, PHASE_LANDED, PHASE_RETIRED] {
            w.phase = p.into();
            assert!(!w.is_live());
        }
    }

    #[test]
    fn a_record_from_before_retirement_loads_with_none_and_a_retired_one_round_trips() {
        let p = scratch();
        let dir = task_dir(&p, "t1").unwrap();
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join(WORKER_FILE), r#"{"task_id":"t1","phase":"stopped"}"#).unwrap();
        let w = read_worker(&dir).unwrap();
        assert!(w.retired.is_none() && !w.undone);
        let mut r = w.clone();
        r.phase = PHASE_RETIRED.into();
        r.retired = Some(Retired { reason: "abandoned".into(), at: "now".into(), kept: vec!["web: dirty".into()], ..Retired::default() });
        write_worker(&dir, &r).unwrap();
        assert_eq!(read_worker(&dir).unwrap(), r);
        let _ = fs::remove_dir_all(&p.root);
    }

    #[test]
    fn a_reviewer_works_from_its_scratch_and_a_review_round_trips_with_later_fields() {
        let p = scratch();
        let dir = task_dir(&p, "r1").unwrap();
        fs::create_dir_all(&dir).unwrap();
        let text = r#"{
            "task_id": "r1", "slug": "rev", "phase": "running", "reviews": "t-builder",
            "reviewed": {"web": "abc"},
            "folders": [{"name": "web", "kind": "git", "worktree": "/wt/rev/web"}],
            "review": {"attempts": 2, "escalated": true, "review_future": 1,
                       "verdicts": [{"verdict": "fail", "issues": [{"expected": "e", "files": ["a.rs"], "issue_future": 2}]}]}
        }"#;
        fs::write(dir.join(WORKER_FILE), text).unwrap();
        let w = read_worker(&dir).unwrap();
        assert!(w.is_reviewer());
        assert_eq!(w.cwd(&p), p.worker_scratch("rev"), "never the reviewed worktree");
        let r = w.review.as_ref().unwrap();
        assert!(r.unresolved() && r.latest().unwrap().issues[0].files == vec!["a.rs"]);
        write_worker(&dir, &w).unwrap();
        let saved = fs::read_to_string(dir.join(WORKER_FILE)).unwrap();
        assert!(saved.contains("review_future") && saved.contains("issue_future"));
        let builder = WorkerRecord { reviews: Some(String::new()), ..WorkerRecord::default() };
        assert!(!builder.is_reviewer(), "an empty id reviews nothing");
        let _ = fs::remove_dir_all(&p.root);
    }
}
