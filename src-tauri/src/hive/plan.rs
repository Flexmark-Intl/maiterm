//! "Show me the plan first" (docs/hive.md §7.4, plan D20): while the Plan checkpoint is Ask me,
//! the coordinator proposes its workers with `hiveProposePlan`, maiTerm asks the human with a
//! decision blocker on the coordinator's "Plan for this Hive" row, and `hiveSpawnWorker` starts
//! only a title the human approved, once each. The record is Rust's, `tasks/coordinator/plan.json`
//! (on every agent's deny lists), so no agent approves its own plan; the approval reaches it only
//! through the human's blocker answer (`hive_blocker_answered`), which no MCP call reaches.
//!
//! Reviewers, restarts and waiting launches are exempt: the plan names the work, not its review
//! cycle, and a worker already registered was planned when it was.

use std::sync::{Mutex, MutexGuard};

use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};

use super::checkpoints::{self, PLAN};
use super::decisions::{self, DecisionLine, DecisionOption};
use super::deliver::one_line;
use super::paths::HivePaths;
use super::rpc::HiveWindow;
use super::tools::{blocking, Caller, Ctx, MAX_TITLE};
use crate::state::workspace::{BlockerOption, TaskBlocker};
use crate::state::Task;

pub const PLAN_VERSION: u32 = 1;
/// The coordinator's row the plan's question waits on.
pub const PLAN_TASK_TITLE: &str = "Plan for this Hive";
pub const STATUS_PROPOSED: &str = "proposed";
pub const STATUS_APPROVED: &str = "approved";
pub const STATUS_DECLINED: &str = "declined";
pub const ANSWER_GO: &str = "Go ahead";
pub const ANSWER_CHANGE: &str = "Change it";
pub const SOURCE_PLAN: &str = "plan";
pub const MAX_SUMMARY: usize = 2000;
pub const MAX_WORKERS: usize = 20;

/// What a spawn hears while the checkpoint holds and no approved title matches.
pub const SPAWN_REFUSAL: &str =
    "Your human sees the plan first (the Plan checkpoint is Ask me): propose it with hiveProposePlan, then spawn once they approve.";

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct PlanRecord {
    pub v: u32,
    /// `PL-<8hex>`.
    pub id: String,
    /// `proposed` | `approved` | `declined`.
    pub status: String,
    pub summary: String,
    /// The workers' titles as proposed.
    pub titles: Vec<String>,
    /// Each title's role, empty where none was named.
    pub roles: Vec<String>,
    /// Normalized titles a spawn has taken (`Task::normalize_title`).
    pub used: Vec<String>,
    /// The row and blocker key the human answers on.
    pub task_id: String,
    pub key: String,
    pub decision_id: String,
    pub asked_at: String,
    pub decided_at: Option<String>,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

impl Default for PlanRecord {
    fn default() -> Self {
        Self {
            v: PLAN_VERSION,
            id: String::new(),
            status: STATUS_PROPOSED.to_string(),
            summary: String::new(),
            titles: Vec::new(),
            roles: Vec::new(),
            used: Vec::new(),
            task_id: String::new(),
            key: String::new(),
            decision_id: String::new(),
            asked_at: String::new(),
            decided_at: None,
            extra: Map::new(),
        }
    }
}

pub fn new_id() -> String {
    format!("PL-{}", &uuid::Uuid::new_v4().simple().to_string()[..8])
}

/// One reader-writer of the record at a time: a claim reads and writes it back whole. Taken under
/// the spawn lock, never the other way round.
static RECORD: Mutex<()> = Mutex::new(());

fn lock() -> MutexGuard<'static, ()> {
    RECORD.lock().unwrap_or_else(|e| e.into_inner())
}

pub fn read(paths: &HivePaths) -> Option<PlanRecord> {
    serde_json::from_slice(&std::fs::read(paths.plan_record()).ok()?).ok()
}

pub fn write(paths: &HivePaths, rec: &PlanRecord) -> Result<(), String> {
    let v = serde_json::to_value(rec).map_err(|e| e.to_string())?;
    super::launch::write_json(&paths.plan_record(), &v)
}

