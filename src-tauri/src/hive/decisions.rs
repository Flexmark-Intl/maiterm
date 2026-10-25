//! The decision log, `logs/decisions.jsonl` (docs/hive.md §7.6): one line per event on a decision
//! (asked, recommended, answered, lapsed, overridden…), append-only, every kind a string. Lines
//! of one decision share its `D-…` id, and `fold` reads them back as decisions: the latest line of
//! each event wins, and an `answered` line carrying only an `outcome` updates just that (a land's
//! result arrives after its decision).
//!
//! The human's blocker answers reach it through `hive_blocker_answered`, and their overrides
//! through `hive_override_decision`, which only the webview calls: no MCP call reaches a Tauri
//! command, so an agent can't log an answer or an override as the human's (plan D16, D17).

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use tauri::AppHandle;

use super::deliver::one_line;
use super::land::LandState;
use super::paths::HivePaths;
use super::record::Hive;
use super::rpc::HiveWindow;
use super::tools::{blocking, Caller, Ctx};
use super::worker::WorkerRecord;
use crate::state::workspace::TaskBlocker;
use crate::state::AppState;

pub const LOG_VERSION: u32 = 1;

/// To the Hive's window when its decisions or precedents change: Memory's Decisions and
/// Precedents tabs refetch. Not `hive-memory-changed`, whose listener rebuilds the memory index.
pub const DECISIONS_CHANGED_EVENT: &str = "hive-decisions-changed";

pub const EVENT_ASKED: &str = "asked";
/// The coordinator's answer under Ask me: recorded, never sent (plan D10).
pub const EVENT_RECOMMENDED: &str = "recommended";
pub const EVENT_ANSWERED: &str = "answered";
/// The prompt closed with no answer through maiTerm.
pub const EVENT_LAPSED: &str = "lapsed";
/// The human overrode the coordinator's decision (plan D17).
pub const EVENT_OVERRIDDEN: &str = "overridden";
/// maiTerm offering a checkpoint (step 7e): asked like any decision.
pub const EVENT_OFFER: &str = "offer";
/// The daily digest (step 7e): no decision.
pub const EVENT_DIGEST: &str = "digest";

/// What no checkpoint can hand to the coordinator (§7.4).
pub const CATEGORY_NEVER: &str = "never";
/// The coordinator's own blockers: they can become precedents, never a streak (plan D15).
pub const CATEGORY_OTHER: &str = "other";
/// Land cards the config rule and the integration mode file: the human's, never earned.
pub const CATEGORY_CONFIG: &str = "config";
pub const CATEGORY_INTEGRATION: &str = "integration";
/// maiTerm's own offers: never a precedent, never a streak.
pub const CATEGORY_OFFER: &str = "offer";

pub const SOURCE_PROMPT: &str = "prompt";
pub const SOURCE_QUESTION: &str = "question";
pub const SOURCE_BLOCKER: &str = "blocker";
pub const SOURCE_LAND: &str = "land";
pub const SOURCE_RESOLVE: &str = "resolve";

pub const ANSWER_LAND: &str = "Land";
pub const ANSWER_DONT_LAND: &str = "Don\u{2019}t land";

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
    /// The precedent the coordinator cited: an active one when it was cited (`precedents`).
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

/// To the Hive's window: its Decisions and Precedents tabs refetch.
pub(crate) fn emit_changed(handle: &AppHandle, label: &str, workspace_id: &str) {
    use tauri::Emitter;
    let payload = serde_json::json!({ "workspace_id": workspace_id });
    if let Err(e) = handle.emit_to(label, DECISIONS_CHANGED_EVENT, payload) {
        log::warn!("hive: decisions-changed not sent to window {label}: {e}");
    }
}

// ── The fold (step 7d) ──

/// A land's result, after its decision: it updates the outcome and nothing else.
fn outcome_only(l: &DecisionLine) -> bool {
    l.event == EVENT_ANSWERED && l.answer.is_none() && l.outcome.is_some()
}

