//! Earned autonomy, the plan checkpoint's question and the daily digest, against a scratch Hive
//! folder and a stub window.

use std::path::PathBuf;
use std::sync::{Arc, Mutex, MutexGuard};

use parking_lot::RwLock;
use serde_json::json;

use super::*;
use crate::hive::decisions::{BlockerAnswerArg, DecisionLine, EVENT_ASKED, EVENT_LAPSED};
use crate::hive::deliver::Sent;
use crate::hive::fuses::{Fuses, FUSES_FILE};
use crate::hive::paths::HivePaths;
use crate::hive::plan;
use crate::hive::rpc::{Front, Outcome};
use crate::hive::worker::WorkerRecord;
use crate::hive::{ROLE_COORDINATOR, ROLE_WORKER};
use crate::state::{AppData, Tab, WindowData, Workspace, Workstream};

#[derive(Default)]
struct WinState {
    delivered: Vec<(String, String)>,
    tasks_pushed: usize,
    hives: Vec<Hive>,
    front: Option<Front>,
    refuse_delivery: bool,
}

#[derive(Clone, Default)]
struct Win(Arc<Mutex<WinState>>);

impl Win {
    fn st(&self) -> MutexGuard<'_, WinState> {
        self.0.lock().unwrap()
    }
}

impl HiveWindow for Win {
    fn request(&self, _verb: &'static str, _args: Value) -> impl std::future::Future<Output = Outcome> + Send {
        async { Outcome::Answered(json!({ "ok": true })) }
    }
    fn workers_changed(&self, _workspace_id: &str) {}
    fn deliver(&self, tab: &str, text: String, _what: &'static str) -> impl std::future::Future<Output = Sent> + Send {
        let mut st = self.st();
        let sent = if st.refuse_delivery { Sent::Refused("no agent".into()) } else { Sent::Delivered };
        if !st.refuse_delivery {
            st.delivered.push((tab.to_string(), text));
        }
        async move { sent }
    }
    fn front(&self, _tab: &str) -> impl std::future::Future<Output = Front> + Send {
        let f = self.st().front.unwrap_or(Front::Job);
        async move { f }
    }
    fn stop_agent(&self, _tab: &str) -> impl std::future::Future<Output = Result<bool, String>> + Send {
        async { Ok(false) }
    }
    fn tasks_changed(&self, _workspace_id: &str, _tasks: &[Task], _workstreams: &[Workstream]) {
        self.st().tasks_pushed += 1;
    }
    fn hive_changed(&self, _workspace_id: &str, hive: &Hive) {
        self.st().hives.push(hive.clone());
    }
}

fn ok_save(_: &AppData) -> Result<(), String> {
    Ok(())
}
fn ok_preflight(_: &HivePaths, _: &WorkerRecord) -> Result<(), String> {
    Ok(())
}

fn tab(id: &str, role: Option<&str>) -> Tab {
    let mut t = Tab::new(id.to_string());
    t.id = id.to_string();
    t.hive_role = role.map(str::to_string);
    t
}

struct Rig {
    base: PathBuf,
    data: RwLock<AppData>,
    fuses: &'static Fuses,
    caller: Caller,
    win: Win,
    coord: String,
}

impl Rig {
    fn new(mode: &str) -> Self {
        let id = uuid::Uuid::new_v4().simple().to_string();
        let base = std::env::temp_dir().join(format!("hive-autonomy-{id}"));
        let coord = format!("coord-{id}");
        let paths = HivePaths::new(base.join("hive/h1"));
        let hive = Hive {
            id: "h1".into(),
            slug: "s".into(),
            goal: "Ship it".into(),
            landing_mode: mode.into(),
            checkpoints: crate::hive::create::default_checkpoints(mode),
            created_at: "2026-10-20T09:00:00Z".into(),
            ..Hive::default()
        };
        let mut ws = Workspace::new("Ship it".into());
        ws.id = format!("ws-{id}");
        ws.hive = Some(hive.clone());
        ws.panes[0].tabs = vec![tab(&coord, Some(ROLE_COORDINATOR)), tab(&format!("w-{id}"), Some(ROLE_WORKER))];
        ws.panes[0].active_tab_id = None;
        let mut win = WindowData::new("main".into());
        win.workspaces = vec![ws.clone()];
        let caller = Caller { tab_id: coord.clone(), label: "main".into(), workspace_id: ws.id.clone(), workstream: "Ship it".into(), hive, paths };
        Self {
            fuses: Box::leak(Box::new(Fuses::at(base.join(FUSES_FILE)))),
            data: RwLock::new(AppData { windows: vec![win], ..AppData::default() }),
            base,
            caller,
            win: Win::default(),
            coord,
        }
    }

