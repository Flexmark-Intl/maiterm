//! The human's approval of a land (docs/hive.md §8.1, §8.3; plan 6d), through agent requests
//! (`crate::agent_requests`, kind `hive_land`): only a click on the card or a paired phone
//! approves. Three things put a land to the human:
//!
//! - the config rule (§8.1), in any mode: after every repo's step 1 and check, any catch the human
//!   hasn't approved as it stands means one card for the whole land (`driver::advance_all`);
//! - Landing = Ask me (base mode), and every land in the integration mode (§8.3): the card is
//!   filed at `hiveLand`, before step 1, pinned per repo.
//!
//! While a card waits the land holds no lock and no temporary worktree (plan D4), and nothing is
//! held from the worker (plan D5: the pinned shas protect the land). `tested` and `base_before`
//! stay, so an approval with Base unmoved goes straight to step 2. On approval Rust carries the
//! land on from the arguments stored with the request and settles it; a denial or a lapse puts
//! the worker back in Reported, and the coordinator hears each as `⟦HIVE LAND⟧`. Requests live in
//! memory, so `refile_pending` files every waiting land's card again at startup, under its id.
//!
//! Every word on a card is maiTerm's but its reason, which quotes the coordinator's summary.

use std::path::Path;
use std::sync::Arc;

use serde_json::{json, Map, Value};

use super::driver::{self, Outcome};
use super::git as lg;
use super::{config_rule, LandRepo, LandState, REPO_LANDED, REPO_TESTED, STATE_AWAITING_HUMAN, STATE_HELD, STATE_LOCKING};
use crate::hive::decisions;
use crate::hive::deliver::{frame_escape, one_line};
use crate::hive::record::Hive;
use crate::hive::rpc::{HiveRequest, HiveWindow};
use crate::hive::tools::{blocking, worker_label, Caller, Ctx};
use crate::hive::worker::{self, WorkerRecord};

pub const KIND: &str = "hive_land";
/// Why a card asks: the config rule, Landing = Ask me, the integration mode.
pub const WHY_CONFIG: &str = "config";
pub const WHY_ASK: &str = "ask";
pub const WHY_INTEGRATION: &str = "integration";

/// The integration mode and Landing = Ask me ask before step 1 (§8.3, §7.4).
pub fn asks_first(hive: &Hive) -> Option<&'static str> {
    if hive.is_integration() {
        Some(WHY_INTEGRATION)
    } else if hive.checkpoints.get("landing").is_some_and(|c| c == "ask") {
        Some(WHY_ASK)
    } else {
        None
    }
}

fn now_ms() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_millis() as u64)
}

fn arg<'a>(args: &'a Value, k: &str) -> &'a str {
    args.get(k).and_then(Value::as_str).unwrap_or_default()
}

/// What a card says and carries.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Card {
    pub summary: String,
    pub effects: Vec<String>,
    pub args: Value,
}

/// `checked` / `unchecked`: the repos to land with a land check and without one. A card in the
/// integration mode is the human's only gate, so it never claims a check a folder doesn't have.
fn why_line(why: &str, target: &str, checked: &[&str], unchecked: &[&str]) -> String {
    match why {
        WHY_INTEGRATION => {
            let mut s = format!("This Hive lands into {target} only with your approval. maiTerm merges exactly the commit its queen reviewed, --no-ff");
            if unchecked.is_empty() {
                s.push_str(", and runs its land check first.");
            } else {
                let has = if unchecked.len() == 1 { "has" } else { "have" };
                s.push_str(&format!(". {} {has} no land check, so nothing tests the merge there before it lands.", unchecked.join(", ")));
                if !checked.is_empty() {
                    s.push_str(&format!(" The land check runs first in {}.", checked.join(", ")));
                }
            }
            s
        }
        WHY_ASK => "Your Landing checkpoint for this Hive is Ask me.".to_string(),
        _ => "It changes files that configure agents or run code on this machine, outside any sandbox: those always come to you."
            .to_string(),
    }
}

