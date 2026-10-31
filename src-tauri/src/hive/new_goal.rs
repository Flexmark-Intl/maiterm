//! New goal (docs/hive.md §11.5): a Done or paused Hive takes a new goal instead of a new Hive.
//!
//! Every worker of the current goal is retired (`retire::retire_worker`: agent stopped, tab gone,
//! clean worktrees removed, branches kept), the old goal is archived as chapter N under
//! `chapters/<N>/`, and a fresh queen session starts in the SAME coordinator tab (its tab id is
//! the phone's identity for it). Kept: gallery, settings, folders, models, cap, checkpoints,
//! budget, the land log and the decision log (never moved or truncated: undo and earned autonomy
//! read them), and memory unless the human sets it aside.
//!
//! What scopes a goal is a number, never a moved file: `Hive.chapters.len()` is the current
//! goal's chapter, each worker records the chapter it was spawned in (`WorkerRecord.chapter`),
//! and each coordinator session the chapter it started in (`SessionEntry.chapter`). So unlanded
//! work, Done and the board read only the current goal's workers, and the coordinator resumes
//! only a session of the current goal, whatever a crash left half done.
//!
//! Refused while any current worker has commits not on its landing target (except one the last
//! Done report of this goal abandoned), while a land or an undo is under way, or while the Hive
//! is active: the human pauses it or waits for Done. `hive_ask_queen_to_land` is the overlay's
//! way out: it asks the queen, waking a Done Hive, to land what is pending.

use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};

use super::deliver::{frame_escape, one_line, Sent};
use super::finish::{self, FinishRecord, Waking};
use super::fuses::{StatusChange, STATUS_PAUSED};
use super::paths::HivePaths;
use super::record::{Hive, CRITERIA_NEEDED, STATUS_ACTIVE, STATUS_DONE};
use super::retire::{self, RetireSpec};
use super::rpc::{self, Front, HiveWindow};
use super::tools::{blocking, coordinator_tab, has_tab, Caller, Ctx};
use super::worker::{self, WorkerRecord};
use crate::state::AppState;

/// One finished (or set-aside) goal, as New goal archived it: `Hive.chapters`, the TS
/// `HiveChapter`. A plain struct, every field defaulting, later fields kept in `extra`.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct HiveChapter {
    pub goal: String,
    /// When this goal started: the Hive's `created_at`, or the previous New goal's time.
    pub started_at: String,
    /// When New goal replaced it.
    pub ended_at: String,
    /// Its Done time (ms, as `Hive.done_at`), if it reached Done.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub done_at: Option<i64>,
    /// The first lines of its Done report; empty if it never finished.
    pub report_summary: String,
    /// What paid generation cost during it.
    pub spent_cents: u64,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

/// A worker's branch of the CURRENT goal with commits not on its land target yet.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct PendingBranch {
    /// "Worker 3 · Fix the login form", as the board names it.
    pub worker: String,
    pub badge: u32,
    pub folder: String,
    pub branch: String,
    pub target: Option<String>,
    pub ahead: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct MemoryInfo {
    pub path: String,
    pub files: u64,
}

/// What the New goal sheet and the pending-commits overlay show. Touches nothing.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct NewGoalPreview {
    pub allowed: bool,
    /// Why New goal can't start now, one plain sentence each. Pending commits are in `unlanded`.
    pub reasons: Vec<String>,
    pub unlanded: Vec<PendingBranch>,
    /// Workers whose agent runs now: they stop.
    pub live_workers: Vec<String>,
    pub goal: String,
    pub brief: String,
    /// Absolute paths in the coordinator home's `brief/`.
    pub brief_files: Vec<String>,
    pub instructions: String,
    /// The number the current goal gets as a chapter.
    pub next_chapter: u32,
    pub memory: Option<MemoryInfo>,
}

/// What the human submits on the New goal sheet.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(default)]
pub struct NewGoalSpec {
    pub goal: String,
    pub brief: String,
    /// Absolute paths: the current ones (from the preview) to keep, plus any newly attached.
    pub brief_files: Vec<String>,
    pub instructions: String,
    /// False archives the memory folder into the old chapter and starts the new goal with none.
    pub keep_memory: bool,
}

impl Default for NewGoalSpec {
    fn default() -> Self {
        Self { goal: String::new(), brief: String::new(), brief_files: Vec::new(), instructions: String::new(), keep_memory: true }
    }
}

/// The board's name for a worker, which the overlay repeats.
pub(crate) fn worker_name(w: &WorkerRecord) -> String {
    format!("Worker {} \u{b7} {}", w.badge, one_line(&w.title, 120))
}

// ── What stands in the way ──

/// What the checks found, read off the disk and git. Blocks.
#[derive(Debug, Clone, Default)]
pub(crate) struct Standing {
    pub reasons: Vec<String>,
    pub unlanded: Vec<PendingBranch>,
    /// The current goal's workers.
    pub workers: Vec<WorkerRecord>,
}

/// The workers the last Done report of the current goal abandoned (its `abandoned`, by label),
/// and when that report was written (ms): their branches stay unlanded by the human's own reading
/// of that report, as it stood then.
fn abandoned_in_report(hive: &Hive, paths: &HivePaths) -> (HashSet<String>, i64) {
    match finish::last_finish(paths) {
        Some(r) if report_in_chapter(hive, &r) => (r.abandoned.iter().map(|a| a.worker.clone()).collect(), r.at_ms),
        _ => (HashSet::new(), 0),
    }
}

/// The branch's tip was committed at or before `at_ms`: nothing on it is newer than the report
/// that abandoned it. Unreadable answers no, so the branch is listed. Blocks on git.
fn tip_not_after(hive: &Hive, folder: &str, branch: &str, at_ms: i64) -> bool {
    let Some(f) = hive.folders.iter().find(|f| f.name == folder) else { return false };
    let out = super::git::run(
        super::git::verify_in(Path::new(&f.path)).args(["log", "-1", "--format=%ct"]).arg(format!("refs/heads/{branch}")),
        super::git::QUERY_TIMEOUT,
    );
    out.ok().and_then(|s| s.trim().parse::<i64>().ok()).is_some_and(|ct| ct.saturating_mul(1000) <= at_ms)
}

