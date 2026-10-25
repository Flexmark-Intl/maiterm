//! Earned autonomy and the daily digest (docs/hive.md §7.6). Confidence is counted from Rust's own
//! decision log, never felt: after five of the coordinator's recommendations in a row are what the
//! human chose in one Rust-assigned category, maiTerm offers that checkpoint to the coordinator;
//! after two overrides of its decisions there in a day, it offers it back. Each offer is maiTerm's
//! own decision blocker on the coordinator's "Hive autonomy: …" row, so it waits where every
//! question does, and only the human's answer (`hive_blocker_answered`) changes the checkpoint.
//!
//! Offers are category `offer`: never a precedent, never a streak. `interfaces` has no Rust signal
//! and `finals` no source until the gallery, so neither is ever offered (plan D15); Landing in the
//! integration mode is always the human's.

use serde_json::{json, Value};

use super::checkpoints::{self, ASK, CHECKPOINTS, COORDINATOR, LANDING, PERMISSIONS, PLAN, QUESTIONS};
use super::decisions::{self, DecisionLine, DecisionOption, BY_COORDINATOR, BY_HUMAN, EVENT_ANSWERED, EVENT_DIGEST, EVENT_LAPSED, EVENT_OFFER, EVENT_OVERRIDDEN};
use super::deliver::one_line;
use super::record::Hive;
use super::rpc::HiveWindow;
use super::tools::{blocking, Caller, Ctx};
use crate::state::workspace::{BlockerOption, TaskBlocker};
use crate::state::Task;

/// Recommendations taken in a row before maiTerm offers the checkpoint.
pub const STREAK: u32 = 5;
/// Overrides in a day before maiTerm offers it back.
pub const OVERRIDES: usize = 2;
const DAY_MS: i64 = 24 * 3_600_000;
const HOUR: std::time::Duration = std::time::Duration::from_secs(3600);

/// An offer line's `outcome` until it is answered.
pub const HAND_OVER: &str = "hand_over";
pub const HAND_BACK: &str = "hand_back";
pub const SOURCE_OFFER: &str = "offer";
pub const CATEGORY_DIGEST: &str = "digest";

pub const YES_HAND_OVER: &str = "Yes, let the coordinator decide";
pub const NO_HAND_OVER: &str = "No, keep asking me";
pub const YES_HAND_BACK: &str = "Yes, ask me";
pub const NO_HAND_BACK: &str = "No";

/// The coordinator's row an offer for `key` waits on.
pub fn row_title(key: &str) -> String {
    format!("Hive autonomy: {}", checkpoints::label(key))
}

/// An ISO time as unix ms; None when it isn't one.
pub fn ms(at: &str) -> Option<i64> {
    (at.is_ascii() && at.len() >= 19).then(|| crate::mailink::transcript::rfc3339_to_ms(at)).filter(|m| *m > 0)
}

/// Checkpoints maiTerm may offer (§7.6): only what Rust sees decided.
pub fn earnable(hive: &Hive, key: &str) -> bool {
    matches!(key, PLAN | QUESTIONS | PERMISSIONS) || (key == LANDING && !hive.is_integration())
}

/// The checkpoint an `offer` line is about.
pub fn offer_checkpoint(l: &DecisionLine) -> Option<&str> {
    if l.event != EVENT_OFFER {
        return None;
    }
    l.extra.get("checkpoint").and_then(Value::as_str)
}

/// The human's answer is the recommendation: by label, ignoring case; Esc is a dialog's No.
fn same(a: &str, b: &str) -> bool {
    let n = |s: &str| {
        let s = s.trim();
        if s.eq_ignore_ascii_case("esc") { "no".to_string() } else { s.to_lowercase() }
    };
    n(a) == n(b)
}