    fn ctx(&self) -> Ctx<'_, Win> {
        Ctx { data: &self.data, fuses: self.fuses, window: self.win.clone(), save: ok_save, preflight: ok_preflight }
    }

    fn set_checkpoint(&mut self, key: &str, value: &str) {
        self.caller.hive.checkpoints.insert(key.into(), value.into());
        self.data.write().windows[0].workspaces[0].hive.as_mut().unwrap().checkpoints.insert(key.into(), value.into());
    }

    fn hive(&self) -> Hive {
        self.data.read().windows[0].workspaces[0].hive.clone().unwrap()
    }

    fn tasks(&self) -> Vec<Task> {
        self.data.read().windows[0].workspaces[0].tasks.clone()
    }

    fn row(&self, title: &str) -> Option<Task> {
        self.tasks().into_iter().find(|t| t.title == title)
    }

    fn lines(&self) -> Vec<DecisionLine> {
        decisions::read(&self.caller.paths)
    }

    fn log(&self, l: DecisionLine) {
        decisions::append(&self.caller.paths, &l).unwrap();
    }

    fn to_coordinator(&self) -> Vec<String> {
        self.win.st().delivered.iter().filter(|(t, _)| *t == self.coord).map(|(_, m)| m.clone()).collect()
    }

    /// The human answers the blocker on `title`'s row as the window would: the row cleared, then
    /// `hive_blocker_answered`.
    async fn answer_row(&self, title: &str, option: &str) -> Option<String> {
        let row = self.row(title).expect("the row");
        let b = row.blocker.clone().expect("a question on it");
        let out = crate::hive::prompts::blocker_answered_in(&self.ctx(), &self.caller, &row.id, b, BlockerAnswerArg { option: Some(option.into()), text: String::new() }).await.unwrap();
        let mut d = self.data.write();
        let t = d.windows[0].workspaces[0].tasks.iter_mut().find(|t| t.id == row.id).unwrap();
        t.status = "active".into();
        t.blocker = None;
        out
    }
}

impl Drop for Rig {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.base);
    }
}

/// The human's answer in `category`, with the coordinator's recommendation (None: it gave none).
fn human(category: &str, recommended: Option<&str>, answer: &str) -> DecisionLine {
    DecisionLine {
        id: decisions::new_id(),
        event: EVENT_ANSWERED.into(),
        hive_id: "h1".into(),
        category: category.into(),
        by: BY_HUMAN.into(),
        recommended: recommended.map(str::to_string),
        answer: Some(answer.into()),
        at: "2026-10-24T10:00:00Z".into(),
        ..DecisionLine::default()
    }
}

fn taken(category: &str) -> DecisionLine {
    human(category, Some("Yes"), "yes")
}

fn overridden(category: &str, precedent: Option<&str>, at: &str) -> DecisionLine {
    DecisionLine {
        id: decisions::new_id(),
        event: EVENT_OVERRIDDEN.into(),
        hive_id: "h1".into(),
        category: category.into(),
        by: BY_HUMAN.into(),
        precedent: precedent.map(str::to_string),
        at: at.into(),
        ..DecisionLine::default()
    }
}

fn offer_for(key: &str, kind: &str) -> DecisionLine {
    let o = Offer { checkpoint: key.into(), kind: kind.into(), count: 5 };
    offer_line("h1", &o, &offer_blocker(&o, "2026-10-24T09:00:00Z"), "t-offer")
}

