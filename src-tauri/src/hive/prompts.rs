//! A worker's prompt or question, routed to one asker (docs/hive.md §4.7, §7.4): its permission
//! dialog, its AskUserQuestion, or a decision blocker on its task goes to the coordinator, or to
//! the human when the checkpoint is Ask me or the prompt is outside `hiveAnswerWorker`'s
//! allowlist. A coordinator that doesn't answer in time has the human told, and either may then
//! answer; a take-over makes it the human's alone. Every route, recommendation and answer is a
//! line in the decision log.
//!
//! Routes are memory only (plan D28): a restart kills every worker's PTY, and a prompt with it. A
//! blocker outlives a restart, unrouted, so it is the human's (plan D13) until the coordinator
//! answers it, which routes it then.

use std::collections::HashMap;
use std::sync::{Arc, LazyLock, Mutex, MutexGuard};
use std::time::Duration;

use serde::Serialize;
use serde_json::{json, Value};
use tauri::AppHandle;

use super::checkpoints::{self, PERMISSIONS};
use super::decisions::{self, DecisionLine, DecisionOption};
use super::deliver::{frame_escape, one_line, Sent};
use super::record::Hive;
use super::rpc::{self, HiveWindow};
use super::tools::{blocking, Caller, Ctx};
use super::worker::{self, WorkerRecord};
use crate::mailink::Answer;
use crate::state::workspace::TaskBlocker;
use crate::state::{AppState, Task};

/// To the Hive's window whenever its routes change: who holds each open prompt.
pub const PROMPTS_CHANGED_EVENT: &str = "hive-prompts-changed";
/// To the Hive's window when a worker's prompt needs the human after all (step 7c's store
/// dispatches it, never suppressed by the gate).
pub const PROMPT_NOTIFY_EVENT: &str = "hive-prompt-notify";

pub const OWNER_COORDINATOR: &str = "coordinator";
pub const OWNER_HUMAN: &str = "human";
pub const WHY_ASK: &str = "ask";
pub const WHY_TIMEOUT: &str = "timeout";
pub const WHY_TAKE_OVER: &str = "take_over";

pub const KIND_PERMISSION: &str = "permission";
pub const KIND_QUESTION: &str = "question";
pub const KIND_TRUST: &str = "trust";
pub const KIND_BLOCKER: &str = "blocker";

/// A prompt hook comes before the hooks handler has recorded the question, and Claude's
/// permission state comes seconds after its dialog: the screen is read after this.
pub(crate) const SETTLE: Duration = Duration::from_millis(800);
pub const MAX_ANSWER: usize = 2000;
pub const MAX_REASON: usize = 500;

/// Words in a dialog that mean one of its rows persists a setting or a rule, widens the sandbox
/// or reads outside the project: the human's, whichever row the coordinator would pick (§7.4).
const HUMAN_MARKERS: [&str; 6] = ["sandbox", "from now on", "keep allowing", "block reads", "outside the working", "outside your working"];

/// A worker's open prompt as the router reads it: the screen's dialog, the question asked, or
/// the blocker on its task.
#[derive(Debug, Clone, Default, PartialEq)]
pub(crate) struct PromptView {
    /// The id the responder checks (`p_…`, `q_…`, `t_…`) or the blocker's key (`b_<task>_<asked_at>`).
    pub key: String,
    /// `permission` | `question` | `trust` | `blocker`.
    pub kind: String,
    pub tool: Option<String>,
    pub detail: Option<String>,
    /// A dialog's rows, or a blocker's option labels.
    pub options: Vec<String>,
    /// `tool` | `plan` | `bare`, or empty when the dialog can't be read.
    pub shape: String,
    /// What the dialog asks, above its rows.
    pub text: String,
    /// The AskUserQuestion's `questions`.
    pub questions: Value,
    pub blocker: Option<TaskBlocker>,
    /// A blocker's task.
    pub task_id: String,
}

impl PromptView {
    pub fn from_blocker(task_id: &str, b: &TaskBlocker) -> Self {
        Self {
            key: blocker_key(task_id, &b.asked_at),
            kind: KIND_BLOCKER.to_string(),
            options: b.options.iter().map(|o| o.label.clone()).collect(),
            blocker: Some(b.clone()),
            task_id: task_id.to_string(),
            ..Self::default()
        }
    }
}

pub fn blocker_key(task_id: &str, asked_at: &str) -> String {
    format!("b_{task_id}_{asked_at}")
}

/// One routed prompt.
#[derive(Debug, Clone, Default, PartialEq)]
pub(crate) struct Routed {
    pub hive_id: String,
    pub workspace_id: String,
    pub label: String,
    pub tab_id: String,
    pub task_id: String,
    pub badge: u32,
    /// `#2 Session store`, escaped.
    pub worker_name: String,
    pub worker_slug: String,
    pub key: String,
    pub kind: String,
    pub checkpoint: String,
    /// The decision log's category: the checkpoint, or `never` when it isn't delegable.
    pub category: String,
    pub owner: String,
    /// Why the human holds it: `ask`, `timeout`, `take_over`, or the allowlist's reason.
    pub why: String,
    pub opened_ms: i64,
    pub due_ms: i64,
    pub taken_over: bool,
    pub timed_out: bool,
    /// The coordinator was told of it: it hears how it ended.
    pub told: bool,
    pub decision_id: String,
    /// The coordinator's answer under Ask me (plan D10): `{answer, precedent, reason}`.
    pub recommendation: Option<Value>,
    /// An answer is going in now: a closing prompt isn't a lapse, and nobody else answers it.
    pub answering: bool,
    /// maiTerm's words for what is asked, for the log.
    pub question: String,
}

/// A route as the window sees it.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct RouteView {
    pub tab_id: String,
    pub task_id: String,
    pub key: String,
    pub kind: String,
    pub checkpoint: String,
    pub owner: String,
    pub why: String,
    pub opened_ms: i64,
    pub due_ms: i64,
    pub taken_over: bool,
    pub timed_out: bool,
}

impl From<&Routed> for RouteView {
    fn from(r: &Routed) -> Self {
        Self {
            tab_id: r.tab_id.clone(),
            task_id: r.task_id.clone(),
            key: r.key.clone(),
            kind: r.kind.clone(),
            checkpoint: r.checkpoint.clone(),
            owner: r.owner.clone(),
            why: r.why.clone(),
            opened_ms: r.opened_ms,
            due_ms: r.due_ms,
            taken_over: r.taken_over,
            timed_out: r.timed_out,
        }
    }
}

/// What the human is told when a worker's prompt turns out to be theirs.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub(crate) struct HumanNotice {
    pub workspace_id: String,
    pub tab_id: String,
    /// The route's kind: `permission` | `question` | `trust` | `blocker`.
    pub kind: String,
    /// `Hive · #2 Session store`.
    pub title: String,
    /// `timeout`, `take_over`, or the reason it is the human's.
    pub why: String,
}

impl HumanNotice {
    /// The doorbell kind it rings as.
    pub fn ring_kind(&self) -> &'static str {
        match self.kind.as_str() {
            KIND_QUESTION => "question",
            KIND_BLOCKER => "ask",
            _ => "permission",
        }
    }
}

/// Who holds a routed prompt, as the human's surfaces read it.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Holder {
    pub owner: String,
    pub why: String,
    pub due_ms: i64,
    pub timed_out: bool,
    pub taken_over: bool,
}

static ROUTED: LazyLock<Mutex<HashMap<String, Vec<Routed>>>> = LazyLock::new(|| Mutex::new(HashMap::new()));