/// A spawn's title taken from the approved plan, once (§7.4): the plan's id and the normalized
/// title, for `release` when the spawn rolls back. Called under the spawn lock.
pub fn claim(paths: &HivePaths, title: &str) -> Result<(String, String), String> {
    let _held = lock();
    let Some(mut rec) = read(paths) else { return Err(SPAWN_REFUSAL.to_string()) };
    match rec.status.as_str() {
        STATUS_APPROVED => {}
        STATUS_PROPOSED => return Err(format!("{SPAWN_REFUSAL} Your plan {} is waiting for their answer.", rec.id)),
        _ => return Err(format!("{SPAWN_REFUSAL} They didn\u{2019}t approve plan {}: propose a changed one.", rec.id)),
    }
    let t = Task::normalize_title(title);
    if !rec.titles.iter().any(|x| Task::normalize_title(x) == t) {
        return Err(format!(
            "\u{201c}{}\u{201d} isn\u{2019}t in the plan your human approved ({}): spawn one of its titles as written, or propose a new plan with hiveProposePlan.",
            one_line(title, MAX_TITLE),
            rec.titles.iter().map(|x| one_line(x, MAX_TITLE)).collect::<Vec<_>>().join("; ")
        ));
    }
    if rec.used.contains(&t) {
        return Err(format!("\u{201c}{}\u{201d} was already started from the approved plan: each title starts once. Propose a new plan for more.", one_line(title, MAX_TITLE)));
    }
    rec.used.push(t.clone());
    write(paths, &rec)?;
    Ok((rec.id, t))
}

/// A claim given back by a spawn that rolled back: the title may start again. Only on the plan it
/// was claimed from.
pub fn release(paths: &HivePaths, plan_id: &str, normalized: &str) {
    let _held = lock();
    let Some(mut rec) = read(paths).filter(|r| r.id == plan_id) else { return };
    let before = rec.used.len();
    rec.used.retain(|t| t != normalized);
    if rec.used.len() != before {
        if let Err(e) = write(paths, &rec) {
            log::warn!("hive: plan {plan_id}: title \u{201c}{normalized}\u{201d} not freed after a rolled-back spawn: {e}");
        }
    }
}

/// The human answered the plan's question (`key`): `Go ahead` approves it, anything else declines
/// it. None when the key isn't the open proposal's (a superseded one, or already answered).
pub fn on_answered(paths: &HivePaths, key: &str, answer: &str, now: &str) -> Result<Option<PlanRecord>, String> {
    let _held = lock();
    let Some(mut rec) = read(paths).filter(|r| r.key == key && r.status == STATUS_PROPOSED) else { return Ok(None) };
    rec.status = if answer.trim().eq_ignore_ascii_case(ANSWER_GO) { STATUS_APPROVED } else { STATUS_DECLINED }.to_string();
    rec.decided_at = Some(now.to_string());
    write(paths, &rec)?;
    Ok(Some(rec))
}

/// `hiveProposePlan`'s arguments: the summary and each worker's title (and role), checked.
#[derive(Debug, Clone, PartialEq)]
pub struct ProposeArgs {
    pub summary: String,
    pub titles: Vec<String>,
    pub roles: Vec<String>,
}

pub fn parse_args(args: &Value) -> Result<ProposeArgs, String> {
    let summary = args.get("summary").and_then(Value::as_str).map(str::trim).unwrap_or_default();
    if summary.is_empty() {
        return Err("summary is required: what the plan does and why these workers, in a few sentences.".to_string());
    }
    if summary.chars().count() > MAX_SUMMARY {
        return Err(format!("Keep the summary under {MAX_SUMMARY} characters."));
    }
    let workers = args.get("workers").and_then(Value::as_array).ok_or("workers is required: [{title, role?}], one per worker you mean to start.")?;
    if workers.is_empty() || workers.len() > MAX_WORKERS {
        return Err(format!("A plan names 1 to {MAX_WORKERS} workers."));
    }
    let (mut titles, mut roles, mut seen) = (Vec::new(), Vec::new(), Vec::new());
    for (i, w) in workers.iter().enumerate() {
        let (title, role) = match w {
            Value::String(s) => (s.as_str(), None),
            Value::Object(o) => (o.get("title").and_then(Value::as_str).unwrap_or_default(), o.get("role").and_then(Value::as_str).map(str::trim).filter(|r| !r.is_empty())),
            _ => return Err(format!("Worker {} needs a title.", i + 1)),
        };
        let title: String = title.split_whitespace().collect::<Vec<_>>().join(" ");
        if title.is_empty() {
            return Err(format!("Worker {} needs a title.", i + 1));
        }
        if title.chars().count() > MAX_TITLE {
            return Err(format!("Worker {}\u{2019}s title is too long: keep it under {MAX_TITLE} characters (it names the branch).", i + 1));
        }
        super::worker::worker_slug(&title)?;
        if let Some(r) = role.filter(|r| super::roles::role(r).is_none()) {
            return Err(format!("There\u{2019}s no role \u{201c}{}\u{201d} (worker {}): pick one from the catalogue in your brief.", one_line(r, 40), i + 1));
        }
        let n = Task::normalize_title(&title);
        if seen.contains(&n) {
            return Err(format!("\u{201c}{}\u{201d} is named twice: each worker\u{2019}s title is its own.", one_line(&title, MAX_TITLE)));
        }
        seen.push(n);
        titles.push(title);
        roles.push(role.unwrap_or_default().to_string());
    }
    Ok(ProposeArgs { summary: summary.to_string(), titles, roles })
}