/// Where the human's answer to the latest offer for `category` (of `kind`, when given) ends in the
/// log, so a count starts after it. Not where the offer was filed: answers the human gave while it
/// stood would count toward the next one. An offer nobody answered (open, or withdrawn) starts
/// nothing.
fn after_answered_offer(lines: &[DecisionLine], category: &str, kind: Option<&str>) -> usize {
    let offers: std::collections::HashSet<&str> =
        lines.iter().filter(|l| offer_checkpoint(l) == Some(category) && kind.is_none_or(|k| l.outcome.as_deref() == Some(k))).map(|l| l.id.as_str()).collect();
    lines.iter().rposition(|l| l.event == EVENT_ANSWERED && offers.contains(l.id.as_str())).map_or(0, |i| i + 1)
}

/// The run of the human's answers in `category` that took the coordinator's recommendation,
/// counted after the human answered the latest offer for it, whatever the answer (plan D18). A
/// human answer without a recommendation, or with one not taken, ends the run; so does an override
/// of a decision that cited a precedent of this category (§7.6).
pub fn streak(lines: &[DecisionLine], category: &str) -> u32 {
    streak_in(lines, category, |_| None)
}

/// `streak`, with each cited precedent's category looked up (None: gone, read as the overridden
/// decision's own).
pub fn streak_in(lines: &[DecisionLine], category: &str, precedent_category: impl Fn(&str) -> Option<String>) -> u32 {
    let start = after_answered_offer(lines, category, None);
    let mut run = 0;
    for l in &lines[start..] {
        match l.event.as_str() {
            EVENT_ANSWERED if l.by == BY_HUMAN && l.category == category && l.answer.is_some() => {
                let taken = matches!((&l.recommended, &l.answer), (Some(r), Some(a)) if same(r, a));
                run = if taken { run + 1 } else { 0 };
            }
            EVENT_OVERRIDDEN => {
                if let Some(p) = &l.precedent {
                    if precedent_category(p).unwrap_or_else(|| l.category.clone()) == category {
                        run = 0;
                    }
                }
            }
            _ => {}
        }
    }
    run
}

/// Overrides of the coordinator's decisions in `category` in the day before `now`, after the
/// human answered the latest offer to hand it back.
pub fn overrides_in_day(lines: &[DecisionLine], category: &str, now: i64) -> usize {
    let start = after_answered_offer(lines, category, Some(HAND_BACK));
    lines[start..]
        .iter()
        .filter(|l| l.event == EVENT_OVERRIDDEN && l.by == BY_HUMAN && l.category == category && ms(&l.at).is_some_and(|t| now - t < DAY_MS))
        .count()
}

/// A hand-back offer for `category` answered in the day before `now`. One standing is `open`; one
/// withdrawn unanswered never held the human's attention, so it holds nothing back.
fn handed_back_lately(lines: &[DecisionLine], category: &str, now: i64) -> bool {
    let offers: std::collections::HashSet<&str> =
        lines.iter().filter(|l| offer_checkpoint(l) == Some(category) && l.outcome.as_deref() == Some(HAND_BACK)).map(|l| l.id.as_str()).collect();
    lines.iter().any(|l| l.event == EVENT_ANSWERED && offers.contains(l.id.as_str()) && ms(&l.at).is_some_and(|t| now - t < DAY_MS))
}

/// An offer to file.
#[derive(Debug, Clone, PartialEq)]
pub struct Offer {
    pub checkpoint: String,
    /// `hand_over` | `hand_back`.
    pub kind: String,
    /// The streak, or the overrides in the day.
    pub count: u32,
}

/// The offers due now (pure): for each earnable checkpoint with no offer standing (`open`), a
/// hand-over at Ask me after `STREAK` in a row, a hand-back at Coordinator after `OVERRIDES` in a
/// day unless the human answered one in that day.
pub fn evaluate(lines: &[DecisionLine], hive: &Hive, open: &[String], now: i64, precedent_category: impl Fn(&str) -> Option<String>) -> Vec<Offer> {
    let mut out = Vec::new();
    for key in CHECKPOINTS {
        if !earnable(hive, key) || open.iter().any(|o| o == key) {
            continue;
        }
        if checkpoints::asks(hive, key) {
            let n = streak_in(lines, key, &precedent_category);
            if n >= STREAK {
                out.push(Offer { checkpoint: key.to_string(), kind: HAND_OVER.to_string(), count: n });
            }
        } else {
            let n = overrides_in_day(lines, key, now);
            if n >= OVERRIDES && !handed_back_lately(lines, key, now) {
                out.push(Offer { checkpoint: key.to_string(), kind: HAND_BACK.to_string(), count: n as u32 });
            }
        }
    }
    out
}