/// A report written during the current goal: at or after its start.
fn report_in_chapter(hive: &Hive, r: &FinishRecord) -> bool {
    r.at.as_str() >= hive.chapter_started_at()
}

/// The current goal's branches with commits not landed, those the report abandoned left out.
/// A repo maiTerm can't read is a reason, never "nothing pending". Blocks on git.
pub(crate) fn pending_branches(hive: &Hive, paths: &HivePaths, workers: &[WorkerRecord]) -> Result<Vec<PendingBranch>, String> {
    let (abandoned, report_ms) = abandoned_in_report(hive, paths);
    let mut out = Vec::new();
    for w in workers {
        let named = abandoned.contains(&super::tools::worker_label(w));
        for u in finish::unlanded(hive, w)? {
            // Abandoned by the report, and nothing committed since: the human read it so. A
            // worker the queen restarted after the report and that committed again is listed.
            if named && tip_not_after(hive, &u.folder, &u.branch, report_ms) {
                continue;
            }
            let target = w.folders.iter().find(|f| f.name == u.folder).and_then(|f| f.target.clone());
            out.push(PendingBranch { worker: worker_name(w), badge: w.badge, folder: u.folder, branch: u.branch, target, ahead: u.ahead });
        }
    }
    Ok(out)
}

/// Everything New goal checks before it touches anything (§11.5). `suspended`: the workspace is;
/// `has_queen`: the coordinator's tab is there. Blocks on the disk and git.
pub(crate) fn standing(hive: &Hive, paths: &HivePaths, suspended: bool, has_queen: bool) -> Standing {
    let mut reasons = Vec::new();
    match hive.status.as_str() {
        STATUS_DONE | STATUS_PAUSED => {}
        STATUS_ACTIVE => reasons.push("The Hive is still working on its goal: pause it or wait for Done.".to_string()),
        other => reasons.push(format!("This Hive is {other}: only a Done or paused Hive takes a new goal.")),
    }
    if !has_queen {
        reasons.push("This Hive has no queen tab to start the new goal in.".to_string());
    }
    if suspended {
        reasons.push("This Hive's workspace is suspended: resume it first.".to_string());
    }
    let workers = worker::current_workers(hive, paths);
    for w in workers.iter().filter(|w| w.phase == worker::PHASE_LANDING && !super::land::held(w)) {
        reasons.push(format!("{} is landing: let its land finish, or hold it, first.", worker_name(w)));
    }
    for u in super::land::undo::read_all(paths) {
        if u.in_progress() {
            reasons.push(format!("The undo of {} is running: let it finish first.", u.undoes));
        } else if u.state == super::land::STATE_AWAITING_HUMAN {
            // Its card is the queen's, and a New goal fails her cards.
            reasons.push(format!("The undo of {} is waiting for your answer: approve or refuse it first.", u.undoes));
        }
    }
    let unlanded = match pending_branches(hive, paths, &workers) {
        Ok(u) => u,
        Err(e) => {
            reasons.push(e);
            Vec::new()
        }
    };
    Standing { reasons, unlanded, workers }
}

fn count_files(dir: &Path) -> u64 {
    let mut n = 0;
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        for e in fs::read_dir(&d).into_iter().flatten().flatten() {
            match fs::symlink_metadata(e.path()) {
                Ok(m) if m.is_dir() => stack.push(e.path()),
                Ok(m) if m.is_file() => n += 1,
                _ => {}
            }
        }
    }
    n
}

fn memory_info(paths: &HivePaths) -> Option<MemoryInfo> {
    let dir = paths.memory();
    fs::symlink_metadata(&dir).ok().filter(|m| m.is_dir())?;
    Some(MemoryInfo { path: dir.to_string_lossy().to_string(), files: count_files(&dir) })
}

fn brief_dir(paths: &HivePaths) -> PathBuf {
    paths.coordinator_home().join(super::create::BRIEF_FILES_DIR)
}

/// The preview, off a `Standing` and the tabs whose agent runs. Pure.
pub(crate) fn preview_of(hive: &Hive, paths: &HivePaths, st: &Standing, running: &dyn Fn(&str) -> bool) -> NewGoalPreview {
    let live_workers = st.workers.iter().filter(|w| w.is_live() && w.tab_id.as_deref().is_some_and(running)).map(worker_name).collect();
    NewGoalPreview {
        allowed: st.reasons.is_empty() && st.unlanded.is_empty(),
        reasons: st.reasons.clone(),
        unlanded: st.unlanded.clone(),
        live_workers,
        goal: hive.goal.clone(),
        brief: hive.brief.clone(),
        brief_files: hive.brief_files.iter().map(|f| brief_dir(paths).join(f).to_string_lossy().to_string()).collect(),
        instructions: hive.instructions.clone(),
        next_chapter: hive.chapter() + 1,
        memory: memory_info(paths),
    }
}

/// The current record and what stands in the way, read now.
async fn read_standing<W: HiveWindow>(ctx: &Ctx<'_, W>, caller: &Caller) -> Result<(Hive, Option<String>, Standing), String> {
    let hive = super::autonomy::current_hive(ctx, caller).ok_or("This Hive is gone.")?;
    let queen = coordinator_tab(ctx.data, caller);
    let suspended = super::tools::hive_suspended(ctx.data, caller);
    let (h, p, has_queen) = (hive.clone(), caller.paths.clone(), queen.is_some());
    let st = blocking(move || Ok(standing(&h, &p, suspended, has_queen))).await?;
    Ok((hive, queen, st))
}

