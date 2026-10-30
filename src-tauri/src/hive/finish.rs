//! Done (docs/hive.md §11.4): the coordinator's completion criteria and `hiveFinish`.
//!
//! The criteria are Rust's, on the Hive record (`hiveSetCriteria`, every change logged to
//! `logs/criteria.jsonl`). Done is a tool maiTerm refuses until nothing is live, waiting, queued,
//! landing or unlanded and every criterion has a receipt maiTerm verified itself: lands from the
//! folded land log, runs from the run log, commits in the repo, files on disk, assets in the
//! gallery. A worker's own report is never a receipt here (plan D5). The report maiTerm writes
//! (`logs/finishes.jsonl`, under a folder no agent writes) carries its own facts beside the
//! coordinator's words: what landed, the final assets, the spend, what the coordinator decided
//! alone (plan D6).
//!
//! On accept the Hive goes `done`, its window stops the stack services it started and cancels its
//! tabs' follow-ups (`hive.finish`), and the coordinator is ended once its answer has reached it
//! (`exit_coordinator`). Every launch in a Done Hive is refused (`launch::DONE_REFUSAL`) until a
//! message from the human wakes it (`wake`).

mod wake;
pub use wake::{wake_text, WakeOutcome, BY_DESKTOP, BY_PHONE, NOT_DONE};
pub(crate) use wake::{done_coordinator, route_send, wake, Route};

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};

use super::deliver::one_line;
use super::fuses::{set_status_with, StatusChange, STATUS_PAUSED};
use super::land::log::{self as landlog, Folded, KIND_BATCH_MEMBER, KIND_INTEGRATION, KIND_LAND};
use super::paths::HivePaths;
use super::decisions::{self, DecisionLine, DecisionOption};
use super::record::{Hive, HiveFolder, CRITERIA_ASKED, CRITERIA_CONFIRMED, CRITERIA_NEEDED, STATUS_ACTIVE, STATUS_DONE};
use crate::state::workspace::{BlockerOption, TaskBlocker};
use super::rpc::{self, HiveWindow};
use super::tools::{blocking, find_worker, worker_label, Caller, Ctx};
use super::worker::{self, WorkerRecord};
use crate::state::Task;

pub const MAX_CRITERIA: usize = 20;
pub const MAX_CRITERION: usize = 500;
pub const MAX_SUMMARY: usize = 8_000;
pub const MAX_HOW: usize = 2_000;
pub const MAX_RECEIPTS: usize = 50;
pub const MAX_REASON: usize = 2_000;
pub const MAX_ABANDONED: usize = 100;
/// Lines of "decided alone" a report keeps; the rest are counted.
const MAX_DECIDED: usize = 200;

pub const RECORD_VERSION: u32 = 1;
pub const KIND_FINISH: &str = "finish";
pub const BY_COORDINATOR: &str = "coordinator";

/// The coordinator is ended only after its answer has reached it, and once it is idle (plan D8).
pub const EXIT_MIN_WAIT: Duration = Duration::from_secs(3);
pub const EXIT_POLL: Duration = Duration::from_secs(1);
pub const EXIT_MAX_WAIT: Duration = Duration::from_secs(120);
/// How long the exit keeps a wake waiting for the signalled agent to give its terminal back.
const EXIT_END_GRACE: Duration = Duration::from_secs(10);

// ── The record ──

/// One `hiveFinish` maiTerm accepted: the coordinator's words and maiTerm's own facts.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct FinishRecord {
    pub v: u32,
    /// `finish`; the same log carries 9c's wake lines.
    pub kind: String,
    /// `F-<8hex>`.
    pub id: String,
    pub at: String,
    pub at_ms: i64,
    /// The coordinator's, quoted.
    pub summary: String,
    pub criteria: Vec<FinishCriterion>,
    /// From the folded land log.
    pub landed: Vec<FinishLanded>,
    pub abandoned: Vec<FinishAbandoned>,
    /// Assets whose status is final, out of the trash.
    pub assets_final: Vec<FinalAsset>,
    /// The spend ledger's total, integer micro-dollars (its own unit, §10.3).
    pub spent_micros: i64,
    /// The coordinator's own decisions, from the decision log.
    pub decided_alone: Vec<String>,
    pub by: String,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