/// The offer as the human reads it: maiTerm's words, Yes recommended.
pub fn offer_blocker(o: &Offer, asked_at: &str) -> TaskBlocker {
    let label = checkpoints::label(&o.checkpoint);
    let (question, context, yes, no) = if o.kind == HAND_OVER {
        (
            format!("You\u{2019}ve agreed with the coordinator\u{2019}s last {} calls on {label}. Let it handle {label} from now on?", o.count),
            "maiTerm counted this from the decision log: each time, you chose what the coordinator recommended. Yes sets this \
             checkpoint to Coordinator; every decision it makes stays in Memory \u{2192} Decisions, and Autonomy in the Hive \
             header sets it back.",
            YES_HAND_OVER,
            NO_HAND_OVER,
        )
    } else {
        let times = if o.count == 2 { "twice".to_string() } else { format!("{} times", o.count) };
        (
            format!("You\u{2019}ve overridden the coordinator {times} in the last day on {label}. Ask you about {label} from now on?"),
            "Yes sets this checkpoint to Ask me: the coordinator recommends, and you decide.",
            YES_HAND_BACK,
            NO_HAND_BACK,
        )
    };
    TaskBlocker {
        kind: "decision".into(),
        question,
        context: Some(context.to_string()),
        options: vec![
            BlockerOption { label: yes.into(), detail: None, recommended: true },
            BlockerOption { label: no.into(), detail: None, recommended: false },
        ],
        command: None,
        asked_at: asked_at.into(),
        asked_by: "agent".into(),
    }
}

/// The offer's line in the log: category `offer`, its checkpoint in `extra`, its kind as the
/// outcome until it is answered.
pub fn offer_line(hive_id: &str, o: &Offer, b: &TaskBlocker, task_id: &str) -> DecisionLine {
    let mut extra = serde_json::Map::new();
    extra.insert("checkpoint".into(), json!(o.checkpoint));
    DecisionLine {
        id: decisions::new_id(),
        event: EVENT_OFFER.to_string(),
        hive_id: hive_id.to_string(),
        category: decisions::CATEGORY_OFFER.to_string(),
        source: SOURCE_OFFER.to_string(),
        task_id: Some(task_id.to_string()),
        key: Some(super::prompts::blocker_key(task_id, &b.asked_at)),
        question: b.question.clone(),
        context: b.context.clone(),
        options: b.options.iter().map(|x| DecisionOption { label: x.label.clone(), recommended: x.recommended }).collect(),
        recommended: b.options.iter().find(|x| x.recommended).map(|x| x.label.clone()),
        outcome: Some(o.kind.clone()),
        extra,
        at: b.asked_at.clone(),
        ..DecisionLine::default()
    }
}

/// The Hive's record as the state has it now: a checkpoint may have changed since the call began.
pub(crate) fn current_hive<W: HiveWindow>(ctx: &Ctx<'_, W>, caller: &Caller) -> Option<Hive> {
    let d = ctx.data.read();
    d.window(&caller.label)?.workspaces.iter().find(|ws| ws.id == caller.workspace_id)?.hive.clone()
}

/// An offer line the log has no answer or lapse for.
#[derive(Debug, Clone)]
struct Unanswered {
    id: String,
    checkpoint: String,
    key: String,
    task_id: Option<String>,
}

fn unanswered(lines: &[DecisionLine]) -> Vec<Unanswered> {
    let closed: std::collections::HashSet<&str> = lines.iter().filter(|l| l.event == EVENT_ANSWERED || l.event == EVENT_LAPSED).map(|l| l.id.as_str()).collect();
    lines
        .iter()
        .filter(|l| !closed.contains(l.id.as_str()))
        .filter_map(|l| Some(Unanswered { id: l.id.clone(), checkpoint: offer_checkpoint(l)?.to_string(), key: l.key.clone()?, task_id: l.task_id.clone() }))
        .collect()
}