/// One decision, its lines folded: the latest line of each event.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Decision {
    pub id: String,
    /// Its `asked` line, or maiTerm's `offer`.
    pub asked: Option<DecisionLine>,
    pub recommended: Option<DecisionLine>,
    pub answered: Option<DecisionLine>,
    pub lapsed: Option<DecisionLine>,
    pub overridden: Option<DecisionLine>,
    /// The latest outcome any of its lines carried.
    pub outcome: Option<String>,
    /// Where its latest line, outcome updates aside, sits in the log: the view's order.
    pub last: usize,
}

impl Decision {
    /// What was asked, else the earliest kind of line it has.
    pub fn first(&self) -> Option<&DecisionLine> {
        self.asked.as_ref().or(self.answered.as_ref()).or(self.recommended.as_ref()).or(self.lapsed.as_ref())
    }

    pub fn category(&self) -> &str {
        self.first().map_or("", |l| l.category.as_str())
    }

    pub fn question(&self) -> &str {
        self.first().map_or("", |l| l.question.as_str())
    }

    pub fn answered_by(&self) -> Option<&str> {
        self.answered.as_ref().map(|l| l.by.as_str())
    }

    /// Only the coordinator's own decisions are overridden, once each (plan D17).
    pub fn can_override(&self) -> bool {
        self.answered_by() == Some(BY_COORDINATOR) && self.overridden.is_none() && self.category() != CATEGORY_OFFER
    }

    /// The precedent its answer, or the recommendation the human answered, cited.
    pub fn precedent(&self) -> Option<&str> {
        self.answered.as_ref().and_then(|l| l.precedent.as_deref()).or_else(|| self.recommended.as_ref().and_then(|l| l.precedent.as_deref()))
    }
}

/// Every decision in the log, newest first (by its latest line, outcome updates aside). A
/// digest is no decision.
pub fn fold(lines: &[DecisionLine]) -> Vec<Decision> {
    let mut order: Vec<String> = Vec::new();
    let mut by_id: HashMap<String, Decision> = HashMap::new();
    for (i, l) in lines.iter().enumerate() {
        if l.id.is_empty() || l.event == EVENT_DIGEST {
            continue;
        }
        let d = by_id.entry(l.id.clone()).or_insert_with(|| {
            order.push(l.id.clone());
            Decision { id: l.id.clone(), last: i, ..Decision::default() }
        });
        if let Some(o) = &l.outcome {
            d.outcome = Some(o.clone());
        }
        if outcome_only(l) {
            continue;
        }
        let slot = match l.event.as_str() {
            EVENT_ASKED | EVENT_OFFER => &mut d.asked,
            EVENT_RECOMMENDED => &mut d.recommended,
            EVENT_ANSWERED => &mut d.answered,
            EVENT_LAPSED => &mut d.lapsed,
            EVENT_OVERRIDDEN => &mut d.overridden,
            _ => continue,
        };
        *slot = Some(l.clone());
        d.last = i;
    }
    let mut out: Vec<Decision> = order.into_iter().filter_map(|id| by_id.remove(&id)).collect();
    out.sort_by_key(|d| std::cmp::Reverse(d.last));
    out
}

pub fn find(lines: &[DecisionLine], id: &str) -> Option<Decision> {
    fold(lines).into_iter().find(|d| d.id == id)
}

/// The precedent a decision cited, as the view shows it.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct PrecedentRef {
    pub id: String,
    /// None when it is gone (the human deleted it).
    pub rule: Option<String>,
    pub status: Option<String>,
}

/// A decision as Memory → Decisions shows it.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct DecisionView {
    pub id: String,
    pub at: String,
    pub category: String,
    pub source: String,
    pub task_id: Option<String>,
    pub worker: Option<String>,
    pub question: String,
    pub context: Option<String>,
    pub options: Vec<DecisionOption>,
    /// The coordinator's recommendation, when it gave one.
    pub recommended: Option<String>,
    pub answer: Option<String>,
    pub reason: Option<String>,
    /// `open` | `recommended` | `answered` | `lapsed`.
    pub status: String,
    /// `coordinator` | `human` | `maiterm`, once answered.
    pub by: String,
    pub by_label: String,
    pub after_timeout: bool,
    pub precedent: Option<PrecedentRef>,
    pub outcome: Option<String>,
    pub overridden: bool,
    pub override_note: Option<String>,
    pub can_override: bool,
}