fn routes() -> MutexGuard<'static, HashMap<String, Vec<Routed>>> {
    ROUTED.lock().unwrap_or_else(|e| e.into_inner())
}

fn find(tab: &str, key: &str) -> Option<Routed> {
    routes().get(tab).and_then(|v| v.iter().find(|r| r.key == key).cloned())
}

fn update(tab: &str, key: &str, f: impl FnOnce(&mut Routed)) -> Option<Routed> {
    let mut all = routes();
    let r = all.get_mut(tab)?.iter_mut().find(|r| r.key == key)?;
    f(r);
    Some(r.clone())
}

fn remove(tab: &str, key: &str) -> Option<Routed> {
    let mut all = routes();
    let v = all.get_mut(tab)?;
    let i = v.iter().position(|r| r.key == key)?;
    let r = v.remove(i);
    if v.is_empty() {
        all.remove(tab);
    }
    Some(r)
}

/// Routed once per (tab, key): two hooks reporting one dialog route it once.
fn admit(r: Routed) -> bool {
    let mut all = routes();
    let v = all.entry(r.tab_id.clone()).or_default();
    if v.iter().any(|x| x.key == r.key) {
        return false;
    }
    v.push(r);
    true
}

pub(crate) fn views_for(workspace_id: &str) -> Vec<RouteView> {
    let mut out: Vec<RouteView> = routes().values().flatten().filter(|r| r.workspace_id == workspace_id).map(RouteView::from).collect();
    out.sort_by(|a, b| a.opened_ms.cmp(&b.opened_ms).then_with(|| a.key.cmp(&b.key)));
    out
}

/// Who holds `key` on `tab`, when it is routed.
pub fn owner(tab: &str, key: &str) -> Option<(String, bool)> {
    find(tab, key).map(|r| (r.owner, r.timed_out))
}

pub(crate) fn holder(tab: &str, key: &str) -> Option<Holder> {
    find(tab, key).map(|r| Holder { owner: r.owner, why: r.why, due_ms: r.due_ms, timed_out: r.timed_out, taken_over: r.taken_over })
}

fn held(r: &Routed) -> bool {
    r.owner == OWNER_COORDINATOR && !r.timed_out && !r.taken_over
}

/// The coordinator holds it alone: not timed out, not taken over. The human's answer paths
/// refuse it then (plan D12).
pub(crate) fn coordinator_holds(tab: &str, key: &str) -> bool {
    find(tab, key).is_some_and(|r| held(&r))
}

/// For a tab's open prompt of a route kind: whether the coordinator holds it alone, or None
/// when no such prompt is routed (`hive::owns_prompt`).
pub(crate) fn coordinator_holds_kind(tab: &str, kind: &str) -> Option<bool> {
    let all = routes();
    let mine: Vec<&Routed> = all.get(tab)?.iter().filter(|r| r.kind == kind).collect();
    if mine.is_empty() {
        return None;
    }
    Some(mine.iter().all(|r| held(r)))
}

/// The keys of `tab`'s prompts the coordinator holds alone, sorted: maiLink's WS ticker diffs
/// them, since a timeout or a take-over moves nothing else it watches.
pub(crate) fn held_keys(tab: &str) -> Vec<String> {
    let mut keys: Vec<String> = routes().get(tab).into_iter().flatten().filter(|r| held(r)).map(|r| r.key.clone()).collect();
    keys.sort();
    keys
}

/// Whether one of `before`'s held keys is still routed and now the human's to answer too.
pub(crate) fn released(tab: &str, before: &[String], now: &[String]) -> bool {
    before.iter().any(|k| !now.contains(k) && find(tab, k).is_some())
}

/// Marks an answer going in; false when one already is.
pub(crate) fn begin_answer(tab: &str, key: &str) -> bool {
    let mut all = routes();
    match all.get_mut(tab).and_then(|v| v.iter_mut().find(|r| r.key == key)) {
        Some(r) if r.answering => false,
        Some(r) => {
            r.answering = true;
            true
        }
        None => true,
    }
}

pub(crate) fn end_answer(tab: &str, key: &str) {
    update(tab, key, |r| r.answering = false);
}

/// Drops `tab`'s screen prompts (not blockers) whose key isn't the one open now, unless an
/// answer is going in. What was dropped.
pub(crate) fn drop_gone(tab: &str, open_key: Option<&str>) -> Vec<Routed> {
    let mut all = routes();
    let Some(v) = all.get_mut(tab) else { return Vec::new() };
    let (gone, kept): (Vec<Routed>, Vec<Routed>) =
        std::mem::take(v).into_iter().partition(|r| r.kind != KIND_BLOCKER && !r.answering && Some(r.key.as_str()) != open_key);
    if kept.is_empty() {
        all.remove(tab);
    } else {
        *v = kept;
    }
    gone
}

/// Whether anything closed on `tab` (`drop_gone`).
pub(crate) fn prompt_gone(tab: &str, open_key: Option<&str>) -> bool {
    !drop_gone(tab, open_key).is_empty()
}

pub(crate) fn has_screen_routes(tab: &str) -> bool {
    routes().get(tab).is_some_and(|v| v.iter().any(|r| r.kind != KIND_BLOCKER))
}

fn drop_key(key: &str) -> Option<Routed> {
    let tab = routes().iter().find(|(_, v)| v.iter().any(|r| r.key == key)).map(|(t, _)| t.clone())?;
    remove(&tab, key)
}

/// Timed-out blocker routes a task write dropped, by key, with when (ms). The human's answer
/// clears the row before `hive_blocker_answered` reaches Rust, so the task write usually drops the
/// route first; its `after_timeout` is read from here then.
static DROPPED_TIMED_OUT: LazyLock<Mutex<HashMap<String, i64>>> = LazyLock::new(|| Mutex::new(HashMap::new()));
const DROPPED_KEEP_MS: i64 = 10 * 60 * 1000;

fn dropped_timed_out() -> MutexGuard<'static, HashMap<String, i64>> {
    DROPPED_TIMED_OUT.lock().unwrap_or_else(|e| e.into_inner())
}

fn note_dropped(r: &Routed, now: i64) {
    let mut d = dropped_timed_out();
    d.retain(|_, at| now - *at < DROPPED_KEEP_MS);
    if r.timed_out {
        d.insert(r.key.clone(), now);
    }
}

/// Whether `key`'s route had timed out: the live route's word, else what a task write dropped.
fn take_after_timeout(key: &str) -> bool {
    let dropped = dropped_timed_out().remove(key).is_some();
    let live = routes().iter().find_map(|(_, v)| v.iter().find(|r| r.key == key).map(|r| r.timed_out));
    live.unwrap_or(false) || dropped
}

// ── The allowlist ──