/// The card for a land as it stands (its pins, its catch). Blocks on git, for the diff stats.
pub(crate) fn card(hive: &Hive, workspace_id: &str, w: &WorkerRecord, land: &LandState, why: &str) -> Card {
    let title = one_line(&frame_escape(&w.title), 120);
    let unlanded: Vec<&LandRepo> = land.repos.iter().filter(|r| r.status != REPO_LANDED).collect();
    // "main in web, api", or "main in web and develop in api".
    let mut by_target: Vec<(String, Vec<String>)> = Vec::new();
    for r in &unlanded {
        match by_target.iter_mut().find(|(t, _)| *t == r.target) {
            Some((_, fs)) => fs.push(r.folder.clone()),
            None => by_target.push((r.target.clone(), vec![r.folder.clone()])),
        }
    }
    let into: Vec<String> = by_target.iter().map(|(t, fs)| format!("{t} in {}", fs.join(", "))).collect();
    let summary = format!("Land “{title}” into {}", into.join(" and "));

    let has_check = |r: &LandRepo| hive.folders.iter().any(|f| f.name == r.folder && f.land_check.as_deref().is_some_and(|c| !c.trim().is_empty()));
    let checked: Vec<&str> = unlanded.iter().filter(|r| has_check(r)).map(|r| r.folder.as_str()).collect();
    let unchecked: Vec<&str> = unlanded.iter().filter(|r| !has_check(r)).map(|r| r.folder.as_str()).collect();
    let mut effects = vec![why_line(why, &by_target.first().map(|(t, _)| t.clone()).unwrap_or_default(), &checked, &unchecked)];
    for r in &unlanded {
        let stat = driver::hive_folder(hive, &r.folder)
            .and_then(|hf| lg::shortstat(Path::new(&hf.path), &r.target, &r.pinned))
            .map(|s| if s.trim().is_empty() { "no file changes".to_string() } else { s.trim().to_string() })
            .unwrap_or_else(|e| format!("its diff couldn't be read ({})", one_line(&e, 120)));
        effects.push(format!("{}: {stat}, at the reviewed commit {}.", r.folder, &r.pinned[..r.pinned.len().min(10)]));
    }
    for r in unlanded.iter().filter(|r| !r.config_hits.is_empty()) {
        effects.push(format!("Changes agent or execution config in {}: {}.", r.folder, driver::files_line(&r.config_hits)));
    }
    effects.push(
        match land.review.as_str() {
            super::log::REVIEW_PASS => "A reviewer passed it at exactly these commits.",
            super::log::REVIEW_ACCEPTED => "Its review failed; the coordinator accepted it as it is.",
            _ => "No reviewer looked at it; its queen did.",
        }
        .to_string(),
    );
    let landed: Vec<&str> = land.landed().iter().map(|r| r.folder.as_str()).collect();
    if !landed.is_empty() {
        effects.push(format!("Already landed: {}.", landed.join(", ")));
    }

    let mut repos = Map::new();
    for r in &unlanded {
        repos.insert(r.folder.clone(), json!({ "pinned": r.pinned, "target": r.target, "config_fp": r.config_fp }));
    }
    let args = json!({
        "hive_id": hive.id,
        "workspace_id": workspace_id,
        "task_id": w.task_id,
        "land_id": land.id,
        "why": why,
        "repos": repos,
    });
    Card { summary, effects, args }
}

/// The config rule for a card filed before step 1: a repo whose step 1 stands is read from Base
/// to its tested commit (what the gate reads); any other from its merge base with the target to
/// the pin, which is what the worker's commits change. Blocks on git.
fn catch_before_step1(hive: &Hive, land: &mut LandState) -> Result<(), String> {
    for r in land.repos.iter_mut().filter(|r| r.status != REPO_LANDED) {
        let repo = Path::new(&driver::hive_folder(hive, &r.folder)?.path).to_path_buf();
        let (from, to) = match (r.status.as_str(), &r.base_before, &r.tested) {
            (REPO_TESTED, Some(b), Some(t)) => (b.clone(), t.clone()),
            _ => {
                let tip = lg::branch_tip(&repo, &r.target).ok_or_else(|| format!("{}: its landing target, {}, doesn't exist", r.folder, r.target))?;
                (lg::merge_base_of(&repo, &tip, &r.pinned).unwrap_or(tip), r.pinned.clone())
            }
        };
        let c = config_rule::caught(&repo, &from, &to, &hive.settings.land.config_paths).map_err(|e| format!("{}: {e}", r.folder))?;
        r.config_fp = Some(c.fingerprint).filter(|f| !f.is_empty());
        r.config_hits = c.files;
    }
    Ok(())
}