/// The offer still waits for the human: its own question is on its row. Read against the log's
/// key, never the row alone, because the row is on the coordinator's tab and the coordinator can
/// write its tasks: a row it cleared isn't an answer, and one it blocked itself isn't an offer.
fn stands(tasks: &[Task], coordinator: &str, u: &Unanswered) -> bool {
    let title = Task::normalize_title(&row_title(&u.checkpoint));
    tasks.iter().any(|t| {
        t.tab_id.as_deref() == Some(coordinator)
            && t.normalized_title == title
            && t.status == "blocked"
            && t.blocker.as_ref().is_some_and(|b| super::prompts::blocker_key(&t.id, &b.asked_at) == u.key)
    })
}

/// The checkpoints with an offer standing.
fn open_rows(tasks: &[Task], coordinator: &str, waiting: &[Unanswered]) -> Vec<String> {
    waiting.iter().filter(|u| stands(tasks, coordinator, u)).map(|u| u.checkpoint.clone()).collect()
}

/// A withdrawn offer's outcome, as Memory → Decisions shows it.
pub const WITHDRAWN: &str = "withdrawn: its question left the row without your answer";

/// An offer whose question left its row with no answer through maiTerm: lapsed, so the log says
/// why it went, and so it neither starts a count nor holds the next offer back.
fn withdrawn_line(hive_id: &str, u: &Unanswered) -> DecisionLine {
    DecisionLine {
        id: u.id.clone(),
        event: EVENT_LAPSED.to_string(),
        hive_id: hive_id.to_string(),
        category: decisions::CATEGORY_OFFER.to_string(),
        source: SOURCE_OFFER.to_string(),
        task_id: u.task_id.clone(),
        key: Some(u.key.clone()),
        outcome: Some(WITHDRAWN.to_string()),
        at: crate::commands::workspace::iso_now(),
        ..DecisionLine::default()
    }
}

/// One review at a time in this process: an offer's row is written before its line, so a second
/// review reading between the two would take maiTerm's own row for a stranger's and file again.
static REVIEWING: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

/// After a human answer or override is logged: offers withdrawn without an answer lapsed, then the
/// offers now due filed. A checkpoint whose offer was just found withdrawn is filed again on the
/// next review, not this one: the window clears a row before the human's answer reaches Rust, so
/// a review in between can't tell the two apart, and a lapse that later meets its answer is
/// harmless where a second card is not.
pub(crate) async fn review<W: HiveWindow>(ctx: &Ctx<'_, W>, caller: &Caller) -> Vec<Offer> {
    let (Some(hive), Some(coordinator)) = (current_hive(ctx, caller), super::tools::coordinator_tab(ctx.data, caller)) else { return Vec::new() };
    if hive.status == "done" {
        return Vec::new();
    }
    let _one = REVIEWING.lock().await;
    let tasks = super::tools::workspace_tasks(ctx.data, caller).unwrap_or_default();
    let (paths, h, c) = (caller.paths.clone(), hive.clone(), coordinator.clone());
    let Ok((offers, waiting, withdrawn)) = blocking(move || {
        let lines = decisions::read(&paths);
        let (waiting, withdrawn): (Vec<_>, Vec<_>) = unanswered(&lines).into_iter().partition(|u| stands(&tasks, &c, u));
        for u in &withdrawn {
            decisions::record(&paths, &withdrawn_line(&h.id, u));
        }
        let skip: Vec<String> = waiting.iter().chain(&withdrawn).map(|u| u.checkpoint.clone()).collect();
        let offers = evaluate(&lines, &h, &skip, super::fuses::now_ms(), |id| super::precedents::get(&paths, id).map(|p| p.category));
        Ok((offers, waiting, withdrawn))
    })
    .await
    else {
        return Vec::new();
    };
    if !withdrawn.is_empty() {
        for u in &withdrawn {
            log::warn!("hive {}: offer {} ({}) left its row unanswered; offered again on the next review", hive.id, u.id, u.checkpoint);
        }
        ctx.window.decisions_changed(&caller.workspace_id);
    }
    let mut filed = Vec::new();
    for o in offers {
        if file_offer(ctx, caller, &hive, &coordinator, &waiting, &o).await {
            filed.push(o);
        }
    }
    filed
}