/// Whether `hiveAnswerWorker` may answer this prompt at all (§7.4, plan D8), or why it is the
/// human's.
pub(crate) fn classify(view: &PromptView) -> Result<(), String> {
    match view.kind.as_str() {
        KIND_TRUST => Err("Claude\u{2019}s workspace-trust dialog is always your human\u{2019}s".to_string()),
        KIND_PERMISSION => {
            match view.shape.as_str() {
                "tool" => {}
                "plan" => return Err("the plan dialog\u{2019}s rows change the permission mode, which only your human decides".to_string()),
                "bare" => return Err("a dialog with no Esc footer (a web fetch, a sandbox network request) is your human\u{2019}s".to_string()),
                _ => return Err("maiTerm can\u{2019}t read this dialog off the screen".to_string()),
            }
            if view.options.first().map(String::as_str) != Some("Yes") {
                return Err("its first row isn\u{2019}t a plain Yes".to_string());
            }
            if view.tool.as_deref().is_some_and(|t| t == "ExitPlanMode" || t == "EnterPlanMode") {
                return Err("a plan-mode change is your human\u{2019}s".to_string());
            }
            let words = std::iter::once(view.text.as_str()).chain(view.options.iter().map(String::as_str)).collect::<Vec<_>>().join("\n").to_lowercase();
            if let Some(m) = HUMAN_MARKERS.iter().find(|m| words.contains(**m)) {
                return Err(format!("it offers to change a setting, a rule, the sandbox or what may be read (\u{201c}{m}\u{201d})"));
            }
            Ok(())
        }
        KIND_QUESTION => {
            let qs = view.questions.as_array().filter(|q| !q.is_empty()).ok_or("maiTerm can\u{2019}t read the question\u{2019}s options")?;
            let all_listed = qs.iter().all(|q| q["options"].as_array().is_some_and(|o| !o.is_empty()));
            if !all_listed {
                return Err("a question without options needs free text, which only your human gives".to_string());
            }
            Ok(())
        }
        KIND_BLOCKER => match view.blocker.as_ref().map(|b| b.kind.as_str()) {
            Some("decision") => Ok(()),
            Some("action") => Err("only your human can do it".to_string()),
            _ => Err("it waits on something outside the Hive".to_string()),
        },
        _ => Err("maiTerm doesn\u{2019}t recognise this prompt".to_string()),
    }
}

// ── Routing ──

fn now_ms() -> i64 {
    super::fuses::now_ms()
}

fn timeout_secs(hive: &Hive) -> u64 {
    checkpoints::clamp_timeout(hive.settings.autonomy.prompt_timeout_secs)
}

fn worker_name(w: &WorkerRecord) -> String {
    format!("#{} {}", w.badge, one_line(&w.title, 80))
}

/// maiTerm's words for what is asked, worker text flattened and escaped: the log's `question`.
fn asked_text(view: &PromptView) -> String {
    match view.kind.as_str() {
        KIND_PERMISSION => match (&view.tool, &view.detail) {
            (Some(t), Some(d)) => format!("{}: `{}`", one_line(t, 60), one_line(d, 300)),
            (Some(t), None) => format!("{}: permission", one_line(t, 60)),
            _ => "A permission dialog".to_string(),
        },
        KIND_QUESTION => {
            let qs: Vec<String> = view.questions.as_array().into_iter().flatten().filter_map(|q| q["question"].as_str()).map(|q| one_line(q, 300)).collect();
            if qs.is_empty() { "A question".to_string() } else { qs.join(" / ") }
        }
        KIND_BLOCKER => view.blocker.as_ref().map(|b| one_line(&b.question, 400)).unwrap_or_default(),
        KIND_TRUST => "Claude\u{2019}s workspace-trust dialog".to_string(),
        _ => "A prompt".to_string(),
    }
}

fn asked_options(view: &PromptView) -> Vec<DecisionOption> {
    match view.kind.as_str() {
        KIND_BLOCKER => view.blocker.iter().flat_map(|b| b.options.iter()).map(|o| DecisionOption { label: one_line(&o.label, 120), recommended: o.recommended }).collect(),
        KIND_QUESTION => {
            let qs = view.questions.as_array().cloned().unwrap_or_default();
            let many = qs.len() > 1;
            qs.iter()
                .enumerate()
                .flat_map(|(i, q)| {
                    let labels: Vec<String> = q["options"].as_array().into_iter().flatten().filter_map(|o| o["label"].as_str()).map(|l| one_line(l, 120)).collect();
                    labels.into_iter().map(move |l| DecisionOption { label: if many { format!("Q{}: {l}", i + 1) } else { l }, recommended: false })
                })
                .collect()
        }
        _ => view.options.iter().map(|o| DecisionOption { label: one_line(o, 120), recommended: false }).collect(),
    }
}

/// The body of a frame: what is asked, every worker string escaped.
fn frame_body(view: &PromptView) -> String {
    let mut s = String::new();
    match view.kind.as_str() {
        KIND_PERMISSION => {
            s.push_str(&format!("{}\n", asked_text(view)));
            if !view.text.is_empty() {
                s.push_str(&format!("On screen: {}\n", one_line(&view.text, 400)));
            }
            let rows: Vec<String> = view.options.iter().enumerate().map(|(i, o)| format!("{}. {}", i + 1, one_line(o, 120))).collect();
            if !rows.is_empty() {
                s.push_str(&format!("Rows: {}\n", rows.join(" · ")));
            }
        }
        KIND_QUESTION => {
            for (i, q) in view.questions.as_array().into_iter().flatten().enumerate() {
                let multi = q["multiSelect"].as_bool().unwrap_or(false);
                let labels: Vec<String> = q["options"].as_array().into_iter().flatten().filter_map(|o| o["label"].as_str()).map(|l| one_line(l, 120)).collect();
                s.push_str(&format!(
                    "Q{} ({}): {}\n  options: {}\n",
                    i + 1,
                    if multi { "pick one or more" } else { "pick one" },
                    one_line(q["question"].as_str().unwrap_or_default(), 400),
                    labels.join(" · ")
                ));
            }
        }
        KIND_BLOCKER => {
            if let Some(b) = &view.blocker {
                s.push_str(&format!("Question: {}\n", one_line(&b.question, 400)));
                if let Some(c) = b.context.as_deref().filter(|c| !c.trim().is_empty()) {
                    s.push_str(&format!("Context: {}\n", frame_escape(c.trim())));
                }
                if b.options.is_empty() {
                    s.push_str("No options: the answer is free text.\n");
                } else {
                    let opts: Vec<String> =
                        b.options.iter().map(|o| format!("{}{}", one_line(&o.label, 120), if o.recommended { " (the worker recommends it)" } else { "" })).collect();
                    s.push_str(&format!("Options: {}\n", opts.join(" · ")));
                }
            }
        }
        _ => s.push_str(&format!("{}\n", asked_text(view))),
    }
    s
}

fn answer_hint(r: &Routed) -> String {
    let worker = r.badge;
    match r.kind.as_str() {
        KIND_PERMISSION => format!("hiveAnswerWorker {{worker: {worker}, prompt_id: \"{}\", answer: \"Yes\" or \"No\"}}", r.key),
        KIND_QUESTION => format!("hiveAnswerWorker {{worker: {worker}, prompt_id: \"{}\", answers: [{{selected: [\"<label>\"]}}, …one per question]}}", r.key),
        _ => format!("hiveAnswerWorker {{worker: {worker}, prompt_id: \"{}\", answer: \"<an option, or your answer>\"}}", r.key),
    }
}

/// What the coordinator is told of a route (§7.4): its own to answer, the human's under Ask me
/// (a recommendation is recorded), or the human's alone.
pub(crate) fn frame(r: &Routed, view: &PromptView, timeout_secs: u64) -> String {
    let what = if r.kind == KIND_BLOCKER { "HIVE QUESTION" } else { "HIVE PROMPT" };
    let tail = match (r.owner.as_str(), r.why.as_str()) {
        (OWNER_COORDINATOR, _) => "",
        (_, WHY_ASK) => " · your human answers",
        _ => " · your human\u{2019}s",
    };
    let mut s = format!("⟦{what} · {}{tail}⟧\n{}", r.worker_name, frame_body(view));
    s.push_str(&format!("prompt_id: {}\n", one_line(&r.key, 200)));
    match (r.owner.as_str(), r.why.as_str()) {
        (OWNER_COORDINATOR, _) => s.push_str(&format!(
            "Answer with {}. If one of your human\u{2019}s precedents settles it, cite it (precedent: \"P-…\"). Unanswered in {} min, \
             your human is told and either of you may answer.",
            answer_hint(r),
            timeout_secs.div_ceil(60)
        )),
        (_, WHY_ASK) => s.push_str(&format!(
            "Your human answers this one ({} is set to Ask me). Give your recommendation with {}: it is recorded, not sent.",
            checkpoints::label(&r.checkpoint),
            answer_hint(r)
        )),
        _ => s.push_str(&format!("It is your human\u{2019}s ({}): nothing for you to do but wait or plan around it.", r.why)),
    }
    s
}

