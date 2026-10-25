//! Precedents (docs/hive.md §7.5): what the human decided, kept so the coordinator decides the
//! same way next time and says so. Rust-owned storage, one JSON file each: a Hive's in
//! `<hive>/precedents/`, the global ones (only ever the human's promotion) in
//! `<app data>/hive/precedents/`, both on every agent's denyWrite (plan D25).
//!
//! A precedent is recorded only from an answer the human gave (an `answered` line by `human` in
//! the decision log), and Rust copies every field of it but the coordinator's `rule` and `label`.
//! `cited` and `overridden` are Rust's, counted in the call that logs the decision (plan D19); two
//! overrides retire a precedent, which is kept and never offered again.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard};

use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};

use super::decisions::{self, DecisionOption};
use super::deliver::one_line;
use super::paths::HivePaths;
use super::rpc::HiveWindow;
use super::tools::{blocking, Caller, Ctx};
use crate::state::AppState;

pub const PRECEDENT_VERSION: u32 = 1;
pub const SCOPE_HIVE: &str = "hive";
pub const SCOPE_GLOBAL: &str = "global";
pub const STATUS_ACTIVE: &str = "active";
pub const STATUS_RETIRED: &str = "retired";
pub const RULE_BY_COORDINATOR: &str = "coordinator";
pub const RULE_BY_HUMAN: &str = "human";
pub const MAX_RULE: usize = 400;
pub const MAX_LABEL: usize = 60;
/// `hivePrecedents` answers at most this many: the relevant ones, not the whole store.
pub const MAX_LISTED: usize = 20;
/// Overridden this often, a precedent is retired (§7.5).
pub const RETIRE_AT: u32 = 2;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Precedent {
    pub v: u32,
    /// `P-<8hex>`.
    pub id: String,
    /// The Hive it came from: a global copy keeps it, so a citation names its source.
    pub hive_id: String,
    pub hive_name: String,
    /// The decision-log decision it records.
    pub decision_id: String,
    pub blocker_key: Option<String>,
    pub asked_at: String,
    pub answered_at: String,
    /// Rust's: the decision's category (§7.5).
    pub category: String,
    /// The coordinator's free label; never counted.
    pub label: Option<String>,
    pub question: String,
    pub context: Option<String>,
    pub options: Vec<DecisionOption>,
    pub recommended: Option<String>,
    /// The human's answer, and their words.
    pub answer: String,
    pub reason: Option<String>,
    /// "When X, choose Y, because Z", and whose wording it is: `coordinator` | `human`.
    pub rule: String,
    pub rule_by: String,
    /// `hive` | `global`.
    pub scope: String,
    /// `active` | `retired`.
    pub status: String,
    pub cited: u32,
    pub overridden: u32,
    /// The global copy the human made of it.
    pub promoted_to: Option<String>,
    pub created_at: String,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

impl Default for Precedent {
    fn default() -> Self {
        Self {
            v: PRECEDENT_VERSION,
            id: String::new(),
            hive_id: String::new(),
            hive_name: String::new(),
            decision_id: String::new(),
            blocker_key: None,
            asked_at: String::new(),
            answered_at: String::new(),
            category: String::new(),
            label: None,
            question: String::new(),
            context: None,
            options: Vec::new(),
            recommended: None,
            answer: String::new(),
            reason: None,
            rule: String::new(),
            rule_by: RULE_BY_COORDINATOR.to_string(),
            scope: SCOPE_HIVE.to_string(),
            status: STATUS_ACTIVE.to_string(),
            cited: 0,
            overridden: 0,
            promoted_to: None,
            created_at: String::new(),
            extra: Map::new(),
        }
    }
}

impl Precedent {
    pub fn is_active(&self) -> bool {
        self.status == STATUS_ACTIVE
    }
}

pub fn new_id() -> String {
    format!("P-{}", &uuid::Uuid::new_v4().simple().to_string()[..8])
}

pub fn is_id(s: &str) -> bool {
    s.len() == 10 && s.starts_with("P-") && s[2..].bytes().all(|b| b.is_ascii_hexdigit())
}

/// One writer at a time, for every Hive's store and the global one: a count is read and written
/// back whole.
static STORE: Mutex<()> = Mutex::new(());

fn store() -> MutexGuard<'static, ()> {
    STORE.lock().unwrap_or_else(|e| e.into_inner())
}