/// The land put to the human: its locks and temporary worktrees go (plan D4), one card is filed
/// from the coordinator's tab, and the land waits as `awaiting_human`. A card that can't be filed
/// refuses the land (or holds it, once something of it landed).
pub(crate) async fn await_human<W: HiveWindow>(ctx: &Ctx<'_, W>, caller: &Caller, w: &WorkerRecord, land: LandState, why: &'static str) -> Outcome {
    let label = worker_label(w);
    if caller.tab_id.is_empty() {
        return driver::fail(ctx, caller, w, land, "This land needs the human's approval, and this Hive has no queen tab to ask from.".to_string()).await;
    }
    let (hive, paths, rec, ws, mut l) = (caller.hive.clone(), caller.paths.clone(), w.clone(), caller.workspace_id.clone(), land.clone());
    let built = blocking(move || {
        driver::cleanup(&hive, &paths, &mut l);
        if why != WHY_CONFIG {
            catch_before_step1(&hive, &mut l)?;
        }
        let card = card(&hive, &ws, &rec, &l, why);
        Ok((l, card))
    })
    .await;
    let (mut land, card) = match built {
        Ok(b) => b,
        Err(e) => return driver::fail(ctx, caller, w, land, format!("maiTerm couldn't put this land to the human: {e}")).await,
    };
    land.state = STATE_AWAITING_HUMAN.to_string();
    land.approval = Some(why.to_string());
    land.waiting_for = None;
    land.held_reason = None;
    let req = HiveRequest {
        tab_id: caller.tab_id.clone(),
        kind: KIND,
        args: card.args,
        summary: card.summary,
        effects: card.effects,
        reason: Some(land.summary.clone()),
        keep: None,
    };
    let (summary, effects) = (req.summary.clone(), req.effects.clone());
    let id = match ctx.window.file_request(req) {
        Ok(id) => id,
        Err(e) => return driver::fail(ctx, caller, w, land, format!("maiTerm couldn't put this land to the human: {e}")).await,
    };
    // The decisions so far hear where it went. Only a card the log shows approved was outlived by
    // its config change; one denied or lapsed (its land held, then resumed) was just replaced.
    // `approved_by` can't tell: it outlives a later card's denial.
    let before = LandState { card_decision: land.card_decision.take(), ..land.clone() };
    if let Some(old) = before.card_decision.clone() {
        let (paths, hive_id, key) = (caller.paths.clone(), caller.hive.id.clone(), land.id.clone());
        let _ = blocking(move || {
            let approved = decisions::find(&decisions::read(&paths), &old)
                .is_some_and(|d| d.answered_by() == Some(decisions::BY_HUMAN) && d.answered.as_ref().and_then(|l| l.answer.as_deref()) == Some(decisions::ANSWER_LAND));
            let text = if approved { "asked again on a new card: the config change was no longer the one approved" } else { "superseded by a new card when the land resumed" };
            decisions::outcome(&paths, &hive_id, &old, decisions::SOURCE_LAND, Some(&key), text);
            Ok(())
        })
        .await;
    }
    if before.decision.is_some() {
        driver::log_outcome(ctx, caller, &LandState { card_decision: None, ..before }, format!("waiting for the human's approval ({why})")).await;
    }
    land.card_decision = Some(decisions::new_id());
    land.request = Some(id.clone());
    land.request_at = now_ms();
    let saved = match worker::task_dir(&caller.paths, &w.task_id) {
        Ok(dir) => driver::save(&dir, &land).await,
        Err(e) => Err(e),
    };
    if let Err(e) = saved {
        ctx.window.withdraw_request(&id, "maiTerm couldn't record the land it asks about");
        return driver::fail(ctx, caller, w, land, format!("maiTerm couldn't record the land it asks the human about: {e}")).await;
    }
    driver::end_hold(w);
    let (paths, hive_id, rec, l) = (caller.paths.clone(), caller.hive.id.clone(), w.clone(), land.clone());
    let _ = blocking(move || {
        decisions::land_card_asked(&paths, &hive_id, &rec, &l, why, &summary, &effects);
        Ok(())
    })
    .await;
    ctx.window.decisions_changed(&caller.workspace_id);
    let what = match why {
        WHY_INTEGRATION => "every land in this Hive's integration mode is theirs".to_string(),
        WHY_ASK => "the Landing checkpoint is Ask me".to_string(),
        _ => {
            let files: Vec<String> = land.repos.iter().filter(|r| r.config_unapproved()).map(|r| format!("{}: {}", r.folder, driver::files_line(&r.config_hits))).collect();
            format!("it changes agent or execution config ({})", files.join("; "))
        }
    };
    if let Err(e) = crate::hive::control::task_note(ctx, caller, &w.task_id, &format!("Landing waits for the human's approval: {}", one_line(&what, 300)), None).await {
        log::info!("hive {}: land approval note for {label} not written: {e}", caller.hive.id);
    }
    ctx.window.workers_changed(&caller.workspace_id);
    log::info!("hive {}: land {} of {label} waits for the human ({why}, request {id})", caller.hive.id, land.id);
    Outcome::Awaiting(json!({
        "land": land.id,
        "state": STATE_AWAITING_HUMAN,
        "request": id,
        "why": why,
        "result": format!("Worker {label}'s land {} waits for the human's approval: {what}. Nothing has landed.", land.id),
        "note": "The card is in their Decisions, in the worker's column and on their phone; don't ask them yourself. The result arrives \
                 as ⟦HIVE LAND⟧ once they answer. A denial, or no answer within 24 hours, puts the worker back in Reported.",
    }))
}