async fn log_line(paths: &super::paths::HivePaths, line: DecisionLine) {
    let p = paths.clone();
    let _ = blocking(move || {
        decisions::record(&p, &line);
        Ok(())
    })
    .await;
}

fn base_line(r: &Routed, event: &str, by: &str) -> DecisionLine {
    DecisionLine {
        id: r.decision_id.clone(),
        event: event.to_string(),
        hive_id: r.hive_id.clone(),
        category: r.category.clone(),
        source: match r.kind.as_str() {
            KIND_BLOCKER => decisions::SOURCE_BLOCKER,
            KIND_QUESTION => decisions::SOURCE_QUESTION,
            _ => decisions::SOURCE_PROMPT,
        }
        .to_string(),
        task_id: Some(r.task_id.clone()),
        worker: Some(r.worker_slug.clone()),
        key: Some(r.key.clone()),
        question: r.question.clone(),
        by: by.to_string(),
        after_timeout: r.timed_out,
        at: crate::commands::workspace::iso_now(),
        ..DecisionLine::default()
    }
}

async fn tell_coordinator<W: HiveWindow>(ctx: &Ctx<'_, W>, caller: &Caller, text: String) -> bool {
    let Some(c) = super::tools::coordinator_tab(ctx.data, caller) else { return false };
    let sent = ctx.window.deliver(&c, text, "Hive prompt").await;
    log::info!("hive {}: prompt notice to the coordinator: {}", caller.hive.id, sent.describe());
    !matches!(sent, Sent::Refused(_))
}

fn notice(r: &Routed, why: &str) -> HumanNotice {
    HumanNotice {
        workspace_id: r.workspace_id.clone(),
        tab_id: r.tab_id.clone(),
        kind: r.kind.clone(),
        title: format!("Hive · {}", r.worker_name),
        why: why.to_string(),
    }
}

/// The routes went out; every route change here also wrote the decision log.
fn emit<W: HiveWindow>(ctx: &Ctx<'_, W>, workspace_id: &str) {
    ctx.window.emit_routes(workspace_id, views_for(workspace_id));
    ctx.window.decisions_changed(workspace_id);
}

pub(crate) async fn route<W: HiveWindow>(ctx: &Ctx<'_, W>, caller: &Caller, w: &WorkerRecord, view: PromptView) -> Option<Routed> {
    route_in(ctx, caller, w, view, now_ms(), true).await
}

/// Routes a worker's open prompt, once per key: owner by the allowlist and the checkpoint, an
/// `asked` line, the coordinator told (unless `tell` is off: it is answering it now), the human
/// told when it is theirs past what the gate let through, the timeout armed. `now` is the clock.
pub(crate) async fn route_in<W: HiveWindow>(ctx: &Ctx<'_, W>, caller: &Caller, w: &WorkerRecord, view: PromptView, now: i64, tell: bool) -> Option<Routed> {
    let tab = w.tab_id.clone().filter(|t| !t.is_empty())?;
    let checkpoint = checkpoints::for_prompt_kind(&view.kind).unwrap_or(PERMISSIONS).to_string();
    let has_coordinator = super::tools::coordinator_tab(ctx.data, caller).is_some();
    let classified = classify(&view);
    let timeout = timeout_secs(&caller.hive);
    let (owner, why, due_ms) = match &classified {
        Err(why) => (OWNER_HUMAN, why.clone(), 0),
        Ok(()) if !has_coordinator => (OWNER_HUMAN, "this Hive has no coordinator to answer it".to_string(), 0),
        Ok(()) if checkpoints::asks(&caller.hive, &checkpoint) => (OWNER_HUMAN, WHY_ASK.to_string(), 0),
        Ok(()) => (OWNER_COORDINATOR, String::new(), now + (timeout as i64) * 1000),
    };
    let r = Routed {
        hive_id: caller.hive.id.clone(),
        workspace_id: caller.workspace_id.clone(),
        label: caller.label.clone(),
        tab_id: tab.clone(),
        task_id: w.task_id.clone(),
        badge: w.badge,
        worker_name: worker_name(w),
        worker_slug: w.slug.clone(),
        key: view.key.clone(),
        kind: view.kind.clone(),
        category: if classified.is_ok() { checkpoint.clone() } else { decisions::CATEGORY_NEVER.to_string() },
        checkpoint,
        owner: owner.to_string(),
        why,
        opened_ms: now,
        due_ms,
        taken_over: false,
        timed_out: false,
        told: false,
        decision_id: decisions::new_id(),
        recommendation: None,
        answering: false,
        question: asked_text(&view),
    };
    if !admit(r.clone()) {
        return None;
    }
    log::info!("hive {}: {} {} on {} routed to the {} ({})", r.hive_id, r.kind, r.key, r.worker_name, r.owner, if r.why.is_empty() { "the coordinator's checkpoint" } else { &r.why });
    let mut asked = base_line(&r, decisions::EVENT_ASKED, decisions::BY_MAITERM);
    asked.context = match view.kind.as_str() {
        KIND_BLOCKER => view.blocker.as_ref().and_then(|b| b.context.as_deref()).map(|c| one_line(c, 1000)),
        KIND_PERMISSION if !view.text.is_empty() => Some(one_line(&view.text, 1000)),
        _ => None,
    };
    asked.options = asked_options(&view);
    asked.recommended = view.blocker.as_ref().and_then(|b| b.options.iter().find(|o| o.recommended)).map(|o| one_line(&o.label, 120));
    log_line(&caller.paths, asked).await;
    let r = if tell && has_coordinator {
        let told = tell_coordinator(ctx, caller, frame(&r, &view, timeout)).await;
        update(&tab, &r.key, |x| x.told = told).unwrap_or(r)
    } else {
        r
    };
    // The window's gate let a coordinator checkpoint's prompt through silently (plan D13): the
    // human hears of it now. Under Ask me the gate never held it back, delegable or not.
    if r.owner == OWNER_HUMAN && !checkpoints::asks(&caller.hive, &r.checkpoint) {
        ctx.window.notify_human(notice(&r, &r.why));
    }
    if r.owner == OWNER_COORDINATOR {
        ctx.window.schedule_due(caller, &tab, &r.key, r.due_ms);
    }
    emit(ctx, &caller.workspace_id);
    Some(r)
}

/// A worker's prompt hook: after the screen settles, route what is open there if it is new.
pub(crate) async fn maybe_route<W: HiveWindow>(ctx: &Ctx<'_, W>, caller: &Caller, tab: &str) {
    let Some(view) = ctx.window.prompt_view(tab) else { return };
    if find(tab, &view.key).is_some() {
        return;
    }
    let (paths, t) = (caller.paths.clone(), tab.to_string());
    let Ok(Some(w)) = blocking(move || Ok(worker::worker_for_tab(&paths, &t))).await else { return };
    route(ctx, caller, &w, view).await;
}