fn file_in(dir: &Path, id: &str) -> Result<PathBuf, String> {
    if !is_id(id) {
        return Err(format!("\u{201c}{}\u{201d} isn\u{2019}t a precedent id (P- and 8 hex digits).", one_line(id, 40)));
    }
    Ok(dir.join(format!("{id}.json")))
}

/// Every precedent in `dir` that parses, newest first.
pub fn list_in(dir: &Path) -> Vec<Precedent> {
    let Ok(entries) = std::fs::read_dir(dir) else { return Vec::new() };
    let mut out: Vec<Precedent> = entries
        .flatten()
        .filter_map(|e| {
            let name = e.file_name().to_string_lossy().to_string();
            let id = name.strip_suffix(".json").filter(|s| is_id(s))?.to_string();
            let p: Precedent = serde_json::from_slice(&std::fs::read(e.path()).ok()?).ok()?;
            (p.id == id).then_some(p)
        })
        .collect();
    out.sort_by(|a, b| b.created_at.cmp(&a.created_at).then_with(|| b.id.cmp(&a.id)));
    out
}

pub fn get_in(dir: &Path, id: &str) -> Option<Precedent> {
    let p: Precedent = serde_json::from_slice(&std::fs::read(file_in(dir, id).ok()?).ok()?).ok()?;
    (p.id == id).then_some(p)
}

pub fn save_in(dir: &Path, p: &Precedent) -> Result<(), String> {
    let v = serde_json::to_value(p).map_err(|e| e.to_string())?;
    crate::hive::launch::write_json(&file_in(dir, &p.id)?, &v)
}

/// Whether there was one to delete.
pub fn delete_in(dir: &Path, id: &str) -> Result<bool, String> {
    match std::fs::remove_file(file_in(dir, id)?) {
        Ok(()) => Ok(true),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(e) => Err(format!("precedent {id}: {e}")),
    }
}

/// A precedent and its folder: this Hive's first, then the global one.
fn locate(paths: &HivePaths, id: &str) -> Option<(PathBuf, Precedent)> {
    [paths.precedents(), paths.global_precedents()].into_iter().find_map(|dir| get_in(&dir, id).map(|p| (dir, p)))
}

pub fn get(paths: &HivePaths, id: &str) -> Option<Precedent> {
    locate(paths, id).map(|(_, p)| p)
}

/// A Hive precedent the human made global whose global copy still exists, retired or not: that
/// copy is the live one, and this one is history (never offered, cited or counted).
fn superseded(p: &Precedent, global: &[Precedent]) -> bool {
    p.promoted_to.as_ref().is_some_and(|g| global.iter().any(|x| &x.id == g))
}

/// `locate`, a promoted Hive precedent resolved to its global copy: citations, overrides and a
/// retirement land on the one the coordinator is offered.
fn resolve(paths: &HivePaths, id: &str) -> Option<(PathBuf, Precedent)> {
    let (dir, p) = locate(paths, id)?;
    let global = paths.global_precedents();
    if dir != global {
        if let Some(g) = p.promoted_to.as_deref().and_then(|g| get_in(&global, g)) {
            return Some((global, g));
        }
    }
    Some((dir, p))
}

/// The precedent a citation of `id` counts for (`resolve`).
pub fn live(paths: &HivePaths, id: &str) -> Option<Precedent> {
    resolve(paths, id).map(|(_, p)| p)
}