/// The human's answer as Rust carries it out: the request's id, its stored arguments, and who
/// decided (`desktop` | `phone`).
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Decision {
    pub id: String,
    pub kind: String,
    pub args: Value,
    pub by: String,
}

impl Decision {
    fn where_from(&self) -> &'static str {
        if self.by == "phone" { " from their phone" } else { "" }
    }
}

/// Why this decision no longer fits the worker's record, or None: the land on the card must still
/// be the worker's, waiting on exactly this card, with the pins it named.
fn stale(cur: &WorkerRecord, d: &Decision) -> Option<String> {
    let label = worker_label(cur);
    if cur.phase != worker::PHASE_LANDING {
        return Some(format!("worker {label} is {} now (stopped, retired or reported again after the card was filed), so nothing landed", cur.phase));
    }
    let Some(l) = cur.land.as_ref().filter(|l| l.id == arg(&d.args, "land_id")) else {
        return Some(format!("that land is no longer worker {label}'s, so nothing landed"));
    };
    if !l.awaits_human() {
        return Some(format!("land {} isn't waiting for approval any more ({})", l.id, l.state));
    }
    if l.request.as_deref() != Some(d.id.as_str()) {
        return Some(format!("a newer card for land {} replaced this one", l.id));
    }
    let named = d.args.get("repos").and_then(Value::as_object);
    let pins_kept = l.repos.iter().filter(|r| r.status != REPO_LANDED).all(|r| named.and_then(|m| m.get(&r.folder)).is_some_and(|v| arg(v, "pinned") == r.pinned));
    if !pins_kept {
        return Some(format!("land {}'s commits changed after the card was filed, so nothing landed", l.id));
    }
    None
}

/// What the request settles as, for an approved land's first run.
fn settle_line(out: &Outcome) -> (bool, String) {
    let result = |v: &Value| v.get("result").and_then(Value::as_str).unwrap_or_default().to_string();
    match out {
        Outcome::Landed(v) | Outcome::Held(v) | Outcome::Waiting(v) => (true, result(v)),
        Outcome::Checking(_) => (true, "approved: its land check runs now; the queen hears the result".to_string()),
        Outcome::Awaiting(_) => (false, "its config change is no longer the one you approved, so a new card asks again".to_string()),
        Outcome::Refused(e) => (false, e.clone()),
        Outcome::Gone => (false, "the land isn't there to carry out any more".to_string()),
    }
}