/// One offer filed: the row's blocker (unless an offer stands there already), its line, the state
/// saved and the tasks store told.
async fn file_offer<W: HiveWindow>(ctx: &Ctx<'_, W>, caller: &Caller, hive: &Hive, coordinator: &str, waiting: &[Unanswered], o: &Offer) -> bool {
    let now = crate::mailink::board::iso_now();
    let b = offer_blocker(o, &now);
    let title = row_title(&o.checkpoint);
    let written = {
        let mut d = ctx.data.write();
        let Ok(ws) = super::tools::workspace_mut(&mut d, caller) else { return false };
        if !open_rows(&ws.tasks, coordinator, waiting).contains(&o.checkpoint) {
            let (task_id, _) = super::fuses::file_row_blocker(ws, Some(coordinator), &title, b.clone(), &format!("Needs you: {}", b.question), &now);
            Some((task_id, ws.tasks.clone(), ws.workstreams.clone()))
        } else {
            None
        }
    };
    let Some((task_id, tasks, workstreams)) = written else { return false };
    let line = offer_line(&hive.id, o, &b, &task_id);
    let paths = caller.paths.clone();
    let _ = blocking(move || {
        decisions::record(&paths, &line);
        Ok(())
    })
    .await;
    if let Err(e) = super::tools::save(ctx) {
        log::warn!("hive {}: autonomy offer row not saved: {e}", hive.id);
    }
    ctx.window.tasks_changed(&caller.workspace_id, &tasks, &workstreams);
    ctx.window.decisions_changed(&caller.workspace_id);
    log::info!("hive {}: offered {} {} ({})", hive.id, o.checkpoint, o.kind, o.count);
    true
}

/// A blocker the human answered on a coordinator row is logged (`line`): an offer's or the plan's
/// answer carried out, then any offer it makes due filed.
pub(crate) async fn after_blocker_answer<W: HiveWindow>(ctx: &Ctx<'_, W>, caller: &Caller, line: &DecisionLine) {
    let (paths, id) = (caller.paths.clone(), line.id.clone());
    let first = blocking(move || Ok(decisions::read(&paths).into_iter().find(|l| l.id == id))).await.ok().flatten();
    match first {
        Some(f) if f.event == EVENT_OFFER => on_offer_answered(ctx, caller, &f, line).await,
        Some(f) if f.source == super::plan::SOURCE_PLAN => {
            let (paths, key, answer) = (caller.paths.clone(), line.key.clone().unwrap_or_default(), line.answer.clone().unwrap_or_default());
            match blocking(move || super::plan::on_answered(&paths, &key, &answer, &crate::commands::workspace::iso_now())).await {
                Ok(Some(rec)) => log::info!("hive {}: plan {} {} by the human", caller.hive.id, rec.id, rec.status),
                Ok(None) => {}
                Err(e) => log::warn!("hive {}: the human's plan answer not recorded: {e}", caller.hive.id),
            }
        }
        _ => {}
    }
    review(ctx, caller).await;
}