/// What the coordinator is offered: this Hive's active precedents but the superseded ones, then
/// the active global ones.
fn offered(paths: &HivePaths) -> (Vec<Precedent>, Vec<Precedent>) {
    let global = list_in(&paths.global_precedents());
    let local = list_in(&paths.precedents()).into_iter().filter(|p| p.is_active() && !superseded(p, &global)).collect();
    (local, global.into_iter().filter(Precedent::is_active).collect())
}

fn update(paths: &HivePaths, id: &str, f: impl FnOnce(&mut Precedent)) -> Result<Precedent, String> {
    let _held = store();
    let (dir, mut p) = resolve(paths, id).ok_or_else(|| format!("There\u{2019}s no precedent {}.", one_line(id, 40)))?;
    f(&mut p);
    save_in(&dir, &p)?;
    Ok(p)
}

/// A citation as given (`precedent` on `hiveAnswerWorker`, `hiveLand`, `hiveResolve`): an active
/// precedent of this Hive or a global one, or why not. A promoted one is cited as its global copy.
pub fn check_citation(paths: &HivePaths, id: &str) -> Result<Precedent, String> {
    let id = id.trim();
    let p = live(paths, id).ok_or_else(|| format!("There\u{2019}s no precedent {}: hivePrecedents lists the ones you may cite.", one_line(id, 40)))?;
    if !p.is_active() {
        let named = if p.id == id { id.to_string() } else { format!("{id} (global as {})", p.id) };
        return Err(format!("Precedent {named} is retired: your human overrode it {} times, so it settles nothing now.", p.overridden));
    }
    Ok(p)
}

/// The `precedent` argument checked (absent or empty: none).
pub(crate) async fn cited(paths: &HivePaths, args: &Value) -> Result<Option<String>, String> {
    let Some(id) = args.get("precedent").and_then(Value::as_str).map(str::trim).filter(|s| !s.is_empty()).map(str::to_string) else { return Ok(None) };
    let p = paths.clone();
    blocking(move || check_citation(&p, &id).map(|x| Some(x.id))).await
}

pub fn cite(paths: &HivePaths, id: &str) -> Result<Precedent, String> {
    update(paths, id, |p| p.cited += 1)
}

/// An override counted against a precedent (§7.5): whether this one retired it.
pub fn override_(paths: &HivePaths, id: &str) -> Result<bool, String> {
    let mut retired = false;
    update(paths, id, |p| {
        p.overridden += 1;
        if p.overridden >= RETIRE_AT && p.is_active() {
            p.status = STATUS_RETIRED.to_string();
            retired = true;
        }
    })?;
    Ok(retired)
}

/// `cite`, logged rather than returned: the decision it counts for has gone through.
pub(crate) fn count_citation(paths: &HivePaths, id: Option<&str>) {
    if let Some(id) = id {
        if let Err(e) = cite(paths, id) {
            log::warn!("hive: citation of precedent {id} not counted: {e}");
        }
    }
}

/// The brief's counts by category (§7.5): active precedents only.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct PrecedentCounts {
    pub hive: BTreeMap<String, u32>,
    pub global: BTreeMap<String, u32>,
}

impl PrecedentCounts {
    pub fn is_empty(&self) -> bool {
        self.hive.is_empty() && self.global.is_empty()
    }
}

fn by_category(list: &[Precedent]) -> BTreeMap<String, u32> {
    let mut out = BTreeMap::new();
    for p in list.iter().filter(|p| p.is_active()) {
        *out.entry(p.category.clone()).or_insert(0) += 1;
    }
    out
}

/// Counted as `hivePrecedents` offers them: a promoted one once, as its global copy.
pub fn counts(paths: &HivePaths) -> PrecedentCounts {
    let (local, global) = offered(paths);
    PrecedentCounts { hive: by_category(&local), global: by_category(&global) }
}

// ── hiveRecordPrecedent ──