/// A worker's hook that may close a prompt: what is no longer open is dropped and logged as
/// lapsed (answered in its terminal, or the agent moved on), and a coordinator that was told
/// hears it is gone.
pub(crate) async fn gone<W: HiveWindow>(ctx: &Ctx<'_, W>, caller: &Caller, tab: &str) {
    if !has_screen_routes(tab) {
        return;
    }
    let open = ctx.window.prompt_view(tab).map(|v| v.key);
    let dropped = drop_gone(tab, open.as_deref());
    if dropped.is_empty() {
        return;
    }
    for r in &dropped {
        let mut l = base_line(r, decisions::EVENT_LAPSED, decisions::BY_MAITERM);
        l.reason = Some("It closed with no answer through maiTerm (answered in its terminal, or the agent moved on).".to_string());
        log_line(&caller.paths, l).await;
        log::info!("hive {}: {} {} on {} closed unanswered", r.hive_id, r.kind, r.key, r.worker_name);
        if r.told && r.owner == OWNER_COORDINATOR {
            tell_coordinator(ctx, caller, format!("[maiTerm] {}\u{2019}s prompt {} closed before it was answered through maiTerm: nothing to answer.", r.worker_name, one_line(&r.key, 200))).await;
        }
    }
    emit(ctx, &caller.workspace_id);
}

/// The coordinator's time on a prompt ran out: the human is told once, and either may answer.
pub(crate) async fn on_due<W: HiveWindow>(ctx: &Ctx<'_, W>, caller: &Caller, tab: &str, key: &str, now: i64) -> bool {
    let Some(r) = find(tab, key).filter(|r| held(r) && now >= r.due_ms) else { return false };
    if r.kind != KIND_BLOCKER && ctx.window.prompt_view(tab).map(|v| v.key).as_deref() != Some(key) {
        gone(ctx, caller, tab).await;
        return false;
    }
    let Some(r) = update(tab, key, |x| x.timed_out = true) else { return false };
    let minutes = timeout_secs(&caller.hive).div_ceil(60);
    log::info!("hive {}: {} {} on {} unanswered by the coordinator in {minutes} min: the human is told", r.hive_id, r.kind, r.key, r.worker_name);
    ctx.window.notify_human(notice(&r, WHY_TIMEOUT));
    if r.told {
        tell_coordinator(
            ctx,
            caller,
            format!("[maiTerm] {}\u{2019}s prompt {} waited {minutes} min: your human has been told; either of you may answer now.", r.worker_name, one_line(&r.key, 200)),
        )
        .await;
    }
    emit(ctx, &caller.workspace_id);
    true
}

// ── Blockers ──

/// The blockers a task write added (new, or asked again) and removed, by (task, asked_at).
pub(crate) fn blocker_diff(old: &[Task], new: &[Task]) -> (Vec<Task>, Vec<(String, String)>) {
    let asked = |list: &[Task]| -> Vec<(String, String)> {
        list.iter().filter_map(|t| t.blocker.as_ref().map(|b| (t.id.clone(), b.asked_at.clone()))).collect()
    };
    let (before, after) = (asked(old), asked(new));
    let added = new.iter().filter(|t| t.blocker.as_ref().is_some_and(|b| !before.contains(&(t.id.clone(), b.asked_at.clone())))).cloned().collect();
    let removed = before.into_iter().filter(|k| !after.contains(k)).collect();
    (added, removed)
}

/// A Hive's task list was written: route each new blocker on a worker's task, and drop the route
/// of each blocker that went. Off the state lock.
pub(crate) fn blockers_changed(state: Arc<AppState>, handle: AppHandle, workspace_id: String, added: Vec<Task>, removed: Vec<(String, String)>) {
    if added.is_empty() && removed.is_empty() {
        return;
    }
    tauri::async_runtime::spawn(async move {
        let Some(caller) = super::tools::caller_for_workspace(&state.app_data.read(), &workspace_id) else { return };
        let ctx = super::tools::app_ctx(&state, &handle, &caller);
        blockers_changed_in(&ctx, &caller, added, removed).await;
    });
}

pub(crate) async fn blockers_changed_in<W: HiveWindow>(ctx: &Ctx<'_, W>, caller: &Caller, added: Vec<Task>, removed: Vec<(String, String)>) {
    let mut changed = false;
    for (task, asked_at) in &removed {
        if let Some(r) = drop_key(&blocker_key(task, asked_at)) {
            note_dropped(&r, now_ms());
            changed = true;
        }
    }
    // Rust's own sandbox blockers (`external`) already told the coordinator.
    let added: Vec<Task> = added.into_iter().filter(|t| t.blocker.as_ref().is_some_and(|b| b.kind != "external")).collect();
    if !added.is_empty() {
        let paths = caller.paths.clone();
        let workers = blocking(move || Ok(worker::list_workers(&paths))).await.unwrap_or_default();
        for row in added {
            // A coordinator's own row is no worker's: its blocker is the human's, as ever.
            let Some(w) = workers.iter().find(|w| w.task_id == row.id) else { continue };
            let Some(b) = &row.blocker else { continue };
            route(ctx, caller, w, PromptView::from_blocker(&row.id, b)).await;
        }
    }
    if changed {
        emit(ctx, &caller.workspace_id);
    }
}

// ── hiveAnswerWorker ──

pub(crate) fn tool_definition(tab_id: &Value) -> Value {
    json!({
        "name": "hiveAnswerWorker",
        "description": format!(
            "Answer a worker's prompt that maiTerm sent you as ⟦HIVE PROMPT⟧ or ⟦HIVE QUESTION⟧, by its prompt_id. A permission \
             dialog takes answer \"Yes\" or \"No\" (only a plain Yes or No: rows that save a rule or change a mode are your \
             human's). An AskUserQuestion takes answers, one {{selected: [labels]}} per question, from the listed options only. A \
             worker's question blocker takes answer: one of its options, or free text when it has none (at most {MAX_ANSWER} \
             characters); it closes the blocker and tells the worker. Refused when the prompt on screen is another one, when it \
             is your human's, or after your human took it over. Under an Ask me checkpoint your answer is recorded as your \
             recommendation and not sent. Every answer is logged for your human."
        ),
        "inputSchema": {
            "type": "object",
            "properties": {
                "tabId": tab_id,
                "worker": { "type": ["string", "number"], "description": "The worker: badge (3 or \"#3\"), name or task id" },
                "prompt_id": { "type": "string", "description": "The prompt_id maiTerm gave" },
                "answer": { "type": "string", "description": "A permission's \"Yes\" or \"No\", or a blocker's answer" },
                "answers": {
                    "type": "array",
                    "items": { "type": "object", "properties": { "selected": { "type": "array", "items": { "type": "string" } } }, "required": ["selected"] },
                    "description": "An AskUserQuestion's answers: one per question, in order"
                },
                "precedent": { "type": "string", "description": "The precedent (P-…, from hivePrecedents) that settles it, if one does: an active one, or the call is refused" },
                "reason": { "type": "string", "description": "Why, in a sentence (shown to your human)" }
            },
            "required": ["worker", "prompt_id"]
        }
    })
}

/// A checked answer, ready to carry out.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Checked {
    /// A permission row's label, or `Esc`.
    Choice(String),
    Answers(Vec<Answer>),
    Blocker(String),
}

impl Checked {
    fn said(&self) -> String {
        match self {
            Checked::Choice(c) if c == "Esc" => "No".to_string(),
            Checked::Choice(c) | Checked::Blocker(c) => c.clone(),
            Checked::Answers(a) => answers_said(a),
        }
    }
}