fn base_hive(mode: &str) -> Hive {
    Hive { id: "h1".into(), landing_mode: mode.into(), checkpoints: crate::hive::create::default_checkpoints(mode), ..Hive::default() }
}

fn asks_all(mode: &str) -> Hive {
    let mut h = base_hive(mode);
    for k in CHECKPOINTS {
        h.checkpoints.insert(k.into(), ASK.into());
    }
    h
}

const NOW: &str = "2026-10-24T12:00:00Z";

fn now() -> i64 {
    ms(NOW).unwrap()
}

#[test]
fn five_recommendations_taken_in_a_row_offer_the_checkpoint() {
    let h = asks_all("base");
    let mut lines: Vec<DecisionLine> = (0..4).map(|_| taken("questions")).collect();
    assert_eq!(streak(&lines, "questions"), 4);
    assert!(evaluate(&lines, &h, &[], now(), |_| None).is_empty(), "four is not five");
    lines.push(human("questions", Some("Postgres"), "postgres"));
    assert_eq!(streak(&lines, "questions"), 5, "by label, ignoring case");
    assert_eq!(evaluate(&lines, &h, &[], now(), |_| None), vec![Offer { checkpoint: "questions".into(), kind: HAND_OVER.into(), count: 5 }]);
    assert!(evaluate(&lines, &h, &["questions".to_string()], now(), |_| None).is_empty(), "one offer standing at a time");
    // Another category's answers are another run; an outcome line and the coordinator's own answers count for nothing.
    assert_eq!(streak(&lines, "permissions"), 0);
    lines.push(DecisionLine { by: BY_COORDINATOR.into(), ..taken("questions") });
    lines.push(DecisionLine { answer: None, outcome: Some("landed".into()), ..taken("questions") });
    assert_eq!(streak(&lines, "questions"), 5);
    // A dialog's Esc is its No.
    assert_eq!(streak(&[human("permissions", Some("No"), "Esc")], "permissions"), 1);
}

#[test]
fn a_run_ends_at_an_answer_without_a_recommendation_or_one_not_taken() {
    let mut lines: Vec<DecisionLine> = (0..5).map(|_| taken("landing")).collect();
    lines.insert(2, human("landing", None, "Land"));
    assert_eq!(streak(&lines, "landing"), 3, "the run restarts after an answer with no recommendation (plan D18)");
    lines.push(human("landing", Some("Land"), "Don\u{2019}t land"));
    assert_eq!(streak(&lines, "landing"), 0);
}

#[test]
fn an_override_of_a_cited_precedent_of_the_category_ends_the_run() {
    let mut lines: Vec<DecisionLine> = (0..4).map(|_| taken("questions")).collect();
    lines.push(overridden("questions", None, NOW));
    lines.push(taken("questions"));
    assert_eq!(streak(&lines, "questions"), 5, "no precedent cited: not this rule");
    lines.push(overridden("landing", Some("P-0000000a"), NOW));
    assert_eq!(streak(&lines, "questions"), 5, "a landing precedent");
    assert_eq!(streak_in(&lines, "questions", |id| (id == "P-0000000a").then(|| "questions".to_string())), 0, "the precedent's own category counts");
    lines.push(overridden("questions", Some("P-0000000b"), NOW));
    assert_eq!(streak(&lines, "questions"), 0);
}

/// The human's answer to `offer`, as `record_blocker_answer` logs it.
fn answer_to(offer: &DecisionLine, answer: &str, at: &str) -> DecisionLine {
    DecisionLine { id: offer.id.clone(), key: offer.key.clone(), at: at.into(), ..human(decisions::CATEGORY_OFFER, offer.recommended.as_deref(), answer) }
}

#[test]
fn an_offer_restarts_the_count_and_offers_never_count() {
    let mut lines: Vec<DecisionLine> = (0..5).map(|_| taken("plan")).collect();
    let offer = offer_for("plan", HAND_OVER);
    lines.push(offer.clone());
    assert_eq!(streak(&lines, "plan"), 5, "an offer nobody answered restarts nothing");
    lines.push(answer_to(&offer, NO_HAND_OVER, NOW));
    assert_eq!(streak(&lines, "plan"), 0, "a declined offer restarts the count");
    assert!(evaluate(&lines, &asks_all("base"), &[], now(), |_| None).iter().all(|o| o.checkpoint != "offer"), "an offer's answer earns nothing");
    lines.extend((0..3).map(|_| taken("plan")));
    let other = offer_for("questions", HAND_OVER);
    lines.push(other.clone());
    lines.push(answer_to(&other, NO_HAND_OVER, NOW));
    assert_eq!(streak(&lines, "plan"), 3, "another checkpoint's offer is not this one's");
}