/// `rule` and `label` as the coordinator gave them, or why not.
pub fn check_rule(rule: &str) -> Result<String, String> {
    let rule = rule.trim();
    if rule.is_empty() {
        return Err("rule is required: \u{201c}When X, choose Y, because Z\u{201d}, in one line.".to_string());
    }
    if rule.contains(['\n', '\r', '\u{2028}', '\u{2029}']) {
        return Err("Keep the rule to one line.".to_string());
    }
    if rule.chars().count() > MAX_RULE {
        return Err(format!("Keep the rule under {MAX_RULE} characters."));
    }
    Ok(one_line(rule, MAX_RULE))
}

#[derive(Debug, Clone, PartialEq)]
pub struct RecordArgs {
    pub blocker_id: String,
    pub rule: String,
    pub label: Option<String>,
}

pub fn parse_record_args(args: &Value) -> Result<RecordArgs, String> {
    // A global precedent steers every Hive: only the human's click makes one (§7.5).
    if let Some(scope) = args.get("scope").filter(|v| !v.is_null()) {
        if scope.as_str().map(str::trim) != Some(SCOPE_HIVE) {
            return Err("Only scope \u{201c}hive\u{201d} is yours: going global is your human\u{2019}s click in the Memory view. Suggest it to them in your chat.".to_string());
        }
    }
    let blocker_id = args.get("blocker_id").and_then(Value::as_str).map(str::trim).filter(|s| !s.is_empty()).ok_or("blocker_id is required: the task id of the blocker your human answered, or a decision id (D-…).")?;
    let rule = check_rule(args.get("rule").and_then(Value::as_str).unwrap_or_default())?;
    let label = match args.get("label").and_then(Value::as_str).map(str::trim).filter(|s| !s.is_empty()) {
        Some(l) if l.chars().count() > MAX_LABEL => return Err(format!("Keep the label under {MAX_LABEL} characters.")),
        Some(l) => Some(one_line(l, MAX_LABEL)),
        None => None,
    };
    Ok(RecordArgs { blocker_id: blocker_id.to_string(), rule, label })
}

/// A precedent recorded from the human's answer that `a.blocker_id` names: every field but the
/// rule and label copied from the decision log.
pub fn record_in(paths: &HivePaths, hive_id: &str, hive_name: &str, a: &RecordArgs, now: &str) -> Result<Precedent, String> {
    let lines = decisions::read(paths);
    let d = decisions::human_answer(&lines, &a.blocker_id)?;
    if d.category() == decisions::CATEGORY_OFFER {
        return Err("An offer of more autonomy isn\u{2019}t a decision to learn from: it never becomes a precedent.".to_string());
    }
    let answered = d.answered.as_ref().expect("human_answer has an answer");
    let _held = store();
    // Its global copy counts too: the Hive's own may have been deleted after Make global.
    let mine = |p: &Precedent| p.decision_id == d.id && (p.scope != SCOPE_GLOBAL || p.hive_id == hive_id);
    if let Some(p) = list_in(&paths.precedents()).into_iter().chain(list_in(&paths.global_precedents())).find(mine) {
        return Err(format!("That answer is already recorded as {}: one precedent per decision.", p.id));
    }
    let first = d.first();
    let p = Precedent {
        id: new_id(),
        hive_id: hive_id.to_string(),
        hive_name: one_line(hive_name, 120),
        decision_id: d.id.clone(),
        blocker_key: answered.key.clone(),
        asked_at: first.map(|l| l.at.clone()).unwrap_or_default(),
        answered_at: answered.at.clone(),
        category: d.category().to_string(),
        label: a.label.clone(),
        question: d.question().to_string(),
        context: d.asked.as_ref().and_then(|l| l.context.clone()),
        options: d.asked.as_ref().map(|l| l.options.clone()).filter(|o| !o.is_empty()).unwrap_or_else(|| answered.options.clone()),
        recommended: answered.recommended.clone().or_else(|| d.asked.as_ref().and_then(|l| l.recommended.clone())),
        answer: answered.answer.clone().unwrap_or_default(),
        reason: answered.reason.clone(),
        rule: a.rule.clone(),
        rule_by: RULE_BY_COORDINATOR.to_string(),
        created_at: now.to_string(),
        ..Precedent::default()
    };
    save_in(&paths.precedents(), &p)?;
    Ok(p)
}