fn answers_said(a: &[Answer]) -> String {
    a.iter()
        .enumerate()
        .map(|(i, x)| {
            let mut s = x.selected.join(", ");
            if let Some(o) = x.other.as_deref().filter(|o| !o.trim().is_empty()) {
                s = if s.is_empty() { format!("\u{201c}{}\u{201d}", one_line(o, 200)) } else { format!("{s}, \u{201c}{}\u{201d}", one_line(o, 200)) };
            }
            if a.len() > 1 { format!("Q{}: {s}", i + 1) } else { s }
        })
        .collect::<Vec<_>>()
        .join("; ")
}

/// How a human's answer reads in the log: the row they chose, or their selections.
pub(crate) fn answer_summary(choice: Option<&str>, answers: Option<&[Answer]>) -> String {
    match (choice, answers) {
        (_, Some(a)) if !a.is_empty() => answers_said(a),
        (Some(c), _) => one_line(c, 200),
        _ => String::new(),
    }
}

/// The coordinator's answer checked against the prompt as read now (§7.4's allowlist): a
/// permission's plain Yes or No (No without a row of its own is Esc), listed labels for a
/// question, an option or free text for a blocker.
pub(crate) fn check_answer(view: &PromptView, args: &Value) -> Result<Checked, String> {
    let answer = args.get("answer").and_then(Value::as_str).map(str::trim);
    match view.kind.as_str() {
        KIND_PERMISSION => {
            let a = answer.ok_or("A permission dialog takes answer \"Yes\" or \"No\".")?;
            if a.eq_ignore_ascii_case("yes") {
                if !view.options.iter().any(|o| o == "Yes") {
                    return Err("This dialog has no row that is exactly Yes.".to_string());
                }
                return Ok(Checked::Choice("Yes".to_string()));
            }
            if a.eq_ignore_ascii_case("no") {
                return Ok(Checked::Choice(if view.options.iter().any(|o| o == "No") { "No" } else { "Esc" }.to_string()));
            }
            Err(format!("\u{201c}{}\u{201d} isn\u{2019}t an answer you may give: only Yes or No. Rows that save a rule or change a mode are your human\u{2019}s.", one_line(a, 80)))
        }
        KIND_QUESTION => {
            let qs = view.questions.as_array().cloned().unwrap_or_default();
            let given = args.get("answers").and_then(Value::as_array).ok_or("An AskUserQuestion takes answers: one {selected: [labels]} per question.")?;
            if given.len() != qs.len() {
                return Err(format!("It asks {} question(s): give one answer for each, in order.", qs.len()));
            }
            let mut out = Vec::new();
            for (i, (q, a)) in qs.iter().zip(given).enumerate() {
                if a.get("other").is_some_and(|o| !o.is_null()) {
                    return Err("Free text isn\u{2019}t an answer you may give: pick from the listed options.".to_string());
                }
                let selected: Vec<String> = a
                    .get("selected")
                    .and_then(Value::as_array)
                    .ok_or(format!("Answer {} needs selected: a list of labels.", i + 1))?
                    .iter()
                    .map(|s| s.as_str().map(str::to_string).ok_or(format!("Answer {}'s selected must be labels.", i + 1)))
                    .collect::<Result<_, _>>()?;
                let labels: Vec<&str> = q["options"].as_array().into_iter().flatten().filter_map(|o| o["label"].as_str()).collect();
                if let Some(bad) = selected.iter().find(|s| !labels.contains(&s.as_str())) {
                    return Err(format!("\u{201c}{}\u{201d} isn\u{2019}t one of question {}\u{2019}s options: {}.", one_line(bad, 80), i + 1, labels.join(", ")));
                }
                let multi = q["multiSelect"].as_bool().unwrap_or(false);
                if !multi && selected.len() != 1 {
                    return Err(format!("Question {} takes exactly one option.", i + 1));
                }
                if multi && selected.is_empty() {
                    return Err(format!("Question {} takes at least one option.", i + 1));
                }
                out.push(Answer { selected, other: None });
            }
            Ok(Checked::Answers(out))
        }
        KIND_BLOCKER => {
            let a = answer.filter(|a| !a.is_empty()).ok_or("A question blocker takes answer: one of its options, or your answer.")?;
            if a.chars().count() > MAX_ANSWER {
                return Err(format!("Keep the answer under {MAX_ANSWER} characters."));
            }
            if view.options.is_empty() {
                return Ok(Checked::Blocker(a.to_string()));
            }
            view.options
                .iter()
                .find(|o| o.trim().eq_ignore_ascii_case(a))
                .map(|o| Checked::Blocker(o.clone()))
                .ok_or(format!("Answer with one of its options: {}.", view.options.join(", ")))
        }
        _ => Err("maiTerm doesn\u{2019}t recognise this prompt".to_string()),
    }
}

/// The blocker on a worker's task row now, as a view, when its key is `key`.
fn blocker_view(data: &parking_lot::RwLock<crate::state::AppData>, caller: &Caller, task_id: &str, key: &str) -> Option<PromptView> {
    let tasks = super::tools::workspace_tasks(data, caller).ok()?;
    let row = tasks.into_iter().find(|t| t.id == task_id)?;
    let b = row.blocker.as_ref().filter(|_| row.status == "blocked")?;
    Some(PromptView::from_blocker(task_id, b)).filter(|v| v.key == key)
}

pub(crate) async fn answer_worker_tool<W: HiveWindow>(ctx: &Ctx<'_, W>, caller: &Caller, args: &Value) -> Result<Value, String> {
    let prompt_id = args.get("prompt_id").and_then(Value::as_str).map(str::trim).filter(|s| !s.is_empty()).ok_or("prompt_id is required: the one maiTerm gave")?.to_string();
    let precedent = super::precedents::cited(&caller.paths, args).await?;
    let reason = args.get("reason").and_then(Value::as_str).map(str::trim).filter(|s| !s.is_empty()).map(|r| one_line(r, MAX_REASON));
    let w = super::control::named_worker(caller, args).await?;
    let tab = w
        .tab_id
        .clone()
        .filter(|t| super::tools::has_tab(ctx.data, caller, t))
        .ok_or_else(|| format!("Worker {} has no tab in this Hive ({}): there\u{2019}s no prompt to answer.", worker_name(&w), w.phase))?;

    // The prompt as it stands now: a blocker on the worker's row, or what its screen shows.
    let view = if prompt_id.starts_with("b_") {
        blocker_view(ctx.data, caller, &w.task_id, &prompt_id).ok_or_else(|| {
            "stale: that question is no longer on the worker\u{2019}s task (answered, or asked again: use the newest prompt_id)".to_string()
        })?
    } else {
        match ctx.window.prompt_view(&tab) {
            Some(v) if v.key == prompt_id => v,
            Some(v) => return Err(format!("stale: the prompt on screen is now {}", v.key)),
            None => return Err("no prompt is open on that worker".to_string()),
        }
    };
    // A prompt no hook reported (or a blocker from before a restart) is routed now, quietly.
    let r = match find(&tab, &prompt_id) {
        Some(r) => r,
        None => route_in(ctx, caller, &w, view.clone(), now_ms(), false).await.or_else(|| find(&tab, &prompt_id)).ok_or("maiTerm couldn\u{2019}t route that prompt")?,
    };
    if r.taken_over {
        return Err("Your human took this one over: leave it.".to_string());
    }
    classify(&view).map_err(|why| format!("It is your human\u{2019}s: {why}."))?;
    let checked = check_answer(&view, args)?;

    // Ask me: the coordinator's answer is its recommendation, recorded and never sent (plan D10).
    // The checkpoint as it stands now counts too: a change racing this call fails closed.
    let ask_me = (r.owner == OWNER_HUMAN && r.why == WHY_ASK) || (r.owner == OWNER_COORDINATOR && checkpoints::asks(&caller.hive, &r.checkpoint));
    if ask_me {
        let rec = json!({ "answer": checked.said(), "precedent": precedent, "reason": reason });
        update(&tab, &prompt_id, |x| x.recommendation = Some(rec));
        let mut l = base_line(&r, decisions::EVENT_RECOMMENDED, decisions::BY_COORDINATOR);
        (l.recommended, l.answer, l.precedent, l.reason) = (Some(checked.said()), Some(checked.said()), precedent, reason);
        log_line(&caller.paths, l).await;
        let why = format!("{} is set to Ask me: your human answers, and your recommendation is shown to them in the log.", checkpoints::label(&r.checkpoint));
        return Ok(json!({ "recorded": true, "sent": false, "prompt_id": prompt_id, "why": why }));
    }
    if r.owner != OWNER_COORDINATOR {
        return Err(format!("It is your human\u{2019}s: {}.", r.why));
    }
    if !begin_answer(&tab, &prompt_id) {
        return Err("Your human is answering this prompt right now: leave it.".to_string());
    }
    let done = carry_out(ctx, caller, &w, &tab, &view, &checked).await;
    end_answer(&tab, &prompt_id);
    let choice = done?;

    let r = remove(&tab, &prompt_id).unwrap_or(r);
    let mut l = base_line(&r, decisions::EVENT_ANSWERED, decisions::BY_COORDINATOR);
    (l.answer, l.precedent, l.reason) = (Some(checked.said()), precedent.clone(), reason);
    log_line(&caller.paths, l).await;
    // Counted for what it decided; a recommendation under Ask me decides nothing.
    if precedent.is_some() {
        let p = caller.paths.clone();
        let _ = blocking(move || {
            super::precedents::count_citation(&p, precedent.as_deref());
            Ok(())
        })
        .await;
    }
    log::info!("hive {}: the coordinator answered {}\u{2019}s {} {}: {}", r.hive_id, r.worker_name, r.kind, r.key, checked.said());
    emit(ctx, &caller.workspace_id);
    Ok(json!({ "answered": true, "prompt_id": prompt_id, "choice": choice }))
}