/// An approval carried out (plan 6d, "On approval"): the land takes the human's say-so and the
/// config they approved, then runs from where it stands, its locks taken again in folder order.
/// With Base unmoved a land whose step 1 stands goes to step 2 with its stored tested commit;
/// otherwise step 1 and the check run again, and a config change that differs from the approved
/// one files a new card. The request is settled here, and the coordinator told.
pub(crate) async fn approved<W: HiveWindow>(ctx: &Ctx<'_, W>, caller: &Caller, d: &Decision, opts: &driver::Opts) -> Outcome {
    if d.kind == super::undo::KIND {
        return super::undo::approved(ctx, caller, d, opts).await;
    }
    if d.kind != KIND {
        ctx.window.settle_request(&d.id, false, "maiTerm can't carry out this kind of Hive request in this build".to_string());
        return Outcome::Gone;
    }
    let task_id = arg(&d.args, "task_id").to_string();
    let dir = match worker::task_dir(&caller.paths, &task_id) {
        Ok(dir) => dir,
        Err(e) => {
            ctx.window.settle_request(&d.id, false, e);
            return Outcome::Gone;
        }
    };
    let decision = d.clone();
    let taken = blocking(move || {
        let mut why: Option<String> = None;
        let w = worker::update_worker(&dir, |cur| {
            why = stale(cur, &decision);
            let Some(l) = cur.land.as_mut().filter(|_| why.is_none()) else { return false };
            l.state = STATE_LOCKING.to_string();
            l.approved_by = Some(decision.by.clone());
            for r in l.repos.iter_mut() {
                let fp = decision.args.get("repos").and_then(|m| m.get(&r.folder)).and_then(|v| v.get("config_fp")).and_then(Value::as_str);
                if let Some(fp) = fp {
                    r.config_approved = Some(fp.to_string());
                }
            }
            true
        })?;
        Ok(match why {
            Some(e) => Err(e),
            None => Ok(w),
        })
    })
    .await;
    let w = match taken {
        Ok(Ok(w)) => w,
        Ok(Err(e)) | Err(e) => {
            log::info!("hive {}: approved request {} not carried out: {e}", caller.hive.id, d.id);
            ctx.window.settle_request(&d.id, false, e);
            return Outcome::Gone;
        }
    };
    let label = worker_label(&w);
    let land_id = w.land.as_ref().map(|l| l.id.clone()).unwrap_or_default();
    log::info!("hive {}: land {land_id} of {label} approved by the human ({})", caller.hive.id, d.by);
    card_closed(ctx, caller, &w, "approved", "").await;
    if let Some(t) = &w.tab_id {
        crate::hive::deliver::hold_for_land(t);
    }
    ctx.window.workers_changed(&caller.workspace_id);
    let out = driver::drive(ctx, caller, &task_id, opts, false).await;
    let (ok, outcome) = settle_line(&out);
    ctx.window.settle_request(&d.id, ok, outcome);
    let rest = match &out {
        Outcome::Checking(_) => Some("Its land check runs now; the result follows as ⟦HIVE LAND⟧.".to_string()),
        Outcome::Refused(e) => Some(e.clone()),
        Outcome::Landed(v) | Outcome::Held(v) | Outcome::Waiting(v) | Outcome::Awaiting(v) => v.get("result").and_then(Value::as_str).map(str::to_string),
        Outcome::Gone => None,
    };
    if let Some(rest) = rest {
        let text = format!("⟦HIVE LAND · {}⟧ The human approved worker {label}'s land {land_id}{}. {rest}", one_line(&w.title, 120), d.where_from());
        driver::tell_text(ctx, caller, &w, text).await;
    }
    out
}