/// The human answered an offer: Yes sets the checkpoint (saved, the window's mirror told, the
/// coordinator told; prompts it held under a checkpoint now Ask me move to the human); No changes
/// nothing, and the next run counts from this answer. The outcome is logged either way.
pub(crate) async fn on_offer_answered<W: HiveWindow>(ctx: &Ctx<'_, W>, caller: &Caller, offer: &DecisionLine, answered: &DecisionLine) {
    let Some(key) = offer_checkpoint(offer).map(str::to_string) else { return };
    let hand_over = offer.outcome.as_deref() == Some(HAND_OVER);
    let yes = matches!((&offer.recommended, &answered.answer), (Some(r), Some(a)) if same(r, a));
    let label = checkpoints::label(&key);
    let outcome = if !yes {
        "kept as it was".to_string()
    } else {
        let value = if hand_over { COORDINATOR } else { ASK };
        let applied = {
            let mut d = ctx.data.write();
            checkpoints::apply(&mut d, &caller.label, &caller.workspace_id, &key, value)
        };
        match applied {
            Err(e) => {
                log::warn!("hive {}: offer {} not carried out: {e}", caller.hive.id, offer.id);
                format!("not changed: {e}")
            }
            Ok(None) => if hand_over { "already the coordinator\u{2019}s" } else { "already Ask me" }.to_string(),
            Ok(Some(hive)) => {
                let snapshot = ctx.data.read().clone();
                if let Err(e) = (ctx.save)(&snapshot) {
                    log::warn!("hive {}: checkpoint {key} set but not saved: {e}", hive.id);
                }
                ctx.window.hive_changed(&caller.workspace_id, &hive);
                let mut c = caller.clone();
                c.hive = hive;
                let text = if hand_over {
                    format!("[maiTerm] Your human handed you {label}: you decide it from now on, and each decision is logged for them.")
                } else {
                    let flipped = super::prompts::flip_to_human(ctx, &c);
                    let mut s = format!("[maiTerm] Your human took {label} back: it is your human\u{2019}s again (Ask me); give your recommendation instead.");
                    if !flipped.is_empty() {
                        s.push_str(&format!(" Now your human\u{2019}s to answer: {}.", super::prompts::flipped_names(&flipped)));
                    }
                    s
                };
                if let Some(t) = super::tools::coordinator_tab(ctx.data, caller) {
                    let sent = ctx.window.deliver(&t, text, "Hive autonomy").await;
                    log::info!("hive {}: autonomy notice to the coordinator: {}", caller.hive.id, sent.describe());
                }
                if hand_over { "handed to the coordinator" } else { "handed back to the human" }.to_string()
            }
        }
    };
    log::info!("hive {}: offer {} ({key}) answered: {outcome}", caller.hive.id, offer.id);
    let (paths, hive_id, id, k) = (caller.paths.clone(), caller.hive.id.clone(), offer.id.clone(), offer.key.clone());
    let _ = blocking(move || {
        decisions::outcome(&paths, &hive_id, &id, SOURCE_OFFER, k.as_deref(), &outcome);
        Ok(())
    })
    .await;
    ctx.window.decisions_changed(&caller.workspace_id);
}

// ── The daily digest (plan D21) ──

/// What the coordinator decided alone since the last digest.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Digest {
    pub since_ms: i64,
    pub decided: u32,
    pub lands: u32,
    pub prompts: u32,
    pub questions: u32,
    pub by_precedent: u32,
    pub overridden: u32,
}

/// The digest due now, or None: the setting on, a day since the last digest (or since the Hive was
/// made), at least one decision the coordinator made alone since, and its tab live.
pub fn digest_due(lines: &[DecisionLine], hive: &Hive, now: i64, live: bool) -> Option<Digest> {
    if !hive.settings.autonomy.daily_digest || hive.status == "done" || !live {
        return None;
    }
    let since = lines
        .iter()
        .rev()
        .find(|l| l.event == EVENT_DIGEST)
        .and_then(|l| ms(&l.at))
        .or_else(|| ms(&hive.created_at))
        .or_else(|| lines.iter().find_map(|l| ms(&l.at)))?;
    if now - since < DAY_MS {
        return None;
    }
    let mut d = Digest { since_ms: since, ..Digest::default() };
    for l in lines.iter().filter(|l| ms(&l.at).is_some_and(|t| t > since && t <= now)) {
        if l.event == EVENT_OVERRIDDEN {
            d.overridden += 1;
        }
        if l.event != EVENT_ANSWERED || l.by != BY_COORDINATOR || l.answer.is_none() {
            continue;
        }
        d.decided += 1;
        match l.source.as_str() {
            decisions::SOURCE_LAND => d.lands += 1,
            decisions::SOURCE_PROMPT => d.prompts += 1,
            decisions::SOURCE_QUESTION | decisions::SOURCE_BLOCKER => d.questions += 1,
            _ => {}
        }
        if l.precedent.is_some() {
            d.by_precedent += 1;
        }
    }
    (d.decided > 0).then_some(d)
}