pub fn record_tool_definition(tab_id: &Value) -> Value {
    json!({
        "name": "hiveRecordPrecedent",
        "description": format!(
            "Record what one of your human's answers teaches, so you can decide the same way next time and cite it. blocker_id is \
             the task id of a blocker your human answered (its latest answer), or a decision id (D-…) from the log. maiTerm copies \
             the question, options, your recommendation and your human's answer from its own log; you give only rule, one line \
             (\"When X, choose Y, because Z\", at most {MAX_RULE} characters), and an optional label (at most {MAX_LABEL}). Refused \
             for anything your human didn't answer themselves, for an answer already recorded, and for scope global: only your \
             human makes a precedent global, in the Memory view (you may suggest it in your chat)."
        ),
        "inputSchema": {
            "type": "object",
            "properties": {
                "tabId": tab_id,
                "blocker_id": { "type": "string", "description": "The answered blocker's task id, or a decision id (D-…)" },
                "rule": { "type": "string", "description": "When X, choose Y, because Z: one line" },
                "label": { "type": "string", "description": "A short label for searching (\"visual style\")" }
            },
            "required": ["blocker_id", "rule"]
        }
    })
}

pub(crate) async fn record_tool<W: HiveWindow>(ctx: &Ctx<'_, W>, caller: &Caller, args: &Value) -> Result<Value, String> {
    let a = parse_record_args(args)?;
    let (paths, hive_id, name) = (caller.paths.clone(), caller.hive.id.clone(), caller.workstream.clone());
    let p = blocking(move || record_in(&paths, &hive_id, &name, &a, &crate::commands::workspace::iso_now())).await?;
    log::info!("hive {}: precedent {} recorded from {} ({})", caller.hive.id, p.id, p.decision_id, p.category);
    ctx.window.decisions_changed(&caller.workspace_id);
    Ok(json!({
        "recorded": p.id,
        "precedent": listed(&p, &caller.hive.id),
        "note": format!(
            "Cite it as precedent: \"{}\" when it settles a decision, and say so in one line in your chat. Your human sees it beside \
             their answer in Memory \u{2192} Precedents, may edit or delete it, and only they can make it global.",
            p.id
        ),
    }))
}

// ── hivePrecedents ──

pub fn list_tool_definition(tab_id: &Value) -> Value {
    json!({
        "name": "hivePrecedents",
        "description": format!(
            "Your human's precedents that bear on a decision: this Hive's first, then global ones (carried from other Hives by your \
             human), at most {MAX_LISTED}, newest first, retired ones never. category filters by where a decision came from \
             (questions, permissions, landing, plan, other…); query matches label, question, rule and answer. Each has cite_as: \
             pass it as precedent to hiveAnswerWorker, hiveLand or hiveResolve when it settles the decision. Also answers how many \
             there are by category."
        ),
        "inputSchema": {
            "type": "object",
            "properties": {
                "tabId": tab_id,
                "category": { "type": "string", "description": "Only this category" },
                "query": { "type": "string", "description": "Words to look for" }
            },
            "required": []
        }
    })
}