pub(crate) async fn preview<W: HiveWindow>(ctx: &Ctx<'_, W>, caller: &Caller) -> Result<NewGoalPreview, String> {
    let (hive, _, st) = read_standing(ctx, caller).await?;
    // Only agents in front of their terminals stop; one maiTerm can't read counts as running.
    let mut running: Vec<String> = Vec::new();
    for t in st.workers.iter().filter(|w| w.is_live()).filter_map(|w| w.tab_id.clone()) {
        if has_tab(ctx.data, caller, &t) && matches!(ctx.window.front(&t).await, Front::Job | Front::Unknown) {
            running.push(t);
        }
    }
    let paths = caller.paths.clone();
    blocking(move || Ok(preview_of(&hive, &paths, &st, &|t: &str| running.iter().any(|r| r == t)))).await
}

// ── The new goal ──

/// The sheet's rules, checked again here: Create's limits, and a goal.
pub(crate) fn check_spec(mut spec: NewGoalSpec) -> Result<NewGoalSpec, String> {
    use super::create::{MAX_BRIEF, MAX_BRIEF_FILES, MAX_INSTRUCTIONS};
    spec.goal = spec.goal.trim().to_string();
    if spec.goal.is_empty() {
        return Err("Give the Hive its new goal.".to_string());
    }
    let n = spec.instructions.chars().count();
    if n > MAX_INSTRUCTIONS {
        return Err(format!("The rules for every agent are {n} characters; the limit is 16,000. Shorten them: nothing is cut."));
    }
    let n = spec.brief.chars().count();
    if n > MAX_BRIEF {
        return Err(format!("The project brief is {n} characters; the limit is 32,000. Shorten it, or attach the rest as a file."));
    }
    if spec.brief_files.len() > MAX_BRIEF_FILES {
        return Err(format!("Attach at most {MAX_BRIEF_FILES} files to the project brief."));
    }
    if spec.brief_files.iter().any(|f| !crate::share::expand_tilde(f.trim()).is_absolute()) {
        return Err("Every attached file needs a full path.".to_string());
    }
    Ok(spec)
}

/// The first lines of a report's summary, for the chapter and the queen's Earlier goals.
fn summary_head(summary: &str) -> String {
    let lines: Vec<&str> = summary.lines().map(str::trim_end).filter(|l| !l.trim().is_empty()).take(8).collect();
    let head = lines.join("\n");
    if head.chars().count() > 1_000 {
        format!("{}\u{2026}", head.chars().take(1_000).collect::<String>())
    } else {
        head
    }
}

/// The chapter the current goal becomes. `report`: the last Done report, `spent_micros` the
/// gallery ledger's total now, `ended_at` the New goal's time. Pure.
pub(crate) fn chapter_of(hive: &Hive, report: Option<&FinishRecord>, spent_micros: i64, ended_at: &str) -> HiveChapter {
    let report = report.filter(|r| report_in_chapter(hive, r));
    let earlier: u64 = hive.chapters.iter().map(|c| c.spent_cents).sum();
    let total_cents = (spent_micros.max(0) / 10_000) as u64;
    HiveChapter {
        goal: hive.goal.clone(),
        started_at: hive.chapter_started_at().to_string(),
        ended_at: ended_at.to_string(),
        done_at: if hive.status == STATUS_DONE { hive.done_at.or(report.map(|r| r.at_ms)) } else { report.map(|r| r.at_ms) },
        report_summary: report.map(|r| summary_head(&r.summary)).unwrap_or_default(),
        spent_cents: total_cents.saturating_sub(earlier),
        extra: Map::new(),
    }
}

/// The record a New goal leaves (§11.5): the old goal a chapter, the new goal, brief and rules,
/// active with nothing of Done or the pause left, and criteria to be set and confirmed again.
/// The id, folders, settings, models, cap, checkpoints, budget and slug stay. Pure.
pub(crate) fn apply_new_goal(h: &mut Hive, chapter: HiveChapter, spec: &NewGoalSpec, brief_files: Vec<String>) {
    h.chapters.push(chapter);
    h.goal = spec.goal.trim().to_string();
    h.brief = spec.brief.clone();
    h.brief_files = brief_files;
    h.instructions = spec.instructions.clone();
    h.status = STATUS_ACTIVE.to_string();
    h.done_at = None;
    h.done_stale.clear();
    h.paused_by.clear();
    h.criteria.clear();
    h.criteria_confirm = CRITERIA_NEEDED.to_string();
    h.criteria_key.clear();
}

/// `chapters/<n>/chapter.json` and copies of the goal's brief: written first, and again on a
/// retry (a crash before the record was saved leaves the Hive on its old goal). Blocks.
pub(crate) fn archive_chapter(hive: &Hive, paths: &HivePaths, n: u32, chapter: &HiveChapter, workers: &[WorkerRecord], report: Option<&FinishRecord>) -> Result<PathBuf, String> {
    let dir = paths.chapter(n);
    fs::create_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    let rows: Vec<Value> = workers
        .iter()
        .map(|w| {
            json!({
                "badge": w.badge,
                "slug": w.slug,
                "title": w.title,
                "task_id": w.task_id,
                "role": w.role,
                "phase": w.phase,
                "branches": w.folders.iter().filter_map(|f| f.branch.as_ref().map(|b| format!("{}: {b}", f.name))).collect::<Vec<_>>(),
                "last_report": w.last_report,
            })
        })
        .collect();
    let record = json!({
        "v": 1,
        "n": n,
        "chapter": chapter,
        "goal": hive.goal,
        "brief": hive.brief,
        "brief_files": hive.brief_files,
        "instructions": hive.instructions,
        "criteria": hive.criteria,
        "criteria_confirm": hive.criteria_confirm,
        "status": hive.status,
        "workers": rows,
        "report": report.filter(|r| report_in_chapter(hive, r)),
    });
    super::launch::write_json(&dir.join("chapter.json"), &record)?;
    let copies = dir.join(super::create::BRIEF_FILES_DIR);
    for name in &hive.brief_files {
        let src = brief_dir(paths).join(name);
        if !fs::symlink_metadata(&src).is_ok_and(|m| m.is_file()) {
            continue;
        }
        fs::create_dir_all(&copies).map_err(|e| format!("{}: {e}", copies.display()))?;
        fs::copy(&src, copies.join(name)).map_err(|e| format!("Couldn't keep a copy of {}: {e}", src.display()))?;
    }
    let brief_md = paths.coordinator_brief();
    if brief_md.is_file() {
        fs::copy(&brief_md, dir.join("brief.md")).map_err(|e| format!("Couldn't keep a copy of {}: {e}", brief_md.display()))?;
    }
    Ok(dir)
}