/// Who decided, in the view's words.
pub fn by_label(d: &Decision) -> String {
    let Some(a) = &d.answered else { return String::new() };
    match a.by.as_str() {
        BY_COORDINATOR if a.precedent.is_some() => "coordinator \u{b7} by precedent".to_string(),
        BY_COORDINATOR => "coordinator".to_string(),
        BY_HUMAN if a.after_timeout => "human, after the coordinator\u{2019}s timeout".to_string(),
        BY_HUMAN => "human".to_string(),
        _ => "maiTerm".to_string(),
    }
}

/// `precedent` looks a cited precedent up: its rule and status.
pub fn view_of(d: &Decision, mut precedent: impl FnMut(&str) -> Option<(String, String)>) -> DecisionView {
    let first = d.first().cloned().unwrap_or_default();
    let latest = d.answered.as_ref().or(d.lapsed.as_ref()).or(d.recommended.as_ref()).unwrap_or(&first);
    // A worker's blocker carries the worker's own recommended option: only the coordinator's is
    // a recommendation here.
    let recommended = d
        .answered
        .as_ref()
        .and_then(|l| l.recommended.clone())
        .or_else(|| d.recommended.as_ref().and_then(|l| l.recommended.clone().or_else(|| l.answer.clone())))
        .or_else(|| d.asked.as_ref().filter(|l| l.worker.is_none()).and_then(|l| l.recommended.clone()));
    let status = if d.answered.is_some() {
        "answered"
    } else if d.lapsed.is_some() {
        "lapsed"
    } else if d.recommended.is_some() {
        "recommended"
    } else {
        "open"
    };
    DecisionView {
        id: d.id.clone(),
        at: latest.at.clone(),
        category: d.category().to_string(),
        source: first.source.clone(),
        task_id: first.task_id.clone(),
        worker: first.worker.clone(),
        question: d.question().to_string(),
        context: d.asked.as_ref().and_then(|l| l.context.clone()),
        options: if first.options.is_empty() { latest.options.clone() } else { first.options.clone() },
        recommended,
        answer: d.answered.as_ref().and_then(|l| l.answer.clone()),
        reason: d.answered.as_ref().or(d.lapsed.as_ref()).and_then(|l| l.reason.clone()),
        status: status.to_string(),
        by: d.answered_by().unwrap_or_default().to_string(),
        by_label: by_label(d),
        after_timeout: d.answered.as_ref().is_some_and(|l| l.after_timeout),
        precedent: d.precedent().map(|id| {
            let found = precedent(id);
            PrecedentRef { id: id.to_string(), rule: found.as_ref().map(|(r, _)| r.clone()), status: found.map(|(_, s)| s) }
        }),
        outcome: d.outcome.clone(),
        overridden: d.overridden.is_some(),
        override_note: d.overridden.as_ref().and_then(|l| l.reason.clone()),
        can_override: d.can_override(),
    }
}

/// The decision log as the view shows it, newest first.
pub fn view(paths: &HivePaths) -> Vec<DecisionView> {
    let mut rules: HashMap<String, Option<(String, String)>> = HashMap::new();
    fold(&read(paths))
        .iter()
        .map(|d| view_of(d, |id| rules.entry(id.to_string()).or_insert_with(|| super::precedents::live(paths, id).map(|p| (p.rule, p.status))).clone()))
        .collect()
}

/// The human-answered decision `blocker_id` names (`hiveRecordPrecedent`, plan D26): a decision id,
/// or a task id, whose latest answer by the human it takes. Only an answer the human gave.
pub fn human_answer(lines: &[DecisionLine], blocker_id: &str) -> Result<Decision, String> {
    let all = fold(lines);
    let only_human = "only an answer your human gave can become a precedent";
    if blocker_id.starts_with("D-") {
        let d = all.into_iter().find(|d| d.id == blocker_id).ok_or_else(|| format!("There\u{2019}s no decision {} in this Hive\u{2019}s log.", one_line(blocker_id, 40)))?;
        return match d.answered_by() {
            Some(BY_HUMAN) => Ok(d),
            Some(BY_COORDINATOR) => Err(format!("{} was your own answer: {only_human}.", d.id)),
            Some(_) => Err(format!("{} was answered by maiTerm: {only_human}.", d.id)),
            None => Err(format!("{} isn\u{2019}t answered: {only_human}.", d.id)),
        };
    }
    all.into_iter()
        .filter(|d| d.answered_by() == Some(BY_HUMAN) && d.first().is_some_and(|l| l.task_id.as_deref() == Some(blocker_id)))
        .max_by_key(|d| d.last)
        .ok_or_else(|| format!("No answer your human gave is logged for task {}: {only_human}.", one_line(blocker_id, 60)))
}