async fn carry_out<W: HiveWindow>(ctx: &Ctx<'_, W>, caller: &Caller, w: &WorkerRecord, tab: &str, view: &PromptView, checked: &Checked) -> Result<String, String> {
    let (choice, answers) = match checked {
        Checked::Choice(c) => (Some(c.clone()), None),
        Checked::Answers(a) => (None, Some(a.clone())),
        Checked::Blocker(answer) => return answer_blocker(ctx, caller, w, tab, view, answer).await.map(|_| answer.clone()),
    };
    let out = ctx.window.answer_prompt(tab, &view.key, choice.clone(), answers).await;
    if out["ok"].as_bool() == Some(true) {
        return Ok(choice.unwrap_or_else(|| checked.said()));
    }
    let why = out["detail"].as_str().or(out["reason"].as_str()).unwrap_or("it didn\u{2019}t go through").to_string();
    Err(format!("maiTerm couldn\u{2019}t answer it: {why}"))
}

/// A worker's question blocker answered by the coordinator: its row back to active with the
/// answer as a note (refused if it was asked again meanwhile), then the worker told.
async fn answer_blocker<W: HiveWindow>(ctx: &Ctx<'_, W>, caller: &Caller, w: &WorkerRecord, tab: &str, view: &PromptView, answer: &str) -> Result<(), String> {
    let b = view.blocker.as_ref().ok_or("no question to answer")?;
    let args = json!({
        "workspace_id": caller.workspace_id,
        "task_id": w.task_id,
        "status": "active",
        "note": format!("Decided by your coordinator: {}", one_line(answer, 300)),
        "expect_asked_at": b.asked_at,
    });
    rpc::applied(ctx.window.request(rpc::VERB_TASK, args).await)?;
    let text = format!(
        "[maiTerm] Your coordinator answered your question on task \u{201c}{}\u{201d} (id {}).\nQuestion: {}\nAnswer: {}\n\nThe task is back in \
         Active. Carry on from here, and close it with updateTasks (task id {}) when it is done.",
        one_line(&w.title, 120),
        w.task_id,
        one_line(&b.question, 400),
        frame_escape(answer),
        w.task_id
    );
    let sent = ctx.window.deliver(tab, text, "Hive answer").await;
    log::info!("hive {}: coordinator's answer to {}: {}", caller.hive.id, worker_name(w), sent.describe());
    Ok(())
}

// ── The human's side ──

/// A routed prompt the human answered (the phone, the Loom): an `answered` line by the human,
/// the route dropped, and the coordinator, if it was told, told how. With no app handle (a
/// router test) the line is still written.
pub(crate) async fn human_answered(app: &Arc<AppState>, handle: Option<&AppHandle>, tab: &str, key: &str, answer: &str) {
    let Some(caller) = super::tools::caller_for_tab(&app.app_data.read(), tab) else {
        remove(tab, key);
        return;
    };
    match handle {
        Some(h) => human_answered_in(&super::tools::app_ctx(app, h, &caller), &caller, tab, key, answer).await,
        None => {
            if let Some(r) = remove(tab, key) {
                log_line(&caller.paths, human_line(&r, answer)).await;
            }
        }
    }
}

fn human_line(r: &Routed, answer: &str) -> DecisionLine {
    let mut l = base_line(r, decisions::EVENT_ANSWERED, decisions::BY_HUMAN);
    l.answer = Some(answer.to_string());
    l.recommended = r.recommendation.as_ref().and_then(|v| v["answer"].as_str()).map(str::to_string);
    l.precedent = r.recommendation.as_ref().and_then(|v| v["precedent"].as_str()).map(str::to_string);
    l
}

pub(crate) async fn human_answered_in<W: HiveWindow>(ctx: &Ctx<'_, W>, caller: &Caller, tab: &str, key: &str, answer: &str) {
    let Some(r) = remove(tab, key) else { return };
    log_line(&caller.paths, human_line(&r, answer)).await;
    log::info!("hive {}: the human answered {}\u{2019}s {} {}", r.hive_id, r.worker_name, r.kind, r.key);
    if r.told {
        let what = if r.kind == KIND_QUESTION { "question" } else { "prompt" };
        tell_coordinator(ctx, caller, format!("[maiTerm] Your human answered {}\u{2019}s {what}: {}.", r.worker_name, one_line(answer, 300))).await;
    }
    emit(ctx, &caller.workspace_id);
}

/// A blocker the human answered through the window (`hive_blocker_answered`): logged by
/// `decisions::record_blocker_answer`, its route dropped, and the coordinator told of a worker's
/// (the worker was told by the window). Rust's copy of the question wins while the row still has
/// it; a row this Hive doesn't have is refused.
pub(crate) async fn blocker_answered_in<W: HiveWindow>(
    ctx: &Ctx<'_, W>,
    caller: &Caller,
    task_id: &str,
    blocker: TaskBlocker,
    answer: decisions::BlockerAnswerArg,
) -> Result<Option<String>, String> {
    let row = super::tools::workspace_tasks(ctx.data, caller)?.into_iter().find(|t| t.id == task_id).ok_or("That task isn\u{2019}t in this Hive.")?;
    let blocker = row.blocker.filter(|b| b.asked_at == blocker.asked_at).unwrap_or(blocker);
    let key = blocker_key(task_id, &blocker.asked_at);
    let after_timeout = take_after_timeout(&key);
    let (paths, hive, task, b, a) = (caller.paths.clone(), caller.hive.clone(), task_id.to_string(), blocker.clone(), answer.clone());
    let (line, workers) = blocking(move || {
        let workers = worker::list_workers(&paths);
        let line = decisions::record_blocker_answer(&paths, &hive, &workers, &task, &b, &a, after_timeout, &crate::commands::workspace::iso_now())?;
        Ok((line, workers))
    })
    .await?;
    if drop_key(&key).is_some() {
        emit(ctx, &caller.workspace_id);
    }
    let Some(line) = line else { return Ok(None) };
    ctx.window.decisions_changed(&caller.workspace_id);
    log::info!("hive {}: the human answered {} on task {task_id} ({})", caller.hive.id, line.key.as_deref().unwrap_or_default(), line.category);
    if let Some(w) = workers.iter().find(|w| w.task_id == task_id) {
        tell_coordinator(ctx, caller, blocker_answer_notice(&worker_name(w), &blocker, &answer)).await;
    }
    Ok(Some(line.id))
}