#[test]
fn answers_given_while_an_offer_stood_count_for_nothing_after_it_is_answered() {
    let mut lines: Vec<DecisionLine> = (0..5).map(|_| taken("permissions")).collect();
    let offer = offer_for("permissions", HAND_OVER);
    lines.push(offer.clone());
    lines.extend((0..5).map(|_| taken("permissions")));
    lines.push(answer_to(&offer, NO_HAND_OVER, NOW));
    assert_eq!(streak(&lines, "permissions"), 0, "the run starts at the No, not at the offer");
    assert!(evaluate(&lines, &asks_all("base"), &[], now(), |_| None).is_empty(), "not asked again at once");
    lines.extend((0..4).map(|_| taken("permissions")));
    assert_eq!(streak(&lines, "permissions"), 4);
}

#[test]
fn interfaces_and_integration_landing_are_never_offered() {
    let lines: Vec<DecisionLine> = ["interfaces", "landing"].iter().flat_map(|c| (0..6).map(|_| taken(c))).collect();
    let integ = asks_all(LANDING_INTEGRATION_MODE);
    assert!(evaluate(&lines, &integ, &[], now(), |_| None).is_empty());
    let base = asks_all("base");
    assert_eq!(evaluate(&lines, &base, &[], now(), |_| None).iter().map(|o| o.checkpoint.as_str()).collect::<Vec<_>>(), vec!["landing"]);
    assert!(!earnable(&integ, "landing") && earnable(&base, "landing"));
    assert!(!earnable(&base, "interfaces"));
}

/// The coordinator's `galleryTag` final/rejected decisions (step 8b) are the finals signal: two
/// overrides of them in a day offer the checkpoint back.
#[test]
fn finals_is_handed_back_after_two_overrides_of_the_coordinators_picks() {
    let h = base_hive("base");
    assert!(earnable(&h, "finals"));
    let lines = vec![overridden("finals", None, "2026-10-24T10:00:00Z"), overridden("finals", None, "2026-10-24T11:00:00Z")];
    assert_eq!(evaluate(&lines, &h, &[], now(), |_| None), vec![Offer { checkpoint: "finals".into(), kind: HAND_BACK.into(), count: 2 }]);
}

const LANDING_INTEGRATION_MODE: &str = crate::hive::record::LANDING_INTEGRATION;

#[test]
fn two_overrides_in_a_day_offer_the_checkpoint_back_once() {
    let h = base_hive("base");
    let mut lines = vec![overridden("permissions", None, "2026-10-23T10:00:00Z"), overridden("permissions", None, "2026-10-24T11:00:00Z")];
    assert!(evaluate(&lines, &h, &[], now(), |_| None).is_empty(), "one is older than a day");
    lines.push(overridden("permissions", None, "2026-10-24T11:30:00Z"));
    let offers = evaluate(&lines, &h, &[], now(), |_| None);
    assert_eq!(offers, vec![Offer { checkpoint: "permissions".into(), kind: HAND_BACK.into(), count: 2 }]);
    assert!(offer_blocker(&offers[0], NOW).question.contains("overridden the coordinator twice in the last day"));
    let mut asks = h.clone();
    asks.checkpoints.insert("permissions".into(), ASK.into());
    assert!(evaluate(&lines, &asks, &[], now(), |_| None).is_empty(), "already the human's");
    // Filed but never answered (withdrawn): it holds nothing back.
    let offer = DecisionLine { at: "2026-10-24T11:40:00Z".into(), ..offer_for("permissions", HAND_BACK) };
    lines.push(offer.clone());
    assert_eq!(evaluate(&lines, &h, &[], now(), |_| None).len(), 1, "an unanswered offer is no reset");
    // Answered No: not again, even with another override, until the day has passed.
    lines.push(answer_to(&offer, NO_HAND_BACK, "2026-10-24T11:41:00Z"));
    lines.push(overridden("permissions", None, "2026-10-24T11:50:00Z"));
    lines.push(overridden("permissions", None, "2026-10-24T11:55:00Z"));
    assert!(evaluate(&lines, &h, &[], now(), |_| None).is_empty());
    let later = ms("2026-10-25T11:45:00Z").unwrap();
    assert_eq!(evaluate(&lines, &h, &[], later, |_| None).len(), 1, "two since the answer, a day after it");
}