// ── Overrides (plan D17) ──

/// An override written: the decision as it was, and the precedent it retired.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Overridden {
    pub decision: Decision,
    pub note: String,
    pub precedent: Option<String>,
    pub retired: bool,
}

/// The human's override of the coordinator's decision: an `overridden` line with their note, and
/// the precedent it cited counted against (retired at its second). Refused for anything but the
/// coordinator's own answer, and twice.
pub(crate) fn override_decision(paths: &HivePaths, decision_id: &str, note: &str, now: &str) -> Result<Overridden, String> {
    let note = one_line(note, super::prompts::MAX_REASON);
    if note.is_empty() {
        return Err("Say what should have happened: your note goes to the coordinator.".to_string());
    }
    let _held = RECORD.lock().unwrap_or_else(|e| e.into_inner());
    let d = find(&read(paths), decision_id).ok_or("That decision isn\u{2019}t in this Hive\u{2019}s log.")?;
    if d.overridden.is_some() {
        return Err("That decision is already overridden.".to_string());
    }
    if !d.can_override() {
        return Err(match d.answered_by() {
            Some(BY_HUMAN) => "That was your own answer: only the coordinator\u{2019}s decisions are overridden.".to_string(),
            None => "That decision isn\u{2019}t answered yet.".to_string(),
            _ => "Only a decision the coordinator made can be overridden.".to_string(),
        });
    }
    let a = d.answered.clone().expect("can_override has an answer");
    let line = DecisionLine {
        id: d.id.clone(),
        event: EVENT_OVERRIDDEN.to_string(),
        hive_id: a.hive_id.clone(),
        category: d.category().to_string(),
        source: a.source.clone(),
        task_id: a.task_id.clone(),
        worker: a.worker.clone(),
        key: a.key.clone(),
        question: d.question().to_string(),
        answer: a.answer.clone(),
        reason: Some(note.clone()),
        by: BY_HUMAN.to_string(),
        precedent: a.precedent.clone(),
        at: now.to_string(),
        ..DecisionLine::default()
    };
    append(paths, &line)?;
    let mut retired = false;
    if let Some(p) = &a.precedent {
        match super::precedents::override_(paths, p) {
            Ok(r) => retired = r,
            Err(e) => log::warn!("hive {}: override of {} not counted against precedent {p}: {e}", a.hive_id, d.id),
        }
    }
    Ok(Overridden { decision: d, note, precedent: a.precedent, retired })
}

/// The coordinator's word of an override. The decision's own words (which may quote a worker)
/// sit on labelled lines, outside the sentence about the human.
fn override_notice(o: &Overridden) -> String {
    let mut s = format!("[maiTerm] Your human overrode your decision {}: {}", o.decision.id, o.note);
    if let (Some(p), true) = (&o.precedent, o.retired) {
        s.push_str(&format!(" (precedent {p} is retired)"));
    }
    s.push_str(&format!("\nThe decision: {}", one_line(o.decision.question(), 300)));
    if let Some(a) = o.decision.answered.as_ref().and_then(|l| l.answer.as_deref()) {
        s.push_str(&format!("\nYou answered: {}", one_line(a, 200)));
    }
    if o.precedent.is_some() && !o.retired {
        s.push_str("\nThe precedent you cited is counted as overridden; a second override retires it.");
    }
    s.push_str("\nRecord what your human wants instead with hiveRecordPrecedent once they answer a blocker on it.");
    s
}