/// A denial, or a card that lapsed: the land ends as a refusal (its worker back in Reported with
/// its branch as it was), or holds what's left once something of it landed. None when the card
/// is no longer the worker's land's (it was stopped, retired or reported again meanwhile).
pub(crate) async fn closed<W: HiveWindow>(ctx: &Ctx<'_, W>, caller: &Caller, d: &Decision, status: &str) -> Option<Outcome> {
    if d.kind == super::undo::KIND {
        return super::undo::closed(ctx, caller, d, status).await;
    }
    let dir = worker::task_dir(&caller.paths, arg(&d.args, "task_id")).ok()?;
    let w = blocking(move || worker::read_worker(&dir)).await.ok()?;
    if stale(&w, d).is_some() {
        return None;
    }
    let land = w.land.clone()?;
    let why = match status {
        "denied" => format!("The human denied it{}.", d.where_from()),
        "expired" => "The human didn't answer its card within 24 hours, so it lapsed.".to_string(),
        other => format!("Its card ended without an approval ({other})."),
    };
    log::info!("hive {}: land {} of {} {status} by the human", caller.hive.id, land.id, worker_label(&w));
    card_closed(ctx, caller, &w, status, &why).await;
    let out = driver::fail(ctx, caller, &w, land, why).await;
    if let Some(text) = out.message(&w) {
        driver::tell_text(ctx, caller, &w, text).await;
    }
    Some(out)
}

/// A land whose card goes with its worker's stop, retirement or new report (`cur` before its phase
/// moves): once something of it landed it holds, as a denial would (`driver::fail`), so the next
/// `hiveLand` keeps what landed and its pins (`refusal`, `start_state` read only a held land).
/// Nothing landed: it stays as it was, and a new land starts over. Call inside the same
/// `update_worker` as the phase change.
pub(crate) fn hold_if_partial(cur: &mut WorkerRecord, why: &str) {
    if cur.phase != worker::PHASE_LANDING {
        return;
    }
    let Some(l) = cur.land.as_mut().filter(|l| l.awaits_human() && !l.landed().is_empty()) else { return };
    let landed: Vec<String> = l.landed().iter().map(|r| r.folder.clone()).collect();
    l.state = STATE_HELD.to_string();
    l.held_reason = Some(format!("Landed {}; its card for the rest was withdrawn ({why}): hiveLand again to land the rest", landed.join(", ")));
    l.waiting_for = None;
    l.updated_at = crate::commands::workspace::iso_now();
}

/// The card's decision closed in the log (§7.6): `approved`, `denied`, or a lapse. Blocks briefly.
async fn card_closed<W: HiveWindow>(ctx: &Ctx<'_, W>, caller: &Caller, w: &WorkerRecord, status: &str, why: &str) {
    let Some(land) = w.land.clone().filter(|l| l.card_decision.is_some()) else { return };
    // A lapse answers nothing.
    let answered = matches!(status, "approved" | "denied");
    let (paths, hive_id, rec, status, why) = (caller.paths.clone(), caller.hive.id.clone(), w.clone(), status.to_string(), why.to_string());
    let _ = blocking(move || {
        decisions::land_card_closed(&paths, &hive_id, &rec, &land, &status, &why);
        Ok(())
    })
    .await;
    ctx.window.decisions_changed(&caller.workspace_id);
    // The human's answer may make an autonomy offer due (§7.6).
    if answered {
        crate::hive::autonomy::review(ctx, caller).await;
    }
}

/// Withdraws the card of a land that no longer waits on it: its worker was stopped, retired or
/// reported again (`w` as it was before). Its decisions hear why.
pub(crate) fn withdraw<W: HiveWindow>(window: &W, caller: &Caller, w: &WorkerRecord, why: &str) {
    let Some(land) = w.land.as_ref().filter(|l| w.phase == worker::PHASE_LANDING && l.awaits_human()) else { return };
    if let Some(id) = land.request.as_deref() {
        window.withdraw_request(id, why);
    }
    if land.decision.is_some() || land.card_decision.is_some() {
        let words = format!("withdrawn: {why}");
        decisions::land_card_closed(&caller.paths, &caller.hive.id, w, land, "withdrawn", &words);
        decisions::land_outcome(&caller.paths, &caller.hive.id, land, &words);
        window.decisions_changed(&caller.workspace_id);
    }
}