/// The human's question, in maiTerm's words: the workers, the coordinator's summary as its own.
pub fn plan_blocker(goal: &str, a: &ProposeArgs, asked_at: &str) -> TaskBlocker {
    let n = a.titles.len();
    let workers = if n == 1 { "this worker".to_string() } else { format!("these {n} workers") };
    let mut context: Vec<String> = a
        .titles
        .iter()
        .zip(&a.roles)
        .enumerate()
        .map(|(i, (t, r))| if r.is_empty() { format!("{}. {}", i + 1, one_line(t, MAX_TITLE)) } else { format!("{}. {} ({r})", i + 1, one_line(t, MAX_TITLE)) })
        .collect();
    context.push(format!("The queen\u{2019}s summary: \u{201c}{}\u{201d}", one_line(&a.summary, MAX_SUMMARY)));
    TaskBlocker {
        kind: "decision".into(),
        question: format!("Start {workers} for \u{201c}{}\u{201d}?", one_line(goal, 160)),
        context: Some(context.join("\n")),
        options: vec![
            BlockerOption { label: ANSWER_GO.into(), detail: Some("the queen starts exactly these".into()), recommended: true },
            BlockerOption { label: ANSWER_CHANGE.into(), detail: Some("answer with what to change".into()), recommended: false },
        ],
        command: None,
        asked_at: asked_at.into(),
        asked_by: "agent".into(),
    }
}

pub fn propose_tool_definition(tab_id: &Value) -> Value {
    json!({
        "name": "hiveProposePlan",
        "description": format!(
            "Show your human the plan before you start workers, when the Plan checkpoint is Ask me (your brief says). maiTerm asks \
             them with a decision on your \"{PLAN_TASK_TITLE}\" row: Go ahead, or Change it with what to change. Once they approve, \
             hiveSpawnWorker starts each listed title once, as written; any other title is refused until a new plan is approved. \
             A new proposal replaces one still waiting. With the checkpoint at Coordinator it answers needed: false and the plan \
             is yours. 1 to {MAX_WORKERS} workers, titles under {MAX_TITLE} characters, summary under {MAX_SUMMARY}. Reviewers \
             (reviews:) and restarts need no plan."
        ),
        "inputSchema": {
            "type": "object",
            "properties": {
                "tabId": tab_id,
                "summary": { "type": "string", "description": "What the plan does and why these workers" },
                "workers": {
                    "type": "array",
                    "items": {
                        "type": "object",
                        "properties": {
                            "title": { "type": "string", "description": "The title you will spawn it with" },
                            "role": { "type": "string", "description": "A role from the catalogue (optional)" }
                        },
                        "required": ["title"]
                    },
                    "description": "One per worker, in the order you mean to start them"
                }
            },
            "required": ["summary", "workers"]
        }
    })
}