/// An override carried out: written, counted, and told to the coordinator.
pub(crate) async fn override_in<W: HiveWindow>(ctx: &Ctx<'_, W>, caller: &Caller, decision_id: &str, note: &str) -> Result<DecisionView, String> {
    let (paths, id, n) = (caller.paths.clone(), decision_id.to_string(), note.to_string());
    let o = blocking(move || override_decision(&paths, &id, &n, &crate::commands::workspace::iso_now())).await?;
    log::info!("hive {}: the human overrode decision {}{}", caller.hive.id, o.decision.id, if o.retired { ", retiring its precedent" } else { "" });
    if let Some(c) = super::tools::coordinator_tab(ctx.data, caller) {
        let sent = ctx.window.deliver(&c, override_notice(&o), "Hive override").await;
        log::info!("hive {}: override notice to the coordinator: {}", caller.hive.id, sent.describe());
    }
    ctx.window.decisions_changed(&caller.workspace_id);
    let paths = caller.paths.clone();
    let id = o.decision.id.clone();
    blocking(move || view(&paths).into_iter().find(|v| v.id == id).ok_or_else(|| "The decision log couldn\u{2019}t be read back.".to_string())).await
}

/// The human undid a land (§8.6) whose decision was the coordinator's: that decision overridden.
/// `decision` is the one its land log line names, the decision that landed that repo: a land
/// resumed on a new pin keeps its id under a new decision, so the land id alone can't tell.
pub(crate) async fn land_undone_by_human<W: HiveWindow>(ctx: &Ctx<'_, W>, caller: &Caller, land_id: &str, decision: Option<&str>) {
    let Some(want) = decision.map(str::to_string) else { return };
    let paths = caller.paths.clone();
    let found = blocking(move || Ok(fold(&read(&paths)).into_iter().find(|d| d.id == want && d.can_override()).map(|d| d.id))).await;
    if let Ok(Some(id)) = found {
        if let Err(e) = override_in(ctx, caller, &id, "Undone by your human").await {
            log::warn!("hive {}: undo of land {land_id} not logged as an override: {e}", caller.hive.id);
        }
    }
}

#[tauri::command]
pub async fn hive_decision_log(window: tauri::Window, state: tauri::State<'_, Arc<AppState>>, workspace_id: String) -> Result<Vec<DecisionView>, String> {
    let caller = super::land::caller_in(state.inner(), window.label(), &workspace_id)?;
    blocking(move || Ok(view(&caller.paths))).await
}

/// The human's Override… on one of the coordinator's decisions (plan D17), with their note.
#[tauri::command]
pub async fn hive_override_decision(
    window: tauri::Window,
    state: tauri::State<'_, Arc<AppState>>,
    app: AppHandle,
    workspace_id: String,
    decision_id: String,
    note: String,
) -> Result<DecisionView, String> {
    let caller = super::land::caller_in(state.inner(), window.label(), &workspace_id)?;
    let ctx = super::tools::app_ctx(state.inner(), &app, &caller);
    override_in(&ctx, &caller, &decision_id, &note).await
}

// ── Lands and resolutions (step 7d) ──

/// "Land “Title” (web, api)": what a land decides, in maiTerm's words.
fn land_question(w: &WorkerRecord, land: &LandState) -> String {
    let folders: Vec<&str> = land.repos.iter().map(|r| r.folder.as_str()).collect();
    format!("Land \u{201c}{}\u{201d} ({})", one_line(&w.title, 120), folders.join(", "))
}

fn land_line(hive_id: &str, w: &WorkerRecord, land: &LandState, id: &str, event: &str, category: &str, by: &str) -> DecisionLine {
    DecisionLine {
        id: id.to_string(),
        event: event.to_string(),
        hive_id: hive_id.to_string(),
        category: category.to_string(),
        source: SOURCE_LAND.to_string(),
        task_id: Some(w.task_id.clone()),
        worker: Some(w.slug.clone()),
        key: Some(land.id.clone()),
        question: land_question(w, land),
        options: vec![DecisionOption { label: ANSWER_LAND.to_string(), recommended: true }, DecisionOption { label: ANSWER_DONT_LAND.to_string(), recommended: false }],
        precedent: land.precedent.clone(),
        by: by.to_string(),
        at: crate::commands::workspace::iso_now(),
        ..DecisionLine::default()
    }
}