#[tokio::test]
async fn an_offer_is_filed_once_and_yes_hands_the_checkpoint_over() {
    let mut rig = Rig::new("base");
    rig.set_checkpoint("questions", ASK);
    for _ in 0..5 {
        rig.log(taken("questions"));
    }
    let filed = review(&rig.ctx(), &rig.caller).await;
    assert_eq!(filed.len(), 1);
    let title = row_title("questions");
    let row = rig.row(&title).expect("maiTerm's row on the coordinator's tab");
    assert_eq!((row.status.as_str(), row.tab_id.as_deref()), ("blocked", Some(rig.coord.as_str())));
    let b = row.blocker.clone().unwrap();
    assert_eq!(b.question, "You\u{2019}ve agreed with the coordinator\u{2019}s last 5 calls on workers\u{2019} questions. Let it handle workers\u{2019} questions from now on?");
    assert_eq!((b.kind.as_str(), b.options[0].label.as_str(), b.options[0].recommended), ("decision", YES_HAND_OVER, true));
    let offer = rig.lines().pop().unwrap();
    assert_eq!((offer.event.as_str(), offer.category.as_str(), offer_checkpoint(&offer), offer.outcome.as_deref()), (EVENT_OFFER, "offer", Some("questions"), Some(HAND_OVER)));
    assert_eq!(offer.key, Some(crate::hive::prompts::blocker_key(&row.id, &b.asked_at)));
    assert_eq!(rig.win.st().tasks_pushed, 1, "the tasks store adopts Rust's row");
    assert!(review(&rig.ctx(), &rig.caller).await.is_empty(), "the row stands: not filed twice");

    rig.answer_row(&title, YES_HAND_OVER).await.unwrap();
    assert!(!checkpoints::asks(&rig.hive(), "questions"), "Yes handed it over");
    assert_eq!(rig.win.st().hives.len(), 1, "the window's mirror was told");
    assert!(rig.to_coordinator().iter().any(|m| m.starts_with("[maiTerm] Your human handed you workers\u{2019} questions")), "{:?}", rig.to_coordinator());
    let d = decisions::find(&rig.lines(), &offer.id).unwrap();
    assert_eq!((d.answered_by(), d.category(), d.outcome.as_deref()), (Some(BY_HUMAN), "offer", Some("handed to the coordinator")));
    assert!(review(&rig.ctx(), &rig.caller).await.is_empty(), "the offer's own answer starts no new run");
}

#[tokio::test]
async fn no_keeps_the_checkpoint_and_the_next_offer_needs_five_more() {
    let mut rig = Rig::new("base");
    rig.set_checkpoint("permissions", ASK);
    for _ in 0..5 {
        rig.log(taken("permissions"));
    }
    review(&rig.ctx(), &rig.caller).await;
    rig.answer_row(&row_title("permissions"), NO_HAND_OVER).await.unwrap();
    assert!(checkpoints::asks(&rig.hive(), "permissions"), "No changes nothing");
    assert!(rig.win.st().hives.is_empty() && rig.to_coordinator().is_empty());
    for _ in 0..4 {
        rig.log(taken("permissions"));
    }
    assert!(review(&rig.ctx(), &rig.caller).await.is_empty());
    rig.log(taken("permissions"));
    assert_eq!(review(&rig.ctx(), &rig.caller).await.len(), 1, "another five");
}