/// One precedent as the coordinator reads it: whose words are whose, said.
fn listed(p: &Precedent, hive_id: &str) -> Value {
    let mut v = json!({
        "cite_as": p.id,
        "category": p.category,
        "label": p.label,
        "question": p.question,
        "options": p.options.iter().map(|o| o.label.clone()).collect::<Vec<_>>(),
        "answer": p.answer,
        "answer_by": "your human\u{2019}s answer",
        "reason": p.reason,
        "rule": p.rule,
        "rule_wording": if p.rule_by == RULE_BY_HUMAN { "your human\u{2019}s wording" } else { "the coordinator\u{2019}s wording" },
        "asked_at": p.asked_at,
        "cited": p.cited,
        "overridden": p.overridden,
    });
    if p.scope == SCOPE_GLOBAL || p.hive_id != hive_id {
        v["from_hive"] = json!(p.hive_name);
    }
    v
}

fn matches(p: &Precedent, category: Option<&str>, query: Option<&str>) -> bool {
    if category.is_some_and(|c| p.category != c) {
        return false;
    }
    let Some(q) = query.map(str::to_lowercase) else { return true };
    [p.label.as_deref().unwrap_or_default(), &p.question, &p.rule, &p.answer].iter().any(|s| s.to_lowercase().contains(&q))
}

/// `hivePrecedents`: the active ones that match, this Hive's before global, at most `MAX_LISTED`.
/// A Hive precedent the human made global is offered as its global copy, whose counts span Hives.
pub fn query_in(paths: &HivePaths, hive_id: &str, category: Option<&str>, query: Option<&str>) -> Value {
    let (local, global) = offered(paths);
    let mut categories: BTreeMap<String, u32> = BTreeMap::new();
    for p in local.iter().chain(&global) {
        *categories.entry(p.category.clone()).or_insert(0) += 1;
    }
    let found: Vec<Value> = local.iter().chain(&global).filter(|p| matches(p, category, query)).take(MAX_LISTED).map(|p| listed(p, hive_id)).collect();
    let note = if found.is_empty() {
        "None match: decide as your checkpoints allow, or ask your human with a blocker. Once they answer, record what it teaches with hiveRecordPrecedent."
    } else {
        "Your human\u{2019}s answers, with a rule in whoever\u{2019}s wording it says. If one settles your decision, pass its cite_as as precedent and cite it in one line in your chat."
    };
    json!({ "precedents": found, "categories": categories, "note": note })
}

pub(crate) async fn list_tool(caller: &Caller, args: &Value) -> Result<Value, String> {
    let arg = |k: &str| args.get(k).and_then(Value::as_str).map(str::trim).filter(|s| !s.is_empty()).map(|s| one_line(s, 200));
    let (category, query) = (arg("category"), arg("query"));
    let (paths, id) = (caller.paths.clone(), caller.hive.id.clone());
    blocking(move || Ok(query_in(&paths, &id, category.as_deref(), query.as_deref()))).await
}

// ── The human's commands ──

/// The Precedents tab: this Hive's and the global ones, retired included.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct PrecedentsView {
    pub hive: Vec<Precedent>,
    pub global: Vec<Precedent>,
}

pub fn view(paths: &HivePaths) -> PrecedentsView {
    PrecedentsView { hive: list_in(&paths.precedents()), global: list_in(&paths.global_precedents()) }
}

/// The human's rewording: the rule is theirs from now on. A promoted one's global copy, the one
/// the coordinator is offered, takes it too.
pub fn edit(paths: &HivePaths, id: &str, rule: &str) -> Result<Precedent, String> {
    let rule = check_rule(rule)?;
    let reword = |p: &mut Precedent| {
        p.rule = rule.clone();
        p.rule_by = RULE_BY_HUMAN.to_string();
    };
    let _held = store();
    let (dir, mut p) = locate(paths, id).ok_or_else(|| format!("There\u{2019}s no precedent {}.", one_line(id, 40)))?;
    reword(&mut p);
    save_in(&dir, &p)?;
    let global = paths.global_precedents();
    if dir != global {
        if let Some(mut g) = p.promoted_to.as_deref().and_then(|g| get_in(&global, g)) {
            reword(&mut g);
            save_in(&global, &g)?;
        }
    }
    Ok(p)
}

