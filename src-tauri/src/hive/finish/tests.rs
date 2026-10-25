//! Done against a scratch Hive: real repos (`core.autocrlf` pinned, no global config), worker
//! records, land and run logs written as maiTerm writes them, and a stub window.

use super::*;
use crate::hive::fuses::{Fuses, FUSES_FILE};
use crate::hive::land::log::{LandLogEntry, CHECK_PASSED, KIND_BASE_CHECK, KIND_UNDO, CHECK_FAILED};
use crate::hive::queue::{self, Job};
use crate::hive::record::{HiveFolder, KIND_GIT};
use crate::hive::rpc::{Front, Outcome, Sent};
use crate::hive::worker::WorkerFolder;
use crate::hive::{ROLE_COORDINATOR, ROLE_WORKER};
use crate::state::workspace::TaskBlocker;
use crate::state::{AppData, Tab, WindowData, Workspace};
use parking_lot::RwLock;
use std::fs;
use std::process::Command;
use std::sync::Arc;

fn git(dir: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .current_dir(dir)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .args(["-c", "user.email=t@example.com", "-c", "user.name=t", "-c", "commit.gpgsign=false"])
        .args(args)
        .output()
        .unwrap();
    assert!(out.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&out.stderr));
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

fn commit(repo: &Path, file: &str) -> String {
    fs::write(repo.join(file), format!("{file}\n")).unwrap();
    git(repo, &["add", "-A"]);
    git(repo, &["commit", "-q", "--no-verify", "-m", file]);
    git(repo, &["rev-parse", "HEAD"])
}

#[derive(Default)]
struct Seen {
    calls: Vec<(String, Value)>,
    changed: Vec<Hive>,
    stopped: Vec<String>,
    front: HashMap<String, Front>,
    busy: bool,
    cards: usize,
}

#[derive(Clone)]
struct Stub(Arc<std::sync::Mutex<Seen>>);

impl HiveWindow for Stub {
    fn request(&self, verb: &'static str, args: Value) -> impl std::future::Future<Output = Outcome> + Send {
        self.0.lock().unwrap().calls.push((verb.to_string(), args));
        async { Outcome::Answered(json!({ "ok": true, "stopped": true, "cancelled": 0 })) }
    }
    fn workers_changed(&self, _workspace_id: &str) {}
    async fn deliver(&self, _tab: &str, _text: String, _what: &'static str) -> Sent {
        Sent::Delivered
    }
    fn front(&self, tab: &str) -> impl std::future::Future<Output = Front> + Send {
        let f = self.0.lock().unwrap().front.get(tab).copied().unwrap_or(Front::NoTerminal);
        async move { f }
    }
    fn stop_agent(&self, tab: &str) -> impl std::future::Future<Output = Result<bool, String>> + Send {
        let mut s = self.0.lock().unwrap();
        let running = s.front.get(tab) == Some(&Front::Job);
        if running {
            s.stopped.push(tab.to_string());
            s.front.insert(tab.to_string(), Front::Shell);
        }
        async move { Ok(running) }
    }
    fn agent_busy(&self, _tab: &str) -> bool {
        self.0.lock().unwrap().busy
    }
    fn requests_waiting(&self, _tab: &str) -> usize {
        self.0.lock().unwrap().cards
    }
    fn hive_changed(&self, _workspace_id: &str, hive: &Hive) {
        self.0.lock().unwrap().changed.push(hive.clone());
    }
}

fn ok_save(_: &AppData) -> Result<(), String> {
    Ok(())
}
fn bad_save(_: &AppData) -> Result<(), String> {
    Err("disk full".into())
}
fn ok_preflight(_: &HivePaths, _: &WorkerRecord) -> Result<(), String> {
    Ok(())
}

struct Rig {
    base: PathBuf,
    web: PathBuf,
    data: RwLock<AppData>,
    fuses: &'static Fuses,
    caller: Caller,
    seen: Arc<std::sync::Mutex<Seen>>,
}

fn tab(id: &str, role: &str) -> Tab {
    let mut t = Tab::new(id.to_string());
    t.id = id.to_string();
    t.hive_role = Some(role.to_string());
    t
}