/// Every land waiting on the human filed again in its window, under its own id and filing time:
/// requests live in memory, and a restart is no answer. One a restart outlasted lapses at the
/// next read, which puts its worker back. Blocks on git.
pub(crate) fn refile_in<W: HiveWindow>(window: &W, caller: &Caller) -> usize {
    let mut n = 0;
    for w in worker::list_workers(&caller.paths) {
        let Some(land) = w.land.as_ref().filter(|l| w.phase == worker::PHASE_LANDING && l.awaits_human()) else { continue };
        let (Some(id), Some(why)) = (land.request.clone(), land.approval.clone()) else { continue };
        let card = card(&caller.hive, &caller.workspace_id, &w, land, &why);
        let req = HiveRequest {
            tab_id: caller.tab_id.clone(),
            kind: KIND,
            args: card.args,
            summary: card.summary,
            effects: card.effects,
            reason: Some(land.summary.clone()),
            keep: Some((id, land.request_at)),
        };
        match window.file_request(req) {
            Ok(_) => n += 1,
            Err(e) => log::warn!("hive {}: land {} of worker {} not put to the human again: {e}", caller.hive.id, land.id, worker_label(&w)),
        }
    }
    n
}

// ── The app's side ──

/// At startup, once the lands are reconciled: every waiting card filed again.
pub fn refile_pending(state: &Arc<crate::state::AppState>, handle: &tauri::AppHandle) -> usize {
    let callers = crate::hive::tools::hive_callers(&state.app_data.read());
    let n: usize = callers
        .iter()
        .map(|c| {
            let window = crate::hive::rpc::AppWindow { app: state.clone(), handle: handle.clone(), label: c.label.clone() };
            refile_in(&window, c) + super::undo::refile_in(&window, c)
        })
        .sum();
    if n > 0 {
        log::info!("hive: {n} land or undo card(s) put to the human again after the restart");
    }
    n
}

fn decision(r: &crate::agent_requests::AgentRequest) -> Decision {
    Decision { id: r.id.clone(), kind: r.kind.clone(), args: r.args.clone(), by: r.decided_by.clone().unwrap_or_else(|| "desktop".to_string()) }
}

fn app_and_caller(app: &tauri::AppHandle, r: &crate::agent_requests::AgentRequest) -> Option<(Arc<crate::state::AppState>, Caller)> {
    use tauri::Manager;
    let state = app.try_state::<Arc<crate::state::AppState>>()?.inner().clone();
    let hive_id = arg(&r.args, "hive_id").to_string();
    let caller = crate::hive::tools::hive_callers(&state.app_data.read()).into_iter().find(|c| c.hive.id == hive_id)?;
    Some((state, caller))
}

/// `agent_requests::decide`'s approval of a Hive request, carried out here rather than by a window.
pub fn on_approved(app: &tauri::AppHandle, r: &crate::agent_requests::AgentRequest) {
    let (app, r) = (app.clone(), r.clone());
    tauri::async_runtime::spawn(async move {
        let Some((state, caller)) = app_and_caller(&app, &r) else {
            crate::agent_requests::settle(&app, &r.id, false, "its Hive is gone".to_string());
            return;
        };
        let ctx = crate::hive::tools::app_ctx(&state, &app, &caller);
        let out = approved(&ctx, &caller, &decision(&r), &driver::Opts::production()).await;
        if out != Outcome::Gone {
            crate::hive::waiters::poll_soon(state.clone(), app.clone(), caller.workspace_id.clone());
        }
    });
}

/// A Hive request denied or lapsed: its land ends, and the coordinator is told.
pub fn on_closed(app: &tauri::AppHandle, r: &crate::agent_requests::AgentRequest) {
    let (app, r) = (app.clone(), r.clone());
    tauri::async_runtime::spawn(async move {
        let Some((state, caller)) = app_and_caller(&app, &r) else { return };
        let ctx = crate::hive::tools::app_ctx(&state, &app, &caller);
        if closed(&ctx, &caller, &decision(&r), &r.status).await.is_some() {
            crate::hive::waiters::poll_soon(state.clone(), app.clone(), caller.workspace_id.clone());
        }
    });
}