/// The coordinator's own call to land under its Landing checkpoint (`land.decision`).
pub(crate) fn land_decided(paths: &HivePaths, hive_id: &str, w: &WorkerRecord, land: &LandState) {
    let Some(id) = land.decision.as_deref() else { return };
    let mut l = land_line(hive_id, w, land, id, EVENT_ANSWERED, super::checkpoints::LANDING, BY_COORDINATOR);
    l.answer = Some(ANSWER_LAND.to_string());
    l.outcome = Some("running".to_string());
    record(paths, &l);
}

/// The category of a land card by why it asks: Landing = Ask me is `landing`, which can be
/// earned; the config rule and the integration mode are never the coordinator's.
pub fn card_category(why: &str) -> &'static str {
    match why {
        super::land::approval::WHY_ASK => super::checkpoints::LANDING,
        super::land::approval::WHY_INTEGRATION => CATEGORY_INTEGRATION,
        _ => CATEGORY_CONFIG,
    }
}

/// A land card filed (`land.card_decision`): asked, the coordinator recommending the land.
pub(crate) fn land_card_asked(paths: &HivePaths, hive_id: &str, w: &WorkerRecord, land: &LandState, why: &str, summary: &str, effects: &[String]) {
    let Some(id) = land.card_decision.as_deref() else { return };
    let mut l = land_line(hive_id, w, land, id, EVENT_ASKED, card_category(why), BY_MAITERM);
    l.question = one_line(summary, 400);
    l.context = Some(one_line(&effects.join(" "), 1000)).filter(|c| !c.is_empty());
    l.recommended = Some(ANSWER_LAND.to_string());
    record(paths, &l);
}

/// A land card's end: the human's approval or denial, or a lapse (`expired`, or withdrawn with
/// `why`).
pub(crate) fn land_card_closed(paths: &HivePaths, hive_id: &str, w: &WorkerRecord, land: &LandState, status: &str, why: &str) {
    let Some(id) = land.card_decision.as_deref() else { return };
    let category = card_category(land.approval.as_deref().unwrap_or_default());
    let mut l = match status {
        "approved" | "denied" => {
            let mut l = land_line(hive_id, w, land, id, EVENT_ANSWERED, category, BY_HUMAN);
            l.answer = Some(if status == "approved" { ANSWER_LAND } else { ANSWER_DONT_LAND }.to_string());
            l
        }
        _ => {
            let mut l = land_line(hive_id, w, land, id, EVENT_LAPSED, category, BY_MAITERM);
            l.reason = Some(one_line(why, 300));
            l
        }
    };
    l.recommended = Some(ANSWER_LAND.to_string());
    record(paths, &l);
}

/// An outcome-only line on one decision: a land's result, after its decision.
pub(crate) fn outcome(paths: &HivePaths, hive_id: &str, id: &str, source: &str, key: Option<&str>, outcome: &str) {
    let l = DecisionLine {
        id: id.to_string(),
        event: EVENT_ANSWERED.to_string(),
        hive_id: hive_id.to_string(),
        source: source.to_string(),
        key: key.map(str::to_string),
        outcome: Some(one_line(outcome, 300)),
        at: crate::commands::workspace::iso_now(),
        ..DecisionLine::default()
    };
    record(paths, &l);
}

/// A land's result on each decision that is about it: the coordinator's and its card's.
pub(crate) fn land_outcome(paths: &HivePaths, hive_id: &str, land: &LandState, text: &str) {
    for id in [&land.decision, &land.card_decision].into_iter().flatten() {
        outcome(paths, hive_id, id, SOURCE_LAND, Some(&land.id), text);
    }
}