impl Rig {
    fn new(tag: &str) -> Self {
        let base = crate::share::canon(&{
            let t = std::env::temp_dir().join(format!("hive-finish-{tag}-{}", uuid::Uuid::new_v4()));
            fs::create_dir_all(&t).unwrap();
            t
        });
        let web = base.join("web");
        fs::create_dir_all(&web).unwrap();
        git(&web, &["init", "-q", "-b", "main"]);
        git(&web, &["config", "core.autocrlf", "false"]);
        commit(&web, "README.md");
        let hive = Hive {
            id: "h1".into(),
            slug: "ship".into(),
            folders: vec![HiveFolder { name: "web".into(), path: web.to_string_lossy().into(), kind: KIND_GIT.into(), base: Some("main".into()), primary: true, ..HiveFolder::default() }],
            criteria: vec!["Login works".into(), "Docs written".into()],
            ..Hive::default()
        };
        // The machine-wide queue is shared by every test in the process: a workspace of its own.
        let ws_id = format!("ws-{}", uuid::Uuid::new_v4());
        let mut ws = Workspace::new("Ship it".into());
        ws.id = ws_id.clone();
        ws.hive = Some(hive.clone());
        ws.panes[0].tabs = vec![tab("coord", ROLE_COORDINATOR), tab("w1", ROLE_WORKER)];
        let mut win = WindowData::new("main".into());
        win.workspaces = vec![ws];
        let caller = Caller { tab_id: "coord".into(), label: "main".into(), workspace_id: ws_id, workstream: "Ship it".into(), hive, paths: HivePaths::new(base.join("hive/h1")) };
        Self {
            fuses: Box::leak(Box::new(Fuses::at(base.join(FUSES_FILE)))),
            data: RwLock::new(AppData { windows: vec![win], ..AppData::default() }),
            base,
            web,
            caller,
            seen: Arc::new(std::sync::Mutex::new(Seen::default())),
        }
    }

    fn ctx(&self, save: fn(&AppData) -> Result<(), String>) -> Ctx<'_, Stub> {
        Ctx { data: &self.data, fuses: self.fuses, window: Stub(self.seen.clone()), save, preflight: ok_preflight }
    }

    async fn finish(&self, args: Value) -> Result<Value, String> {
        finish_tool(&self.ctx(ok_save), &self.caller, &args).await
    }

    fn hive(&self) -> Hive {
        self.data.read().windows[0].workspaces[0].hive.clone().unwrap()
    }

    fn edit_hive(&self, f: impl FnOnce(&mut Hive)) {
        f(self.data.write().windows[0].workspaces[0].hive.as_mut().unwrap());
    }

    /// A worker on its own branch off main, with `extra` commits on it.
    fn worker(&self, badge: u32, slug: &str, phase: &str, extra: usize) -> WorkerRecord {
        let base_sha = git(&self.web, &["rev-parse", "main"]);
        let branch = format!("hive/ship--{slug}");
        git(&self.web, &["branch", &branch, "main"]);
        if extra > 0 {
            git(&self.web, &["checkout", "-q", &branch]);
            for i in 0..extra {
                commit(&self.web, &format!("{slug}-{i}.txt"));
            }
            git(&self.web, &["checkout", "-q", "main"]);
        }
        let w = WorkerRecord {
            task_id: format!("t-{slug}"),
            badge,
            slug: slug.into(),
            title: slug.into(),
            phase: phase.into(),
            folders: vec![WorkerFolder { name: "web".into(), kind: KIND_GIT.into(), branch: Some(branch), target: Some("main".into()), base_sha: Some(base_sha), ..WorkerFolder::default() }],
            ..WorkerRecord::default()
        };
        let dir = self.caller.paths.task(&w.task_id);
        fs::create_dir_all(&dir).unwrap();
        worker::write_worker(&dir, &w).unwrap();
        w
    }

    fn land_line(&self, id: &str, kind: &str, check: &str) -> LandLogEntry {
        let (land, folder) = id.split_once('/').unwrap();
        let e = LandLogEntry {
            id: id.into(),
            land: land.into(),
            kind: kind.into(),
            folder: folder.into(),
            target: "main".into(),
            worker: "api".into(),
            title: "Login API".into(),
            base_after: git(&self.web, &["rev-parse", "main"]),
            tree_tested: "t".into(),
            tree_landed: "t".into(),
            check: check.into(),
            ..LandLogEntry::default()
        };
        landlog::append(&self.caller.paths, &e).unwrap();
        e
    }

    fn run(&self, id: &str, commit: &str, exit: i32) {
        let mut j = Job::default();
        j.id = id.into();
        j.hive_id = "h1".into();
        j.task_id = "t-api".into();
        j.folder = "web".into();
        j.entry = "cargo test".into();
        j.state = queue::DONE.into();
        j.began = true;
        j.exit_code = Some(exit);
        j.commit = Some(commit.into());
        queue::append_run(&self.caller.paths, &j).unwrap();
    }

    fn file_in_web(&self) -> String {
        let p = self.web.join("README.md");
        p.to_string_lossy().to_string()
    }
}

impl Drop for Rig {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.base);
    }
}

fn report(criteria: Value) -> Value {
    json!({ "report": { "summary": "Shipped the login.", "criteria": criteria } })
}

fn with(criteria: Value, abandoned: Value) -> Value {
    json!({ "report": { "summary": "Shipped the login.", "criteria": criteria, "abandoned": abandoned } })
}

