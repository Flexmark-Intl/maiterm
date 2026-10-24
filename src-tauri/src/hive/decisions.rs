//! The decision log, `logs/decisions.jsonl` (docs/hive.md §7.6): one line per event on a decision
//! (asked, recommended, answered, lapsed…), append-only, every kind a string. Lines of one
//! decision share its `D-…` id; reading them as decisions (the fold, the view) is step 7d's.
//!
//! The human's blocker answers reach it through `hive_blocker_answered`, which only the webview
//! calls (`overlordStore.answerBlocker`, shared by the desktop card, the Loom and the phone): no
//! MCP call reaches a Tauri command, so an agent can't log an answer as the human's (plan D16).

use std::sync::{Arc, Mutex};

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use super::deliver::one_line;
use super::paths::HivePaths;
use super::record::Hive;
use super::worker::WorkerRecord;
use crate::state::workspace::TaskBlocker;
use crate::state::AppState;

pub const LOG_VERSION: u32 = 1;

pub const EVENT_ASKED: &str = "asked";
/// The coordinator's answer under Ask me: recorded, never sent (plan D10).
pub const EVENT_RECOMMENDED: &str = "recommended";
pub const EVENT_ANSWERED: &str = "answered";
/// The prompt closed with no answer through maiTerm.
pub const EVENT_LAPSED: &str = "lapsed";

/// What no checkpoint can hand to the coordinator (§7.4).
pub const CATEGORY_NEVER: &str = "never";
/// The coordinator's own blockers: they can become precedents, never a streak (plan D15).
pub const CATEGORY_OTHER: &str = "other";

pub const SOURCE_PROMPT: &str = "prompt";
pub const SOURCE_QUESTION: &str = "question";
pub const SOURCE_BLOCKER: &str = "blocker";

pub const BY_COORDINATOR: &str = "coordinator";
pub const BY_HUMAN: &str = "human";
pub const BY_MAITERM: &str = "maiterm";

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct DecisionOption {
    pub label: String,
    pub recommended: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct DecisionLine {
    pub v: u32,
    /// `D-<8hex>`, shared by every line of one decision.
    pub id: String,
    /// `asked` | `recommended` | `answered` | `lapsed` | `overridden` | `offer` | `digest`.
    pub event: String,
    pub hive_id: String,
    /// The checkpoint it fell under, `never`, or later steps' own (`config`, `other`, `offer`…).
    pub category: String,
    /// `prompt` | `question` | `blocker` | `land` | `resolve` | `plan` | `offer`.
    pub source: String,
    pub task_id: Option<String>,
    /// The worker's slug.
    pub worker: Option<String>,
    /// The prompt id (`p_…`, `q_…`) or blocker key (`b_<task>_<asked_at>`) it answers.
    pub key: Option<String>,
    /// maiTerm's words for what was asked, worker text flattened and escaped.
    pub question: String,
    pub context: Option<String>,
    pub options: Vec<DecisionOption>,
    pub recommended: Option<String>,
    pub answer: Option<String>,
    pub reason: Option<String>,
    /// `coordinator` | `human` | `maiterm`.
    pub by: String,
    /// Answered after the coordinator's time ran out (§4.7).
    pub after_timeout: bool,
    /// The precedent the coordinator cited, as given (checked from step 7d).
    pub precedent: Option<String>,
    pub outcome: Option<String>,
    pub at: String,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

impl Default for DecisionLine {
    fn default() -> Self {
        Self {
            v: LOG_VERSION,
            id: String::new(),
            event: EVENT_ASKED.to_string(),
            hive_id: String::new(),
            category: String::new(),
            source: String::new(),
            task_id: None,
            worker: None,
            key: None,
            question: String::new(),
            context: None,
            options: Vec::new(),
            recommended: None,
            answer: None,
            reason: None,
            by: BY_MAITERM.to_string(),
            after_timeout: false,
            precedent: None,
            outcome: None,
            at: String::new(),
            extra: Map::new(),
        }
    }
}

pub fn new_id() -> String {
    format!("D-{}", &uuid::Uuid::new_v4().simple().to_string()[..8])
}

/// One writer at a time in this process; each line one `O_APPEND` write.
static WRITE: Mutex<()> = Mutex::new(());

pub fn append(paths: &HivePaths, l: &DecisionLine) -> Result<(), String> {
    let _held = WRITE.lock().unwrap_or_else(|e| e.into_inner());
    let v = serde_json::to_value(l).map_err(|e| e.to_string())?;
    super::queue::append_line(&paths.decision_log(), &v)
}

/// `append`, logged rather than returned: a decision that went through is never undone because
/// its line didn't write.
pub fn record(paths: &HivePaths, l: &DecisionLine) {
    if let Err(e) = append(paths, l) {
        log::warn!("hive {}: decision {} ({}) not logged: {e}", l.hive_id, l.id, l.event);
    }
}

/// Every line that parses, oldest first.
pub fn read(paths: &HivePaths) -> Vec<DecisionLine> {
    let Ok(text) = std::fs::read_to_string(paths.decision_log()) else { return Vec::new() };
    text.lines().filter_map(|l| serde_json::from_str::<DecisionLine>(l.trim_end_matches('\r')).ok()).collect()
}

/// The latest line logged under a prompt id or blocker key.
pub fn by_key<'a>(lines: &'a [DecisionLine], key: &str) -> Option<&'a DecisionLine> {
    lines.iter().rev().find(|l| l.key.as_deref() == Some(key))
}