/// Each wanted brief file, as `Ok(name)` when it is already one of the current goal's copies in
/// `brief/` (kept as it is), or `Err(source)` when it must be copied in.
fn sort_brief_files(paths: &HivePaths, current: &[String], wanted: &[String]) -> Vec<Result<String, String>> {
    let canon = |p: &Path| fs::canonicalize(p).map(|c| crate::share::canon(&c)).unwrap_or_else(|_| p.to_path_buf());
    let dir_c = canon(&brief_dir(paths));
    wanted
        .iter()
        .map(|raw| {
            let p = crate::share::expand_tilde(raw.trim());
            let in_brief = p.parent().is_some_and(|d| canon(d) == dir_c);
            let name = p.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
            if in_brief && current.contains(&name) && fs::symlink_metadata(&p).is_ok_and(|m| m.is_file()) {
                Ok(name)
            } else {
                Err(raw.clone())
            }
        })
        .collect()
}

/// What `copy_brief_files` would refuse, asked before anything of the New goal moves: a missing
/// path, a folder, a file over 5 MB, one with no name. Blocks.
pub(crate) fn check_brief_files(paths: &HivePaths, current: &[String], wanted: &[String]) -> Result<(), String> {
    for src in sort_brief_files(paths, current, wanted).into_iter().filter_map(Result::err) {
        let p = crate::share::expand_tilde(src.trim());
        let meta = fs::metadata(&p).map_err(|_| format!("The attached file {} isn't there.", p.display()))?;
        if !meta.is_file() {
            return Err(format!("{} isn't a file: attach files, not folders.", p.display()));
        }
        if meta.len() > super::create::MAX_BRIEF_FILE_BYTES {
            return Err(format!("{} is over 5 MB: attach a smaller file, or put it in one of the Hive's folders.", p.display()));
        }
        if p.file_name().is_none_or(|n| n.is_empty()) {
            return Err(format!("{} has no file name.", p.display()));
        }
    }
    Ok(())
}

/// The brief files the new goal has, as names in `brief/`: a path already there keeps its name,
/// any other is copied in as Create copies (`copy_brief_files`). Nothing is removed here: the
/// files no longer listed leave only once the record names the new list
/// (`remove_unlisted_brief_files`). Blocks.
pub(crate) fn copy_new_brief_files(paths: &HivePaths, current: &[String], wanted: &[String]) -> Result<Vec<String>, String> {
    let mut names: Vec<String> = Vec::new();
    for item in sort_brief_files(paths, current, wanted) {
        match item {
            Ok(name) if !names.contains(&name) => names.push(name),
            Ok(_) => {}
            Err(src) => names.extend(super::create::copy_brief_files(&[src], &paths.coordinator_home())?),
        }
    }
    Ok(names)
}

/// Files in `brief/` the record no longer names (the chapter kept a copy of each). Best effort. Blocks.
pub(crate) fn remove_unlisted_brief_files(paths: &HivePaths, names: &[String]) {
    for e in fs::read_dir(brief_dir(paths)).into_iter().flatten().flatten() {
        let name = e.file_name().to_string_lossy().to_string();
        if !names.contains(&name) {
            if let Err(err) = retire::remove_path(&e.path()) {
                log::warn!("hive: brief file {} left in place: {err}", e.path().display());
            }
        }
    }
}

/// `to`, or beside it a name no file has yet: an archive step never removes what is there.
fn free_dest(to: &Path) -> PathBuf {
    if fs::symlink_metadata(to).is_err() {
        return to.to_path_buf();
    }
    let name = to.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
    let (stem, ext) = match name.rsplit_once('.') {
        Some((s, e)) if !s.is_empty() => (s.to_string(), format!(".{e}")),
        _ => (name, String::new()),
    };
    to.with_file_name(format!("{stem}-{}{ext}", &uuid::Uuid::new_v4().simple().to_string()[..8]))
}

/// Moves `from` into `to` when it exists, beside an earlier one rather than over it. Blocks.
fn move_if_there(from: &Path, to: &Path) -> Result<(), String> {
    if fs::symlink_metadata(from).is_err() {
        return Ok(());
    }
    let to = free_dest(to);
    fs::rename(from, &to).map_err(|e| format!("Couldn't move {} to {}: {e}", from.display(), to.display()))
}

const ARCHIVED_MEMORY: &str = "memory";
const ARCHIVED_MEMORY_RECORD: &str = "memory.json";

/// A memory folder an earlier attempt set aside into this chapter goes back in place: the live
/// one is then the seed that attempt made (or what was written to it since), which moves aside
/// into the chapter as `memory.discarded-…`, never deleted. A no-op when nothing was set aside.
/// Blocks.
pub(crate) fn restore_memory(paths: &HivePaths, chapter_dir: &Path) -> Result<(), String> {
    let archived = chapter_dir.join(ARCHIVED_MEMORY);
    if fs::symlink_metadata(&archived).is_err() {
        return Ok(());
    }
    move_if_there(&paths.memory(), &chapter_dir.join("memory.discarded"))?;
    fs::rename(&archived, paths.memory()).map_err(|e| format!("Couldn't put the memory folder back from {}: {e}", archived.display()))?;
    let record = chapter_dir.join(ARCHIVED_MEMORY_RECORD);
    if fs::symlink_metadata(&record).is_ok() {
        move_if_there(&paths.memory_record(), &chapter_dir.join("memory.discarded.json"))?;
        fs::rename(&record, paths.memory_record()).map_err(|e| format!("Couldn't put the memory record back: {e}"))?;
    }
    Ok(())
}