/// Both criteria proved by a file in the primary folder.
fn by_file(rig: &Rig) -> Value {
    let f = rig.file_in_web();
    json!([
        { "criterion": 1, "how": "The login page works", "receipts": { "files": [f] } },
        { "criterion": 2, "how": "The README says how", "receipts": { "files": ["README.md"] } },
    ])
}

async fn refused(rig: &Rig, args: Value, says: &str) {
    let e = rig.finish(args).await.unwrap_err();
    assert!(e.contains(says), "{e}");
    assert_eq!(rig.hive().status, STATUS_ACTIVE, "a refusal changes nothing");
    assert!(last_finish(&rig.caller.paths).is_none(), "and writes no report");
}

// ── hiveSetCriteria ──

#[test]
fn criteria_are_checked() {
    let (ok, reason) = parse_criteria(&json!({ "criteria": ["  Login works ", "Docs written"], "reason": "agreed" })).unwrap();
    assert_eq!((ok, reason.as_deref()), (vec!["Login works".to_string(), "Docs written".to_string()], Some("agreed")));
    let bad = |v: Value| parse_criteria(&v).unwrap_err();
    assert!(bad(json!({})).contains("criteria is required"));
    assert!(bad(json!({ "criteria": [] })).contains("1 to 20"));
    assert!(bad(json!({ "criteria": vec!["x"; 21].iter().enumerate().map(|(i, _)| format!("c{i}")).collect::<Vec<_>>() })).contains("1 to 20"));
    assert!(bad(json!({ "criteria": ["a", "a"] })).contains("repeats"));
    assert!(bad(json!({ "criteria": ["a\nb"] })).contains("spans lines"));
    assert!(bad(json!({ "criteria": ["a\u{2028}b"] })).contains("spans lines"));
    assert!(bad(json!({ "criteria": ["  "] })).contains("empty"));
    assert!(bad(json!({ "criteria": ["x".repeat(MAX_CRITERION + 1)] })).contains("over 500"));
    assert!(bad(json!({ "criteria": [3] })).contains("isn't text"));
}

#[tokio::test]
async fn set_criteria_writes_the_record_logs_it_and_tells_the_window() {
    let rig = Rig::new("criteria");
    let out = set_criteria_tool(&rig.ctx(ok_save), &rig.caller, &json!({ "criteria": ["A", "B", "C"], "reason": "the human asked" })).await.unwrap();
    assert_eq!(out["criteria"], json!([{ "n": 1, "text": "A" }, { "n": 2, "text": "B" }, { "n": 3, "text": "C" }]));
    assert_eq!(rig.hive().criteria, vec!["A", "B", "C"]);
    assert_eq!(rig.seen.lock().unwrap().changed.last().unwrap().criteria, vec!["A", "B", "C"], "the window's mirror follows");
    let log = fs::read_to_string(rig.caller.paths.criteria_log()).unwrap();
    let line: Value = serde_json::from_str(log.lines().last().unwrap()).unwrap();
    assert_eq!((line["by"].as_str(), line["reason"].as_str(), line["criteria"].as_array().unwrap().len()), (Some("coordinator"), Some("the human asked"), 3));
    assert_eq!(criteria_view(&rig.hive())[1], json!({ "n": 2, "text": "B" }), "hiveListWorkers' list");

    // A failed save takes the record back: memory never says what the disk doesn't.
    let e = set_criteria_tool(&rig.ctx(bad_save), &rig.caller, &json!({ "criteria": ["Z"] })).await.unwrap_err();
    assert!(e.contains("disk full"), "{e}");
    assert_eq!(rig.hive().criteria, vec!["A", "B", "C"]);

    rig.edit_hive(|h| h.status = STATUS_DONE.into());
    let e = set_criteria_tool(&rig.ctx(ok_save), &rig.caller, &json!({ "criteria": ["Z"] })).await.unwrap_err();
    assert!(e.contains("is done"), "{e}");
    assert_eq!(fs::read_to_string(rig.caller.paths.criteria_log()).unwrap().lines().count(), 1, "a refusal logs nothing");
}

// ── hiveFinish's refusals ──

#[tokio::test]
async fn no_criteria_or_a_pause_refuses_first() {
    let rig = Rig::new("first");
    let args = by_file(&rig);
    rig.edit_hive(|h| h.status = STATUS_PAUSED.into());
    let e = rig.finish(report(args.clone())).await.unwrap_err();
    assert!(e.contains("Resume first"), "{e}");
    rig.edit_hive(|h| {
        h.status = STATUS_ACTIVE.into();
        h.criteria.clear();
    });
    refused(&rig, report(args), "set them with hiveSetCriteria").await;
}