impl Default for FinishRecord {
    fn default() -> Self {
        Self {
            v: RECORD_VERSION,
            kind: KIND_FINISH.to_string(),
            id: String::new(),
            at: String::new(),
            at_ms: 0,
            summary: String::new(),
            criteria: Vec::new(),
            landed: Vec::new(),
            abandoned: Vec::new(),
            assets_final: Vec::new(),
            spent_micros: 0,
            decided_alone: Vec::new(),
            by: BY_COORDINATOR.to_string(),
            extra: Map::new(),
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct FinishCriterion {
    /// From 1, as the Hive's list numbers it.
    pub n: u32,
    pub text: String,
    /// The coordinator's, quoted.
    pub how: String,
    /// maiTerm's description of each receipt it verified.
    pub receipts: Vec<String>,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct FinishLanded {
    /// The land log line's id, `L-…/<folder>`.
    pub land: String,
    pub worker: String,
    pub title: String,
    pub folder: String,
    pub target: String,
    pub base_after: String,
    pub undone: bool,
    pub suspect: bool,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct FinishAbandoned {
    /// `#3 api`.
    pub worker: String,
    /// What stays unlanded, by maiTerm's count: `web: hive/ship--api, 2 commits not on main`.
    pub branches: Vec<String>,
    /// The coordinator's, quoted.
    pub reason: String,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct FinalAsset {
    pub id: String,
    pub title: String,
    pub kind: String,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

pub fn new_id() -> String {
    format!("F-{}", &uuid::Uuid::new_v4().simple().to_string()[..8])
}

/// The newest finish line, or None. Blocks on the disk.
pub fn last_finish(paths: &HivePaths) -> Option<FinishRecord> {
    let text = std::fs::read_to_string(paths.finishes()).ok()?;
    text.lines()
        .rev()
        .filter_map(|l| serde_json::from_str::<Value>(l).ok())
        .find(|v| v.get("kind").and_then(Value::as_str) == Some(KIND_FINISH))
        .and_then(|v| serde_json::from_value(v).ok())
}

/// Whether the newest report relied on land log line `land` (its `landed` names it, not already
/// undone then).
pub fn report_relies_on(rec: &FinishRecord, land: &str) -> bool {
    rec.landed.iter().any(|l| l.land == land && !l.undone)
}

/// A land a Done Hive's report relied on was undone (the human's Undo; a Done coordinator's own
/// is refused): Done stands, but the header says it is stale. Nothing wakes the coordinator: the
/// undo's result waits for its next session (`owed::owe_if_refused`), when the human wakes it.
pub(crate) async fn undone_after_done<W: HiveWindow>(ctx: &Ctx<'_, W>, caller: &Caller, land: &str) {
    let done = hive_in(&ctx.data.read(), &caller.hive.id).is_some_and(|h| h.status == STATUS_DONE);
    if !done {
        return;
    }
    let (paths, line) = (caller.paths.clone(), land.to_string());
    let relied = blocking(move || Ok(last_finish(&paths).is_some_and(|r| report_relies_on(&r, &line)))).await.unwrap_or(false);
    if !relied {
        return;
    }
    let line = land.to_string();
    match write_status(ctx, caller, &[STATUS_DONE], None, move |h| h.done_stale = line) {
        Ok(_) => log::info!("hive {}: Done is stale: land {land}, which its report relied on, was undone", caller.hive.id),
        Err(e) => log::info!("hive {}: Done not marked stale after undoing {land}: {e}", caller.hive.id),
    }
}

// ── Writing the record ──

/// The Hive's record edited under the state's lock while its status is one of `from`, moved to
/// `to` (kept when None), saved, and only then announced to its window. A failed save puts the
/// record back as it was, so memory never says Done when the disk doesn't.
pub(crate) fn write_status<W: HiveWindow>(ctx: &Ctx<'_, W>, caller: &Caller, from: &[&str], to: Option<&str>, edit: impl FnOnce(&mut Hive)) -> Result<Hive, String> {
    let change = save_status(ctx, caller, from, to, edit)?;
    ctx.window.hive_changed(&change.workspace_id, &change.hive);
    Ok(change.hive)
}

/// `write_status` without the announcement, for a change that may yet be taken back.
fn save_status<W: HiveWindow>(ctx: &Ctx<'_, W>, caller: &Caller, from: &[&str], to: Option<&str>, edit: impl FnOnce(&mut Hive)) -> Result<StatusChange, String> {
    let id = caller.hive.id.as_str();
    let (prev, change, snapshot) = {
        let mut d = ctx.data.write();
        let prev = hive_in(&d, id).ok_or("This Hive is gone.")?;
        let c = set_status_with(&mut d, id, from, to, edit).ok_or_else(|| format!("This Hive is {} now.", prev.status))?;
        (prev, c, d.clone())
    };
    if let Err(e) = (ctx.save)(&snapshot) {
        let mut d = ctx.data.write();
        if let Some(h) = d.windows.iter_mut().flat_map(|w| w.workspaces.iter_mut()).filter_map(|ws| ws.hive.as_mut()).find(|h| h.id == id) {
            // Only our own write is taken back: a change made since stands.
            if *h == change.hive {
                *h = prev;
            }
        }
        return Err(format!("maiTerm couldn't save the Hive ({e}), so nothing changed."));
    }
    Ok(change)
}

fn hive_in(d: &crate::state::AppData, id: &str) -> Option<Hive> {
    d.windows.iter().flat_map(|w| w.workspaces.iter()).filter_map(|ws| ws.hive.as_ref()).find(|h| h.id == id).cloned()
}

fn current_hive<W: HiveWindow>(ctx: &Ctx<'_, W>, caller: &Caller) -> Result<Hive, String> {
    super::autonomy::current_hive(ctx, caller).ok_or_else(|| "This Hive is gone.".to_string())
}

// ── hiveSetCriteria ──

/// The criteria and the coordinator's reason, checked: 1 to 20, each one line of 1 to 500
/// characters, none repeated.
pub fn parse_criteria(args: &Value) -> Result<(Vec<String>, Option<String>), String> {
    let list = args.get("criteria").and_then(Value::as_array).ok_or("criteria is required: a list of checkable statements, one per criterion.")?;
    if list.is_empty() || list.len() > MAX_CRITERIA {
        return Err(format!("Set 1 to {MAX_CRITERIA} criteria."));
    }
    let mut out: Vec<String> = Vec::new();
    for (i, v) in list.iter().enumerate() {
        let n = i + 1;
        let raw = v.as_str().ok_or_else(|| format!("Criterion {n} isn't text."))?;
        if raw.contains(['\n', '\r', '\u{2028}', '\u{2029}', '\u{85}']) {
            return Err(format!("Criterion {n} spans lines: write each as one line."));
        }
        let c = raw.trim();
        if c.is_empty() {
            return Err(format!("Criterion {n} is empty."));
        }
        if c.chars().count() > MAX_CRITERION {
            return Err(format!("Criterion {n} is over {MAX_CRITERION} characters: make it a checkable statement, and split it if it holds two."));
        }
        if out.iter().any(|o| o == c) {
            return Err(format!("Criterion {n} repeats an earlier one."));
        }
        out.push(c.to_string());
    }
    let reason = args.get("reason").and_then(Value::as_str).map(str::trim).filter(|r| !r.is_empty()).map(|r| one_line(r, MAX_REASON));
    Ok((out, reason))
}

pub fn set_criteria_tool_definition(tab_id: &Value) -> Value {
    json!({
        "name": "hiveSetCriteria",
        "description": format!(
            "Set this Hive's completion criteria: what must be true for the goal to be met, each one a checkable statement \
             (a land, a passing run, a commit, a file, an asset proves it). maiTerm keeps the list on the Hive, numbered, \
             shows it to your human in the Hive header, and logs every change. Call it at the start, and again only as your \
             human agrees: the list replaces the old one whole. In a new Hive, maiTerm asks your human to confirm the list \
             and no worker starts until they do. hiveFinish addresses each criterion by its number. 1 to \
             {MAX_CRITERIA} criteria, one line each, at most {MAX_CRITERION} characters. Refused once the Hive is done."
        ),
        "inputSchema": {
            "type": "object",
            "properties": {
                "tabId": tab_id,
                "criteria": { "type": "array", "items": { "type": "string" }, "description": "The whole list, in order" },
                "reason": { "type": "string", "description": "Why it changed (your human asked, the goal was clarified)" }
            },
            "required": ["criteria"]
        }
    })
}

/// `hiveSetCriteria` (§11.4): the list on the record, then a line in `logs/criteria.jsonl`.
pub(crate) async fn set_criteria_tool<W: HiveWindow>(ctx: &Ctx<'_, W>, caller: &Caller, args: &Value) -> Result<Value, String> {
    // Never while a hiveFinish is checked: Done would be saved over criteria its report never
    // addressed. The same turn `finish_tool` takes, by folder.
    let _held = Finishing::take(&caller.paths.root.to_string_lossy()).ok_or("A hiveFinish of this Hive is being checked: set the criteria once it has answered.")?;
    if current_hive(ctx, caller)?.status == STATUS_DONE {
        return Err("This Hive is done: its criteria stand as it finished. A message from your human wakes it.".to_string());
    }
    let (criteria, reason) = parse_criteria(args)?;
    let list = criteria.clone();
    let hive = write_status(ctx, caller, &[STATUS_ACTIVE, STATUS_PAUSED], None, move |h| h.criteria = list)?;
    let line = json!({ "v": RECORD_VERSION, "at": crate::mailink::board::iso_now(), "criteria": criteria, "reason": reason, "by": BY_COORDINATOR });
    let path = caller.paths.criteria_log();
    if let Err(e) = blocking(move || super::queue::append_line(&path, &line)).await {
        log::warn!("hive {}: criteria set but not logged: {e}", caller.hive.id);
    }
    log::info!("hive {}: {} completion criteria set", caller.hive.id, hive.criteria.len());
    if hive.criteria_unconfirmed() {
        ask_criteria(ctx, caller, &hive).await?;
        return Ok(json!({
            "criteria": numbered(&hive.criteria),
            "confirmed": false,
            "note": format!(
                "Your human confirms these before any worker starts: maiTerm asks them on your \"{CRITERIA_TASK_TITLE}\" row \
                 ({CRITERIA_CONFIRM}, or {CRITERIA_CHANGE} with what to change), and their answer is typed to you. Meanwhile \
                 keep planning. If they ask for changes, call hiveSetCriteria again with the new list."
            ),
        }));
    }
    Ok(json!({
        "criteria": numbered(&hive.criteria),
        "note": "hiveFinish addresses each criterion by its number. Your human sees this list in the Hive header.",
    }))
}

// ── Confirming the criteria before work starts (§11.4) ──

/// The coordinator's row the criteria question waits on.
pub const CRITERIA_TASK_TITLE: &str = "Completion criteria for this Hive";
pub const CRITERIA_CONFIRM: &str = "Confirm";
pub const CRITERIA_CHANGE: &str = "Change them";
/// The question's decision-log source and category.
pub const SOURCE_CRITERIA: &str = "criteria";

/// What a spawn hears while the human hasn't confirmed the criteria. A reviewer never does: it
/// reviews work already started.
/// `tasks` is the workspace's rows: a question dismissed, cleared or deleted without an answer
/// leaves nothing to wait for, so the coordinator is told to ask again rather than to wait.
pub fn criteria_spawn_refusal(hive: &Hive, tasks: &[Task]) -> Option<&'static str> {
    match hive.criteria_confirm.as_str() {
        CRITERIA_NEEDED => Some(
            "Your human confirms this Hive's completion criteria before any worker starts: set them with hiveSetCriteria, \
             and spawn once they confirm.",
        ),
        CRITERIA_ASKED if !criteria_question_waiting(hive, tasks) => Some(
            "Your question on the completion criteria is no longer waiting: it was cleared without an answer. If your human \
             told you their answer another way, ask again with hiveSetCriteria; no worker starts until they confirm on the row.",
        ),
        CRITERIA_ASKED => Some(
            "Your human hasn't confirmed the completion criteria yet: their answer comes on your \u{201c}Completion criteria \
             for this Hive\u{201d} row. Keep planning meanwhile, and spawn once they confirm.",
        ),
        _ => None,
    }
}

/// The question `criteria_key` names still stands on its row.
fn criteria_question_waiting(hive: &Hive, tasks: &[Task]) -> bool {
    tasks.iter().any(|t| t.blocker.as_ref().is_some_and(|b| super::prompts::blocker_key(&t.id, &b.asked_at) == hive.criteria_key))
}

/// The human's question, in maiTerm's words: the criteria as the coordinator set them.
pub fn criteria_blocker(goal: &str, criteria: &[String], asked_at: &str) -> TaskBlocker {
    let mut context: Vec<String> = criteria.iter().enumerate().map(|(i, c)| format!("{}. {}", i + 1, one_line(c, MAX_CRITERION))).collect();
    context.push("No worker starts until you confirm them.".to_string());
    TaskBlocker {
        kind: "decision".into(),
        question: format!("Are these the completion criteria for \u{201c}{}\u{201d}?", one_line(goal, 160)),
        context: Some(context.join("\n")),
        options: vec![
            BlockerOption { label: CRITERIA_CONFIRM.into(), detail: Some("the queen plans her workers against these".into()), recommended: true },
            BlockerOption { label: CRITERIA_CHANGE.into(), detail: Some("answer with what to change".into()), recommended: false },
        ],
        command: None,
        asked_at: asked_at.into(),
        asked_by: "agent".into(),
    }
}

/// Asks the human to confirm the criteria just set, on the coordinator's row: a newer list
/// replaces a question still waiting, and only the newest question's answer counts.
async fn ask_criteria<W: HiveWindow>(ctx: &Ctx<'_, W>, caller: &Caller, hive: &Hive) -> Result<(), String> {
    let coordinator = super::tools::coordinator_tab(ctx.data, caller).ok_or("This Hive has no coordinator tab to ask on.")?;
    let now = crate::mailink::board::iso_now();
    let blocker = criteria_blocker(&hive.goal, &hive.criteria, &now);
    let note = format!("Needs you: {}", blocker.question);
    let (task_id, asked_at, tasks, workstreams) = {
        let mut d = ctx.data.write();
        let ws = super::tools::workspace_mut(&mut d, caller)?;
        let (task_id, asked_at) = super::fuses::file_row_blocker(ws, Some(&coordinator), CRITERIA_TASK_TITLE, blocker.clone(), &note, &now);
        (task_id, asked_at, ws.tasks.clone(), ws.workstreams.clone())
    };
    let key = super::prompts::blocker_key(&task_id, &asked_at);
    let k = key.clone();
    // Saves the row with it: both are in the state this write saves.
    write_status(ctx, caller, &[STATUS_ACTIVE, STATUS_PAUSED], None, move |h| {
        h.criteria_confirm = CRITERIA_ASKED.to_string();
        h.criteria_key = k;
    })?;
    let line = DecisionLine {
        id: decisions::new_id(),
        hive_id: hive.id.clone(),
        category: SOURCE_CRITERIA.to_string(),
        source: SOURCE_CRITERIA.to_string(),
        task_id: Some(task_id),
        key: Some(key),
        question: one_line(&blocker.question, 400),
        context: blocker.context.as_deref().map(|c| one_line(c, 1000)),
        options: blocker.options.iter().map(|o| DecisionOption { label: o.label.clone(), recommended: o.recommended }).collect(),
        recommended: Some(CRITERIA_CONFIRM.to_string()),
        at: asked_at,
        ..DecisionLine::default()
    };
    let paths = caller.paths.clone();
    let _ = blocking(move || {
        decisions::record(&paths, &line);
        Ok(())
    })
    .await;
    ctx.window.tasks_changed(&caller.workspace_id, &tasks, &workstreams);
    ctx.window.decisions_changed(&caller.workspace_id);
    Ok(())
}

/// The human answered the criteria question `key`: Confirm lets workers start, anything else
/// sends the coordinator back to hiveSetCriteria (their words reach it with the answer). A
/// superseded question's answer changes nothing.
pub fn criteria_answer(hive: &mut Hive, key: &str, answer: &str) -> bool {
    if hive.criteria_confirm != CRITERIA_ASKED || hive.criteria_key != key {
        return false;
    }
    hive.criteria_confirm = if answer.trim().eq_ignore_ascii_case(CRITERIA_CONFIRM) { CRITERIA_CONFIRMED } else { CRITERIA_NEEDED }.to_string();
    hive.criteria_key.clear();
    true
}

pub(crate) fn on_criteria_answered<W: HiveWindow>(ctx: &Ctx<'_, W>, caller: &Caller, key: &str, answer: &str) {
    let (key, answer) = (key.to_string(), answer.to_string());
    let mut changed = false;
    let result = write_status(ctx, caller, &[STATUS_ACTIVE, STATUS_PAUSED], None, |h| changed = criteria_answer(h, &key, &answer));
    match result {
        Ok(h) if changed => log::info!("hive {}: completion criteria {} by the human", caller.hive.id, if h.criteria_unconfirmed() { "sent back" } else { "confirmed" }),
        Ok(_) => {}
        Err(e) => log::warn!("hive {}: the human's answer on the criteria not recorded: {e}", caller.hive.id),
    }
}

fn numbered(criteria: &[String]) -> Vec<Value> {
    criteria.iter().enumerate().map(|(i, c)| json!({ "n": i + 1, "text": c })).collect()
}

/// `hiveListWorkers`'s criteria.
pub fn criteria_view(hive: &Hive) -> Vec<Value> {
    numbered(&hive.criteria)
}

// ── hiveFinish's arguments ──

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Receipts {
    pub lands: Vec<String>,
    pub checks: Vec<String>,
    pub commits: Vec<String>,
    pub files: Vec<String>,
    pub assets: Vec<String>,
}

impl Receipts {
    fn len(&self) -> usize {
        self.lands.len() + self.checks.len() + self.commits.len() + self.files.len() + self.assets.len()
    }
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct CriterionArg {
    pub n: usize,
    pub how: String,
    pub receipts: Receipts,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct AbandonArg {
    pub worker: String,
    pub reason: String,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct FinishArgs {
    pub summary: String,
    pub criteria: Vec<CriterionArg>,
    pub abandoned: Vec<AbandonArg>,
}

fn text(o: &Map<String, Value>, key: &str, what: &str, max: usize) -> Result<String, String> {
    let s = o.get(key).and_then(Value::as_str).map(str::trim).unwrap_or_default();
    if s.is_empty() {
        return Err(format!("{what} is required."));
    }
    if s.chars().count() > max {
        return Err(format!("{what} is over {max} characters."));
    }
    Ok(s.to_string())
}

/// The report's shape and caps (§11.4 step 3), before anything is looked up. Indexes are checked
/// against the criteria later, with the receipts.
pub fn parse_finish(args: &Value) -> Result<FinishArgs, String> {
    let report = args.get("report").and_then(Value::as_object).ok_or("report is required: {summary, criteria: [{criterion, how, receipts}], abandoned?}.")?;
    let summary = text(report, "summary", "report.summary", MAX_SUMMARY)?;
    let list = report.get("criteria").and_then(Value::as_array).ok_or("report.criteria is required: one entry per criterion, each with its receipts.")?;
    if list.len() > MAX_CRITERIA {
        return Err(format!("report.criteria has more entries than a Hive has criteria ({MAX_CRITERIA})."));
    }
    let mut criteria = Vec::new();
    for (i, c) in list.iter().enumerate() {
        let at = format!("report.criteria[{i}]");
        let o = c.as_object().ok_or_else(|| format!("{at} must be {{criterion, how, receipts}}."))?;
        let n = o.get("criterion").and_then(Value::as_u64).filter(|n| *n >= 1).ok_or_else(|| format!("{at}.criterion must be the criterion's number, from 1."))? as usize;
        let how = text(o, "how", &format!("{at}.how"), MAX_HOW)?;
        let r = o.get("receipts").and_then(Value::as_object).ok_or_else(|| format!("{at}.receipts is required: {{lands?, checks?, commits?, files?, assets?}}."))?;
        let list = |key: &str| -> Result<Vec<String>, String> {
            match r.get(key) {
                None | Some(Value::Null) => Ok(Vec::new()),
                Some(Value::Array(a)) => a
                    .iter()
                    .map(|v| v.as_str().map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).ok_or_else(|| format!("{at}.receipts.{key} must be a list of strings.")))
                    .collect(),
                Some(_) => Err(format!("{at}.receipts.{key} must be a list.")),
            }
        };
        if let Some(k) = r.keys().find(|k| !["lands", "checks", "commits", "files", "assets"].contains(&k.as_str())) {
            return Err(format!("{at}.receipts.{} isn't a receipt kind: lands, checks, commits, files or assets.", one_line(k, 40)));
        }
        let receipts = Receipts { lands: list("lands")?, checks: list("checks")?, commits: list("commits")?, files: list("files")?, assets: list("assets")? };
        if receipts.len() > MAX_RECEIPTS {
            return Err(format!("{at} has over {MAX_RECEIPTS} receipts: name the ones that prove it."));
        }
        criteria.push(CriterionArg { n, how, receipts });
    }
    let mut abandoned = Vec::new();
    match report.get("abandoned") {
        None | Some(Value::Null) => {}
        Some(Value::Array(a)) => {
            if a.len() > MAX_ABANDONED {
                return Err(format!("report.abandoned names over {MAX_ABANDONED} workers."));
            }
            for (i, v) in a.iter().enumerate() {
                let at = format!("report.abandoned[{i}]");
                let o = v.as_object().ok_or_else(|| format!("{at} must be {{worker, reason}}."))?;
                let worker = match o.get("worker") {
                    Some(Value::String(s)) if !s.trim().is_empty() => s.trim().to_string(),
                    Some(Value::Number(n)) => format!("#{n}"),
                    _ => return Err(format!("{at}.worker is required: its badge, name or task id.")),
                };
                abandoned.push(AbandonArg { worker, reason: text(o, "reason", &format!("{at}.reason"), MAX_REASON)? });
            }
        }
        Some(_) => return Err("report.abandoned must be a list of {worker, reason}.".to_string()),
    }
    Ok(FinishArgs { summary, criteria, abandoned })
}

// ── What Done is checked against ──

/// What the state and the queue hold, gathered off the disk.
#[derive(Debug, Clone)]
pub(crate) struct Standing {
    pub hive: Hive,
    pub tasks: Vec<Task>,
    /// The machine-wide queue's active jobs in this workspace.
    pub jobs: Vec<super::queue::Job>,
    /// Agent requests from the coordinator's tab waiting for the human.
    pub cards: usize,
}

/// Commits on a worker's branch its landing target doesn't have, counted in the repo.
#[derive(Debug, Clone, PartialEq)]
pub struct Unlanded {
    pub folder: String,
    pub branch: String,
    pub target: String,
    pub ahead: u64,
}

/// Every git read Done makes is a verifying one (`git::verifying`): no commit-graph, graft or
/// replace ref a worker planted decides what landed or what is left.
fn git_query(repo: &Path, args: &[&str]) -> Result<String, String> {
    super::git::run(super::git::verify_in(repo).args(args), super::git::QUERY_TIMEOUT)
}

/// A walk from a receipt to what landed can be long (an old commit, a busy Base).
const WALK_TIMEOUT: Duration = Duration::from_secs(120);

/// Every branch of `w` with commits not on its landing target (§11.4), counted in the Hive
/// folder's repo, never the worktree, which may be gone. A branch the human deleted counts as
/// nothing unlanded; a repo maiTerm can't read is an error, never "deleted". Blocks on git.
pub fn unlanded(hive: &Hive, w: &WorkerRecord) -> Result<Vec<Unlanded>, String> {
    let mut out = Vec::new();
    if w.is_reviewer() {
        return Ok(out);
    }
    for f in &w.folders {
        let Some(branch) = f.branch.as_deref() else { continue };
        let Some(folder) = hive.folders.iter().find(|h| h.name == f.name && h.is_git()) else {
            log::warn!("hive {}: worker {}'s folder {} is no longer the Hive's: not counted", hive.id, w.slug, f.name);
            continue;
        };
        let repo = Path::new(&folder.path);
        let branch_ref = format!("refs/heads/{branch}");
        // for-each-ref answers an absent ref with nothing and exit 0, so any failure (the folder
        // gone, not a repo, a timeout) stays a failure. Its pattern also matches `<branch>/…`.
        let refs = git_query(repo, &["for-each-ref", "--format=%(refname)", &branch_ref])
            .map_err(|e| format!("maiTerm couldn't read {} ({}) to count {}'s unlanded work on {branch}: {e}", f.name, folder.path, worker_label(w)))?;
        if !refs.lines().any(|l| l.trim() == branch_ref) {
            continue;
        }
        let (base, target) = match (&f.target, &f.base_sha) {
            (Some(t), _) => (format!("refs/heads/{t}"), t.clone()),
            (None, Some(sha)) => (sha.clone(), short(sha).to_string()),
            (None, None) => return Err(format!("maiTerm can't tell what {}'s branch {branch} lands on, so it can't count its unlanded work.", worker_label(w))),
        };
        let n = git_query(repo, &["rev-list", "--count", &format!("{base}..{branch_ref}")])
            .map_err(|e| format!("maiTerm couldn't count {}'s commits on {branch} in {}: {e}", worker_label(w), f.name))?;
        let ahead: u64 = n.trim().parse().unwrap_or(0);
        if ahead > 0 {
            out.push(Unlanded { folder: f.name.clone(), branch: branch.to_string(), target, ahead });
        }
    }
    Ok(out)
}

fn short(sha: &str) -> &str {
    &sha[..sha.len().min(7)]
}

fn plural(n: u64, one: &str, many: &str) -> String {
    format!("{n} {}", if n == 1 { one } else { many })
}

fn unlanded_line(u: &Unlanded) -> String {
    format!("{}: {}, {} not on {}", u.folder, u.branch, plural(u.ahead, "commit", "commits"), u.target)
}

/// A git folder's landing target: the integration branch, else its Base. A detached Base has none.
fn landing_target(hive: &Hive, f: &HiveFolder) -> Result<String, String> {
    if !f.is_git() {
        return Err(format!("{} isn't a git folder: nothing lands there.", f.name));
    }
    if hive.is_integration() {
        return Ok(hive.integration_branch());
    }
    f.base.clone().filter(|b| !b.is_empty()).ok_or_else(|| format!("{}'s Base is a detached HEAD, so no commit can be on it.", f.name))
}

fn is_ancestor(repo: &Path, sha: &str, target: &str) -> bool {
    git_query(repo, &["merge-base", "--is-ancestor", sha, &format!("refs/heads/{target}")]).is_ok()
}

/// `sha` is, or is an ancestor of, a commit maiTerm's own land log records as landed in `f` (a
/// land, batch member or integration land, neither undone nor suspect), and every commit on the
/// way there hashes to its id. Git's word alone is never a receipt: a worker writes the shared
/// object store, and a loose object it overwrote reads as real on git's ancestry path (plan D3).
/// `what` names the receipt in a refusal. Blocks on git and the land log's fold.
fn landed_per_record(folded: &Folded, f: &HiveFolder, sha: &str, what: &str) -> Result<(), String> {
    let repo = Path::new(&f.path);
    let sha = sha.to_ascii_lowercase();
    let recorded: Vec<&str> = folded
        .entries
        .iter()
        .filter(|e| e.entry.folder == f.name && is_landed_kind(&e.entry.kind) && e.undone_by.is_none() && !e.suspect && !e.entry.base_after.is_empty())
        .map(|e| e.entry.base_after.as_str())
        .collect();
    // Oldest first: the first landed commit that holds it is the shortest walk.
    let held = recorded.iter().find(|b| b.eq_ignore_ascii_case(&sha) || git_query(repo, &["merge-base", "--is-ancestor", &sha, b]).is_ok());
    let Some(landed) = held else {
        return Err(format!("{what} isn't in anything maiTerm's land log records as landed in {}: only what landed proves a criterion.", f.name));
    };
    // Git only picked the candidate; the proof is maiTerm's own walk down from it, hashing each
    // commit before following its parents (no range git computes is trusted, plan D3).
    use super::git::Ancestry;
    match super::git::verified_ancestry(repo, landed, &sha, WALK_TIMEOUT) {
        Ok(Ancestry::Reached) => Ok(()),
        Ok(Ancestry::NotReached) => {
            log::warn!("hive: {} ({}): git says {sha} is under {landed}, the hashed objects say it isn't", f.name, f.path);
            Err(format!("{what} isn't in anything maiTerm's land log records as landed in {}: only what landed proves a criterion.", f.name))
        }
        Ok(Ancestry::Unverified(b)) => {
            log::warn!("hive: {} ({}) has a commit object that doesn't hash to its id: {b}", f.name, f.path);
            Err(objects_refusal(what, f, &format!("commit {} doesn't hash to its id", short(&b))))
        }
        Ok(Ancestry::TooLong) => Err(format!(
            "{what} is too far back from what landed in {} for maiTerm to verify (over {} commits): name a newer one.",
            f.name,
            super::git::WALK_CAP
        )),
        Err(e) => Err(format!("maiTerm couldn't walk from {what} to what landed in {}: {e}", f.name)),
    }
}

fn objects_refusal(what: &str, f: &HiveFolder, why: &str) -> String {
    format!(
        "{what} can't be verified: {}'s objects don't verify ({why}), so git's history there can't be trusted. Your human should run `git fsck` in {}.",
        f.name, f.path
    )
}

/// Undos running or waiting on the human, as `U-… (state)`. Blocks on the disk.
fn undos_under_way(paths: &HivePaths) -> Vec<String> {
    super::land::undo::read_all(paths)
        .into_iter()
        .filter(|u| u.in_progress() || u.state == super::land::STATE_AWAITING_HUMAN)
        .map(|u| format!("{} ({})", u.id, u.state))
        .collect()
}

/// Why Done is refused (§11.4, in the plan's order), or what the report proves: each criterion
/// with maiTerm's description of its receipts, and what is abandoned. Blocks on git, the logs
/// and the gallery's index.
pub(crate) fn check(s: &Standing, paths: &HivePaths, args: &Value) -> Result<(FinishArgs, Vec<FinishCriterion>, Vec<FinishAbandoned>), String> {
    let hive = &s.hive;
    match hive.status.as_str() {
        STATUS_DONE => return Err("This Hive is already done.".to_string()),
        STATUS_PAUSED => return Err(format!("Resume first: this Hive is paused ({}).", super::fuses::paused_why(hive))),
        _ => {}
    }
    if hive.criteria.is_empty() {
        return Err("This Hive has no completion criteria: set them with hiveSetCriteria first, as your human agrees.".to_string());
    }
    let a = parse_finish(args)?;
    let workers = worker::list_workers(paths);
    let busy: Vec<String> = workers.iter().filter(|w| w.is_live() || w.phase == worker::PHASE_WAITING).map(|w| format!("{} ({})", worker_label(w), w.phase)).collect();
    if !busy.is_empty() {
        return Err(format!(
            "Workers are still in this Hive's work: {}. Stop, land or hiveRetireWorker each first (a landing worker: let its land finish, or retire a held one); a waiting worker never launches once the Hive is done.",
            busy.join(", ")
        ));
    }
    if !s.jobs.is_empty() {
        let names: Vec<String> = s.jobs.iter().map(|j| format!("{} ({} for {}, {})", j.id, one_line(&j.argv.join(" "), 60), j.worker_slug, j.state)).collect();
        return Err(format!("Runs are still queued or running: {}. Wait for them to end.", names.join(", ")));
    }
    // Only one under way: a held one has no way to be withdrawn, and the coordinator may keep the
    // land it was undoing. One waiting on the human is a card, below.
    let undos = undos_under_way(paths);
    if !undos.is_empty() {
        return Err(format!("An undo is under way: {}. Let it finish.", undos.join(", ")));
    }
    if s.cards > 0 {
        return Err(format!("{} for your human: a land or an undo waits on their answer. Done waits for it.", plural(s.cards as u64, "A card waits", "cards wait")));
    }
    let open: Vec<String> = s
        .tasks
        .iter()
        .filter(|t| t.status == "blocked" && t.blocker.as_ref().is_some_and(|b| b.kind == "decision" || b.kind == "action"))
        .map(|t| format!("\u{201c}{}\u{201d}", one_line(&t.title, 80)))
        .collect();
    if !open.is_empty() {
        return Err(format!(
            "Your human has a question open on {}: Done must not leave one unanswered. Wait for their answer, or withdraw your own question.",
            open.join(", ")
        ));
    }

    // Unlanded work, against what the report says it abandons.
    let mut named: HashMap<String, &AbandonArg> = HashMap::new();
    for ab in &a.abandoned {
        let w = find_worker(&workers, &ab.worker).ok_or_else(|| format!("report.abandoned names \"{}\", which is no worker of this Hive.", one_line(&ab.worker, 60)))?;
        if named.insert(w.task_id.clone(), ab).is_some() {
            return Err(format!("report.abandoned names {} twice.", worker_label(w)));
        }
    }
    let mut kept = Vec::new();
    let mut abandoned = Vec::new();
    for w in &workers {
        let left = unlanded(hive, w)?;
        match (named.get(&w.task_id), left.is_empty()) {
            (Some(ab), false) => abandoned.push(FinishAbandoned {
                worker: worker_label(w),
                branches: left.iter().map(unlanded_line).collect(),
                reason: ab.reason.clone(),
                ..FinishAbandoned::default()
            }),
            (Some(_), true) => {
                return Err(format!("report.abandoned names {}, which has nothing unlanded: the report must be true. Leave it out.", worker_label(w)));
            }
            (None, false) => kept.extend(left.iter().map(|u| format!("{} {}", worker_label(w), unlanded_line(u)))),
            (None, true) => {}
        }
    }
    if !kept.is_empty() {
        return Err(format!("Unlanded work: {}. Land it, or name the worker in report.abandoned with why (its branch is kept).", kept.join("; ")));
    }

    // Every criterion once, each with receipts that verify.
    let mut seen = HashSet::new();
    for c in &a.criteria {
        if c.n > hive.criteria.len() {
            return Err(format!("report.criteria names criterion {}, and this Hive has {} (hiveListWorkers lists them).", c.n, hive.criteria.len()));
        }
        if !seen.insert(c.n) {
            return Err(format!("report.criteria addresses criterion {} twice.", c.n));
        }
    }
    if let Some(missing) = (1..=hive.criteria.len()).find(|n| !seen.contains(n)) {
        return Err(format!("report.criteria doesn't address criterion {missing} (\u{201c}{}\u{201d}): every one needs its receipts.", one_line(&hive.criteria[missing - 1], 120)));
    }
    let folded = landlog::folded(paths);
    let gallery = super::gallery::view::existing(&hive.id, paths);
    let mut out = Vec::new();
    let mut by_n: Vec<&CriterionArg> = a.criteria.iter().collect();
    by_n.sort_by_key(|c| c.n);
    for c in by_n {
        if c.receipts.len() == 0 {
            return Err(format!("Criterion {} has no receipt: name the land, run, commit, file or asset that proves it.", c.n));
        }
        let fail = |e: String| format!("Criterion {}: {e}", c.n);
        let mut lines = Vec::new();
        for l in &c.receipts.lands {
            lines.push(verify_land(&folded, l).map_err(fail)?);
        }
        for j in &c.receipts.checks {
            lines.push(verify_check(hive, paths, &folded, j).map_err(fail)?);
        }
        for sha in &c.receipts.commits {
            lines.push(verify_commit(hive, &folded, sha).map_err(fail)?);
        }
        for f in &c.receipts.files {
            lines.push(verify_file(hive, paths, f).map_err(fail)?);
        }
        for id in &c.receipts.assets {
            lines.push(verify_asset(gallery.as_deref(), id).map_err(fail)?);
        }
        lines.dedup();
        out.push(FinishCriterion { n: c.n as u32, text: hive.criteria[c.n - 1].clone(), how: c.how.clone(), receipts: lines, ..FinishCriterion::default() });
    }
    Ok((a, out, abandoned))
}

// ── Receipts, as maiTerm verifies them (plan D5) ──

fn is_landed_kind(kind: &str) -> bool {
    matches!(kind, KIND_LAND | KIND_BATCH_MEMBER | KIND_INTEGRATION)
}

/// `L-…/<folder>`, or `L-…` for every repo of that land: in the land log, landed, neither undone
/// nor suspect.
fn verify_land(folded: &Folded, name: &str) -> Result<String, String> {
    let shown = one_line(name, 60);
    let exact: Vec<_> = folded.entries.iter().filter(|e| e.entry.id == name && is_landed_kind(&e.entry.kind)).collect();
    let hits = if exact.is_empty() { folded.entries.iter().filter(|e| e.entry.land == name && is_landed_kind(&e.entry.kind)).collect() } else { exact };
    if hits.is_empty() {
        return Err(format!("land {shown} isn't a land in this Hive's land log."));
    }
    let mut lines = Vec::new();
    for e in hits {
        if let Some(by) = &e.undone_by {
            return Err(format!("land {} was undone ({by}): it proves nothing now.", e.entry.id));
        }
        if e.suspect {
            return Err(format!("land {} is suspect: its target failed its land check after it. Fix forward and name that land.", e.entry.id));
        }
        lines.push(format!("land {}: \u{201c}{}\u{201d} into {}, at {}", e.entry.id, one_line(&e.entry.title, 80), e.entry.target, short(&e.entry.base_after)));
    }
    Ok(lines.join("; "))
}

/// A `hiveRun` job from maiTerm's run log that passed, whose commit has reached its folder's
/// landing target and is under a land maiTerm recorded (`landed_per_record`): it checked work
/// that landed.
fn verify_check(hive: &Hive, paths: &HivePaths, folded: &Folded, job: &str) -> Result<String, String> {
    let shown = one_line(job, 40);
    if !super::queue::is_job_id(job) {
        return Err(format!("\"{shown}\" isn't a run (a hiveRun job id is j- and 8 hex digits)."));
    }
    let r = super::queue::find_run(paths, job).ok_or_else(|| format!("run {shown} has no record in this Hive's run log."))?;
    if !super::run::is_pass(&r) {
        let why = if r.service_id.is_some() {
            "a service copy, which tests nothing".to_string()
        } else if r.dirty {
            "it ran on uncommitted changes".to_string()
        } else {
            format!("{}, exit {}", r.state, r.exit_code.map_or("unknown".to_string(), |c| c.to_string()))
        };
        return Err(format!("run {shown} didn't pass ({why})."));
    }
    let commit = r.commit.clone().ok_or_else(|| format!("run {shown} has no commit on record."))?;
    let f = hive.folders.iter().find(|f| f.name == r.folder).ok_or_else(|| format!("run {shown} ran in {}, which is no folder of this Hive.", r.folder))?;
    let target = landing_target(hive, f)?;
    if !is_ancestor(Path::new(&f.path), &commit, &target) {
        return Err(format!("run {shown} tested {}, which isn't on {target} in {}: it checked work that didn't land.", short(&commit), f.name));
    }
    landed_per_record(folded, f, &commit, &format!("run {shown}'s commit {}", short(&commit)))?;
    Ok(format!("check {job}: `{}` passed on {} in {}, on {target}", one_line(&r.entry, 80), short(&commit), f.name))
}

/// `folder:<40-hex sha>`, a commit on that folder's landing target, under a land maiTerm
/// recorded (`landed_per_record`): git and maiTerm's record both, never git alone.
fn verify_commit(hive: &Hive, folded: &Folded, receipt: &str) -> Result<String, String> {
    let shown = one_line(receipt, 80);
    let (folder, sha) = receipt.split_once(':').ok_or_else(|| format!("commit \"{shown}\" must be folder:<40-hex sha>."))?;
    if sha.len() != 40 || !sha.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err(format!("commit \"{shown}\" must be folder:<40-hex sha> (the full sha)."));
    }
    let f = hive.folders.iter().find(|f| f.name == folder).ok_or_else(|| format!("commit \"{shown}\" names no folder of this Hive."))?;
    let target = landing_target(hive, f)?;
    let repo = Path::new(&f.path);
    if git_query(repo, &["cat-file", "-e", &format!("{sha}^{{commit}}")]).is_err() {
        return Err(format!("commit {} isn't in {}.", short(sha), f.name));
    }
    if !is_ancestor(repo, sha, &target) {
        return Err(format!("commit {} isn't on {target} in {}: only what landed proves a criterion.", short(sha), f.name));
    }
    landed_per_record(folded, f, sha, &format!("commit {}", short(sha)))?;
    Ok(format!("commit {folder}:{} on {target}", short(sha)))
}

/// `p` with its symlinks resolved as far as it exists and the rest appended as written; None when
/// nothing of it exists or the rest climbs (`..`).
fn resolve(p: &Path) -> Option<PathBuf> {
    let mut tail: Vec<std::ffi::OsString> = Vec::new();
    let mut cur = p.to_path_buf();
    loop {
        if let Ok(r) = std::fs::canonicalize(&cur) {
            let mut out = crate::share::canon(&r);
            out.extend(tail.iter().rev());
            return Some(out);
        }
        tail.push(cur.file_name()?.to_os_string());
        if !cur.pop() {
            return None;
        }
    }
}

/// A file receipt (plan D4), symlinks resolved: never inside a `.git` or maiTerm's own folders
/// (the Hive's, but for the gallery's stored assets), so nothing a worker wrote into git's
/// internals or a Hive agent into its corner is a receipt. In a git folder it is a file at the
/// landing target, so it names landed work, not whatever sits in the human's checkout; in a plain
/// folder or the gallery, a file on disk. A relative path is read from the primary folder.
fn verify_file(hive: &Hive, paths: &HivePaths, f: &str) -> Result<String, String> {
    let shown = one_line(f, 200);
    let p = Path::new(f);
    let p = if p.is_absolute() {
        p.to_path_buf()
    } else {
        hive.primary().map(|h| Path::new(&h.path).join(p)).ok_or_else(|| format!("file {shown}: give an absolute path."))?
    };
    let real = resolve(&p).ok_or_else(|| format!("file {shown} doesn't exist."))?;
    if real.components().any(|c| c.as_os_str().to_string_lossy().eq_ignore_ascii_case(".git")) {
        return Err(format!("file {shown} is inside a .git folder: git's own files prove nothing."));
    }
    let ours = |p: PathBuf| resolve(&p).unwrap_or(p);
    if real.starts_with(ours(paths.worktrees())) {
        // The read copies show the landing target, but the receipt names the folder itself.
        if let Some((h, rel)) = hive.git_folders().find_map(|h| real.strip_prefix(ours(paths.coordinator_worktree(&h.name))).ok().map(|r| (h, r.to_path_buf()))) {
            return Err(format!("file {shown} is in your read copy of {}: name it in the folder itself ({}), where maiTerm checks it landed.", h.name, Path::new(&h.path).join(rel).display()));
        }
        return Err(format!("file {shown} is in a worktree under the Hive's wt.noindex/: name the file in its Hive folder once it has landed."));
    }
    let in_gallery = real.starts_with(ours(paths.gallery_files()));
    let maiterm = [paths.root.clone(), paths.global_precedents()].into_iter().chain(super::paths::hive_bin_dir()).map(ours).find(|o| real.starts_with(o));
    if let Some(o) = maiterm.filter(|_| !in_gallery) {
        return Err(format!("file {shown} is in maiTerm's own folders ({}): only the gallery's stored assets there are receipts.", o.display()));
    }
    let on_disk = || -> Result<String, String> {
        match std::fs::metadata(&real) {
            Err(_) => Err(format!("file {shown} doesn't exist.")),
            Ok(m) if !m.is_file() => Err(format!("{shown} is a folder: name the files in it.")),
            Ok(_) => Ok(format!("file {}", real.display())),
        }
    };
    if in_gallery {
        return on_disk();
    }
    // The innermost Hive folder holding it: a repo nested in a plain folder answers for its files.
    let folder = hive.folders.iter().filter_map(|h| resolve(Path::new(&h.path)).filter(|r| real.starts_with(r)).map(|r| (h, r))).max_by_key(|(_, r)| r.components().count());
    let Some((h, root)) = folder else {
        return Err(format!("file {shown} isn't in a Hive folder or the gallery."));
    };
    if !h.is_git() {
        return on_disk();
    }
    let target = landing_target(hive, h)?;
    let rel: Vec<String> = real.strip_prefix(&root).unwrap_or(Path::new("")).components().map(|c| c.as_os_str().to_string_lossy().to_string()).collect();
    if rel.is_empty() {
        return Err(format!("{shown} is a folder: name the files in it."));
    }
    let spec = format!("refs/heads/{target}:{}", rel.join("/"));
    match git_query(Path::new(&h.path), &["cat-file", "-t", &spec]).as_deref() {
        Ok("blob") => Ok(format!("file {}, on {target}", real.display())),
        Ok("tree") => Err(format!("{shown} is a folder: name the files in it.")),
        _ if std::fs::symlink_metadata(&real).is_err() => Err(format!("file {shown} doesn't exist.")),
        _ => Err(format!("file {shown} isn't on {target} in {}: only a file that landed proves a criterion, not one uncommitted or on a branch.", h.name)),
    }
}

/// An asset in this Hive's gallery, out of the trash, not rejected.
fn verify_asset(g: Option<&super::gallery::Gallery>, id: &str) -> Result<String, String> {
    let shown = one_line(id, 40);
    if !super::gallery::model::is_asset_id(id) {
        return Err(format!("\"{shown}\" isn't a gallery asset id (a- and 12 hex digits)."));
    }
    let a = g.and_then(|g| g.get(id).ok().flatten()).ok_or_else(|| format!("asset {shown} isn't in this Hive's gallery."))?;
    if a.in_trash() {
        return Err(format!("asset {shown} is in the gallery's trash."));
    }
    if a.status == super::gallery::model::STATUS_REJECTED {
        return Err(format!("asset {shown} is rejected."));
    }
    Ok(format!("asset {}: \u{201c}{}\u{201d} ({})", a.id, one_line(&a.title, 80), a.kind))
}

// ── maiTerm's own facts (plan D6) ──

fn landed(folded: &Folded) -> Vec<FinishLanded> {
    folded
        .entries
        .iter()
        .filter(|e| is_landed_kind(&e.entry.kind))
        .map(|e| FinishLanded {
            land: e.entry.id.clone(),
            worker: e.entry.worker.clone(),
            title: e.entry.title.clone(),
            folder: e.entry.folder.clone(),
            target: e.entry.target.clone(),
            base_after: e.entry.base_after.clone(),
            undone: e.undone_by.is_some(),
            suspect: e.suspect,
            ..FinishLanded::default()
        })
        .collect()
}

fn decided_alone(paths: &HivePaths) -> Vec<String> {
    use super::decisions;
    let lines = decisions::read(paths);
    let mut mine: Vec<decisions::Decision> = decisions::fold(&lines)
        .into_iter()
        .filter(|d| d.answered_by() == Some(decisions::BY_COORDINATOR) && d.category() != decisions::CATEGORY_OFFER)
        .collect();
    mine.sort_by_key(|d| d.last);
    let n = mine.len();
    let mut out: Vec<String> = mine
        .iter()
        .take(MAX_DECIDED)
        .map(|d| {
            let answer = d.answered.as_ref().and_then(|a| a.answer.as_deref()).unwrap_or_default();
            let mut s = format!("{}: {} \u{2192} {}", d.category(), one_line(d.question(), 200), one_line(answer, 120));
            if let Some(p) = d.precedent() {
                s.push_str(&format!(" (by precedent {p})"));
            }
            if d.overridden.is_some() {
                s.push_str(" \u{2014} overridden by your human");
            }
            s
        })
        .collect();
    if n > MAX_DECIDED {
        out.push(format!("and {} more", n - MAX_DECIDED));
    }
    out
}

fn assets_final(g: Option<&super::gallery::Gallery>) -> Vec<FinalAsset> {
    let Some(g) = g else { return Vec::new() };
    let f = super::gallery::model::ListFilter { status: Some(super::gallery::model::STATUS_FINAL.to_string()), limit: Some(1000), ..Default::default() };
    match g.list(&f) {
        Ok(rows) => rows.into_iter().map(|r| FinalAsset { id: r.id, title: r.title, kind: r.kind, ..FinalAsset::default() }).collect(),
        Err(e) => {
            log::warn!("hive: final assets not read for the report: {e}");
            Vec::new()
        }
    }
}

/// The record maiTerm keeps of an accepted finish. Blocks on the logs and the gallery's index.
pub(crate) fn compose(hive: &Hive, paths: &HivePaths, a: &FinishArgs, criteria: Vec<FinishCriterion>, abandoned: Vec<FinishAbandoned>, now_ms: i64) -> FinishRecord {
    let gallery = super::gallery::view::existing(&hive.id, paths);
    FinishRecord {
        id: new_id(),
        at: crate::mailink::board::iso_now(),
        at_ms: now_ms,
        summary: a.summary.clone(),
        criteria,
        landed: landed(&landlog::folded(paths)),
        abandoned,
        assets_final: assets_final(gallery.as_deref()),
        spent_micros: super::gallery::budget::spent_in(gallery.as_deref(), &hive.id),
        decided_alone: decided_alone(paths),
        ..FinishRecord::default()
    }
}

// ── hiveFinish ──

/// One finish checked at a time per Hive: two at once would both pass and write two reports.
static FINISHING: Mutex<Vec<String>> = Mutex::new(Vec::new());

struct Finishing(String);

impl Finishing {
    fn take(hive_id: &str) -> Option<Self> {
        let mut f = FINISHING.lock().unwrap_or_else(|e| e.into_inner());
        if f.iter().any(|h| h == hive_id) {
            return None;
        }
        f.push(hive_id.to_string());
        Some(Self(hive_id.to_string()))
    }
}

impl Drop for Finishing {
    fn drop(&mut self) {
        FINISHING.lock().unwrap_or_else(|e| e.into_inner()).retain(|h| h != &self.0);
    }
}

pub fn finish_tool_definition(tab_id: &Value) -> Value {
    json!({
        "name": "hiveFinish",
        "description": format!(
            "Finish this Hive: Done is this tool, never a word in the chat. maiTerm refuses it while any worker is live or \
             waiting, a run is queued or running, a land or undo is under way or waits on your human, your human has a \
             question open, a worker has commits not on its landing target (unless report.abandoned names it, with why), or \
             any criterion lacks a receipt maiTerm can verify. Receipts are what reached the target: lands (L-… or \
             L-…/folder, not undone or suspect), checks (a passing hiveRun job whose commit landed), commits \
             (folder:<40-hex sha> on the folder's landing target, in a land maiTerm recorded), files (in a git folder, a \
             file on its landing target, named by its path in the folder, not your read copy; in a plain folder or the \
             gallery's stored assets, a file on disk; never under .git) and gallery assets. A worker's own report is no receipt. maiTerm adds what landed, the final assets, the spend and \
             what you decided alone, shows your human the report, stops the stack services this Hive started, cancels every \
             Hive tab's follow-ups and ends your session once you are idle. A message from your human wakes the Hive. \
             summary at most {MAX_SUMMARY} characters, how {MAX_HOW}, {MAX_RECEIPTS} receipts per criterion."
        ),
        "inputSchema": {
            "type": "object",
            "properties": {
                "tabId": tab_id,
                "report": {
                    "type": "object",
                    "properties": {
                        "summary": { "type": "string", "description": "What was done, for your human" },
                        "criteria": {
                            "type": "array",
                            "items": {
                                "type": "object",
                                "properties": {
                                    "criterion": { "type": "number", "description": "Its number in the Hive's list" },
                                    "how": { "type": "string", "description": "How it is met" },
                                    "receipts": {
                                        "type": "object",
                                        "properties": {
                                            "lands": { "type": "array", "items": { "type": "string" } },
                                            "checks": { "type": "array", "items": { "type": "string" } },
                                            "commits": { "type": "array", "items": { "type": "string" }, "description": "folder:<40-hex sha>" },
                                            "files": { "type": "array", "items": { "type": "string" } },
                                            "assets": { "type": "array", "items": { "type": "string" } }
                                        }
                                    }
                                },
                                "required": ["criterion", "how", "receipts"]
                            },
                            "description": "Every criterion exactly once"
                        },
                        "abandoned": {
                            "type": "array",
                            "items": {
                                "type": "object",
                                "properties": {
                                    "worker": { "type": ["string", "number"], "description": "Badge, name or task id" },
                                    "reason": { "type": "string" }
                                },
                                "required": ["worker", "reason"]
                            },
                            "description": "Workers whose unlanded commits you leave on their branches"
                        }
                    },
                    "required": ["summary", "criteria"]
                }
            },
            "required": ["report"]
        }
    })
}

/// `hiveFinish` (§11.4): checked, recorded, Done, then the window's half. The caller ends the
/// coordinator afterwards (`exit_coordinator`), never inside this call.
pub(crate) async fn finish_tool<W: HiveWindow>(ctx: &Ctx<'_, W>, caller: &Caller, args: &Value) -> Result<Value, String> {
    // By folder, not id: a test's scratch Hives share ids.
    let _held = Finishing::take(&caller.paths.root.to_string_lossy()).ok_or("A hiveFinish of this Hive is already being checked.")?;
    let hive = current_hive(ctx, caller)?;
    let tasks = super::tools::workspace_tasks(ctx.data, caller)?;
    let cards = super::tools::coordinator_tab(ctx.data, caller).map_or(0, |t| ctx.window.requests_waiting(&t));
    let standing = Standing { hive: hive.clone(), tasks, jobs: super::queue::active_in(&caller.workspace_id), cards };
    let (paths, a) = (caller.paths.clone(), args.clone());
    let now = super::fuses::now_ms();
    let record = blocking(move || {
        let (a, criteria, abandoned) = check(&standing, &paths, &a)?;
        let rec = compose(&standing.hive, &paths, &a, criteria, abandoned, now);
        let v = serde_json::to_value(&rec).map_err(|e| e.to_string())?;
        super::queue::append_line(&paths.finishes(), &v).map_err(|e| format!("maiTerm couldn't write the report ({e}), so the Hive isn't done."))?;
        Ok(rec)
    })
    .await?;
    // The line stays if this fails: the report shows only while the status is done. Saved, not yet
    // announced: the window hears Done (and the human its notice) only once it is final.
    let done = save_status(ctx, caller, &[STATUS_ACTIVE], Some(STATUS_DONE), |h| {
        h.done_at = Some(now);
        h.done_stale.clear();
    })?;
    // Work a tool call racing this one started after `check` read, and the status gate let in
    // because it ran first: a spawn registers before it reserves, an undo writes its record
    // before it reads the status again (land/undo.rs `start`), a job queues. Any of it takes Done
    // back.
    let paths = caller.paths.clone();
    let mut late: Vec<String> = blocking(move || {
        let workers = worker::list_workers(&paths).iter().filter(|w| w.is_live() || w.phase == worker::PHASE_WAITING).map(worker_label).collect::<Vec<_>>();
        Ok(workers.into_iter().chain(undos_under_way(&paths).into_iter().map(|u| format!("undo {u}"))).collect())
    })
    .await?;
    late.extend(super::queue::active_in(&caller.workspace_id).iter().map(|j| format!("run {}", j.id)));
    if !late.is_empty() {
        if let Err(e) = save_status(ctx, caller, &[STATUS_DONE], Some(STATUS_ACTIVE), |h| h.done_at = None) {
            log::warn!("hive {}: Done not taken back after {} started: {e}", caller.hive.id, late.join(", "));
            // It stands, so the window must hear it.
            ctx.window.hive_changed(&done.workspace_id, &done.hive);
            return Err(format!("{} started while this was checked, and maiTerm couldn't take Done back: {e}", late.join(", ")));
        }
        return Err(format!("{} started while this was checked: the Hive isn't done.", late.join(", ")));
    }
    ctx.window.hive_changed(&done.workspace_id, &done.hive);
    log::info!("hive {}: done ({}, {} criteria)", caller.hive.id, record.id, record.criteria.len());
    let tab_ids = hive_tab_ids(ctx, caller);
    let outcome = ctx.window.request(rpc::VERB_FINISH, json!({ "workspace_id": caller.workspace_id, "tab_ids": tab_ids })).await;
    let stack = match rpc::applied(outcome) {
        Ok(v) => v,
        Err(e) => {
            log::warn!("hive {}: the window's half of Done didn't happen: {e}", caller.hive.id);
            json!({ "ok": false, "error": e })
        }
    };
    Ok(json!({
        "done": true,
        "finish": record.id,
        "stack": stack,
        "note": "maiTerm ends your session in a moment. A message from your human wakes this Hive.",
    }))
}

fn hive_tab_ids<W: HiveWindow>(ctx: &Ctx<'_, W>, caller: &Caller) -> Vec<String> {
    let d = ctx.data.read();
    d.window(&caller.label)
        .and_then(|w| w.workspaces.iter().find(|ws| ws.id == caller.workspace_id))
        .map(|ws| ws.panes.iter().flat_map(|p| p.tabs.iter()).filter(|t| t.hive_role.is_some()).map(|t| t.id.clone()).collect())
        .unwrap_or_default()
}

/// Whether the coordinator is ended now: only while the Hive is still done (a wake cancels it),
/// once it is idle or has had its time.
pub fn should_exit(status: &str, busy: bool, waited_out: bool) -> bool {
    status == STATUS_DONE && (!busy || waited_out)
}

/// After `hiveFinish` answered: the coordinator's agent is signalled once idle, the tab and its
/// shell kept (plan D8). Whether a signal went.
pub(crate) async fn exit_coordinator<W: HiveWindow>(ctx: &Ctx<'_, W>, caller: &Caller, min_wait: Duration, poll: Duration, max_wait: Duration) -> bool {
    tokio::time::sleep(min_wait).await;
    let start = std::time::Instant::now();
    let key = caller.paths.root.to_string_lossy().to_string();
    loop {
        // Each read and the signal under the Hive's turn: a wake holds it from its status flip
        // through its delivery (or its rollback), so neither half of one is ever seen alone.
        let Some(_turn) = wake::Waking::take(&key) else {
            tokio::time::sleep(poll).await;
            continue;
        };
        let status = super::autonomy::current_hive(ctx, caller).map(|h| h.status).unwrap_or_default();
        if status != STATUS_DONE {
            log::info!("hive {}: the coordinator stays: the Hive is {status} again", caller.hive.id);
            return false;
        }
        let Some(tab) = super::tools::coordinator_tab(ctx.data, caller) else { return false };
        if should_exit(&status, ctx.window.agent_busy(&tab), start.elapsed() >= max_wait) {
            return match ctx.window.stop_agent(&tab).await {
                Ok(sent) => {
                    log::info!("hive {}: coordinator {}", caller.hive.id, if sent { "ended after Done" } else { "had already exited" });
                    if sent {
                        // A wake waiting on the turn then finds the shell, not a dying agent.
                        wake::hold_until_ended(ctx, caller, &tab, EXIT_END_GRACE.min(max_wait)).await;
                    }
                    sent
                }
                Err(e) => {
                    log::warn!("hive {}: coordinator not ended after Done: {e}", caller.hive.id);
                    false
                }
            };
        }
        drop(_turn);
        tokio::time::sleep(poll).await;
    }
}

/// The last report, for the Hive header's Report (plan D7).
#[tauri::command]
pub async fn hive_finish_report(window: tauri::Window, state: tauri::State<'_, std::sync::Arc<crate::state::AppState>>, workspace_id: String) -> Result<Option<FinishRecord>, String> {
    let caller = super::land::caller_in(&state, window.label(), &workspace_id)?;
    blocking(move || Ok(last_finish(&caller.paths))).await
}

#[cfg(test)]
mod tests;