/// The coordinator's `hiveResolve` of an escalated review: its own decision, `other`.
pub(crate) fn resolution_decided(paths: &HivePaths, hive_id: &str, w: &WorkerRecord, kind: &str, reason: &str, precedent: Option<&str>) {
    let l = DecisionLine {
        id: new_id(),
        event: EVENT_ANSWERED.to_string(),
        hive_id: hive_id.to_string(),
        category: CATEGORY_OTHER.to_string(),
        source: SOURCE_RESOLVE.to_string(),
        task_id: Some(w.task_id.clone()),
        worker: Some(w.slug.clone()),
        question: format!("Worker #{} \u{201c}{}\u{201d} failed {} reviews: how is it settled?", w.badge, one_line(&w.title, 120), super::review::MAX_ATTEMPTS),
        options: super::review::RESOLUTIONS.iter().map(|r| DecisionOption { label: r.to_string(), recommended: false }).collect(),
        answer: Some(kind.to_string()),
        reason: Some(one_line(reason, super::prompts::MAX_REASON)).filter(|r| !r.is_empty()),
        by: BY_COORDINATOR.to_string(),
        precedent: precedent.map(str::to_string),
        at: crate::commands::workspace::iso_now(),
        ..DecisionLine::default()
    };
    record(paths, &l);
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

    fn line(id: &str, event: &str, by: &str) -> DecisionLine {
        DecisionLine { id: id.into(), event: event.into(), by: by.into(), hive_id: "h1".into(), category: "questions".into(), question: format!("Q {id}"), ..DecisionLine::default() }
    }

    #[test]
    fn the_fold_is_newest_first_and_an_outcome_only_line_changes_only_the_outcome() {
        let lines = vec![
            DecisionLine { recommended: Some("Redis".into()), worker: Some("w".into()), ..line("D-a", EVENT_ASKED, BY_MAITERM) },
            DecisionLine { category: "landing".into(), source: SOURCE_LAND.into(), answer: Some("Land".into()), outcome: Some("running".into()), precedent: Some("P-1".into()), ..line("D-land", EVENT_ANSWERED, BY_COORDINATOR) },
            DecisionLine { recommended: Some("Postgres".into()), answer: Some("Postgres".into()), ..line("D-a", EVENT_RECOMMENDED, BY_COORDINATOR) },
            line("D-b", EVENT_ASKED, BY_MAITERM),
            DecisionLine { answer: Some("Postgres".into()), recommended: Some("Postgres".into()), after_timeout: true, ..line("D-a", EVENT_ANSWERED, BY_HUMAN) },
            // The land's result, later: the outcome, nothing else.
            DecisionLine { source: SOURCE_LAND.into(), outcome: Some("landed".into()), ..line("D-land", EVENT_ANSWERED, BY_MAITERM) },
            DecisionLine { reason: Some("closed".into()), ..line("D-b", EVENT_LAPSED, BY_MAITERM) },
            DecisionLine { event: EVENT_DIGEST.into(), ..line("D-dig", EVENT_DIGEST, BY_MAITERM) },
        ];
        let folded = fold(&lines);
        assert_eq!(folded.iter().map(|d| d.id.as_str()).collect::<Vec<_>>(), vec!["D-b", "D-a", "D-land"], "by its latest line; outcome updates and digests aside");
        let land = folded.iter().find(|d| d.id == "D-land").unwrap();
        assert_eq!((land.outcome.as_deref(), land.answered_by(), land.answered.as_ref().unwrap().answer.as_deref()), (Some("landed"), Some(BY_COORDINATOR), Some("Land")));
        assert!(land.can_override());

        let views: Vec<DecisionView> = folded.iter().map(|d| view_of(d, |id| (id == "P-1").then(|| ("When green, land".to_string(), "active".to_string())))).collect();
        let (b, a, l) = (&views[0], &views[1], &views[2]);
        assert_eq!((b.status.as_str(), b.by_label.as_str(), b.reason.as_deref(), b.can_override), ("lapsed", "", Some("closed"), false));
        assert_eq!((a.status.as_str(), a.by_label.as_str(), a.answer.as_deref()), ("answered", "human, after the coordinator\u{2019}s timeout", Some("Postgres")));
        assert_eq!(a.recommended.as_deref(), Some("Postgres"), "the coordinator's, never the worker's");
        assert!(!a.can_override, "the human's own answer");
        assert_eq!((l.by_label.as_str(), l.outcome.as_deref(), l.source.as_str()), ("coordinator \u{b7} by precedent", Some("landed"), "land"));
        assert_eq!(l.precedent.as_ref().map(|p| (p.id.as_str(), p.rule.as_deref())), Some(("P-1", Some("When green, land"))));
        // Open, and recommended under Ask me, before anyone answers.
        let open = fold(&lines[..3]);
        let a = view_of(open.iter().find(|d| d.id == "D-a").unwrap(), |_| None);
        assert_eq!((a.status.as_str(), a.recommended.as_deref()), ("recommended", Some("Postgres")));
        assert_eq!(view_of(&fold(&lines[..1])[0], |_| None).recommended, None, "a worker's own recommended option isn't the coordinator's");
    }

    #[test]
    fn an_override_is_the_humans_on_the_coordinators_decision_once_and_counts_against_its_precedent() {
        let (base, paths) = scratch();
        let p = crate::hive::precedents::Precedent { id: "P-0000aaaa".into(), category: "questions".into(), ..Default::default() };
        crate::hive::precedents::save_in(&paths.precedents(), &p).unwrap();
        for (id, by) in [("D-c1", BY_COORDINATOR), ("D-c2", BY_COORDINATOR), ("D-h", BY_HUMAN)] {
            append(&paths, &DecisionLine { answer: Some("Yes".into()), precedent: (by == BY_COORDINATOR).then(|| "P-0000aaaa".into()), ..line(id, EVENT_ANSWERED, by) }).unwrap();
        }
        assert!(override_decision(&paths, "D-c1", "  ", "now").unwrap_err().contains("Say what should have happened"));
        let o = override_decision(&paths, "D-c1", "No:\nnot on prod", "now").unwrap();
        assert_eq!((o.note.as_str(), o.precedent.as_deref(), o.retired), ("No: not on prod", Some("P-0000aaaa"), false));
        let l = read(&paths).pop().unwrap();
        assert_eq!((l.event.as_str(), l.by.as_str(), l.reason.as_deref(), l.answer.as_deref()), (EVENT_OVERRIDDEN, BY_HUMAN, Some("No: not on prod"), Some("Yes")));
        assert!(override_decision(&paths, "D-c1", "again", "now").unwrap_err().contains("already overridden"));
        assert!(override_decision(&paths, "D-h", "x", "now").unwrap_err().contains("your own answer"));
        assert!(override_decision(&paths, "D-zz", "x", "now").unwrap_err().contains("isn\u{2019}t in this Hive"));
        let o = override_decision(&paths, "D-c2", "Ask me first", "now").unwrap();
        assert!(o.retired, "the second override of its precedent retires it");
        assert_eq!(crate::hive::precedents::get(&paths, "P-0000aaaa").unwrap().status, crate::hive::precedents::STATUS_RETIRED);
        let notice = override_notice(&o);
        assert!(notice.starts_with("[maiTerm] Your human overrode your decision D-c2: Ask me first (precedent P-0000aaaa is retired)"), "{notice}");
        assert!(notice.contains("\nThe decision: Q D-c2") && notice.contains("\nYou answered: Yes"), "{notice}");
        let v = view(&paths);
        let c1 = v.iter().find(|d| d.id == "D-c1").unwrap();
        assert!(c1.overridden && !c1.can_override && c1.override_note.as_deref() == Some("No: not on prod"));
        assert_eq!(c1.precedent.as_ref().and_then(|p| p.status.as_deref()), Some("retired"));
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn a_task_id_names_its_latest_human_answer() {
        let lines = vec![
            DecisionLine { task_id: Some("t-1".into()), answer: Some("A".into()), ..line("D-1", EVENT_ANSWERED, BY_HUMAN) },
            DecisionLine { task_id: Some("t-1".into()), answer: Some("Yes".into()), ..line("D-2", EVENT_ANSWERED, BY_COORDINATOR) },
            DecisionLine { task_id: Some("t-1".into()), ..line("D-3", EVENT_ASKED, BY_MAITERM) },
            DecisionLine { task_id: Some("t-1".into()), answer: Some("B".into()), ..line("D-3", EVENT_ANSWERED, BY_HUMAN) },
            DecisionLine { task_id: Some("t-2".into()), answer: Some("C".into()), ..line("D-4", EVENT_ANSWERED, BY_HUMAN) },
        ];
        assert_eq!(human_answer(&lines, "t-1").unwrap().id, "D-3");
        assert_eq!(human_answer(&lines, "D-1").unwrap().id, "D-1");
        assert!(human_answer(&lines, "D-2").is_err());
        assert!(human_answer(&lines, "t-9").is_err());
    }
}