#[test]
fn the_reports_shape_and_caps_are_checked() {
    let bad = |v: Value| parse_finish(&v).unwrap_err();
    assert!(bad(json!({})).contains("report is required"));
    assert!(bad(json!({ "report": { "criteria": [] } })).contains("summary is required"));
    assert!(bad(json!({ "report": { "summary": "x".repeat(MAX_SUMMARY + 1), "criteria": [] } })).contains("over 8000"));
    assert!(bad(json!({ "report": { "summary": "s" } })).contains("report.criteria is required"));
    assert!(bad(report(json!([{ "criterion": 0, "how": "h", "receipts": {} }]))).contains("from 1"));
    assert!(bad(report(json!([{ "criterion": 1, "how": "x".repeat(MAX_HOW + 1), "receipts": {} }]))).contains("over 2000"));
    assert!(bad(report(json!([{ "criterion": 1, "how": "h", "receipts": { "votes": [] } }]))).contains("isn't a receipt kind"));
    assert!(bad(report(json!([{ "criterion": 1, "how": "h", "receipts": { "files": "a" } }]))).contains("must be a list"));
    let many: Vec<String> = (0..=MAX_RECEIPTS).map(|i| format!("f{i}")).collect();
    assert!(bad(report(json!([{ "criterion": 1, "how": "h", "receipts": { "files": many } }]))).contains("over 50 receipts"));
    assert!(bad(with(json!([]), json!([{ "worker": 3 }]))).contains("reason is required"));
    let ok = parse_finish(&with(json!([{ "criterion": 2, "how": "h", "receipts": { "commits": ["web:x"] } }]), json!([{ "worker": 3, "reason": "r" }]))).unwrap();
    assert_eq!((ok.criteria[0].n, ok.abandoned[0].worker.as_str()), (2, "#3"));
}

#[tokio::test]
async fn a_running_or_waiting_worker_refuses() {
    let rig = Rig::new("live");
    rig.worker(1, "api", worker::PHASE_RUNNING, 0);
    refused(&rig, report(by_file(&rig)), "#1 api (running)").await;
    let dir = rig.caller.paths.task("t-api");
    worker::update_worker(&dir, |w| {
        w.phase = worker::PHASE_WAITING.into();
        true
    })
    .unwrap();
    refused(&rig, report(by_file(&rig)), "#1 api (waiting)").await;
    worker::update_worker(&dir, |w| {
        w.phase = worker::PHASE_LANDING.into();
        true
    })
    .unwrap();
    refused(&rig, report(by_file(&rig)), "let its land finish").await;
}

#[tokio::test]
async fn an_active_job_refuses() {
    let rig = Rig::new("job");
    let spec = queue::JobSpec {
        hive_id: "h1".into(),
        workspace_id: rig.caller.workspace_id.clone(),
        window: "main".into(),
        task_id: "t-gone".into(),
        worker_slug: "gone".into(),
        argv: vec!["cargo".into(), "test".into()],
        ..queue::JobSpec::default()
    };
    let job = queue::enqueue(spec).unwrap();
    assert_eq!(queue::active_in(&rig.caller.workspace_id).len(), 1);
    refused(&rig, report(by_file(&rig)), "Runs are still queued or running").await;
    queue::release_workspace(&rig.caller.workspace_id, "test over");
    assert!(queue::active_in(&rig.caller.workspace_id).is_empty(), "{} released", job.id);
}