/// `hiveProposePlan` (§7.4): the record written, superseding an open proposal, the human asked on
/// the coordinator's row, and the question logged under `plan`.
pub(crate) async fn propose_tool<W: HiveWindow>(ctx: &Ctx<'_, W>, caller: &Caller, args: &Value) -> Result<Value, String> {
    let hive = super::autonomy::current_hive(ctx, caller).unwrap_or_else(|| caller.hive.clone());
    if !checkpoints::asks(&hive, PLAN) {
        return Ok(json!({ "needed": false, "note": "The plan is yours to make: your human set the Plan checkpoint to Coordinator. Spawn your workers." }));
    }
    let a = parse_args(args)?;
    let coordinator = super::tools::coordinator_tab(ctx.data, caller).ok_or("This Hive has no coordinator tab to ask on.")?;
    let now = crate::mailink::board::iso_now();
    let blocker = plan_blocker(&hive.goal, &a, &now);
    let note = format!("Needs you: {}", blocker.question);
    let (task_id, asked_at, tasks, workstreams) = {
        let mut d = ctx.data.write();
        let ws = super::tools::workspace_mut(&mut d, caller)?;
        let (task_id, asked_at) = super::fuses::file_row_blocker(ws, Some(&coordinator), PLAN_TASK_TITLE, blocker.clone(), &note, &now);
        (task_id, asked_at, ws.tasks.clone(), ws.workstreams.clone())
    };
    let rec = PlanRecord {
        id: new_id(),
        summary: a.summary.clone(),
        titles: a.titles.clone(),
        roles: a.roles.clone(),
        key: super::prompts::blocker_key(&task_id, &asked_at),
        task_id: task_id.clone(),
        decision_id: decisions::new_id(),
        asked_at: asked_at.clone(),
        ..PlanRecord::default()
    };
    let asked = DecisionLine {
        id: rec.decision_id.clone(),
        hive_id: hive.id.clone(),
        category: PLAN.to_string(),
        source: SOURCE_PLAN.to_string(),
        task_id: Some(task_id.clone()),
        key: Some(rec.key.clone()),
        question: one_line(&blocker.question, 400),
        context: blocker.context.as_deref().map(|c| one_line(c, 1000)),
        options: blocker.options.iter().map(|o| DecisionOption { label: o.label.clone(), recommended: o.recommended }).collect(),
        recommended: Some(ANSWER_GO.to_string()),
        at: asked_at.clone(),
        ..DecisionLine::default()
    };
    let (paths, r, hive_id) = (caller.paths.clone(), rec.clone(), hive.id.clone());
    let superseded = blocking(move || {
        let _held = lock();
        let prev = read(&paths).filter(|p| p.status == STATUS_PROPOSED);
        write(&paths, &r)?;
        decisions::record(&paths, &asked);
        if let Some(p) = &prev {
            let lapsed = DecisionLine {
                id: p.decision_id.clone(),
                event: decisions::EVENT_LAPSED.to_string(),
                hive_id,
                category: PLAN.to_string(),
                source: SOURCE_PLAN.to_string(),
                task_id: Some(p.task_id.clone()),
                key: Some(p.key.clone()),
                reason: Some(format!("Replaced by plan {}", r.id)),
                at: crate::commands::workspace::iso_now(),
                ..DecisionLine::default()
            };
            decisions::record(&paths, &lapsed);
        }
        Ok(prev.map(|p| p.id))
    })
    .await?;
    if let Err(e) = super::tools::save(ctx) {
        log::warn!("hive {}: plan row not saved: {e}", caller.hive.id);
    }
    ctx.window.tasks_changed(&caller.workspace_id, &tasks, &workstreams);
    ctx.window.decisions_changed(&caller.workspace_id);
    log::info!("hive {}: plan {} proposed ({} workers){}", caller.hive.id, rec.id, rec.titles.len(), superseded.as_ref().map(|p| format!(", replacing {p}")).unwrap_or_default());
    Ok(json!({
        "needed": true,
        "plan": rec.id,
        "task_id": task_id,
        "state": STATUS_PROPOSED,
        "replaced": superseded,
        "workers": rec.titles,
        "note": "Your human answers on your plan row (Go ahead, or Change it with what to change); their answer is typed to you. \
                 Meanwhile keep planning; once approved, spawn each title exactly as written here.",
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch() -> (std::path::PathBuf, HivePaths) {
        let base = std::env::temp_dir().join(format!("hive-plan-{}", uuid::Uuid::new_v4()));
        (base.clone(), HivePaths::new(base.join("hive/h1")))
    }

    fn approved(paths: &HivePaths, titles: &[&str]) -> PlanRecord {
        let rec = PlanRecord { id: new_id(), status: STATUS_APPROVED.into(), titles: titles.iter().map(|t| t.to_string()).collect(), key: "b_t_1".into(), ..PlanRecord::default() };
        write(paths, &rec).unwrap();
        rec
    }

    #[test]
    fn an_approved_title_is_claimed_once_and_a_release_frees_it() {
        let (base, paths) = scratch();
        assert_eq!(claim(&paths, "Login API").unwrap_err(), SPAWN_REFUSAL, "no plan, no spawn");
        let rec = approved(&paths, &["Login API", "Login UI"]);
        let (id, t) = claim(&paths, "login  api").unwrap();
        assert_eq!((id.as_str(), t.as_str()), (rec.id.as_str(), "login api"), "matched as the task title is");
        assert!(claim(&paths, "Login API").unwrap_err().contains("already started"));
        assert!(claim(&paths, "Billing").unwrap_err().contains("isn\u{2019}t in the plan"));
        release(&paths, "PL-other", &t);
        assert!(claim(&paths, "Login API").is_err(), "a release names the plan it claimed from");
        release(&paths, &id, &t);
        assert!(claim(&paths, "Login API").is_ok(), "a rolled-back spawn frees its title");
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn only_go_ahead_on_the_open_proposal_approves_it() {
        let (base, paths) = scratch();
        let rec = PlanRecord { id: new_id(), titles: vec!["A".into()], key: "b_t_1".into(), ..PlanRecord::default() };
        write(&paths, &rec).unwrap();
        assert!(claim(&paths, "A").unwrap_err().contains("waiting for their answer"));
        assert!(on_answered(&paths, "b_t_old", ANSWER_GO, "now").unwrap().is_none(), "a superseded proposal's answer changes nothing");
        let r = on_answered(&paths, "b_t_1", "go ahead", "now").unwrap().unwrap();
        assert_eq!((r.status.as_str(), r.decided_at.as_deref()), (STATUS_APPROVED, Some("now")));
        assert!(on_answered(&paths, "b_t_1", ANSWER_CHANGE, "later").unwrap().is_none(), "answered once");
        write(&paths, &PlanRecord { status: STATUS_PROPOSED.into(), decided_at: None, ..r }).unwrap();
        assert_eq!(on_answered(&paths, "b_t_1", ANSWER_CHANGE, "now").unwrap().unwrap().status, STATUS_DECLINED);
        assert!(claim(&paths, "A").unwrap_err().contains("didn\u{2019}t approve"));
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn the_arguments_are_checked_and_the_question_is_maiterms() {
        let ok = parse_args(&json!({ "summary": " Two halves. ", "workers": [{ "title": "Login  API", "role": "backend" }, "Login UI"] })).unwrap();
        assert_eq!((ok.summary.as_str(), ok.titles.clone(), ok.roles.clone()), ("Two halves.", vec!["Login API".to_string(), "Login UI".to_string()], vec!["backend".to_string(), String::new()]));
        let bad = |v: Value| parse_args(&v).unwrap_err();
        assert!(bad(json!({ "workers": ["A"] })).contains("summary is required"));
        assert!(bad(json!({ "summary": "s", "workers": [] })).contains("1 to 20"));
        assert!(bad(json!({ "summary": "s", "workers": vec!["x"; 21] })).contains("1 to 20"));
        assert!(bad(json!({ "summary": "s", "workers": ["A", "a"] })).contains("named twice"));
        assert!(bad(json!({ "summary": "s", "workers": [{ "title": "A", "role": "wizard" }] })).contains("no role"));
        assert!(bad(json!({ "summary": "s", "workers": ["t".repeat(MAX_TITLE + 1)] })).contains("too long"));
        assert!(bad(json!({ "summary": "x".repeat(MAX_SUMMARY + 1), "workers": ["A"] })).contains("under 2000"));
        let b = plan_blocker("Ship\nit", &ok, "2026-10-24T10:00:00.000Z");
        assert_eq!(b.question, "Start these 2 workers for \u{201c}Ship it\u{201d}?");
        assert_eq!(plan_blocker("g", &ProposeArgs { titles: vec!["A".into()], roles: vec![String::new()], ..ok.clone() }, "x").question, "Start this worker for \u{201c}g\u{201d}?");
        assert_eq!(b.context.as_deref(), Some("1. Login API (backend)\n2. Login UI\nThe queen\u{2019}s summary: \u{201c}Two halves.\u{201d}"));
        assert_eq!(b.options.iter().map(|o| (o.label.as_str(), o.recommended)).collect::<Vec<_>>(), vec![(ANSWER_GO, true), (ANSWER_CHANGE, false)]);
        assert_eq!((b.kind.as_str(), b.asked_by.as_str()), ("decision", "agent"));
    }
}