/// What the old goal's coordinator was owed or had approved leaves with it: `owed.json` (notices
/// about the old goal's work) and `plan.json` (titles approved under "Show me the plan first"),
/// moved into the chapter; the kickoff re-armed for the fresh session. With `keep_memory` false
/// the memory folder and maiTerm's record of it go to the chapter too, and the new goal starts
/// with a seeded empty one. A retry after a failed attempt first puts back what that attempt set
/// aside (`restore_memory`), so the real memory is never the one replaced. Blocks.
pub(crate) fn set_aside(paths: &HivePaths, chapter_dir: &Path, keep_memory: bool, new_goal: &str) -> Result<(), String> {
    let task = paths.coordinator_task();
    restore_memory(paths, chapter_dir)?;
    move_if_there(&task.join(super::owed::OWED_FILE), &chapter_dir.join(super::owed::OWED_FILE))?;
    move_if_there(&paths.plan_record(), &chapter_dir.join("plan.json"))?;
    super::launch::reset_kickoff(&task)?;
    if !keep_memory {
        move_if_there(&paths.memory(), &chapter_dir.join(ARCHIVED_MEMORY))?;
        move_if_there(&paths.memory_record(), &chapter_dir.join(ARCHIVED_MEMORY_RECORD))?;
        fs::create_dir_all(paths.memory()).map_err(|e| format!("{}: {e}", paths.memory().display()))?;
        super::launch::write_bytes(&paths.memory_index(), super::layout::memory_seed(new_goal).as_bytes())?;
        let rec = super::memory::record::MemoryRecord { v: super::memory::record::RECORD_VERSION, kept: super::layout::seed_kept(new_goal), ..Default::default() };
        super::memory::record::save(paths, &rec)?;
    }
    Ok(())
}

/// Kept memory's goal line: the one maiTerm seeded for the old goal (`- Goal: …`) now names the
/// new goal, through the kept block's own Set, so agents don't read the old goal as current. A
/// block the human changed that no longer has that exact line is theirs and left alone. Best
/// effort. Blocks.
fn retitle_memory_goal(hive: &Hive, paths: &HivePaths, new_goal: &str) {
    let (Some(old_line), Some(new_line)) = (super::layout::seed_kept(&hive.goal).pop(), super::layout::seed_kept(new_goal).pop()) else { return };
    let text = fs::read_to_string(paths.memory_index()).unwrap_or_default();
    let shown = super::memory::index::split(&text).map(|s| s.kept).or_else(|| super::memory::record::load(paths).map(|r| r.kept));
    let Some(shown) = shown.filter(|s| s.contains(&old_line)) else { return };
    let lines: Vec<String> = shown.iter().map(|l| if *l == old_line { new_line.clone() } else { l.clone() }).collect();
    if let Err(e) = super::memory::kept(&hive.id, paths, super::memory::KEPT_SET, Some(lines), Some(shown)) {
        log::warn!("hive {}: memory's goal line not updated for the new goal: {e}", hive.id);
    }
}

/// The record saved under the state's lock while the Hive is still Done or paused, with the
/// pause's question to the human closed, and only then announced. A failed save puts both back.
fn save_record<W: HiveWindow>(ctx: &Ctx<'_, W>, caller: &Caller, edit: impl FnOnce(&mut Hive)) -> Result<(StatusChange, bool), String> {
    let now = crate::mailink::board::iso_now();
    let (prev, prev_tasks, prev_follow_ups, change, snapshot) = {
        let mut d = ctx.data.write();
        let label = caller.label.clone();
        let ws = super::tools::workspace_mut(&mut d, caller).map_err(|_| "This Hive is gone.".to_string())?;
        let coordinator = ws.panes.iter().flat_map(|p| p.tabs.iter()).find(|t| t.hive_role.as_deref() == Some(super::ROLE_COORDINATOR)).map(|t| t.id.clone());
        let prev_tasks = ws.tasks.clone();
        let h = ws.hive.as_mut().ok_or("This Hive is gone.")?;
        if h.status != STATUS_DONE && h.status != STATUS_PAUSED {
            return Err(format!("This Hive is {} now: only a Done or paused Hive takes a new goal.", h.status));
        }
        let prev = h.clone();
        edit(h);
        let hive = h.clone();
        let closed = super::fuses::close_pause_blocker(ws, coordinator.as_deref(), &now);
        let tasks = closed.then(|| (ws.tasks.clone(), ws.workstreams.clone()));
        // The queen's follow-ups and watch scripts go in this same write (Rust owns them,
        // docs/follow-ups.md; a delivery takes before it types, so a cleared one never goes out):
        // only once the new goal is certain, and after her agent has stopped, so none she made
        // meanwhile survives. Until then `in_progress` refuses a new one and a due one's relaunch.
        let queen_tab = coordinator.as_deref().and_then(|c| ws.panes.iter_mut().flat_map(|p| p.tabs.iter_mut()).find(|t| t.id == c));
        let prev_follow_ups = queen_tab.map(|t| std::mem::take(&mut t.follow_ups)).unwrap_or_default();
        let change = StatusChange { window_label: label, workspace_id: ws.id.clone(), hive, coordinator, tasks };
        (prev, prev_tasks, prev_follow_ups, change, d.clone())
    };
    if let Err(e) = (ctx.save)(&snapshot) {
        let mut d = ctx.data.write();
        if let Ok(ws) = super::tools::workspace_mut(&mut d, caller) {
            if ws.hive.as_ref() == Some(&change.hive) {
                ws.hive = Some(prev);
                if change.tasks.is_some() {
                    ws.tasks = prev_tasks;
                }
                let queen = change.coordinator.as_deref();
                if let Some(t) = ws.panes.iter_mut().flat_map(|p| p.tabs.iter_mut()).find(|t| Some(t.id.as_str()) == queen) {
                    t.follow_ups = prev_follow_ups;
                }
            }
        }
        return Err(format!("maiTerm couldn't save the Hive ({e}), so it keeps its old goal. Its workers are retired; try again."));
    }
    if !prev_follow_ups.is_empty() {
        log::info!("hive {}: {} follow-up(s) of the queen cleared for the new goal", caller.hive.id, prev_follow_ups.len());
    }
    Ok((change, !prev_follow_ups.is_empty()))
}