/// The coordinator's word of a human's blocker answer. Only maiTerm's words and the human's share
/// the `[maiTerm]` sentence: the worker's question and option labels sit on their own labelled
/// lines, so a quote closed inside them can't read as the human speaking.
fn blocker_answer_notice(worker: &str, blocker: &TaskBlocker, answer: &decisions::BlockerAnswerArg) -> String {
    let what = if blocker.kind == "decision" { "question" } else { "blocker" };
    let option = answer.option.as_deref().map(str::trim).and_then(|o| blocker.options.iter().position(|x| x.label == o));
    let text = one_line(&answer.text, 300);
    let how = match (option, blocker.kind.as_str()) {
        (Some(i), _) => format!(": they chose the worker\u{2019}s option {}", i + 1),
        (None, _) if !text.is_empty() => String::new(),
        (None, "action") => ": they marked it done".to_string(),
        (None, _) => ": they cleared it without words".to_string(),
    };
    let mut s = format!("[maiTerm] Your human answered {worker}\u{2019}s {what}{how}.");
    if !text.is_empty() {
        s.push_str(&format!("\nTheir words: {text}"));
    }
    s.push_str(&format!("\nThe worker\u{2019}s question: {}", one_line(&blocker.question, 300)));
    if let Some(i) = option {
        s.push_str(&format!("\nThe worker\u{2019}s option {}: {}", i + 1, one_line(&blocker.options[i].label, 120)));
    }
    s
}

/// The human took one prompt over: theirs alone from now on.
pub(crate) async fn take_over_in<W: HiveWindow>(ctx: &Ctx<'_, W>, caller: &Caller, tab: &str, key: &str) -> Result<(), String> {
    let r = find(tab, key).filter(|r| r.workspace_id == caller.workspace_id).ok_or("That prompt is no longer waiting.")?;
    if r.taken_over {
        return Ok(());
    }
    let r = update(tab, key, |x| {
        x.owner = OWNER_HUMAN.to_string();
        x.why = WHY_TAKE_OVER.to_string();
        x.taken_over = true;
        x.due_ms = 0;
    })
    .ok_or("That prompt is no longer waiting.")?;
    log::info!("hive {}: the human took over {}\u{2019}s {} {}", r.hive_id, r.worker_name, r.kind, r.key);
    if r.told {
        tell_coordinator(ctx, caller, format!("[maiTerm] Your human took over {}\u{2019}s prompt {}; leave it.", r.worker_name, one_line(&r.key, 200))).await;
    }
    emit(ctx, &caller.workspace_id);
    Ok(())
}

/// The checkpoints changed: the coordinator is told, and each prompt it owns under a checkpoint
/// now Ask me becomes the human's, timed out or not; the human is told of each they hadn't been.
pub(crate) async fn checkpoints_changed(state: &Arc<AppState>, handle: &AppHandle, label: &str, workspace_id: &str, hive: &Hive, changes: Vec<String>) {
    let Some(mut caller) = super::tools::caller_for_workspace(&state.app_data.read(), workspace_id).filter(|c| c.label == label) else { return };
    caller.hive = hive.clone();
    let ctx = super::tools::app_ctx(state, handle, &caller);
    checkpoints_changed_in(&ctx, &caller, changes).await;
}

pub(crate) async fn checkpoints_changed_in<W: HiveWindow>(ctx: &Ctx<'_, W>, caller: &Caller, changes: Vec<String>) -> Vec<Routed> {
    let flipped: Vec<Routed> = {
        let mut all = routes();
        all.values_mut()
            .flatten()
            // A timed-out route is still the coordinator's to answer: it moves too. One an answer
            // is going into now is left to finish.
            .filter(|r| {
                r.workspace_id == caller.workspace_id
                    && r.owner == OWNER_COORDINATOR
                    && !r.taken_over
                    && !r.answering
                    && checkpoints::asks(&caller.hive, &r.checkpoint)
            })
            .map(|r| {
                r.owner = OWNER_HUMAN.to_string();
                r.why = WHY_ASK.to_string();
                r.due_ms = 0;
                r.clone()
            })
            .collect()
    };
    // The timeout already told the human of a timed-out one.
    for r in flipped.iter().filter(|r| !r.timed_out) {
        ctx.window.notify_human(notice(r, WHY_ASK));
    }
    let mut text = format!("[maiTerm] Your human changed this Hive\u{2019}s checkpoints: {}.", changes.join("; "));
    if !flipped.is_empty() {
        let names: Vec<String> = flipped.iter().map(|r| format!("{}\u{2019}s {}", r.worker_name, r.key)).collect();
        text.push_str(&format!(" Now your human\u{2019}s to answer: {}.", names.join(", ")));
    }
    tell_coordinator(ctx, caller, text).await;
    if !flipped.is_empty() {
        emit(ctx, &caller.workspace_id);
    }
    flipped
}

/// The window's Take over (§7.4): one prompt of a worker's moves to the human.
#[tauri::command]
pub async fn hive_take_over(
    window: tauri::Window,
    state: tauri::State<'_, Arc<AppState>>,
    app: AppHandle,
    workspace_id: String,
    tab_id: String,
    key: String,
) -> Result<(), String> {
    let caller = super::tools::caller_for_workspace(&state.app_data.read(), &workspace_id)
        .filter(|c| c.label == window.label())
        .ok_or("That workspace isn\u{2019}t a Hive in this window")?;
    let ctx = super::tools::app_ctx(state.inner(), &app, &caller);
    take_over_in(&ctx, &caller, &tab_id, &key).await
}

/// Who holds each open prompt of a Hive in this window, for its view's mount.
#[tauri::command]
pub fn hive_prompt_routes(window: tauri::Window, state: tauri::State<'_, Arc<AppState>>, workspace_id: String) -> Result<Vec<RouteView>, String> {
    let ok = state.app_data.read().window(window.label()).is_some_and(|w| w.workspaces.iter().any(|ws| ws.id == workspace_id && ws.hive.is_some()));
    if !ok {
        return Err("That workspace isn\u{2019}t a Hive in this window".to_string());
    }
    Ok(views_for(&workspace_id))
}

/// A route the coordinator holds, for another module's tests (the phone's and the Loom's side).
#[cfg(test)]
pub(crate) fn hold_for_test(tab: &str, key: &str, kind: &str) {
    admit(Routed { tab_id: tab.into(), key: key.into(), kind: kind.into(), owner: OWNER_COORDINATOR.into(), ..Routed::default() });
}

#[cfg(test)]
pub(crate) fn time_out_for_test(tab: &str, key: &str) {
    update(tab, key, |r| r.timed_out = true);
}

#[cfg(test)]
mod tests;