// ── The human's blocker answers (step 7c) ──

/// What the human chose on a blocker card: an option's label, or none, and their words.
#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(default)]
pub struct BlockerAnswerArg {
    pub option: Option<String>,
    pub text: String,
}

/// One blocker answer logged at a time, so two calls for one question can't both write it.
static RECORD: Mutex<()> = Mutex::new(());

/// A blocker the human answered, as decision-log lines: the `asked` line its route wrote (or one
/// written now: a coordinator's row is never routed, and a worker's from before a restart wasn't),
/// then `answered` by the human. A worker's row is `questions` (`never` for what the allowlist
/// keeps from the coordinator) and carries the coordinator's recorded recommendation; any other
/// row is `other` with the blocker's own recommended option, which its coordinator set; a key a
/// later step logged first (an offer, the plan) keeps that line's id and category. None when this
/// question's answer is already logged.
#[allow(clippy::too_many_arguments)]
pub fn record_blocker_answer(
    paths: &HivePaths,
    hive: &Hive,
    workers: &[WorkerRecord],
    task_id: &str,
    blocker: &TaskBlocker,
    answer: &BlockerAnswerArg,
    after_timeout: bool,
    now: &str,
) -> Result<Option<DecisionLine>, String> {
    if blocker.asked_at.trim().is_empty() {
        return Err("That question has no asked_at.".to_string());
    }
    let text = one_line(&answer.text, super::prompts::MAX_ANSWER);
    let option = match answer.option.as_deref().map(str::trim).filter(|o| !o.is_empty()) {
        Some(o) => Some(blocker.options.iter().find(|x| x.label == o).map(|x| one_line(&x.label, 120)).ok_or("That option is not one of the choices offered.")?),
        None => None,
    };
    if option.is_none() && text.is_empty() && blocker.kind == "decision" {
        return Err("An answer needs an option or words.".to_string());
    }
    let key = super::prompts::blocker_key(task_id, &blocker.asked_at);
    let _held = RECORD.lock().unwrap_or_else(|e| e.into_inner());
    let lines = read(paths);
    let mine: Vec<&DecisionLine> = lines.iter().filter(|l| l.key.as_deref() == Some(key.as_str())).collect();
    if mine.iter().any(|l| l.event == EVENT_ANSWERED) {
        return Ok(None);
    }
    let worker = workers.iter().find(|w| w.task_id == task_id);
    let own_recommended = blocker.options.iter().find(|o| o.recommended).map(|o| one_line(&o.label, 120));
    let (id, category) = match mine.first() {
        Some(first) => (first.id.clone(), first.category.clone()),
        None => {
            let category = match worker {
                Some(_) if super::prompts::classify(&super::prompts::PromptView::from_blocker(task_id, blocker)).is_ok() => super::checkpoints::QUESTIONS,
                Some(_) => CATEGORY_NEVER,
                None => CATEGORY_OTHER,
            };
            let asked = DecisionLine {
                id: new_id(),
                hive_id: hive.id.clone(),
                category: category.to_string(),
                source: SOURCE_BLOCKER.to_string(),
                task_id: Some(task_id.to_string()),
                worker: worker.map(|w| w.slug.clone()),
                key: Some(key.clone()),
                question: one_line(&blocker.question, 400),
                context: blocker.context.as_deref().filter(|c| !c.trim().is_empty()).map(|c| one_line(c, 1000)),
                options: blocker.options.iter().map(|o| DecisionOption { label: one_line(&o.label, 120), recommended: o.recommended }).collect(),
                recommended: own_recommended.clone(),
                // When it was asked, not when the log heard of it.
                at: blocker.asked_at.clone(),
                ..DecisionLine::default()
            };
            append(paths, &asked)?;
            (asked.id, asked.category)
        }
    };
    // Only the coordinator's recommendation counts toward what it earns (plan D10, D18).
    let (recommended, precedent) = match worker {
        Some(_) => mine
            .iter()
            .rev()
            .find(|l| l.event == EVENT_RECOMMENDED)
            .map(|l| (l.recommended.clone().or_else(|| l.answer.clone()), l.precedent.clone()))
            .unwrap_or((None, None)),
        None => (own_recommended, None),
    };
    let answered = DecisionLine {
        id,
        event: EVENT_ANSWERED.to_string(),
        hive_id: hive.id.clone(),
        category,
        source: SOURCE_BLOCKER.to_string(),
        task_id: Some(task_id.to_string()),
        worker: worker.map(|w| w.slug.clone()),
        key: Some(key),
        question: one_line(&blocker.question, 400),
        answer: Some(match (&option, blocker.kind.as_str()) {
            (Some(o), _) => o.clone(),
            (None, _) if !text.is_empty() => text.clone(),
            (None, "action") => "Done by the human".to_string(),
            (None, _) => "Cleared by the human".to_string(),
        }),
        reason: option.as_ref().and(Some(text)).filter(|t| !t.is_empty()),
        recommended,
        precedent,
        by: BY_HUMAN.to_string(),
        after_timeout,
        at: now.to_string(),
        ..DecisionLine::default()
    };
    append(paths, &answered)?;
    Ok(Some(answered))
}