/// Hives (by folder) a New goal is changing: their queen doesn't launch meanwhile.
static IN_PROGRESS: std::sync::Mutex<Vec<PathBuf>> = std::sync::Mutex::new(Vec::new());

/// `prepare_launch`'s refusal while a New goal runs.
pub const IN_PROGRESS_REFUSAL: &str = "This Hive is taking a new goal: its queen starts once that's done.";

/// A New goal is under way in the Hive at `root`.
pub fn in_progress(root: &Path) -> bool {
    IN_PROGRESS.lock().unwrap_or_else(|e| e.into_inner()).iter().any(|r| r == root)
}

/// `tab_id` is a Hive's queen and that Hive is taking a new goal: no follow-up may be added to her
/// tab (`add_tab_follow_up`), since the record's save clears them with the old goal.
pub fn queen_follow_up_refusal(data: &crate::state::AppData, tab_id: &str) -> bool {
    let Some((ws, tab)) = super::find_hive_tab(data, tab_id) else { return false };
    tab.hive_role.as_deref() == Some(super::ROLE_COORDINATOR)
        && ws.hive.as_ref().and_then(|h| super::paths::hive_root(&h.id)).is_some_and(|root| in_progress(&root))
}

/// Held from the checks to the saved record; dropped on every way out.
pub(crate) struct InProgress(PathBuf);

impl InProgress {
    pub(crate) fn take(root: &Path) -> Self {
        IN_PROGRESS.lock().unwrap_or_else(|e| e.into_inner()).push(root.to_path_buf());
        Self(root.to_path_buf())
    }
}

impl Drop for InProgress {
    fn drop(&mut self) {
        let mut v = IN_PROGRESS.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(i) = v.iter().position(|r| *r == self.0) {
            v.remove(i);
        }
    }
}

/// The retirement every current worker gets: abandoned with its task dropped, its branches kept.
fn retire_spec() -> RetireSpec {
    use worker::*;
    RetireSpec {
        phase: PHASE_RETIRED,
        from: vec![PHASE_WAITING, PHASE_LAUNCHING, PHASE_RUNNING, PHASE_BLOCKED, PHASE_REPORTED, PHASE_FAILED, PHASE_STOPPED],
        reason: "the Hive took a new goal".to_string(),
        task_status: "dropped",
        note: "Retired: your human gave the Hive a new goal. Its branches are kept.".to_string(),
        landed: None,
        // A held land is abandoned; one under way was refused by the checks.
        held_land: true,
    }
}

/// What a New goal did beyond the record.
#[derive(Debug)]
pub(crate) struct Done {
    pub hive: Hive,
    pub retired: usize,
    /// The queen's relaunch went to the window and it answered.
    pub relaunched: bool,
}