#[tokio::test]
async fn no_after_answers_given_while_the_offer_stood_is_not_asked_again_at_once() {
    let mut rig = Rig::new("base");
    rig.set_checkpoint("permissions", ASK);
    for _ in 0..5 {
        rig.log(taken("permissions"));
    }
    assert_eq!(review(&rig.ctx(), &rig.caller).await.len(), 1);
    for _ in 0..5 {
        rig.log(taken("permissions"));
        assert!(review(&rig.ctx(), &rig.caller).await.is_empty(), "the offer stands");
    }
    rig.answer_row(&row_title("permissions"), NO_HAND_OVER).await.unwrap();
    let offers = |rig: &Rig| rig.lines().iter().filter(|l| l.event == EVENT_OFFER).count();
    assert_eq!(offers(&rig), 1, "the No filed no new offer");
    for _ in 0..4 {
        rig.log(taken("permissions"));
        assert!(review(&rig.ctx(), &rig.caller).await.is_empty());
    }
    rig.log(taken("permissions"));
    assert_eq!(review(&rig.ctx(), &rig.caller).await.len(), 1, "five after the No");
}

/// The coordinator moves `title`'s row out of Blocked with `updateTasks`: the blocker goes, and
/// nothing reaches `hive_blocker_answered`.
fn coordinator_clears(rig: &Rig, title: &str) {
    let mut d = rig.data.write();
    let t = d.windows[0].workspaces[0].tasks.iter_mut().find(|t| t.title == title).unwrap();
    t.status = "active".into();
    t.blocker = None;
}

#[tokio::test]
async fn a_hand_back_the_coordinator_withdrew_is_logged_and_offered_again() {
    let rig = Rig::new("base");
    let at = crate::mailink::board::iso_now();
    rig.log(overridden("landing", None, &at));
    rig.log(overridden("landing", None, &at));
    assert_eq!(review(&rig.ctx(), &rig.caller).await.len(), 1);
    let first = rig.lines().into_iter().rfind(|l| l.event == EVENT_OFFER).unwrap();
    coordinator_clears(&rig, &row_title("landing"));

    assert!(review(&rig.ctx(), &rig.caller).await.is_empty(), "lapsed now, offered on the next review");
    let d = decisions::find(&rig.lines(), &first.id).unwrap();
    assert_eq!((d.lapsed.is_some(), d.answered.is_some(), d.outcome.as_deref()), (true, false, Some(WITHDRAWN)), "the log says why it went");
    let again = review(&rig.ctx(), &rig.caller).await;
    assert_eq!(again.iter().map(|o| o.kind.as_str()).collect::<Vec<_>>(), vec![HAND_BACK], "no day's wait, the overrides still count");
    let row = rig.row(&row_title("landing")).unwrap();
    assert_eq!(row.status, "blocked");
    assert!(review(&rig.ctx(), &rig.caller).await.is_empty(), "the new one stands");
    rig.answer_row(&row_title("landing"), YES_HAND_BACK).await.unwrap();
    assert!(checkpoints::asks(&rig.hive(), "landing"));
}

#[tokio::test]
async fn a_row_the_coordinator_blocked_itself_is_not_an_offer() {
    let mut rig = Rig::new("base");
    rig.set_checkpoint("questions", ASK);
    let title = row_title("questions");
    {
        let mut d = rig.data.write();
        let ws = &mut d.windows[0].workspaces[0];
        let own = TaskBlocker { question: "Mine?".into(), ..offer_blocker(&Offer { checkpoint: "questions".into(), kind: HAND_OVER.into(), count: 5 }, "2026-10-24T08:00:00Z") };
        crate::hive::fuses::file_row_blocker(ws, Some(&rig.coord), &title, own, "x", "2026-10-24T08:00:00Z");
    }
    for _ in 0..5 {
        rig.log(taken("questions"));
    }
    assert_eq!(review(&rig.ctx(), &rig.caller).await.len(), 1, "filed over the coordinator's own question");
    assert!(rig.row(&title).unwrap().blocker.unwrap().question.starts_with("You\u{2019}ve agreed"), "maiTerm's words on the row");
    assert_eq!(rig.tasks().iter().filter(|t| t.title == title).count(), 1);
}