#[tokio::test]
async fn an_undo_under_way_or_a_waiting_card_refuses() {
    let rig = Rig::new("unsettled");
    fs::create_dir_all(rig.caller.paths.undos()).unwrap();
    let undo = rig.caller.paths.undos().join("U-0000abcd.json");
    fs::write(&undo, r#"{"id":"U-0000abcd","state":"checking"}"#).unwrap();
    refused(&rig, report(by_file(&rig)), "An undo is under way: U-0000abcd (checking)").await;
    fs::write(&undo, r#"{"id":"U-0000abcd","state":"awaiting_human"}"#).unwrap();
    refused(&rig, report(by_file(&rig)), "U-0000abcd (awaiting_human)").await;
    fs::write(&undo, r#"{"id":"U-0000abcd","state":"done"}"#).unwrap();
    rig.seen.lock().unwrap().cards = 1;
    refused(&rig, report(by_file(&rig)), "A card waits for your human").await;
    // A held undo can't be withdrawn, and the coordinator may keep the land it held on: no bar.
    fs::write(&undo, r#"{"id":"U-0000abcd","state":"held"}"#).unwrap();
    rig.seen.lock().unwrap().cards = 0;
    rig.finish(report(by_file(&rig))).await.unwrap();
}

#[tokio::test]
async fn an_open_question_to_the_human_refuses() {
    let rig = Rig::new("blocker");
    let blocker = TaskBlocker { kind: "decision".into(), question: "Ship it?".into(), context: None, options: Vec::new(), command: None, asked_at: "now".into(), asked_by: "agent".into() };
    {
        let mut d = rig.data.write();
        let ws = &mut d.windows[0].workspaces[0];
        crate::hive::fuses::file_row_blocker(ws, Some("coord"), "Pick a theme", blocker.clone(), "Needs you", "now");
    }
    refused(&rig, report(by_file(&rig)), "question open on \u{201c}Pick a theme\u{201d}").await;
    // Waiting on something outside maiTerm is no question to the human.
    rig.data.write().windows[0].workspaces[0].tasks[0].blocker = Some(TaskBlocker { kind: "external".into(), ..blocker });
    rig.finish(report(by_file(&rig))).await.unwrap();
}

#[tokio::test]
async fn unlanded_commits_need_abandoned_and_abandoned_must_be_true() {
    let rig = Rig::new("unlanded");
    rig.worker(3, "api", worker::PHASE_RETIRED, 2);
    rig.worker(4, "ui", worker::PHASE_LANDED, 0);
    refused(&rig, report(by_file(&rig)), "#3 api web: hive/ship--api, 2 commits not on main").await;
    refused(&rig, with(by_file(&rig), json!([{ "worker": 3, "reason": "superseded" }, { "worker": "ui", "reason": "?" }])), "names #4 ui, which has nothing unlanded").await;
    refused(&rig, with(by_file(&rig), json!([{ "worker": "nobody", "reason": "?" }])), "no worker of this Hive").await;
    refused(&rig, with(by_file(&rig), json!([{ "worker": 3, "reason": "a" }, { "worker": "api", "reason": "b" }])), "names #3 api twice").await;
    // A branch the human deleted counts as nothing unlanded.
    git(&rig.web, &["branch", "-D", "hive/ship--ui"]);
    let out = rig.finish(with(by_file(&rig), json!([{ "worker": "#3", "reason": "superseded by the redesign" }]))).await.unwrap();
    assert_eq!(out["done"], true);
    let rec = last_finish(&rig.caller.paths).unwrap();
    assert_eq!(rec.abandoned.len(), 1);
    assert_eq!((rec.abandoned[0].worker.as_str(), rec.abandoned[0].reason.as_str()), ("#3 api", "superseded by the redesign"));
    assert_eq!(rec.abandoned[0].branches, vec!["web: hive/ship--api, 2 commits not on main"]);
}

#[tokio::test]
async fn a_repo_maiterm_cant_read_is_no_deleted_branch() {
    let rig = Rig::new("unreadable");
    rig.worker(3, "api", worker::PHASE_RETIRED, 2);
    // A branch under the deleted one's name isn't it: for-each-ref matches by prefix.
    rig.worker(4, "ui", worker::PHASE_LANDED, 0);
    git(&rig.web, &["branch", "-D", "hive/ship--ui"]);
    git(&rig.web, &["checkout", "-q", "-b", "hive/ship--ui/old"]);
    commit(&rig.web, "old.txt");
    git(&rig.web, &["checkout", "-q", "main"]);
    // The folder moved away (an unmounted volume, a move outside Move project).
    let gone = rig.base.join("web-moved");
    fs::rename(&rig.web, &gone).unwrap();
    let assets = json!([
        { "criterion": 1, "how": "h", "receipts": { "assets": ["a-0123456789ab"] } },
        { "criterion": 2, "how": "h", "receipts": { "assets": ["a-0123456789ab"] } },
    ]);
    refused(&rig, report(assets), "maiTerm couldn't read web").await;
    fs::rename(&gone, &rig.web).unwrap();
    refused(&rig, report(by_file(&rig)), "#3 api web: hive/ship--api, 2 commits not on main").await;
    rig.finish(with(by_file(&rig), json!([{ "worker": 3, "reason": "superseded" }]))).await.unwrap();
    assert_eq!(last_finish(&rig.caller.paths).unwrap().abandoned.iter().map(|a| a.worker.as_str()).collect::<Vec<_>>(), vec!["#3 api"]);
}

#[tokio::test]
async fn every_criterion_exactly_once_each_with_a_receipt() {
    let rig = Rig::new("index");
    let f = rig.file_in_web();
    let one = json!({ "criterion": 1, "how": "h", "receipts": { "files": [f] } });
    refused(&rig, report(json!([one.clone()])), "doesn't address criterion 2 (\u{201c}Docs written\u{201d})").await;
    refused(&rig, report(json!([one.clone(), one.clone()])), "addresses criterion 1 twice").await;
    refused(&rig, report(json!([one.clone(), { "criterion": 3, "how": "h", "receipts": { "files": [f] } }])), "names criterion 3, and this Hive has 2").await;
    refused(&rig, report(json!([one, { "criterion": 2, "how": "h", "receipts": {} }])), "Criterion 2 has no receipt").await;
}

#[tokio::test]
async fn each_receipt_is_verified_by_maiterm() {
    let rig = Rig::new("receipts");
    let f = rig.file_in_web();
    let crit = |receipts: Value| report(json!([{ "criterion": 1, "how": "h", "receipts": { "files": [f.clone()] } }, { "criterion": 2, "how": "h", "receipts": receipts }]));

    // Lands: in the log, landed, neither undone nor suspect.
    rig.land_line("L-aaaa0001/web", KIND_LAND, CHECK_PASSED);
    let mut undo = rig.land_line("U-aaaa0002/web", KIND_UNDO, "");
    undo.undoes = Some("L-aaaa0001/web".into());
    landlog::append(&rig.caller.paths, &undo).unwrap();
    refused(&rig, crit(json!({ "lands": ["L-aaaa0001/web"] })), "was undone").await;
    refused(&rig, crit(json!({ "lands": ["L-nope"] })), "isn't a land in this Hive's land log").await;
    rig.land_line("L-aaaa0003/web", KIND_LAND, "");
    rig.land_line("B-aaaa0004/web", KIND_BASE_CHECK, CHECK_FAILED);
    refused(&rig, crit(json!({ "lands": ["L-aaaa0003"] })), "is suspect").await;

    // Checks: a passing run whose commit reached main.
    let main_tip = git(&rig.web, &["rev-parse", "main"]);
    git(&rig.web, &["checkout", "-q", "-b", "side"]);
    let off = commit(&rig.web, "side.txt");
    git(&rig.web, &["checkout", "-q", "main"]);
    rig.run("j-0000aaa1", &main_tip, 1);
    rig.run("j-0000aaa2", &off, 0);
    rig.run("j-0000aaa3", &main_tip, 0);
    refused(&rig, crit(json!({ "checks": ["j-0000aaa1"] })), "didn't pass (done, exit 1)").await;
    refused(&rig, crit(json!({ "checks": ["j-0000aaa2"] })), "isn't on main in web").await;
    refused(&rig, crit(json!({ "checks": ["j-0000ffff"] })), "no record in this Hive's run log").await;

    // Commits: the full sha, on the folder's landing target.
    refused(&rig, crit(json!({ "commits": [format!("web:{off}")] })), "isn't on main in web").await;
    refused(&rig, crit(json!({ "commits": [format!("web:{}", &main_tip[..7])] })), "the full sha").await;
    refused(&rig, crit(json!({ "commits": [format!("api:{main_tip}")] })), "names no folder").await;

    // Files: inside a Hive folder, a read copy or the gallery.
    let outside = rig.base.join("outside.txt");
    fs::write(&outside, "x").unwrap();
    refused(&rig, crit(json!({ "files": [outside.to_string_lossy()] })), "isn't in a Hive folder").await;
    refused(&rig, crit(json!({ "files": ["missing.md"] })), "doesn't exist").await;

    // Assets: in this Hive's gallery.
    refused(&rig, crit(json!({ "assets": ["a-0123456789ab"] })), "isn't in this Hive's gallery").await;
    refused(&rig, crit(json!({ "assets": ["picture.png"] })), "isn't a gallery asset id").await;

    // What verifies, described in maiTerm's words.
    rig.land_line("B-aaaa0005/web", KIND_BASE_CHECK, CHECK_PASSED);
    rig.finish(crit(json!({ "checks": ["j-0000aaa3"], "commits": [format!("web:{main_tip}")], "lands": ["L-aaaa0003/web"] }))).await.unwrap();
    let rec = last_finish(&rig.caller.paths).unwrap();
    let lines = &rec.criteria[1].receipts;
    assert!(lines.iter().any(|l| l.starts_with("land L-aaaa0003/web: \u{201c}Login API\u{201d} into main")), "{lines:?}");
    assert!(lines.iter().any(|l| l.starts_with("check j-0000aaa3: `cargo test` passed on")), "{lines:?}");
    assert!(lines.iter().any(|l| *l == format!("commit web:{} on main", &main_tip[..7])), "{lines:?}");
    assert!(rec.criteria[0].receipts[0].starts_with("file ") && rec.criteria[0].receipts[0].ends_with("README.md"));
}

#[tokio::test]
async fn a_detached_base_has_no_commit_on_it() {
    let rig = Rig::new("detached");
    rig.edit_hive(|h| h.folders[0].base = None);
    let tip = git(&rig.web, &["rev-parse", "main"]);
    let f = rig.file_in_web();
    refused(&rig, report(json!([{ "criterion": 1, "how": "h", "receipts": { "files": [f] } }, { "criterion": 2, "how": "h", "receipts": { "commits": [format!("web:{tip}")] } }])), "detached HEAD").await;
}

// ── Accepting ──

#[tokio::test]
async fn an_accepted_finish_records_the_report_and_leaves_the_hive_costing_nothing() {
    let rig = Rig::new("accept");
    rig.land_line("L-bbbb0001/web", KIND_LAND, CHECK_PASSED);
    rig.worker(1, "api", worker::PHASE_LANDED, 0);
    let out = rig.finish(report(by_file(&rig))).await.unwrap();
    assert_eq!(out["done"], true);
    assert!(out["note"].as_str().unwrap().contains("wakes this Hive"));
    assert_eq!(out["stack"]["ok"], true);

    let rec = last_finish(&rig.caller.paths).unwrap();
    assert_eq!(out["finish"].as_str(), Some(rec.id.as_str()));
    assert!(rec.id.starts_with("F-") && rec.kind == KIND_FINISH && rec.by == BY_COORDINATOR);
    assert_eq!(rec.summary, "Shipped the login.");
    assert_eq!(rec.criteria.iter().map(|c| (c.n, c.text.as_str())).collect::<Vec<_>>(), vec![(1, "Login works"), (2, "Docs written")]);
    assert_eq!(rec.landed.len(), 1);
    assert_eq!((rec.landed[0].land.as_str(), rec.landed[0].target.as_str(), rec.landed[0].undone, rec.landed[0].suspect), ("L-bbbb0001/web", "main", false, false));
    assert!(rec.assets_final.is_empty() && rec.spent_micros == 0 && rec.decided_alone.is_empty(), "nothing recorded, nothing claimed");

    let h = rig.hive();
    assert_eq!(h.status, STATUS_DONE);
    assert_eq!(h.done_at, Some(rec.at_ms));
    assert_eq!(rig.seen.lock().unwrap().changed.last().map(|h| h.status.clone()).as_deref(), Some(STATUS_DONE));
    let calls = rig.seen.lock().unwrap().calls.clone();
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].0, rpc::VERB_FINISH);
    assert_eq!(calls[0].1["workspace_id"].as_str(), Some(rig.caller.workspace_id.as_str()));
    assert_eq!(calls[0].1["tab_ids"], json!(["coord", "w1"]), "every Hive tab's follow-ups go");

    let e = rig.fuses.reserve(&h, "next", Vec::new, crate::hive::fuses::now_ms()).err().unwrap();
    assert!(e.contains("done"), "a Done Hive starts no worker: {e}");
    let e = rig.finish(report(by_file(&rig))).await.unwrap_err();
    assert!(e.contains("already done"), "{e}");
    assert_eq!(fs::read_to_string(rig.caller.paths.finishes()).unwrap().lines().count(), 1);
}

#[tokio::test]
async fn the_report_carries_what_the_coordinator_decided_alone() {
    use crate::hive::decisions::{self, DecisionLine};
    let rig = Rig::new("decided");
    let line = |id: &str, event: &str, by: &str, answer: Option<&str>| DecisionLine {
        id: id.into(),
        event: event.into(),
        hive_id: "h1".into(),
        category: "permissions".into(),
        source: decisions::SOURCE_PROMPT.into(),
        question: "Run npm install?".into(),
        answer: answer.map(str::to_string),
        by: by.into(),
        at: "2026-10-25T10:00:00.000Z".into(),
        ..DecisionLine::default()
    };
    for l in [
        line("D-1", decisions::EVENT_ASKED, decisions::BY_MAITERM, None),
        line("D-1", decisions::EVENT_ANSWERED, decisions::BY_COORDINATOR, Some("Yes")),
        line("D-2", decisions::EVENT_ASKED, decisions::BY_MAITERM, None),
        line("D-2", decisions::EVENT_ANSWERED, decisions::BY_HUMAN, Some("No")),
    ] {
        decisions::append(&rig.caller.paths, &l).unwrap();
    }
    rig.finish(report(by_file(&rig))).await.unwrap();
    assert_eq!(last_finish(&rig.caller.paths).unwrap().decided_alone, vec!["permissions: Run npm install? \u{2192} Yes"], "the human's answers aren't the coordinator's");
}

thread_local! {
    /// What a racing tool call does between `check` and the save of Done: run by `racing_save`.
    static RACE: std::cell::RefCell<Option<Box<dyn FnOnce()>>> = const { std::cell::RefCell::new(None) };
}

/// The save of Done, with a racing call landing just before it (the save runs on the test's own
/// thread: `save_status` is called from the tool's task, never a blocking one).
fn racing_save(d: &AppData) -> Result<(), String> {
    if d.windows[0].workspaces[0].hive.as_ref().is_some_and(|h| h.status == STATUS_DONE) {
        if let Some(f) = RACE.with(|r| r.borrow_mut().take()) {
            f();
        }
    }
    Ok(())
}

#[tokio::test]
async fn work_that_raced_the_check_takes_done_back_and_the_window_never_hears_done() {
    for (tag, what) in [("race-worker", "#2 late"), ("race-undo", "undo U-0000beef (locking)")] {
        let rig = Rig::new(tag);
        let paths = rig.caller.paths.clone();
        RACE.with(|r| {
            *r.borrow_mut() = Some(if tag == "race-worker" {
                Box::new(move || {
                    let w = WorkerRecord { task_id: "t-late".into(), badge: 2, slug: "late".into(), title: "late".into(), phase: worker::PHASE_RUNNING.into(), ..WorkerRecord::default() };
                    let dir = paths.task(&w.task_id);
                    fs::create_dir_all(&dir).unwrap();
                    worker::write_worker(&dir, &w).unwrap();
                })
            } else {
                Box::new(move || {
                    fs::create_dir_all(paths.undos()).unwrap();
                    fs::write(paths.undos().join("U-0000beef.json"), r#"{"id":"U-0000beef","state":"locking"}"#).unwrap();
                })
            })
        });
        let e = finish_tool(&rig.ctx(racing_save), &rig.caller, &report(by_file(&rig))).await.unwrap_err();
        assert!(e.contains("started while this was checked") && e.contains(what), "{tag}: {e}");
        assert_eq!(rig.hive().status, STATUS_ACTIVE, "{tag}: Done taken back");
        assert_eq!(rig.hive().done_at, None);
        let seen = rig.seen.lock().unwrap();
        assert!(seen.changed.iter().all(|h| h.status != STATUS_DONE), "{tag}: the window never heard Done, so no notice");
        assert!(seen.calls.is_empty(), "{tag}: the window's half never ran");
    }
}

#[test]
fn a_done_hive_refuses_every_tool_but_reads() {
    let mut h = Hive { status: STATUS_ACTIVE.into(), ..Hive::default() };
    assert_eq!(crate::hive::tools::done_refusal(&h, "hiveUndoLand"), None);
    h.status = STATUS_DONE.into();
    for t in ["hiveUndoLand", "hiveLand", "hiveRestartWorker", "hiveMessageWorker", "hiveRecordPrecedent", "hiveRun", "hiveSpawnWorker", "galleryDeliver", "galleryTag"] {
        assert!(crate::hive::tools::done_refusal(&h, t).is_some_and(|e| e.contains("This Hive is done")), "{t}");
    }
    for t in crate::hive::tools::DONE_TOOLS {
        assert_eq!(crate::hive::tools::done_refusal(&h, t), None, "{t}");
    }
}

#[tokio::test]
async fn a_failed_save_leaves_the_hive_active() {
    let rig = Rig::new("save");
    let e = finish_tool(&rig.ctx(bad_save), &rig.caller, &report(by_file(&rig))).await.unwrap_err();
    assert!(e.contains("disk full"), "{e}");
    assert_eq!(rig.hive().status, STATUS_ACTIVE);
    assert!(rig.seen.lock().unwrap().calls.is_empty(), "the window's half never ran");
}

// ── Ending the coordinator ──

#[test]
fn the_coordinator_is_ended_only_while_done_once_idle_or_out_of_time() {
    for (status, busy, waited, exit) in [
        (STATUS_DONE, false, false, true),
        (STATUS_DONE, true, false, false),
        (STATUS_DONE, true, true, true),
        (STATUS_ACTIVE, false, false, false),
        (STATUS_ACTIVE, true, true, false),
        (STATUS_PAUSED, false, true, false),
    ] {
        assert_eq!(should_exit(status, busy, waited), exit, "{status} busy={busy} waited={waited}");
    }
}

#[tokio::test]
async fn a_wake_cancels_the_exit_and_an_idle_coordinator_is_signalled() {
    let rig = Rig::new("exit");
    let ms = Duration::from_millis;
    rig.seen.lock().unwrap().front.insert("coord".into(), Front::Job);
    // Woken (active again) before the wait ended: nothing is signalled.
    assert!(!exit_coordinator(&rig.ctx(ok_save), &rig.caller, ms(1), ms(1), ms(50)).await);
    rig.edit_hive(|h| h.status = STATUS_DONE.into());
    // Busy past its time: signalled anyway.
    rig.seen.lock().unwrap().busy = true;
    assert!(exit_coordinator(&rig.ctx(ok_save), &rig.caller, ms(1), ms(5), ms(20)).await);
    assert_eq!(rig.seen.lock().unwrap().stopped, vec!["coord"]);
    // Idle: signalled at once; an agent already gone is no signal.
    rig.seen.lock().unwrap().busy = false;
    assert!(!exit_coordinator(&rig.ctx(ok_save), &rig.caller, ms(1), ms(1), ms(50)).await, "the shell has its prompt now");
    rig.seen.lock().unwrap().front.insert("coord".into(), Front::Job);
    assert!(exit_coordinator(&rig.ctx(ok_save), &rig.caller, ms(1), ms(1), ms(50)).await);
}