/// Gives the caller's Hive a new goal (§11.5). The order is chosen so a crash or a failure at any
/// step leaves a state the Hive already knows, and New goal can simply be tried again:
///
/// 1. Everything checked again under the Hive's turn (a wake and the exit after Done hold it too),
///    the attached files included (what can fail on the human's input fails here, before anything
///    moves). Then no queen launch is accepted until the record is saved (`in_progress`).
/// 2. The old goal archived to `chapters/<n>/` (copies only; rewritten on a retry).
/// 3. Every current worker retired. Stopping here leaves a Done or paused Hive with fewer (or no)
///    workers, its goal unchanged: valid, and the checks still pass.
/// 4. The queen's agent stopped, her open cards failed. Stopping here: the same, with a Done or
///    paused Hive whose queen isn't running, as a Done Hive always is. Her follow-ups stay until
///    step 6 clears them in the record's own write; `in_progress` holds them off meanwhile.
/// 5. The new brief files copied in, then what the old goal's queen was owed or had approved moved
///    into the chapter, the kickoff re-armed, the memory set aside if asked. A failure from here to
///    the saved record puts the memory back; a crash leaves it in the chapter, and the retry's
///    `set_aside` puts it back before anything else. No archive step removes what it finds.
/// 6. The record saved: the chapter appended, the new goal, criteria to confirm, `active`. From
///    here every reader sees the new goal, and since the coordinator resumes only a session of the
///    current chapter, any launch from now (this one, a restart's) starts it fresh. Only now do the
///    unlisted brief files leave `brief/`.
/// 7. What the Pause held dropped, and the queen relaunched in her own tab through the builder.
pub(crate) async fn new_goal<W: HiveWindow>(ctx: &Ctx<'_, W>, caller: &Caller, spec: NewGoalSpec) -> Result<Done, String> {
    let spec = check_spec(spec)?;
    let key = Waking::key(caller);
    let _turn = Waking::wait(&key, finish::TURN_WAIT).await.ok_or("This Hive's queen is being woken or ended right now: try again in a moment.")?;

    // 1.
    let (hive, queen, st) = read_standing(ctx, caller).await?;
    if !st.reasons.is_empty() {
        return Err(st.reasons.join(" "));
    }
    if !st.unlanded.is_empty() {
        let list: Vec<String> = st.unlanded.iter().map(|u| format!("{} ({}: {}, {} not landed)", u.worker, u.folder, u.branch, u.ahead)).collect();
        return Err(format!("Commits of this goal aren't landed yet: {}. Ask the queen to land them first.", list.join("; ")));
    }
    let queen = queen.ok_or("This Hive has no queen tab to start the new goal in.")?;
    // What the attached files would refuse, before anything moves: a retry must find the old
    // goal exactly as this attempt found it.
    let (p, current, wanted) = (caller.paths.clone(), hive.brief_files.clone(), spec.brief_files.clone());
    blocking(move || check_brief_files(&p, &current, &wanted)).await?;
    // From here until the record is saved, no launch may start the queen: a follow-up's resume
    // would bring back the old goal's session (`launch::prepare_launch`).
    let marker = InProgress::take(&caller.paths.root);

    // 2.
    let n = hive.chapter() + 1;
    let (h, p, workers) = (hive.clone(), caller.paths.clone(), st.workers.clone());
    let (chapter, chapter_dir) = blocking(move || {
        let report = finish::last_finish(&p);
        let chapter = chapter_of(&h, report.as_ref(), super::gallery::budget::spent_of(&h.id, &p), &crate::mailink::board::iso_now());
        let dir = archive_chapter(&h, &p, n, &chapter, &workers, report.as_ref())?;
        Ok((chapter, dir))
    })
    .await?;

    // 3.
    let mut retired = 0;
    for w in st.workers.iter().filter(|w| !matches!(w.phase.as_str(), worker::PHASE_LANDED | worker::PHASE_RETIRED)) {
        retire::retire_worker(ctx, caller, w, retire_spec())
            .await
            .map_err(|e| format!("{} couldn't be retired ({e}), so the Hive keeps its goal. Try New goal again.", worker_name(w)))?;
        retired += 1;
    }

    // 4. Her follow-ups stay until the record is saved (cleared in the same write, `save_record`):
    // meanwhile `in_progress` refuses a due one's relaunch and any new one.
    match ctx.window.front(&queen).await {
        Front::Job => {
            match ctx.window.stop_agent(&queen).await {
                Ok(true) => retire::wait_for_exit(ctx, &queen).await,
                Ok(false) => {}
                Err(e) => return Err(format!("The queen couldn't be stopped ({e}), so the Hive keeps its goal. Try again in a moment.")),
            }
            if ctx.window.front(&queen).await == Front::Job {
                return Err("The queen is still running after maiTerm asked her to stop, so the Hive keeps its goal. Try again in a moment.".to_string());
            }
        }
        Front::Unknown => return Err("maiTerm can't tell what holds the queen's terminal, so nothing more was changed. Try again in a moment.".to_string()),
        Front::Shell | Front::NoTerminal => {}
    }
    let failed = ctx.window.fail_requests(std::slice::from_ref(&queen), "the Hive took a new goal");
    if failed > 0 {
        log::info!("hive {}: {failed} of the queen's open cards failed: the Hive took a new goal", hive.id);
    }

    // 5. No watch may follow the folder as it moves (a retry that keeps memory still puts back
    // what a crashed attempt set aside); `hive_new_goal` watches the live one again on every exit.
    super::memory::unwatch(&hive.id);
    // The new files are copied in first (nothing is removed yet), then what only the old goal
    // owned moves. Any failure from here to the saved record puts the memory back.
    let (p, dir, keep, goal, current, wanted) = (caller.paths.clone(), chapter_dir.clone(), spec.keep_memory, spec.goal.clone(), hive.brief_files.clone(), spec.brief_files.clone());
    let prepared = blocking(move || {
        let names = copy_new_brief_files(&p, &current, &wanted)?;
        set_aside(&p, &dir, keep, &goal)?;
        Ok(names)
    })
    .await;
    let saved = match prepared {
        Ok(names) => save_record(ctx, caller, |h| apply_new_goal(h, chapter, &spec, names)),
        Err(e) => Err(e),
    };
    let (change, follow_ups_cleared) = match saved {
        Ok(c) => c,
        Err(e) => {
            let (p, dir) = (caller.paths.clone(), chapter_dir.clone());
            if let Err(re) = blocking(move || restore_memory(&p, &dir)).await {
                log::warn!("hive {}: a failed New goal's set-aside memory is still in {}: {re}", hive.id, chapter_dir.display());
            }
            return Err(e);
        }
    };

    // 6. Saved: the files the record no longer names leave `brief/`, and kept memory names the goal.
    let (h, p, names, keep, goal) = (hive.clone(), caller.paths.clone(), change.hive.brief_files.clone(), spec.keep_memory, spec.goal.clone());
    if let Err(e) = blocking(move || {
        remove_unlisted_brief_files(&p, &names);
        if keep {
            retitle_memory_goal(&h, &p, &goal);
        }
        Ok(())
    })
    .await
    {
        log::warn!("hive {}: after the new goal: {e}", hive.id);
    }
    ctx.window.hive_changed(&change.workspace_id, &change.hive);
    if follow_ups_cleared {
        ctx.window.follow_ups_changed(&change.workspace_id, &queen, &[]);
    }
    if let Some((tasks, workstreams)) = &change.tasks {
        ctx.window.tasks_changed(&change.workspace_id, tasks, workstreams);
    }
    ctx.window.workers_changed(&caller.workspace_id);
    log::info!("hive {}: new goal (chapter {n} archived at {}), {retired} worker(s) retired", hive.id, chapter_dir.display());

    // 7.
    drop(marker);
    super::deliver::drop_pause_holds(&hive.id);
    let outcome = ctx.window.request(rpc::VERB_RELAUNCH, json!({ "workspace_id": caller.workspace_id, "tab_id": queen })).await;
    let relaunched = match rpc::applied(outcome) {
        Ok(_) => true,
        Err(e) => {
            // The new goal stands: the Hive view's mount, a restart or a resume launches her
            // through the builder, fresh, since no session of this goal exists.
            log::warn!("hive {}: new goal set, the queen's relaunch not confirmed: {e}", hive.id);
            false
        }
    };
    Ok(Done { hive: change.hive, retired, relaunched })
}

// ── Asking the queen to land ──