#[tokio::test]
async fn yes_to_a_hand_back_sets_ask_me() {
    let rig = Rig::new("base");
    let at = crate::mailink::board::iso_now();
    rig.log(overridden("landing", None, &at));
    rig.log(overridden("landing", None, &at));
    let filed = review(&rig.ctx(), &rig.caller).await;
    assert_eq!(filed.iter().map(|o| o.kind.as_str()).collect::<Vec<_>>(), vec![HAND_BACK]);
    assert!(review(&rig.ctx(), &rig.caller).await.is_empty(), "once");
    rig.answer_row(&row_title("landing"), YES_HAND_BACK).await.unwrap();
    assert!(checkpoints::asks(&rig.hive(), "landing"));
    assert!(rig.to_coordinator().iter().any(|m| m.contains("took landing back")), "{:?}", rig.to_coordinator());
}

#[tokio::test]
async fn an_integration_hive_is_never_offered_landing() {
    let rig = Rig::new(LANDING_INTEGRATION_MODE);
    for _ in 0..6 {
        rig.log(taken("landing"));
    }
    assert!(review(&rig.ctx(), &rig.caller).await.is_empty());
}

// ── The plan's question ──

#[tokio::test]
async fn a_plan_is_asked_on_the_coordinators_row_and_go_ahead_approves_it() {
    let mut rig = Rig::new("base");
    let args = json!({ "summary": "Split it.", "workers": [{ "title": "Login API", "role": "backend" }, { "title": "Login UI" }] });
    let out = plan::propose_tool(&rig.ctx(), &rig.caller, &args).await.unwrap();
    assert_eq!(out["needed"], false, "the plan is the coordinator's by default");
    assert!(plan::read(&rig.caller.paths).is_none());

    rig.set_checkpoint("plan", ASK);
    let first = plan::propose_tool(&rig.ctx(), &rig.caller, &args).await.unwrap();
    let out = plan::propose_tool(&rig.ctx(), &rig.caller, &args).await.unwrap();
    assert_eq!((out["needed"].as_bool(), out["replaced"].as_str()), (Some(true), first["plan"].as_str()), "a new proposal replaces one waiting");
    let row = rig.row(plan::PLAN_TASK_TITLE).unwrap();
    assert_eq!(rig.tasks().iter().filter(|t| t.title == plan::PLAN_TASK_TITLE).count(), 1, "one row");
    assert_eq!(row.blocker.as_ref().unwrap().question, "Start these 2 workers for \u{201c}Ship it\u{201d}?");
    let rec = plan::read(&rig.caller.paths).unwrap();
    assert_eq!((rec.status.as_str(), rec.titles.len(), rec.key.clone()), (plan::STATUS_PROPOSED, 2, crate::hive::prompts::blocker_key(&row.id, &row.blocker.as_ref().unwrap().asked_at)));
    let lines = rig.lines();
    assert!(lines.iter().any(|l| l.event == EVENT_LAPSED && Some(&l.id) != Some(&rec.decision_id)), "the replaced one lapsed");
    let asked = lines.iter().find(|l| l.id == rec.decision_id && l.event == EVENT_ASKED).unwrap();
    assert_eq!((asked.category.as_str(), asked.source.as_str(), asked.recommended.as_deref()), ("plan", "plan", Some(plan::ANSWER_GO)));

    rig.answer_row(plan::PLAN_TASK_TITLE, plan::ANSWER_GO).await.unwrap();
    assert_eq!(plan::read(&rig.caller.paths).unwrap().status, plan::STATUS_APPROVED);
    let d = decisions::find(&rig.lines(), &rec.decision_id).unwrap();
    let a = d.answered.unwrap();
    assert_eq!((a.category.as_str(), a.recommended.as_deref(), a.answer.as_deref()), ("plan", Some(plan::ANSWER_GO), Some(plan::ANSWER_GO)), "a taken recommendation toward a plan streak");
}

// ── The daily digest ──