fn plural(n: u32, one: &str, many: &str) -> String {
    format!("{n} {}", if n == 1 { one } else { many })
}

/// `Oct 23, 14:20`, local.
fn local_when(ms: i64) -> String {
    #[cfg(unix)]
    {
        const MONTHS: [&str; 12] = ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"];
        let secs = ms.div_euclid(1000) as libc::time_t;
        // SAFETY: a zeroed `tm` is valid; localtime_r only writes the struct given.
        let mut tm: libc::tm = unsafe { std::mem::zeroed() };
        if !unsafe { libc::localtime_r(&secs, &mut tm) }.is_null() {
            return format!("{} {}, {:02}:{:02}", MONTHS[tm.tm_mon.clamp(0, 11) as usize], tm.tm_mday, tm.tm_hour, tm.tm_min);
        }
    }
    super::fuses::local_hhmm(ms)
}

pub fn digest_text(d: &Digest) -> String {
    format!(
        "[maiTerm] Daily digest, for your human (no reply needed): since {} you decided {} alone: {}, {}, {}; {} by precedent; {} by your human.",
        local_when(d.since_ms),
        plural(d.decided, "thing", "things"),
        plural(d.lands, "land", "lands"),
        plural(d.prompts, "prompt answer", "prompt answers"),
        plural(d.questions, "question", "questions"),
        d.by_precedent,
        plural(d.overridden, "overridden", "overridden"),
    )
}

/// The digest delivered into the coordinator's chat when one is due, and logged once it went
/// (held counts: it is typed when the hold clears). Whether one went.
pub(crate) async fn digest_in<W: HiveWindow>(ctx: &Ctx<'_, W>, caller: &Caller, now: i64) -> bool {
    let (Some(hive), Some(tab)) = (current_hive(ctx, caller), super::tools::coordinator_tab(ctx.data, caller)) else { return false };
    let live = matches!(ctx.window.front(&tab).await, super::rpc::Front::Job | super::rpc::Front::Unknown);
    let paths = caller.paths.clone();
    let h = hive.clone();
    let Ok(Some(d)) = blocking(move || Ok(digest_due(&decisions::read(&paths), &h, now, live))).await else { return false };
    let text = digest_text(&d);
    let sent = ctx.window.deliver(&tab, text.clone(), "Hive daily digest").await;
    log::info!("hive {}: daily digest: {}", hive.id, sent.describe());
    if matches!(sent, super::deliver::Sent::Refused(_)) {
        return false;
    }
    let line = DecisionLine {
        id: decisions::new_id(),
        event: EVENT_DIGEST.to_string(),
        hive_id: hive.id.clone(),
        category: CATEGORY_DIGEST.to_string(),
        source: CATEGORY_DIGEST.to_string(),
        question: one_line(text.trim_start_matches("[maiTerm] "), 600),
        at: crate::commands::workspace::iso_now(),
        ..DecisionLine::default()
    };
    let paths = caller.paths.clone();
    let _ = blocking(move || {
        decisions::record(&paths, &line);
        Ok(())
    })
    .await;
    ctx.window.decisions_changed(&caller.workspace_id);
    true
}

/// Every Hive checked for its digest once an hour, for the app's life.
pub fn start_digest_timer(app: std::sync::Arc<crate::state::AppState>, handle: tauri::AppHandle) {
    tauri::async_runtime::spawn(async move {
        loop {
            tokio::time::sleep(HOUR).await;
            let callers = super::tools::hive_callers(&app.app_data.read());
            for caller in callers.into_iter().filter(|c| !c.tab_id.is_empty()) {
                let ctx = super::tools::app_ctx(&app, &handle, &caller);
                digest_in(&ctx, &caller, super::fuses::now_ms()).await;
            }
        }
    });
}

#[cfg(test)]
mod tests;