/// The human answered a blocker in this window (`overlordStore.answerBlocker`). Logged when the
/// workspace is a Hive, with the route dropped and the coordinator told of a worker's question.
/// The blocker is the store's copy from before the answer cleared it. Answers the decision id
/// when a line was written. Dismissing ("handled in the tab") isn't a decision and never calls this.
#[tauri::command]
pub async fn hive_blocker_answered(
    window: tauri::Window,
    state: tauri::State<'_, Arc<AppState>>,
    app: tauri::AppHandle,
    workspace_id: String,
    task_id: String,
    blocker: TaskBlocker,
    answer: BlockerAnswerArg,
) -> Result<Option<String>, String> {
    let Some(caller) = super::tools::caller_for_workspace(&state.app_data.read(), &workspace_id).filter(|c| c.label == window.label()) else {
        return Ok(None);
    };
    let ctx = super::tools::app_ctx(state.inner(), &app, &caller);
    super::prompts::blocker_answered_in(&ctx, &caller, &task_id, blocker, answer).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lines_append_and_read_back_with_a_later_builds_fields() {
        let base = std::env::temp_dir().join(format!("hive-decisions-{}", uuid::Uuid::new_v4()));
        let paths = HivePaths::new(&base);
        let id = new_id();
        assert!(id.starts_with("D-") && id.len() == 10, "{id}");
        let asked = DecisionLine { id: id.clone(), key: Some("p_t_1".into()), question: "Bash: `ls`".into(), at: "now".into(), ..DecisionLine::default() };
        append(&paths, &asked).unwrap();
        let answered = DecisionLine { event: EVENT_ANSWERED.into(), answer: Some("Yes".into()), by: BY_COORDINATOR.into(), ..asked.clone() };
        append(&paths, &answered).unwrap();
        let text = std::fs::read_to_string(paths.decision_log()).unwrap();
        // A CRLF line, a broken one and a later build's field.
        std::fs::write(paths.decision_log(), format!("{text}not json\r\n{}\r\n", r#"{"v":1,"id":"D-x","event":"offer","key":"b_t_2","later":7}"#)).unwrap();
        let all = read(&paths);
        assert_eq!(all.len(), 3, "a line that doesn't parse is skipped");
        assert_eq!((all[0].clone(), all[1].clone()), (asked, answered.clone()));
        assert_eq!(all[2].extra.get("later"), Some(&serde_json::json!(7)));
        assert_eq!(by_key(&all, "p_t_1"), Some(&answered), "the latest line of the key");
        assert!(by_key(&all, "nope").is_none());
        let first: Value = serde_json::from_str(text.lines().next().unwrap()).unwrap();
        for k in ["task_id", "recommended", "answer", "precedent", "after_timeout"] {
            assert!(first.get(k).is_some(), "{k} is written even when empty");
        }
        let _ = std::fs::remove_dir_all(&base);
    }

    use crate::state::workspace::BlockerOption;

    fn scratch() -> (std::path::PathBuf, HivePaths) {
        let base = std::env::temp_dir().join(format!("hive-blocker-answers-{}", uuid::Uuid::new_v4()));
        let paths = HivePaths::new(&base);
        (base, paths)
    }

    fn blocker(kind: &str, asked_at: &str, options: &[(&str, bool)]) -> TaskBlocker {
        TaskBlocker {
            kind: kind.into(),
            question: "Redis or Postgres?".into(),
            context: Some("Both are\nin the stack.".into()),
            options: options.iter().map(|(l, r)| BlockerOption { label: l.to_string(), detail: None, recommended: *r }).collect(),
            command: None,
            asked_at: asked_at.into(),
            asked_by: "agent".into(),
        }
    }

    fn hive() -> Hive {
        Hive { id: "h1".into(), ..Hive::default() }
    }

    fn worker() -> WorkerRecord {
        WorkerRecord { task_id: "t-w".into(), slug: "session-store".into(), title: "Session store".into(), badge: 2, ..WorkerRecord::default() }
    }

    fn pick(option: Option<&str>, text: &str) -> BlockerAnswerArg {
        BlockerAnswerArg { option: option.map(str::to_string), text: text.into() }
    }

    #[test]
    fn a_workers_question_is_logged_under_questions_with_the_coordinators_recommendation() {
        let (base, paths) = scratch();
        let b = blocker("decision", "2026-10-24T10:00:00Z", &[("Redis", true), ("Postgres", false)]);
        let key = super::super::prompts::blocker_key("t-w", &b.asked_at);
        // What routing wrote: its asked line (the worker recommends Redis) and, under Ask me, the
        // coordinator's recommendation.
        let asked = DecisionLine { id: "D-route".into(), category: "questions".into(), key: Some(key.clone()), recommended: Some("Redis".into()), ..DecisionLine::default() };
        append(&paths, &asked).unwrap();
        let rec = DecisionLine { event: EVENT_RECOMMENDED.into(), by: BY_COORDINATOR.into(), recommended: Some("Postgres".into()), answer: Some("Postgres".into()), precedent: Some("P-1".into()), ..asked.clone() };
        append(&paths, &rec).unwrap();

        let l = record_blocker_answer(&paths, &hive(), &[worker()], "t-w", &b, &pick(Some("Postgres"), "sessions outlive a restart"), true, "now").unwrap().unwrap();
        assert_eq!((l.id.as_str(), l.event.as_str(), l.category.as_str(), l.by.as_str()), ("D-route", "answered", "questions", "human"));
        assert_eq!((l.answer.as_deref(), l.reason.as_deref()), (Some("Postgres"), Some("sessions outlive a restart")));
        assert_eq!((l.recommended.as_deref(), l.precedent.as_deref()), (Some("Postgres"), Some("P-1")), "the coordinator's, not the worker's");
        assert_eq!((l.worker.as_deref(), l.after_timeout, l.source.as_str()), (Some("session-store"), true, "blocker"));
        assert_eq!(read(&paths).len(), 3, "no second asked line");

        // The same question again (a second card, the phone): nothing new.
        assert!(record_blocker_answer(&paths, &hive(), &[worker()], "t-w", &b, &pick(Some("Redis"), ""), false, "later").unwrap().is_none());
        assert_eq!(read(&paths).len(), 3);
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn a_coordinators_own_row_is_other_with_its_own_recommendation() {
        let (base, paths) = scratch();
        let b = blocker("decision", "2026-10-24T10:00:00Z", &[("Ship", true), ("Wait", false)]);
        let l = record_blocker_answer(&paths, &hive(), &[worker()], "t-coord", &b, &pick(None, "wait for the review"), false, "now").unwrap().unwrap();
        let all = read(&paths);
        assert_eq!(all.iter().map(|x| x.event.as_str()).collect::<Vec<_>>(), vec!["asked", "answered"]);
        let asked = &all[0];
        assert_eq!((asked.category.as_str(), asked.by.as_str(), asked.at.as_str()), ("other", "maiterm", "2026-10-24T10:00:00Z"), "asked when it was asked");
        assert_eq!(asked.context.as_deref(), Some("Both are in the stack."), "flattened");
        assert!(asked.options.iter().any(|o| o.label == "Ship" && o.recommended));
        assert!(asked.worker.is_none());
        assert_eq!((l.id.as_str(), l.category.as_str()), (asked.id.as_str(), "other"));
        assert_eq!((l.answer.as_deref(), l.reason, l.recommended.as_deref()), (Some("wait for the review"), None, Some("Ship")));

        // A key a later step logged first (an offer) keeps its line's id and category.
        let b2 = blocker("decision", "2026-10-24T12:00:00Z", &[("Yes", true), ("No", false)]);
        let offer = DecisionLine { id: "D-offer".into(), event: "offer".into(), category: "offer".into(), key: Some(super::super::prompts::blocker_key("t-coord", &b2.asked_at)), ..DecisionLine::default() };
        append(&paths, &offer).unwrap();
        let l = record_blocker_answer(&paths, &hive(), &[], "t-coord", &b2, &pick(Some("No"), ""), false, "now").unwrap().unwrap();
        assert_eq!((l.id.as_str(), l.category.as_str(), l.reason), ("D-offer", "offer", None));
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn unrouted_worker_blockers_follow_the_allowlist_and_bad_answers_are_refused() {
        let (base, paths) = scratch();
        let act = blocker("action", "2026-10-24T10:00:00Z", &[]);
        let l = record_blocker_answer(&paths, &hive(), &[worker()], "t-w", &act, &pick(None, ""), false, "now").unwrap().unwrap();
        assert_eq!((l.category.as_str(), l.answer.as_deref()), (CATEGORY_NEVER, Some("Done by the human")), "an action is never the coordinator's");
        let q = blocker("decision", "2026-10-24T11:00:00Z", &[("A", false)]);
        let l = record_blocker_answer(&paths, &hive(), &[worker()], "t-w", &q, &pick(Some("A"), ""), false, "now").unwrap().unwrap();
        assert_eq!((l.category.as_str(), l.recommended), ("questions", None), "no coordinator recommendation: none");
        let e = record_blocker_answer(&paths, &hive(), &[worker()], "t-w", &blocker("decision", "x", &[("A", false)]), &pick(Some("B"), ""), false, "now").unwrap_err();
        assert!(e.contains("not one of the choices"), "{e}");
        let e = record_blocker_answer(&paths, &hive(), &[worker()], "t-w", &blocker("decision", "y", &[]), &pick(None, "  "), false, "now").unwrap_err();
        assert!(e.contains("needs an option or words"), "{e}");
        assert_eq!(read(&paths).len(), 4, "a refused answer writes nothing");
        let _ = std::fs::remove_dir_all(&base);
    }
}