fn by_coordinator(source: &str, at: &str, precedent: Option<&str>) -> DecisionLine {
    DecisionLine {
        id: decisions::new_id(),
        event: EVENT_ANSWERED.into(),
        source: source.into(),
        by: BY_COORDINATOR.into(),
        answer: Some("Yes".into()),
        precedent: precedent.map(str::to_string),
        at: at.into(),
        ..DecisionLine::default()
    }
}

#[test]
fn a_digest_is_due_a_day_on_with_something_decided_alone() {
    let mut h = base_hive("base");
    h.created_at = "2026-10-23T10:00:00Z".into();
    let lines = vec![
        by_coordinator(decisions::SOURCE_LAND, "2026-10-23T11:00:00Z", Some("P-1")),
        by_coordinator(decisions::SOURCE_PROMPT, "2026-10-23T12:00:00Z", None),
        by_coordinator(decisions::SOURCE_BLOCKER, "2026-10-23T13:00:00Z", None),
        by_coordinator(decisions::SOURCE_RESOLVE, "2026-10-23T14:00:00Z", None),
        human("questions", None, "x"),
        overridden("landing", Some("P-1"), "2026-10-23T15:00:00Z"),
    ];
    let d = digest_due(&lines, &h, now(), true).unwrap();
    assert_eq!((d.decided, d.lands, d.prompts, d.questions, d.by_precedent, d.overridden), (4, 1, 1, 1, 1, 1));
    let text = digest_text(&d);
    assert!(text.starts_with("[maiTerm] Daily digest, for your human (no reply needed): since "), "{text}");
    assert!(text.ends_with("you decided 4 things alone: 1 land, 1 prompt answer, 1 question; 1 by precedent; 1 overridden by your human."), "{text}");

    assert!(digest_due(&lines, &h, ms("2026-10-24T09:59:00Z").unwrap(), true).is_none(), "not a day yet");
    assert!(digest_due(&lines, &h, now(), false).is_none(), "the coordinator isn't live");
    let mut off = h.clone();
    off.settings.autonomy.daily_digest = false;
    assert!(digest_due(&lines, &off, now(), true).is_none(), "the setting is off");
    assert!(digest_due(&lines[4..], &h, now(), true).is_none(), "nothing decided alone");
    let mut after = lines.clone();
    after.push(DecisionLine { id: decisions::new_id(), event: EVENT_DIGEST.into(), at: "2026-10-24T09:00:00Z".into(), ..DecisionLine::default() });
    assert!(digest_due(&after, &h, now(), true).is_none(), "the last digest is the start");
    after.push(by_coordinator(decisions::SOURCE_LAND, "2026-10-24T10:00:00Z", None));
    assert_eq!(digest_due(&after, &h, ms("2026-10-25T09:30:00Z").unwrap(), true).map(|d| d.decided), Some(1));
}

#[tokio::test]
async fn one_digest_line_per_delivery() {
    let rig = Rig::new("base");
    rig.log(by_coordinator(decisions::SOURCE_PROMPT, "2026-10-21T10:00:00Z", None));
    let at = ms("2026-10-21T12:00:00Z").unwrap();
    rig.win.st().refuse_delivery = true;
    assert!(!digest_in(&rig.ctx(), &rig.caller, at).await, "not delivered: not logged, tried again next hour");
    rig.win.st().refuse_delivery = false;
    rig.win.st().front = Some(Front::Shell);
    assert!(!digest_in(&rig.ctx(), &rig.caller, at).await, "no agent in front");
    rig.win.st().front = None;
    assert!(digest_in(&rig.ctx(), &rig.caller, at).await);
    assert!(!digest_in(&rig.ctx(), &rig.caller, at + 3_600_000).await, "the digest just sent is the start");
    let digests: Vec<DecisionLine> = rig.lines().into_iter().filter(|l| l.event == EVENT_DIGEST).collect();
    assert_eq!(digests.len(), 1);
    assert_eq!(rig.to_coordinator().len(), 1);
    assert!(rig.to_coordinator()[0].contains("1 prompt answer"));
    let view = decisions::view(&rig.caller.paths);
    assert_eq!((view[0].status.as_str(), view[0].category.as_str()), ("digest", CATEGORY_DIGEST), "listed among the decisions, newest first");
}