pub fn delete(paths: &HivePaths, id: &str) -> Result<(), String> {
    let _held = store();
    let (dir, _) = locate(paths, id).ok_or_else(|| format!("There\u{2019}s no precedent {}.", one_line(id, 40)))?;
    delete_in(&dir, id).map(|_| ())
}

/// The human's Make global: a copy in the global folder that every Hive's coordinator may cite,
/// naming this Hive as its source. Its counts start afresh; the Hive's own keeps its history.
pub fn promote(paths: &HivePaths, id: &str, now: &str) -> Result<Precedent, String> {
    let _held = store();
    let mut p = get_in(&paths.precedents(), id).ok_or_else(|| format!("{} isn\u{2019}t one of this Hive\u{2019}s precedents.", one_line(id, 40)))?;
    if !p.is_active() {
        return Err(format!("Precedent {id} is retired: it was overridden {} times, so it isn\u{2019}t carried to other Hives.", p.overridden));
    }
    let global = paths.global_precedents();
    if let Some(g) = p.promoted_to.as_deref().and_then(|g| get_in(&global, g)) {
        return Err(format!("Precedent {id} is already global as {}.", g.id));
    }
    let copy = Precedent { id: new_id(), scope: SCOPE_GLOBAL.to_string(), cited: 0, overridden: 0, promoted_to: None, created_at: now.to_string(), ..p.clone() };
    save_in(&global, &copy)?;
    p.promoted_to = Some(copy.id.clone());
    save_in(&paths.precedents(), &p)?;
    Ok(copy)
}

async fn human_act(
    window: &tauri::Window,
    state: &Arc<AppState>,
    app: &tauri::AppHandle,
    workspace_id: &str,
    f: impl FnOnce(&HivePaths) -> Result<(), String> + Send + 'static,
) -> Result<PrecedentsView, String> {
    let caller = super::land::caller_in(state, window.label(), workspace_id)?;
    let paths = caller.paths.clone();
    let v = blocking(move || {
        f(&paths)?;
        Ok(view(&paths))
    })
    .await?;
    decisions::emit_changed(app, window.label(), workspace_id);
    Ok(v)
}

#[tauri::command]
pub async fn hive_precedents(window: tauri::Window, state: tauri::State<'_, Arc<AppState>>, workspace_id: String) -> Result<PrecedentsView, String> {
    let caller = super::land::caller_in(state.inner(), window.label(), &workspace_id)?;
    blocking(move || Ok(view(&caller.paths))).await
}

/// The human's rewording of a precedent's rule, this Hive's or a global one.
#[tauri::command]
pub async fn hive_precedent_edit(
    window: tauri::Window,
    state: tauri::State<'_, Arc<AppState>>,
    app: tauri::AppHandle,
    workspace_id: String,
    id: String,
    rule: String,
) -> Result<PrecedentsView, String> {
    human_act(&window, state.inner(), &app, &workspace_id, move |p| edit(p, &id, &rule).map(|_| ())).await
}

#[tauri::command]
pub async fn hive_precedent_delete(
    window: tauri::Window,
    state: tauri::State<'_, Arc<AppState>>,
    app: tauri::AppHandle,
    workspace_id: String,
    id: String,
) -> Result<PrecedentsView, String> {
    human_act(&window, state.inner(), &app, &workspace_id, move |p| delete(p, &id)).await
}

/// Make global (§7.5): only ever this click, never a tool.
#[tauri::command]
pub async fn hive_precedent_promote(
    window: tauri::Window,
    state: tauri::State<'_, Arc<AppState>>,
    app: tauri::AppHandle,
    workspace_id: String,
    id: String,
) -> Result<PrecedentsView, String> {
    human_act(&window, state.inner(), &app, &workspace_id, move |p| promote(p, &id, &crate::commands::workspace::iso_now()).map(|_| ())).await
}

#[cfg(test)]
mod tests;