/// maiTerm's message to the queen: the pending branches, each worker named by the board (its title
/// is agent text, so escaped), and what to do. `done`: the Hive was Done, so she finishes again.
pub(crate) fn land_request_text(pending: &[PendingBranch], done: bool) -> String {
    let mut t = String::from(
        "[maiTerm] Your human wants to give this Hive a new goal, and these branches of the current goal still have commits \
         that haven't landed:\n",
    );
    for p in pending {
        let target = p.target.as_deref().map(|t| format!(" on {t}")).unwrap_or_default();
        t.push_str(&format!(
            "- {}: {}: {}, {} commit{} not{target}\n",
            frame_escape(&p.worker),
            frame_escape(&p.folder),
            frame_escape(&p.branch),
            p.ahead,
            if p.ahead == 1 { "" } else { "s" }
        ));
    }
    t.push_str("Land each with hiveLand as usual. If one shouldn't land, don't land it: tell your human which and why");
    if done {
        t.push_str(
            ", and name its worker in report.abandoned. Once nothing is left, call hiveFinish again with your report, so your \
             human can give the Hive its new goal.",
        );
    } else {
        t.push_str(
            ". Your human has the Hive paused: once these have landed they can give it its new goal. A branch that shouldn't \
             land is left out only by a Done report that abandons it.",
        );
    }
    t
}

/// The overlay's button (§11.5): wakes a Done Hive's queen (the Hive goes `active`, as any wake)
/// or reaches a paused Hive's queen past the Pause (the human's own click), relaunching her if her
/// agent isn't running, and gives her maiTerm's message listing what is pending.
pub(crate) async fn ask_queen_to_land<W: HiveWindow>(ctx: &Ctx<'_, W>, caller: &Caller) -> Result<(), String> {
    const WHAT: &str = "Hive land request";
    let hive = super::autonomy::current_hive(ctx, caller).ok_or("This Hive is gone.")?;
    let (h, p) = (hive.clone(), caller.paths.clone());
    let pending = blocking(move || {
        let workers = worker::current_workers(&h, &p);
        pending_branches(&h, &p, &workers)
    })
    .await?;
    if pending.is_empty() {
        return Err("Nothing of this goal is waiting to land.".to_string());
    }
    let done = hive.status == STATUS_DONE;
    let text = land_request_text(&pending, done);
    if done {
        return match finish::wake_with_notice(ctx, caller, WHAT, text).await {
            Ok(_) => Ok(()),
            // Woken meanwhile: ask her as an active Hive's.
            Err(e) if e == finish::NOT_DONE => Box::pin(ask_queen_to_land(ctx, caller)).await,
            Err(e) => Err(e),
        };
    }
    let queen = coordinator_tab(ctx.data, caller).ok_or("This Hive has no queen tab.")?;
    match ctx.window.front(&queen).await {
        Front::Job => match ctx.window.deliver_through_pause(&queen, text, WHAT).await {
            Sent::Delivered | Sent::Held(_) => Ok(()),
            Sent::Refused(why) => Err(format!("The queen didn't take maiTerm's message ({why}). Try again in a moment.")),
        },
        Front::Unknown => Err("maiTerm can't tell what holds the queen's terminal. Try again in a moment.".to_string()),
        Front::Shell | Front::NoTerminal => {
            // Her agent isn't running: owed to her next session start, and that start made now.
            let paths = caller.paths.clone();
            blocking(move || super::owed::owe_notice(&paths, WHAT, text)).await?;
            if super::tools::hive_suspended(ctx.data, caller) {
                return Err("This Hive's workspace is suspended: the queen gets the message when it's resumed.".to_string());
            }
            let outcome = ctx.window.request(rpc::VERB_RELAUNCH, json!({ "workspace_id": caller.workspace_id, "tab_id": queen })).await;
            rpc::applied(outcome).map(|_| ()).map_err(|e| format!("The queen couldn't be started ({e}); she gets the message at her next start."))
        }
    }
}

// ── The window's commands ──

/// Whether the Hive can take a new goal, what's in the way, and the current goal's fields to
/// prefill the sheet (§11.5). Touches nothing.
#[tauri::command]
pub async fn hive_new_goal_preview(window: tauri::Window, app: tauri::AppHandle, state: tauri::State<'_, Arc<AppState>>, workspace_id: String) -> Result<NewGoalPreview, String> {
    let caller = super::land::caller_in(state.inner(), window.label(), &workspace_id)?;
    let ctx = super::tools::app_ctx(state.inner(), &app, &caller);
    preview(&ctx, &caller).await
}

/// Gives the Hive a new goal: the human's click on the sheet, the only way in. The Hive keeps its id.
#[tauri::command]
pub async fn hive_new_goal(window: tauri::Window, app: tauri::AppHandle, state: tauri::State<'_, Arc<AppState>>, workspace_id: String, spec: NewGoalSpec) -> Result<Hive, String> {
    let caller = super::land::caller_in(state.inner(), window.label(), &workspace_id)?;
    let ctx = super::tools::app_ctx(state.inner(), &app, &caller);
    let res = new_goal(&ctx, &caller, spec).await;
    // A New goal unwatches memory before any rename (set aside, or put back on a retry): watched
    // again whatever happened, on the folder now live. A watch replaces any still there.
    super::memory::watch(state.inner(), &app, &caller.hive.id, &caller.paths);
    super::memory::emit_changed(&app, &caller.label, &caller.workspace_id);
    let done = res?;
    super::phone::bump();
    Ok(done.hive)
}

/// Asks the queen to land the current goal's pending commits (the overlay's button).
#[tauri::command]
pub async fn hive_ask_queen_to_land(window: tauri::Window, app: tauri::AppHandle, state: tauri::State<'_, Arc<AppState>>, workspace_id: String) -> Result<(), String> {
    let caller = super::land::caller_in(state.inner(), window.label(), &workspace_id)?;
    let ctx = super::tools::app_ctx(state.inner(), &app, &caller);
    ask_queen_to_land(&ctx, &caller).await
}

#[cfg(test)]
mod tests;
