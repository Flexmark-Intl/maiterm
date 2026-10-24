//! The land engine against scratch repos and worktrees (docs/hive.md §8.1, §8.2, §8.5).

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use parking_lot::RwLock;
use serde_json::{json, Value};

use super::driver::{CheckRequest, CheckStart, Checker, Opts};
use super::*;
use crate::hive::fuses::{Fuses, FUSES_FILE};
use crate::hive::paths::HivePaths;
use crate::hive::record::{Hive, HiveFolder, KIND_GIT, LANDING_INTEGRATION};
use crate::hive::rpc::{Front, HiveRequest, HiveWindow, Outcome, Sent};
use crate::hive::tools::{Caller, Ctx};
use crate::hive::worker::{self, Review, Verdict, WorkerRecord, WorkerReport};
use crate::hive::ROLE_COORDINATOR;
use crate::state::{AppData, Tab, WindowData, Workspace};

fn git(dir: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .current_dir(dir)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .args(["-c", "user.email=t@example.com", "-c", "user.name=t", "-c", "commit.gpgsign=false"])
        .args(args)
        .output()
        .unwrap();
    assert!(out.status.success(), "git {args:?} in {}: {}", dir.display(), String::from_utf8_lossy(&out.stderr));
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

fn commit(dir: &Path, file: &str, text: &str) -> String {
    if let Some(p) = Path::new(file).parent() {
        fs::create_dir_all(dir.join(p)).unwrap();
    }
    fs::write(dir.join(file), text).unwrap();
    git(dir, &["add", "-A"]);
    git(dir, &["commit", "-q", "--no-verify", "-m", &format!("edit {file}")]);
    git(dir, &["rev-parse", "HEAD"])
}

#[derive(Default)]
struct Seen {
    verbs: Vec<(String, Value)>,
    delivered: Vec<(String, String)>,
    busy: bool,
    filed: Vec<HiveRequest>,
    settled: Vec<(String, bool, String)>,
    withdrawn: Vec<String>,
}

#[derive(Clone)]
struct Stub(Arc<Mutex<Seen>>);

impl HiveWindow for Stub {
    fn request(&self, verb: &'static str, args: Value) -> impl std::future::Future<Output = Outcome> + Send {
        self.0.lock().unwrap().verbs.push((verb.to_string(), args));
        async { Outcome::Answered(json!({ "ok": true })) }
    }
    fn workers_changed(&self, _workspace_id: &str) {}
    fn deliver(&self, tab: &str, text: String, _what: &'static str) -> impl std::future::Future<Output = Sent> + Send {
        self.0.lock().unwrap().delivered.push((tab.to_string(), text));
        async { Sent::Delivered }
    }
    async fn front(&self, _tab: &str) -> Front {
        Front::NoTerminal
    }
    async fn stop_agent(&self, _tab: &str) -> Result<bool, String> {
        Ok(false)
    }
    fn agent_busy(&self, _tab: &str) -> bool {
        self.0.lock().unwrap().busy
    }
    fn file_request(&self, req: HiveRequest) -> Result<String, String> {
        let mut seen = self.0.lock().unwrap();
        let id = req.keep.clone().map_or_else(|| format!("r_{}", seen.filed.len() + 1), |(id, _)| id);
        seen.filed.push(req);
        Ok(id)
    }
    fn settle_request(&self, id: &str, ok: bool, outcome: String) {
        self.0.lock().unwrap().settled.push((id.to_string(), ok, outcome));
    }
    fn withdraw_request(&self, id: &str, _why: &str) {
        self.0.lock().unwrap().withdrawn.push(id.to_string());
    }
}

fn ok_save(_: &AppData) -> Result<(), String> {
    Ok(())
}
fn ok_preflight(_: &HivePaths, _: &WorkerRecord) -> Result<(), String> {
    Ok(())
}

/// Never asked: these lands have no land check.
struct NoChecker;
impl Checker for NoChecker {
    fn start(&self, req: &CheckRequest) -> Result<CheckStart, String> {
        panic!("no land check was set for {}", req.folder)
    }
}

/// Runs the check where the queue would, to its end, and counts its runs.
#[derive(Default)]
struct ShChecker(Mutex<Vec<String>>);
impl Checker for ShChecker {
    fn start(&self, req: &CheckRequest) -> Result<CheckStart, String> {
        self.0.lock().unwrap().push(req.folder.clone());
        let out = Command::new("/bin/sh").arg("-c").arg(&req.command).current_dir(&req.tmp.path).output().map_err(|e| e.to_string())?;
        let tail = format!("{}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr));
        Ok(CheckStart::Done { passed: out.status.success(), tail })
    }
}

fn opts() -> Opts {
    Opts { lock_wait: Duration::from_secs(20), checker: Arc::new(NoChecker), before_advance: None }
}

struct Rig {
    base: PathBuf,
    repos: HashMap<String, PathBuf>,
    data: RwLock<AppData>,
    fuses: &'static Fuses,
    caller: Caller,
    seen: Arc<Mutex<Seen>>,
}

impl Drop for Rig {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.base);
    }
}

impl Rig {
    fn new(tag: &str, names: &[&str]) -> Self {
        Self::with(tag, names, |_| {})
    }

    fn with(tag: &str, names: &[&str], edit: impl FnOnce(&mut Hive)) -> Self {
        let base = crate::share::canon(&{
            let t = std::env::temp_dir().join(format!("hive-land-{tag}-{}", uuid::Uuid::new_v4()));
            fs::create_dir_all(&t).unwrap();
            t
        });
        let mut repos = HashMap::new();
        let mut folders = Vec::new();
        for (i, n) in names.iter().enumerate() {
            let r = base.join(n);
            fs::create_dir_all(&r).unwrap();
            git(&r, &["init", "-q", "-b", "main"]);
            commit(&r, "README.md", "r\n");
            commit(&r, "shared.txt", "one\ntwo\nthree\n");
            folders.push(HiveFolder { name: n.to_string(), path: r.to_string_lossy().into(), kind: KIND_GIT.into(), base: Some("main".into()), primary: i == 0, ..HiveFolder::default() });
            repos.insert(n.to_string(), r);
        }
        let mut hive = Hive { id: format!("land-{}", uuid::Uuid::new_v4().simple()), slug: "ship".into(), folders, ..Hive::default() };
        edit(&mut hive);
        let mut ws = Workspace::new("Ship".into());
        ws.id = "ws".into();
        ws.hive = Some(hive.clone());
        let mut coord = Tab::new("coord".into());
        coord.id = "coord".into();
        coord.hive_role = Some(ROLE_COORDINATOR.into());
        ws.panes[0].tabs = vec![coord];
        let mut win = WindowData::new("main".into());
        win.workspaces = vec![ws];
        let caller = Caller {
            tab_id: "coord".into(),
            label: "main".into(),
            workspace_id: "ws".into(),
            workstream: "Ship".into(),
            hive,
            paths: HivePaths::new(base.join("hive/h1")),
        };
        Self {
            fuses: Box::leak(Box::new(Fuses::at(base.join(FUSES_FILE)))),
            data: RwLock::new(AppData { windows: vec![win], ..AppData::default() }),
            base,
            repos,
            caller,
            seen: Arc::new(Mutex::new(Seen::default())),
        }
    }

    fn repo(&self, name: &str) -> &Path {
        &self.repos[name]
    }

    fn ctx(&self) -> Ctx<'_, Stub> {
        Ctx { data: &self.data, fuses: self.fuses, window: Stub(self.seen.clone()), save: ok_save, preflight: ok_preflight }
    }

    /// A worker on `folders` that committed `file` in each and reported done.
    fn reported(&self, slug: &str, folders: &[&str], file: &str, text: &str) -> WorkerRecord {
        let names: Vec<String> = folders.iter().map(|s| s.to_string()).collect();
        let fs_ = crate::hive::worktree::create_worker_worktrees(&self.caller.hive, &self.caller.paths, slug, &names).unwrap();
        let mut commits = Vec::new();
        for f in &fs_ {
            commits.push(commit(Path::new(f.worktree.as_ref().unwrap()), file, text));
        }
        let w = WorkerRecord {
            task_id: format!("t-{slug}"),
            slug: slug.into(),
            title: format!("Title {slug}"),
            badge: worker::next_badge(&worker::list_workers(&self.caller.paths)),
            phase: worker::PHASE_REPORTED.into(),
            folders: fs_,
            last_report: Some(WorkerReport { status: "done".into(), receipts: worker::WorkerReceipts { commits, ..Default::default() }, ..Default::default() }),
            ..WorkerRecord::default()
        };
        worker::write_worker(&self.caller.paths.task(&w.task_id), &w).unwrap();
        w
    }

    fn record(&self, task: &str) -> WorkerRecord {
        worker::read_worker(&self.caller.paths.task(task)).unwrap()
    }

    fn tips(&self, w: &WorkerRecord) -> Value {
        let mut m = serde_json::Map::new();
        for f in w.git_folders() {
            m.insert(f.name.clone(), json!(git(self.repo(&f.name), &["rev-parse", &format!("refs/heads/{}", f.branch.as_ref().unwrap())])));
        }
        Value::Object(m)
    }

    async fn land_with(&self, w: &WorkerRecord, extra: Value, o: Opts) -> Result<Value, String> {
        let mut args = json!({ "worker": w.slug, "summary": "Ship it.", "commits": self.tips(w) });
        if let Value::Object(e) = extra {
            for (k, v) in e {
                args[k] = v;
            }
        }
        land_tool_with(&self.ctx(), &self.caller, &args, o).await
    }

    async fn land(&self, w: &WorkerRecord) -> Result<Value, String> {
        self.land_with(w, json!({}), opts()).await
    }

    fn main_tip(&self, repo: &str) -> String {
        git(self.repo(repo), &["rev-parse", "refs/heads/main"])
    }

    fn log(&self) -> Vec<log::LandLogEntry> {
        log::read(&self.caller.paths)
    }

    /// The latest card filed, as the human's answer to it would carry it.
    fn last_card(&self) -> (HiveRequest, approval::Decision) {
        let req = self.seen.lock().unwrap().filed.last().cloned().expect("a card was filed");
        let id = req.keep.clone().map_or_else(|| format!("r_{}", self.seen.lock().unwrap().filed.len()), |(id, _)| id);
        let d = approval::Decision { id, kind: req.kind.to_string(), args: req.args.clone(), by: "desktop".into() };
        (req, d)
    }

    fn told(&self) -> Vec<String> {
        self.seen.lock().unwrap().delivered.iter().filter(|(t, _)| t == "coord").map(|(_, s)| s.clone()).collect()
    }

    /// Nothing a land holds is left: no lock file, no temporary worktree, no land folder.
    fn assert_no_leftovers(&self) {
        for (name, r) in &self.repos {
            let common = git::common_dir(r).unwrap();
            assert!(!lock::lock_path(&common).exists(), "{name}: no lock left");
            assert!(!git(r, &["worktree", "list"]).contains("/land/"), "{name}: no temporary worktree: {}", git(r, &["worktree", "list"]));
        }
        let land_root = self.caller.paths.worktrees().join("land");
        assert!(fs::read_dir(&land_root).map_or(true, |mut d| d.next().is_none()), "no land folder left in {}", land_root.display());
    }
}

// ── Landing ──

#[tokio::test]
async fn a_fast_forward_land_with_base_not_checked_out_moves_it_by_compare_and_swap() {
    let rig = Rig::new("cas", &["web"]);
    git(rig.repo("web"), &["checkout", "-q", "--detach"]);
    let before = rig.main_tip("web");
    let w = rig.reported("login", &["web"], "login.rs", "fn login() {}\n");
    let pinned = rig.tips(&w)["web"].as_str().unwrap().to_string();

    let out = rig.land(&w).await.unwrap();
    assert_eq!(out["state"], STATE_LANDED, "{out}");
    assert_eq!(rig.main_tip("web"), pinned, "Base was already in it: Base moves to exactly the pinned commit");
    assert_eq!(out["repos"][0]["merged_base"], false);

    let r = rig.record(&w.task_id);
    assert_eq!(r.phase, worker::PHASE_LANDED);
    assert_eq!(r.retired.as_ref().unwrap().landed.as_deref(), Some(pinned.as_str()));
    assert!(r.tab_id.is_none());
    let wt = w.folders[0].worktree.clone().unwrap();
    assert!(!Path::new(&wt).exists(), "its clean worktree was removed");
    assert_eq!(git(rig.repo("web"), &["rev-parse", "refs/heads/hive/ship--login"]), pinned, "its branch holds what landed");

    let log = rig.log();
    assert_eq!(log.len(), 1);
    let e = &log[0];
    assert_eq!((e.kind.as_str(), e.folder.as_str(), e.target.as_str(), e.by.as_str(), e.review.as_str()), ("land", "web", "main", "coordinator", "none"));
    assert_eq!((e.reviewed_sha.as_str(), e.tested_sha.as_str(), e.base_before.as_str(), e.base_after.as_str()), (pinned.as_str(), pinned.as_str(), before.as_str(), pinned.as_str()));
    assert_eq!(e.tree_tested, e.tree_landed);
    assert_eq!((e.pushed.as_str(), e.summary.as_str(), e.id.as_str()), ("off", "Ship it.", format!("{}/web", out["land"].as_str().unwrap()).as_str()));
    let verbs: Vec<String> = rig.seen.lock().unwrap().verbs.iter().map(|(v, _)| v.clone()).collect();
    assert!(verbs.contains(&crate::hive::rpc::VERB_RETIRE.to_string()), "{verbs:?}");
    let retire = rig.seen.lock().unwrap().verbs.iter().find(|(v, _)| v == crate::hive::rpc::VERB_RETIRE).unwrap().1.clone();
    assert_eq!(retire["status"], "done", "its waiters launch from the new Base");
    rig.assert_no_leftovers();
}

#[tokio::test]
async fn a_land_into_a_checked_out_base_leaves_an_unrelated_dirty_file_byte_identical() {
    let rig = Rig::new("dirty-ok", &["web"]);
    let w = rig.reported("api", &["web"], "api.rs", "fn api() {}\n");
    // The human moved on meanwhile: Base has a commit the worker doesn't.
    commit(rig.repo("web"), "docs.md", "docs\n");
    let human = rig.main_tip("web");
    fs::write(rig.repo("web").join("README.md"), "the human's edit\n").unwrap();
    fs::write(rig.repo("web").join("scratch.txt"), "untracked\n").unwrap();

    let out = rig.land(&w).await.unwrap();
    assert_eq!(out["state"], STATE_LANDED, "{out}");
    assert_eq!(out["repos"][0]["merged_base"], true, "Base had moved: step 1 merged it");
    let tip = rig.main_tip("web");
    assert_eq!(git(rig.repo("web"), &["rev-parse", "HEAD"]), tip, "the checkout fast-forwarded");
    assert_eq!(git(rig.repo("web"), &["rev-list", "--parents", "-n1", &tip]).split_whitespace().count(), 3, "a merge of the pin and Base");
    assert!(git(rig.repo("web"), &["merge-base", "--is-ancestor", &human, &tip]).is_empty());
    assert_eq!(fs::read_to_string(rig.repo("web").join("README.md")).unwrap(), "the human's edit\n", "byte-identical");
    assert_eq!(fs::read_to_string(rig.repo("web").join("scratch.txt")).unwrap(), "untracked\n");
    assert_eq!(fs::read_to_string(rig.repo("web").join("api.rs")).unwrap(), "fn api() {}\n");
    let msg = git(rig.repo("web"), &["log", "-1", "--format=%s%n%an <%ae>", &tip]);
    assert!(msg.starts_with(&format!("Land Title api onto main [maiTerm {}]", out["land"].as_str().unwrap())), "{msg}");
    rig.assert_no_leftovers();
}

#[tokio::test]
async fn an_uncommitted_or_untracked_file_in_the_way_holds_and_nothing_changes_then_it_resumes() {
    let rig = Rig::new("hold", &["web"]);
    let w = rig.reported("touch", &["web"], "shared.txt", "one\nTWO\nthree\n");
    let before = rig.main_tip("web");
    fs::write(rig.repo("web").join("shared.txt"), "the human's\n").unwrap();

    let out = rig.land(&w).await.unwrap();
    assert_eq!(out["state"], STATE_HELD, "{out}");
    let reason = out["reason"].as_str().unwrap();
    assert!(reason.contains("uncommitted changes to `shared.txt`"), "{reason}");
    assert_eq!(rig.main_tip("web"), before, "Base didn't move");
    assert_eq!(fs::read_to_string(rig.repo("web").join("shared.txt")).unwrap(), "the human's\n", "nothing stashed or overwritten");
    let r = rig.record(&w.task_id);
    assert_eq!(r.phase, worker::PHASE_LANDING, "held lands stay Landing");
    let land = r.land.unwrap();
    assert!(land.is_held() && land.repos[0].status == REPO_HELD && land.repos[0].tested.is_some());
    rig.assert_no_leftovers();

    // An untracked file the land would write holds the same way.
    fs::write(rig.repo("web").join("shared.txt"), "one\ntwo\nthree\n").unwrap();
    let w2 = rig.reported("newfile", &["web"], "fresh.txt", "from the worker\n");
    fs::write(rig.repo("web").join("fresh.txt"), "the human's own\n").unwrap();
    let out = rig.land(&w2).await.unwrap();
    assert_eq!(out["state"], STATE_HELD, "{out}");
    assert!(out["reason"].as_str().unwrap().contains("untracked files that would be overwritten: `fresh.txt`"), "{out}");
    assert_eq!(fs::read_to_string(rig.repo("web").join("fresh.txt")).unwrap(), "the human's own\n");

    // The reason gone, hiveLand again with the same commits lands it.
    let out = rig.land(&w).await.unwrap();
    assert_eq!(out["state"], STATE_LANDED, "{out}");
    assert_eq!(fs::read_to_string(rig.repo("web").join("shared.txt")).unwrap(), "one\nTWO\nthree\n");
    rig.assert_no_leftovers();
}

#[tokio::test]
async fn a_conflict_lists_its_files_and_leaves_base_the_branch_and_no_lock() {
    let rig = Rig::new("conflict", &["web"]);
    let w = rig.reported("clash", &["web"], "shared.txt", "one\nworker\nthree\n");
    commit(rig.repo("web"), "shared.txt", "one\nhuman\nthree\n");
    let (base, branch) = (rig.main_tip("web"), rig.tips(&w)["web"].as_str().unwrap().to_string());

    let e = rig.land(&w).await.unwrap_err();
    assert!(e.contains("conflicts in web: `shared.txt`") && e.contains("back in Reported"), "{e}");
    assert_eq!(rig.main_tip("web"), base);
    assert_eq!(rig.tips(&w)["web"], branch.as_str());
    let r = rig.record(&w.task_id);
    assert_eq!(r.phase, worker::PHASE_REPORTED);
    assert!(r.land.is_none());
    assert!(rig.log().is_empty());
    rig.assert_no_leftovers();
}

#[tokio::test]
async fn base_moving_under_step_2_retries_then_holds_after_three() {
    let rig = Rig::new("moved", &["web"]);
    git(rig.repo("web"), &["checkout", "-q", "--detach"]);
    let w = rig.reported("mover", &["web"], "m.rs", "m\n");
    let repo = rig.repo("web").to_path_buf();
    // Base moves once, by a commit made beside the land (in a scratch worktree on main).
    let side = rig.base.join("side");
    git(&repo, &["worktree", "add", "-q", side.to_str().unwrap(), "main"]);
    git(&repo, &["checkout", "-q", "--detach"]);
    let once = Arc::new(Mutex::new(0));
    let (s, n) = (side.clone(), once.clone());
    let mut o = opts();
    o.before_advance = Some(Arc::new(move |_f: &str| {
        let mut n = n.lock().unwrap();
        if *n == 0 {
            commit(&s, &format!("side-{}.txt", *n), "side\n");
        }
        *n += 1;
    }));
    let out = rig.land_with(&w, json!({}), o).await.unwrap();
    assert_eq!(out["state"], STATE_LANDED, "{out}");
    assert_eq!(*once.lock().unwrap(), 2, "one retry");
    assert!(fs::read_to_string(side.join("side-0.txt")).is_ok());
    let tip = rig.main_tip("web");
    assert!(git(&repo, &["show", &format!("{tip}:side-0.txt")]) == "side", "the land merged the new Base");

    // Base moving every time: three tries, then a hold.
    let w2 = rig.reported("mover2", &["web"], "m2.rs", "m2\n");
    let (s, n) = (side.clone(), Arc::new(Mutex::new(0)));
    let count = n.clone();
    let mut o = opts();
    o.before_advance = Some(Arc::new(move |_f: &str| {
        let mut n = n.lock().unwrap();
        // The side worktree is on main: it follows each land's fast-forward of `main` only by its
        // own commits, so pull it up first.
        git(&s, &["reset", "-q", "--hard", "main"]);
        commit(&s, &format!("again-{}.txt", *n), "x\n");
        *n += 1;
    }));
    let out = rig.land_with(&w2, json!({}), o).await.unwrap();
    assert_eq!(out["state"], STATE_HELD, "{out}");
    assert!(out["reason"].as_str().unwrap().contains("main keeps moving (3 tries)"), "{out}");
    assert_eq!(*count.lock().unwrap(), 3);
    assert_eq!(rig.record(&w2.task_id).land.unwrap().repos[0].attempts, 3);
    rig.assert_no_leftovers();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn two_lands_on_one_repo_serialize_and_the_second_merges_the_new_base() {
    let rig = Rig::new("serial", &["web"]);
    let a = rig.reported("first", &["web"], "a.rs", "a\n");
    let b = rig.reported("second", &["web"], "b.rs", "b\n");
    let mut slow = opts();
    slow.before_advance = Some(Arc::new(|_f: &str| std::thread::sleep(Duration::from_millis(400))));
    let (ra, rb) = tokio::join!(rig.land_with(&a, json!({}), slow), async {
        tokio::time::sleep(Duration::from_millis(100)).await;
        rig.land(&b).await
    });
    let (ra, rb) = (ra.unwrap(), rb.unwrap());
    assert_eq!((ra["state"].as_str(), rb["state"].as_str()), (Some(STATE_LANDED), Some(STATE_LANDED)), "{ra} {rb}");
    let tip = rig.main_tip("web");
    for f in ["a.rs", "b.rs"] {
        assert!(fs::read_to_string(rig.repo("web").join(f)).is_ok(), "{f} landed");
    }
    assert_eq!(rb["repos"][0]["merged_base"], true, "the second merged the Base the first left");
    assert_eq!(rb["repos"][0]["landed"], tip.as_str());
    let log = rig.log();
    assert_eq!(log.len(), 2);
    assert_eq!(log[1].base_before, log[0].base_after, "the second started from the first's Base");
    rig.assert_no_leftovers();
}

// Pid 1 is a live process only where there is one.
#[cfg(unix)]
#[tokio::test]
async fn a_busy_lock_answers_who_holds_it() {
    let rig = Rig::new("busy", &["web"]);
    let w = rig.reported("waits", &["web"], "w.rs", "w\n");
    let common = git::common_dir(rig.repo("web")).unwrap();
    let other = lock::LockInfo { pid: 1, build: "prod".into(), hive_id: "other".into(), land_id: "L-0ther000".into(), at: "now".into() };
    fs::write(lock::lock_path(&common), serde_json::to_vec(&other).unwrap()).unwrap();
    let mut o = opts();
    o.lock_wait = Duration::from_millis(300);
    let out = rig.land_with(&w, json!({}), o).await.unwrap();
    assert_eq!(out["state"], STATE_LOCKING, "{out}");
    assert!(out["waiting_for"].as_str().unwrap().contains("L-0ther000 of maiTerm prod (pid 1)"), "{out}");
    assert_eq!(rig.record(&w.task_id).land.unwrap().waiting_for.as_deref(), out["waiting_for"].as_str());
    assert!(lock::lock_path(&common).exists(), "a live holder's lock stays");
    fs::remove_file(lock::lock_path(&common)).unwrap();
}

// ── Refusals ──

#[tokio::test]
async fn refusals_before_anything_is_written() {
    let rig = Rig::new("refuse", &["web"]);
    let w = rig.reported("r", &["web"], "r.rs", "r\n");
    let say = |e: Result<Value, String>, what: &str| {
        let e = e.unwrap_err();
        assert!(e.contains(what), "{what}: {e}");
    };
    let sha = rig.tips(&w)["web"].as_str().unwrap().to_string();
    say(land_tool_with(&rig.ctx(), &rig.caller, &json!({ "worker": "r", "summary": "s", "commits": { "web": &sha[..10] } }), opts()).await, "full sha");
    say(land_tool_with(&rig.ctx(), &rig.caller, &json!({ "worker": "r", "summary": "s", "commits": {} }), opts()).await, "exactly worker #1 r's repos (web)");
    say(land_tool_with(&rig.ctx(), &rig.caller, &json!({ "worker": "r", "summary": "s", "commits": { "web": sha, "api": sha } }), opts()).await, "exactly");
    say(land_tool_with(&rig.ctx(), &rig.caller, &json!({ "worker": "r", "commits": { "web": sha } }), opts()).await, "summary is required");
    say(rig.land_with(&w, json!({ "order": ["api"] }), opts()).await, "order names \"api\"");

    // Moved since the review.
    let reviewed = rig.tips(&w);
    commit(Path::new(w.folders[0].worktree.as_ref().unwrap()), "more.rs", "more\n");
    let e = land_tool_with(&rig.ctx(), &rig.caller, &json!({ "worker": "r", "summary": "s", "commits": reviewed }), opts()).await;
    say(e, "moved since you reviewed it");
    // Dirty: an untracked file counts.
    fs::write(Path::new(w.folders[0].worktree.as_ref().unwrap()).join("stray.txt"), "x").unwrap();
    say(rig.land(&w).await, "uncommitted or untracked files (web: 1 file)");
    fs::remove_file(Path::new(w.folders[0].worktree.as_ref().unwrap()).join("stray.txt")).unwrap();
    // Mid-turn.
    let mut tabbed = rig.record(&w.task_id);
    tabbed.tab_id = Some("wtab".into());
    worker::write_worker(&rig.caller.paths.task(&w.task_id), &tabbed).unwrap();
    {
        let mut d = rig.data.write();
        let mut t = Tab::new("w".into());
        t.id = "wtab".into();
        t.hive_role = Some(crate::hive::ROLE_WORKER.into());
        d.windows[0].workspaces[0].panes[0].tabs.push(t);
    }
    rig.seen.lock().unwrap().busy = true;
    say(rig.land(&w).await, "mid-turn");
    rig.seen.lock().unwrap().busy = false;
    assert_eq!(rig.record(&w.task_id).phase, worker::PHASE_REPORTED, "nothing was written");
    assert!(rig.log().is_empty());

    // Not reported done.
    let mut running = rig.record(&w.task_id);
    running.phase = worker::PHASE_RUNNING.into();
    worker::write_worker(&rig.caller.paths.task(&w.task_id), &running).unwrap();
    say(rig.land(&w).await, "only a worker that has reported done lands");
    running.phase = worker::PHASE_REPORTED.into();
    running.last_report.as_mut().unwrap().status = "blocked".into();
    worker::write_worker(&rig.caller.paths.task(&w.task_id), &running).unwrap();
    say(rig.land(&w).await, "reported blocked, not done");
}

#[tokio::test]
async fn a_detached_base_has_nowhere_to_land() {
    let rig = Rig::with("detached", &["web"], |h| h.folders[0].base = None);
    // A detached Base still gets a worktree (off HEAD); the land is what refuses it.
    let w = rig.reported("x", &["web"], "x.rs", "x\n");
    let e = rig.land(&w).await.unwrap_err();
    assert!(e.contains("detached HEAD"), "{e}");
    assert!(rig.seen.lock().unwrap().filed.is_empty(), "nothing put to the human");
}

#[tokio::test]
async fn a_review_must_pass_at_exactly_these_commits_or_be_accepted() {
    let rig = Rig::new("review", &["web"]);
    let w = rig.reported("rev", &["web"], "rev.rs", "rev\n");
    let tips: std::collections::BTreeMap<String, String> = serde_json::from_value(rig.tips(&w)).unwrap();
    let verdict = |v: &str, shas: std::collections::BTreeMap<String, String>| Verdict { reviewer_task: "t-r".into(), verdict: v.into(), shas, ..Verdict::default() };
    let set_review = |r: Review| {
        let mut rec = rig.record(&w.task_id);
        rec.review = Some(r);
        worker::write_worker(&rig.caller.paths.task(&w.task_id), &rec).unwrap();
    };
    set_review(Review { attempts: 1, verdicts: vec![verdict("fail", tips.clone())], ..Review::default() });
    let e = rig.land(&w).await.unwrap_err();
    assert!(e.contains("last review failed") && e.contains("reviews: rev"), "{e}");
    let other = std::collections::BTreeMap::from([("web".to_string(), "0".repeat(40))]);
    set_review(Review { verdicts: vec![verdict("pass", other)], ..Review::default() });
    assert!(rig.land(&w).await.unwrap_err().contains("passed at other commits"));
    let accepted = Review {
        attempts: 3,
        escalated: true,
        verdicts: vec![verdict("fail", tips.clone())],
        resolution: Some(worker::Resolution { kind: "accept".into(), ..Default::default() }),
        ..Review::default()
    };
    set_review(accepted);
    assert!(rig.land(&w).await.unwrap_err().contains("accept_review: true"));
    let out = rig.land_with(&w, json!({ "accept_review": true }), opts()).await.unwrap();
    assert_eq!(out["review"], "accepted");
    assert_eq!(rig.log()[0].review, "accepted");

    let w2 = rig.reported("rev2", &["web"], "rev2.rs", "rev2\n");
    let tips2: std::collections::BTreeMap<String, String> = serde_json::from_value(rig.tips(&w2)).unwrap();
    let mut rec = rig.record(&w2.task_id);
    rec.review = Some(Review { verdicts: vec![verdict("pass", tips2)], ..Review::default() });
    worker::write_worker(&rig.caller.paths.task(&w2.task_id), &rec).unwrap();
    // A reviewer still at work holds the land.
    let reviewer = WorkerRecord { task_id: "t-live".into(), slug: "live".into(), badge: 9, reviews: Some(w2.task_id.clone()), phase: worker::PHASE_RUNNING.into(), ..WorkerRecord::default() };
    worker::write_worker(&rig.caller.paths.task("t-live"), &reviewer).unwrap();
    assert!(rig.land(&w2).await.unwrap_err().contains("still reviewing"));
    fs::remove_dir_all(rig.caller.paths.task("t-live")).unwrap();
    assert_eq!(rig.land(&w2).await.unwrap()["review"], "pass");
}

#[tokio::test]
async fn a_worker_with_no_repos_is_retired_as_landed() {
    let rig = Rig::new("norepo", &["web"]);
    let w = rig.reported("media", &[], "x", "x");
    let out = land_tool_with(&rig.ctx(), &rig.caller, &json!({ "worker": "media", "summary": "Art", "commits": {} }), opts()).await.unwrap();
    assert_eq!(out["state"], STATE_LANDED);
    assert_eq!(rig.record(&w.task_id).phase, worker::PHASE_LANDED);
    assert!(rig.log().is_empty(), "nothing merged, nothing logged");
}

// ── Several repos ──

#[tokio::test]
async fn several_repos_land_all_or_nothing_through_step_1_then_in_order() {
    let rig = Rig::new("multi", &["web", "api"]);
    // api conflicts: nothing lands, web included.
    let w = rig.reported("both", &["web", "api"], "shared.txt", "one\nworker\nthree\n");
    commit(rig.repo("api"), "shared.txt", "one\nhuman\nthree\n");
    let (web0, api0) = (rig.main_tip("web"), rig.main_tip("api"));
    let e = rig.land(&w).await.unwrap_err();
    assert!(e.contains("conflicts in api"), "{e}");
    assert_eq!((rig.main_tip("web"), rig.main_tip("api")), (web0.clone(), api0.clone()), "nothing landed");
    rig.assert_no_leftovers();

    // api held at step 2: web lands, api waits, and the next call lands api only.
    let w2 = rig.reported("pair", &["web", "api"], "pair.txt", "pair\n");
    fs::write(rig.repo("api").join("pair.txt"), "the human's\n").unwrap();
    let out = rig.land(&w2).await.unwrap();
    assert_eq!(out["state"], STATE_HELD, "{out}");
    assert_eq!(out["landed"], json!(["web"]));
    assert!(out["result"].as_str().unwrap().contains("Landed web; holding api: "), "{out}");
    let web1 = rig.main_tip("web");
    assert_ne!(web1, web0);
    assert_eq!(rig.main_tip("api"), api0);
    assert_eq!(rig.log().len(), 1, "web's line, written when it landed");
    rig.assert_no_leftovers();

    fs::remove_file(rig.repo("api").join("pair.txt")).unwrap();
    let out = rig.land(&w2).await.unwrap();
    assert_eq!(out["state"], STATE_LANDED, "{out}");
    assert_eq!(rig.main_tip("web"), web1, "web isn't landed twice");
    assert_ne!(rig.main_tip("api"), api0);
    let log = rig.log();
    assert_eq!(log.iter().map(|e| e.folder.as_str()).collect::<Vec<_>>(), vec!["web", "api"]);
    assert_eq!(log[0].land, log[1].land, "one land");
    rig.assert_no_leftovers();
}

#[tokio::test]
async fn the_order_given_lands_first() {
    let rig = Rig::new("order", &["web", "api"]);
    let w = rig.reported("ordered", &["web", "api"], "o.txt", "o\n");
    let out = rig.land_with(&w, json!({ "order": ["api"] }), opts()).await.unwrap();
    assert_eq!(out["state"], STATE_LANDED);
    assert_eq!(rig.log().iter().map(|e| e.folder.as_str()).collect::<Vec<_>>(), vec!["api", "web"]);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn two_multi_repo_lands_in_opposite_orders_dont_deadlock() {
    let rig = Rig::new("deadlock", &["web", "api"]);
    let a = rig.reported("ab", &["web", "api"], "ab.txt", "ab\n");
    let b = rig.reported("ba", &["web", "api"], "ba.txt", "ba\n");
    let mut slow = opts();
    slow.before_advance = Some(Arc::new(|_f: &str| std::thread::sleep(Duration::from_millis(200))));
    let run = tokio::time::timeout(Duration::from_secs(60), async {
        tokio::join!(rig.land_with(&a, json!({ "order": ["web", "api"] }), slow.clone()), rig.land_with(&b, json!({ "order": ["api", "web"] }), slow))
    });
    let (ra, rb) = run.await.expect("no deadlock");
    assert_eq!((ra.unwrap()["state"].clone(), rb.unwrap()["state"].clone()), (json!(STATE_LANDED), json!(STATE_LANDED)));
    assert_eq!(rig.log().len(), 4);
    rig.assert_no_leftovers();
}

// ── The land check ──

#[cfg(unix)]
#[tokio::test]
async fn a_failing_check_refuses_with_its_output_and_a_passing_one_lands() {
    // The setup needs the network the check's profile closes, so it never runs there: what the
    // worker's setup left in its worktree (ignored) is seeded in instead.
    let rig = Rig::with("check", &["web"], |h| {
        h.folders[0].setup = Some("echo 'FAIL: the setup ran'; exit 7".into());
        h.folders[0].land_check = Some("test -f node_modules/dep/index.js && grep -q ok verdict.txt || { echo 'FAIL: verdict is not ok'; exit 3; }".into());
    });
    commit(rig.repo("web"), ".gitignore", "node_modules/\n");
    let installed = |w: &WorkerRecord| {
        let wt = PathBuf::from(w.folders[0].worktree.clone().unwrap());
        fs::create_dir_all(wt.join("node_modules/dep")).unwrap();
        fs::write(wt.join("node_modules/dep/index.js"), "module.exports = 1;\n").unwrap();
        wt
    };
    let checker = Arc::new(ShChecker::default());
    let mut o = opts();
    o.checker = checker.clone();
    let bad = rig.reported("bad", &["web"], "verdict.txt", "nope\n");
    installed(&bad);
    let before = rig.main_tip("web");
    let e = rig.land_with(&bad, json!({}), o.clone()).await.unwrap_err();
    assert!(e.contains("land check failed in web") && e.contains("FAIL: verdict is not ok"), "{e}");
    assert_eq!(rig.main_tip("web"), before);
    assert_eq!(rig.record(&bad.task_id).phase, worker::PHASE_REPORTED);
    rig.assert_no_leftovers();

    let good = rig.reported("good", &["web"], "verdict.txt", "ok\n");
    // Without the worker's installed state the check has nothing to run against.
    let e = rig.land_with(&good, json!({}), o.clone()).await.unwrap_err();
    assert!(e.contains("FAIL: verdict is not ok") && !e.contains("the setup ran"), "{e}");
    let wt = installed(&good);
    let out = rig.land_with(&good, json!({}), o).await.unwrap();
    assert_eq!(out["state"], STATE_LANDED, "{out}");
    assert_eq!(checker.0.lock().unwrap().len(), 3, "one check per land");
    assert!(!rig.repo("web").join("node_modules").exists(), "seeded into the temporary worktree, not the checkout");
    assert!(!wt.exists() || wt.join("node_modules/dep/index.js").exists(), "copied, not moved");
    rig.assert_no_leftovers();
}

#[cfg(unix)]
#[test]
fn seeding_never_writes_through_a_link_the_merged_tree_tracks() {
    let rig = Rig::new("seedlink", &["web"]);
    let w = rig.reported("seed", &["web"], "s.rs", "s\n");
    let wt = PathBuf::from(w.folders[0].worktree.clone().unwrap());
    fs::write(wt.join(".gitignore"), "cache/\nout/\n").unwrap();
    fs::create_dir_all(wt.join("cache")).unwrap();
    fs::write(wt.join("cache/a"), "a").unwrap();
    fs::create_dir_all(wt.join("out")).unwrap();
    fs::write(wt.join("out/b"), "b").unwrap();
    // The temporary worktree: `out` is a link (as a tracked one would check out), `cache` free.
    let tmp = rig.base.join("tmpwt");
    let elsewhere = rig.base.join("elsewhere");
    fs::create_dir_all(&tmp).unwrap();
    fs::create_dir_all(&elsewhere).unwrap();
    std::os::unix::fs::symlink(&elsewhere, tmp.join("out")).unwrap();
    let seeded = git::seed_ignored(&w.folders[0], &tmp);
    assert_eq!(seeded, vec!["cache".to_string()], "{seeded:?}");
    assert_eq!(fs::read_to_string(tmp.join("cache/a")).unwrap(), "a");
    assert!(fs::read_dir(&elsewhere).unwrap().next().is_none(), "nothing went through the link");
}

#[tokio::test]
async fn a_queued_check_pauses_the_land_and_its_end_carries_it_on() {
    struct Queued(Mutex<Vec<String>>);
    impl Checker for Queued {
        fn start(&self, req: &CheckRequest) -> Result<CheckStart, String> {
            self.0.lock().unwrap().push(req.tmp.path.to_string_lossy().to_string());
            Ok(CheckStart::Queued(format!("j-{:08x}", self.0.lock().unwrap().len())))
        }
    }
    let rig = Rig::with("queued", &["web"], |h| h.folders[0].land_check = Some("true".into()));
    let q = Arc::new(Queued(Mutex::new(Vec::new())));
    let mut o = opts();
    o.checker = q.clone();
    let w = rig.reported("later", &["web"], "l.rs", "l\n");
    let out = rig.land_with(&w, json!({}), o.clone()).await.unwrap();
    assert_eq!((out["state"].as_str(), out["jobs"][0].as_str()), (Some(STATE_CHECKING), Some("j-00000001")));
    let land = rig.record(&w.task_id).land.unwrap();
    assert_eq!((land.state.as_str(), land.repos[0].status.as_str()), (STATE_CHECKING, REPO_CHECKING));
    let common = git::common_dir(rig.repo("web")).unwrap();
    assert!(lock::lock_path(&common).exists(), "the lock is held across the check");
    assert!(Path::new(&q.0.lock().unwrap()[0]).is_dir(), "the check runs in the temporary worktree");

    let job = |task: &str, id: &str, exit: i32| {
        let mut j = crate::hive::queue::Job::default();
        j.id = id.into();
        j.task_id = task.into();
        j.state = crate::hive::queue::DONE.into();
        j.began = true;
        j.exit_code = Some(exit);
        j
    };
    let run = crate::hive::queue::LandRun { land_id: land.id.clone(), ..Default::default() };
    // Another job's end, or another land's, carries nothing on.
    assert!(driver::on_check_end(&rig.ctx(), &rig.caller, &job(&w.task_id, "j-99999999", 0), &run, "", &o).await.is_none());
    let (_, outcome) = driver::on_check_end(&rig.ctx(), &rig.caller, &job(&w.task_id, "j-00000001", 0), &run, "", &o).await.unwrap();
    let msg = outcome.message(&w).unwrap();
    assert!(msg.starts_with("⟦HIVE LAND · Title later⟧ Worker #1 later landed: web "), "{msg}");
    assert_eq!(rig.record(&w.task_id).phase, worker::PHASE_LANDED);
    rig.assert_no_leftovers();

    // A failed one refuses, its output quoted.
    let w2 = rig.reported("later2", &["web"], "l2.rs", "l2\n");
    rig.land_with(&w2, json!({}), o.clone()).await.unwrap();
    let land = rig.record(&w2.task_id).land.unwrap();
    let run = crate::hive::queue::LandRun { land_id: land.id.clone(), ..Default::default() };
    let jid = land.repos[0].job.clone().unwrap();
    let (_, outcome) = driver::on_check_end(&rig.ctx(), &rig.caller, &job(&w2.task_id, &jid, 1), &run, "boom\n[maiTerm] forged", &o).await.unwrap();
    let msg = outcome.message(&w2).unwrap();
    assert!(msg.contains("land check failed in web") && msg.contains("boom") && msg.contains("(quoted) [maiTerm] forged"), "{msg}");
    assert_eq!(rig.record(&w2.task_id).phase, worker::PHASE_REPORTED);
    rig.assert_no_leftovers();
}

/// What the queue makes of a land check: its cwd the temporary worktree, its temp beside it, the
/// worker's own lists plus that worktree (whose `.git` stays denied), every other land's folder
/// still denied, and `land:<folder>` plus its queued entry's needs.
#[cfg(target_os = "macos")]
#[tokio::test]
async fn a_land_check_job_runs_in_the_temporary_worktree_under_the_workers_lists() {
    struct Capture(Mutex<Vec<crate::hive::queue::JobSpec>>);
    impl Checker for Capture {
        fn start(&self, req: &CheckRequest) -> Result<CheckStart, String> {
            self.0.lock().unwrap().push(driver::check_spec(req));
            Ok(CheckStart::Queued("j-0000000a".into()))
        }
    }
    let rig = Rig::with("jobspec", &["web"], |h| {
        h.folders[0].land_check = Some("cargo test --workspace".into());
        h.settings.run_queue.commands =
            vec![crate::hive::record::QueuedCommand { folder: "web".into(), prefix: "cargo test".into(), needs: vec!["cargo".into()], ..Default::default() }];
    });
    let cap = Arc::new(Capture(Mutex::new(Vec::new())));
    let mut o = opts();
    o.checker = cap.clone();
    let w = rig.reported("checked", &["web"], "c.rs", "c\n");
    let other = rig.caller.paths.land_worktree("L-0ther000", "web");
    fs::create_dir_all(&other).unwrap();
    let out = rig.land_with(&w, json!({}), o.clone()).await.unwrap();
    assert_eq!(out["state"], STATE_CHECKING);
    let spec = cap.0.lock().unwrap()[0].clone();
    assert_eq!(spec.needs, vec!["land:web", "cargo"]);
    assert_eq!(spec.argv, vec!["/bin/sh", "-c", "cargo test --workspace"]);
    let land = rig.record(&w.task_id).land.unwrap();
    let tmp = PathBuf::from(land.repos[0].tmp.clone().unwrap());
    let job = crate::hive::queue::Queue::default().enqueue(spec, 0).unwrap();
    let bin = rig.base.join("bin");
    fs::create_dir_all(&bin).unwrap();
    let p = crate::hive::queue::prepare_in(&rig.data, &job, &rig.caller.paths, &bin, &[]);
    crate::stack::ports::release(&job.id);
    let p = p.unwrap();
    assert!(p.line.starts_with(&format!("cd {} ", crate::hive::launch::sh_quote(&tmp.to_string_lossy()))), "{}", p.line);
    let land_tmp = rig.caller.paths.land_tmp(&land.id);
    assert!(p.line.contains(&format!("TMPDIR={}", crate::hive::launch::sh_quote(&land_tmp.to_string_lossy()))), "{}", p.line);
    assert_eq!(p.tree.0, land.repos[0].tested.clone().unwrap(), "the tested commit, read off the temporary worktree");
    let profile = fs::read_to_string(rig.caller.paths.run(&job.id).join(crate::hive::job_sandbox::PROFILE_FILE)).unwrap();
    let (allow, deny) = profile.split_once("(deny file-write*\n").unwrap();
    let canon = |p: &Path| crate::share::canon(&fs::canonicalize(p).unwrap()).to_string_lossy().to_string();
    assert!(allow.contains(&format!("(subpath \"{}\")", canon(&tmp))), "{profile}");
    assert!(allow.contains(&canon(&w.folders[0].worktree.clone().map(PathBuf::from).unwrap())), "the worker's own lists: {profile}");
    assert!(deny.contains(&format!("(literal \"{}/.git\")", canon(&tmp))), "{profile}");
    assert!(deny.contains(&format!("(subpath \"{}\")", canon(&rig.caller.paths.land_dir("L-0ther000")))), "another land stays denied: {profile}");
    assert!(!deny.contains(&format!("(subpath \"{}\")", canon(&rig.caller.paths.worktrees().join("land")))), "this land's folder isn't: {profile}");

    // Settled, so nothing is left holding the lock.
    let mut j = crate::hive::queue::Job::default();
    j.id = "j-0000000a".into();
    j.task_id = w.task_id.clone();
    j.state = crate::hive::queue::DONE.into();
    j.began = true;
    j.exit_code = Some(1);
    let run = crate::hive::queue::LandRun { land_id: land.id.clone(), ..Default::default() };
    driver::on_check_end(&rig.ctx(), &rig.caller, &j, &run, "", &o).await.unwrap();
    fs::remove_dir_all(rig.caller.paths.land_dir("L-0ther000")).unwrap();
    rig.assert_no_leftovers();
}

// ── A restart ──

#[tokio::test]
async fn reconcile_settles_a_land_caught_in_step_2() {
    let rig = Rig::new("reconcile", &["web", "api"]);
    git(rig.repo("web"), &["checkout", "-q", "--detach"]);
    git(rig.repo("api"), &["checkout", "-q", "--detach"]);
    let w = rig.reported("crash", &["web", "api"], "c.rs", "c\n");
    let tips: std::collections::BTreeMap<String, String> = serde_json::from_value(rig.tips(&w)).unwrap();
    let (web0, api0) = (rig.main_tip("web"), rig.main_tip("api"));
    // web's step 2 happened before the crash; api's didn't.
    git(rig.repo("web"), &["update-ref", "refs/heads/main", &tips["web"]]);
    let repo = |f: &str, base: &str| LandRepo {
        folder: f.into(),
        branch: "hive/ship--crash".into(),
        target: "main".into(),
        pinned: tips[f].clone(),
        base_before: Some(base.into()),
        tested: Some(tips[f].clone()),
        status: REPO_ADVANCING.into(),
        ..LandRepo::default()
    };
    let mut rec = rig.record(&w.task_id);
    rec.phase = worker::PHASE_LANDING.into();
    rec.land = Some(LandState {
        id: "L-crash000".into(),
        state: STATE_ADVANCING.into(),
        summary: "s".into(),
        repos: vec![repo("web", &web0), repo("api", &api0)],
        ..LandState::default()
    });
    worker::write_worker(&rig.caller.paths.task(&w.task_id), &rec).unwrap();
    let common = git::common_dir(rig.repo("web")).unwrap();
    let stale = lock::LockInfo { pid: 2_147_483_000, build: "dev".into(), hive_id: rig.caller.hive.id.clone(), land_id: "L-crash000".into(), at: "then".into() };
    fs::write(lock::lock_path(&common), serde_json::to_vec(&stale).unwrap()).unwrap();

    assert_eq!(reconcile_in(&rig.caller.hive, &rig.caller.paths), 1);
    let land = rig.record(&w.task_id).land.unwrap();
    assert_eq!(land.state, STATE_HELD);
    assert_eq!((land.repos[0].status.as_str(), land.repos[1].status.as_str()), (REPO_LANDED, REPO_TESTED));
    assert!(land.held_reason.unwrap().contains("landed web; interrupted by a maiTerm restart"));
    let log = rig.log();
    assert_eq!((log.len(), log[0].folder.as_str(), log[0].base_after.as_str()), (1, "web", tips["web"].as_str()));
    assert!(!lock::lock_path(&common).exists(), "its own lock is released");
    assert_eq!(reconcile_in(&rig.caller.hive, &rig.caller.paths), 0, "a held land is settled");

    // The coordinator resumes it: api lands from its stored test, web isn't landed twice.
    let out = rig.land_with(&w, json!({}), opts()).await.unwrap();
    assert_eq!(out["state"], STATE_LANDED, "{out}");
    assert_eq!(rig.main_tip("api"), tips["api"]);
    assert_eq!(rig.log().len(), 2);
    rig.assert_no_leftovers();
}

// ── Pushing ──

#[tokio::test]
async fn push_after_land_pushes_base_and_a_refused_push_holds_only_the_push() {
    let rig = Rig::with("push", &["web"], |h| h.push_after_land = true);
    let bare = rig.base.join("remote.git");
    git(&rig.base, &["init", "-q", "--bare", "-b", "main", bare.to_str().unwrap()]);
    git(rig.repo("web"), &["remote", "add", "origin", bare.to_str().unwrap()]);
    git(rig.repo("web"), &["push", "-q", "-u", "origin", "main"]);

    let w = rig.reported("pushed", &["web"], "p.rs", "p\n");
    let out = rig.land(&w).await.unwrap();
    assert_eq!(out["repos"][0]["pushed"], "ok", "{out}");
    assert_eq!(git(&bare, &["rev-parse", "main"]), rig.main_tip("web"));
    assert_eq!(rig.log()[0].pushed, "ok");

    // The remote moved on: the push is refused (never forced), and the land stands.
    let other = rig.base.join("other");
    git(&rig.base, &["clone", "-q", bare.to_str().unwrap(), other.to_str().unwrap()]);
    commit(&other, "theirs.txt", "theirs\n");
    git(&other, &["push", "-q", "origin", "main"]);
    let remote_tip = git(&bare, &["rev-parse", "main"]);
    let w2 = rig.reported("pushed2", &["web"], "p2.rs", "p2\n");
    let out = rig.land(&w2).await.unwrap();
    assert_eq!(out["state"], STATE_LANDED);
    let pushed = out["repos"][0]["pushed"].as_str().unwrap();
    assert!(pushed.starts_with("held: "), "{pushed}");
    assert!(out["result"].as_str().unwrap().contains("push held"), "{out}");
    assert_eq!(git(&bare, &["rev-parse", "main"]), remote_tip, "nothing forced");
    assert_eq!(rig.main_tip("web"), rig.tips(&w2)["web"].as_str().unwrap(), "landed locally all the same");
    // The human sees it too: a Landed card carries what its pushes came to.
    assert_eq!(rig.record(&w2.task_id).land.unwrap().state, STATE_LANDED, "written once the worker retired");
    let board = crate::hive::tools::board_view(&rig.caller.hive, &rig.caller.paths, rig.fuses, crate::hive::fuses::now_ms(), false);
    let card = board.workers.iter().find(|c| c.task_id == w2.task_id).unwrap();
    assert_eq!(card.phase, worker::PHASE_LANDED);
    let pushed = card.land.as_ref().expect("a Landed card carries its land").repos[0].pushed.clone().unwrap();
    assert!(pushed.starts_with("held: "), "{pushed}");
}

// ── While it lands ──

#[tokio::test]
async fn a_landing_worker_is_neither_stopped_nor_reports_and_a_held_land_doesnt_block_a_delete() {
    let rig = Rig::new("landing", &["web"]);
    let w = rig.reported("busy", &["web"], "b.rs", "b\n");
    let mut rec = rig.record(&w.task_id);
    rec.phase = worker::PHASE_LANDING.into();
    rec.tab_id = Some("wtab".into());
    rec.land = Some(LandState { id: "L-11111111".into(), state: STATE_CHECKING.into(), ..LandState::default() });
    worker::write_worker(&rig.caller.paths.task(&w.task_id), &rec).unwrap();
    let e = crate::hive::control::stop_worker(&rig.ctx(), &rig.caller, &json!({ "worker": "busy", "reason": "r" })).await.unwrap_err();
    assert!(e.contains("is landing"), "{e}");
    let mut as_worker = rig.caller.clone();
    as_worker.tab_id = "wtab".into();
    let e = crate::hive::report::report(&rig.ctx(), &as_worker, &json!({ "status": "done", "summary": "again", "receipts": { "commits": [] } })).await.unwrap_err();
    assert!(e.contains("landing the work you reported"), "{e}");
    let e = crate::hive::retire::retire_tool(&rig.ctx(), &rig.caller, &json!({ "worker": "busy", "reason": "r" })).await.unwrap_err();
    assert!(e.contains("is landing"), "{e}");
    assert!(in_flight(&rec), "a running land refuses a delete");
    rec.land.as_mut().unwrap().state = STATE_HELD.into();
    assert!(!in_flight(&rec), "a held land holds no lock");
    rec.land.as_mut().unwrap().state = STATE_LANDED.into();
    assert!(!in_flight(&rec), "nor does one that landed everything");
    let e = rig.land(&w).await.unwrap_err();
    assert!(e.contains("land L-11111111 is checking now"), "a land in progress isn't started twice: {e}");
}

// ── Ways out of a hold ──

#[tokio::test]
async fn a_partly_landed_land_that_conflicts_on_resume_lands_the_workers_new_commit() {
    let rig = Rig::new("partial", &["web", "api"]);
    let w = rig.reported("split", &["web", "api"], "shared.txt", "one\nworker\nthree\n");
    let pins = rig.tips(&w);
    // The human's edit in api's checkout is in the way: web lands, api holds.
    fs::write(rig.repo("api").join("shared.txt"), "one\nhuman\nthree\n").unwrap();
    let out = rig.land(&w).await.unwrap();
    assert_eq!((out["state"].as_str(), out["landed"].clone()), (Some(STATE_HELD), json!(["web"])), "{out}");
    let web1 = rig.main_tip("web");

    // The human commits it: resumed, api's merge conflicts now, and once something landed that holds.
    git(rig.repo("api"), &["commit", "-q", "--no-verify", "-am", "human"]);
    let out = rig.land(&w).await.unwrap();
    assert_eq!(out["state"], STATE_HELD, "{out}");
    assert!(out["reason"].as_str().unwrap().contains("conflicts in api"), "{out}");
    let r = rig.record(&w.task_id);
    assert!(held(&r) && !in_flight(&r));

    // As it was told: the worker merges main, resolves, and reports again.
    let api_wt = PathBuf::from(w.folders.iter().find(|f| f.name == "api").unwrap().worktree.clone().unwrap());
    git(&api_wt, &["merge", "-q", "--no-edit", "-X", "ours", "main"]);
    let new_api = git(&api_wt, &["rev-parse", "HEAD"]);
    let mut rec = rig.record(&w.task_id);
    rec.tab_id = Some("wtab".into());
    worker::write_worker(&rig.caller.paths.task(&w.task_id), &rec).unwrap();
    let mut as_worker = rig.caller.clone();
    as_worker.tab_id = "wtab".into();
    crate::hive::report::report(&rig.ctx(), &as_worker, &json!({ "status": "done", "summary": "merged main", "receipts": { "commits": [new_api] } }))
        .await
        .unwrap();
    let r = rig.record(&w.task_id);
    assert_eq!(r.phase, worker::PHASE_REPORTED);
    assert!(r.land.as_ref().is_some_and(|l| l.is_held() && l.landed().len() == 1), "what landed stays on record");

    // What landed keeps its commit; api takes the new one.
    let mut wrong = rig.tips(&w);
    wrong["web"] = json!(new_api);
    let e = rig.land_with(&w, json!({ "commits": wrong }), opts()).await.unwrap_err();
    assert!(e.contains("already landed web at"), "{e}");
    let out = rig.land(&w).await.unwrap();
    assert_eq!(out["state"], STATE_LANDED, "{out}");
    assert_eq!(rig.main_tip("web"), web1, "web isn't landed twice");
    assert_eq!(rig.main_tip("api"), new_api);
    let log = rig.log();
    let landed: Vec<(&str, &str)> = log.iter().map(|e| (e.folder.as_str(), e.reviewed_sha.as_str())).collect();
    assert_eq!(landed, vec![("web", pins["web"].as_str().unwrap()), ("api", new_api.as_str())]);
    assert_eq!(log[0].land, log[1].land, "one land");
    assert_eq!(rig.record(&w.task_id).phase, worker::PHASE_LANDED);
    rig.assert_no_leftovers();
}

#[tokio::test]
async fn a_held_land_can_be_abandoned_or_its_worker_stopped() {
    let rig = Rig::new("abandon", &["web"]);
    let a = rig.reported("drop", &["web"], "shared.txt", "one\nTWO\nthree\n");
    fs::write(rig.repo("web").join("shared.txt"), "the human's\n").unwrap();
    assert_eq!(rig.land(&a).await.unwrap()["state"], STATE_HELD);
    let out = crate::hive::retire::retire_tool(&rig.ctx(), &rig.caller, &json!({ "worker": "drop", "reason": "not needed" })).await.unwrap();
    assert_eq!(out["phase"], worker::PHASE_RETIRED, "{out}");
    assert!(out.get("landed").is_none(), "nothing of it landed: {out}");
    let r = rig.record(&a.task_id);
    assert!(!in_flight(&r) && r.land.as_ref().is_some_and(LandState::is_held), "the land stays as history");

    let b = rig.reported("pause", &["web"], "shared.txt", "one\nTHREE\nthree\n");
    assert_eq!(rig.land(&b).await.unwrap()["state"], STATE_HELD);
    crate::hive::control::stop_worker(&rig.ctx(), &rig.caller, &json!({ "worker": "pause", "reason": "later" })).await.unwrap();
    assert_eq!(rig.record(&b.task_id).phase, worker::PHASE_STOPPED);
    assert!(rig.land(&b).await.unwrap_err().contains("is stopped"));
    rig.assert_no_leftovers();
}

#[tokio::test]
async fn a_land_whose_worker_didnt_retire_holds_at_restart_and_hiveland_retires_it() {
    let rig = Rig::new("unretired", &["web"]);
    git(rig.repo("web"), &["checkout", "-q", "--detach"]);
    // Caught after its last step 2 and before the worker retired, or saved `landed` with the
    // retirement then not happening: either way every repo landed and the worker is Landing.
    for (slug, id, state) in [("crashed", "L-0000000a", STATE_ADVANCING), ("saved", "L-0000000b", STATE_LANDED)] {
        let w = rig.reported(slug, &["web"], &format!("{slug}.rs"), "x\n");
        let tip = rig.tips(&w)["web"].as_str().unwrap().to_string();
        let before = rig.main_tip("web");
        git(rig.repo("web"), &["update-ref", "refs/heads/main", &tip]);
        let mut rec = rig.record(&w.task_id);
        rec.phase = worker::PHASE_LANDING.into();
        rec.land = Some(LandState {
            id: id.into(),
            state: state.into(),
            summary: "s".into(),
            review: log::REVIEW_NONE.into(),
            repos: vec![LandRepo {
                folder: "web".into(),
                branch: w.folders[0].branch.clone().unwrap(),
                target: "main".into(),
                pinned: tip.clone(),
                base_before: Some(before),
                tested: Some(tip.clone()),
                base_after: Some(tip.clone()),
                status: REPO_LANDED.into(),
                logged: true,
                ..LandRepo::default()
            }],
            ..LandState::default()
        });
        worker::write_worker(&rig.caller.paths.task(&w.task_id), &rec).unwrap();

        assert_eq!(reconcile_in(&rig.caller.hive, &rig.caller.paths), 1, "{state}");
        let land = rig.record(&w.task_id).land.unwrap();
        assert!(land.is_held() && land.held_reason.unwrap().contains("hiveLand again to retire the worker"), "{state}");
        let out = rig.land(&w).await.unwrap();
        assert_eq!(out["state"], STATE_LANDED, "{state}: {out}");
        let r = rig.record(&w.task_id);
        assert_eq!((r.phase.as_str(), r.land.as_ref().unwrap().state.as_str()), (worker::PHASE_LANDED, STATE_LANDED));
        assert_eq!(rig.main_tip("web"), tip, "nothing landed twice");
    }
    // A worker that retired keeps its land as history, whatever it reads.
    let mut r = rig.record("t-saved");
    r.land.as_mut().unwrap().state = STATE_ADVANCING.into();
    worker::write_worker(&rig.caller.paths.task("t-saved"), &r).unwrap();
    assert_eq!(reconcile_in(&rig.caller.hive, &rig.caller.paths), 0);
    assert!(rig.log().is_empty(), "their lines were written when they landed");
    rig.assert_no_leftovers();
}

// ── The card ──

#[test]
fn a_cards_line_says_what_the_land_waits_on() {
    let mut l = LandState { id: "L-1".into(), state: STATE_LOCKING.into(), waiting_for: Some("land L-2 of maiTerm prod (pid 9) in web".into()), ..LandState::default() };
    assert_eq!(describe(&l), "waiting for land L-2 of maiTerm prod (pid 9) in web");
    l.state = STATE_HELD.into();
    l.held_reason = Some("Landed web; holding api: main keeps moving (3 tries)".into());
    assert!(describe(&l).starts_with("Landed web; holding api"));
    let v = LandView::of(&l);
    assert_eq!((v.id.as_str(), v.state.as_str()), ("L-1", STATE_HELD));
    let saved: LandState = serde_json::from_value(json!({ "id": "L-9", "state": "someday", "repos": [{ "folder": "web", "status": "later", "x": 1 }], "y": 2 })).unwrap();
    assert!(!saved.in_progress() && !saved.is_held(), "an unknown state still loads");
    assert_eq!(saved.repos[0].extra.get("x"), Some(&json!(1)));
    assert_eq!(saved.extra.get("y"), Some(&json!(2)));
    assert_eq!(driver::land_resource("Web App"), "land:web-app");
    assert!(crate::hive::queue::valid_resource(&driver::land_resource("Ünïcode Folder/x")));
    l.state = STATE_AWAITING_HUMAN.into();
    l.request = Some("r_9".into());
    assert_eq!(describe(&l), "waiting for the human's approval");
    assert_eq!(LandView::of(&l).request.as_deref(), Some("r_9"), "the worker's column finds its card");
}

// ── The human's approval (plan 6d) ──

#[tokio::test]
async fn agent_config_files_one_card_and_its_approval_lands_the_stored_tested_commit() {
    let rig = Rig::new("config-card", &["web"]);
    git(rig.repo("web"), &["checkout", "-q", "--detach"]);
    let w = rig.reported("hook", &["web"], ".claude/settings.json", "{\"hooks\":{}}\n");
    let before = rig.main_tip("web");
    let out = rig.land(&w).await.unwrap();
    assert_eq!(out["state"], STATE_AWAITING_HUMAN, "{out}");
    assert!(out["result"].as_str().unwrap().contains("`.claude/settings.json`"), "{out}");
    assert_eq!(rig.main_tip("web"), before, "nothing landed");
    rig.assert_no_leftovers();
    let r = rig.record(&w.task_id);
    assert_eq!(r.phase, worker::PHASE_LANDING);
    let land = r.land.clone().unwrap();
    assert_eq!((land.approval.as_deref(), land.request.as_deref()), (Some("config"), Some("r_1")));
    assert!(land.repos[0].status == REPO_TESTED && land.repos[0].tested.is_some() && land.repos[0].base_before.is_some(), "its step 1 stands");
    assert!(!in_flight(&r) && held(&r), "it holds no lock while it waits");

    let (req, d) = rig.last_card();
    assert_eq!((req.tab_id.as_str(), req.kind, req.reason.as_deref()), ("coord", "hive_land", Some("Ship it.")));
    assert_eq!(req.summary, "Land “Title hook” into main in web");
    assert!(req.effects.iter().any(|e| e == "Changes agent or execution config in web: `.claude/settings.json`."), "{:?}", req.effects);
    assert!(req.effects.iter().any(|e| e.starts_with("web: 1 file changed, 1 insertion(+), at the reviewed commit")), "{:?}", req.effects);
    assert!(req.effects.iter().any(|e| e.contains("No reviewer looked at it")), "{:?}", req.effects);
    assert_eq!((req.args["land_id"].as_str(), req.args["task_id"].as_str()), (Some(land.id.as_str()), Some(w.task_id.as_str())));
    assert_eq!(req.args["repos"]["web"]["pinned"], land.repos[0].pinned.as_str());
    assert!(rig.land(&w).await.unwrap_err().contains("waits for the human's approval"), "a waiting land isn't started twice");

    // Base unmoved: step 2 takes the stored tested commit, step 1 isn't run again. A stand-in
    // commit put there proves it.
    let tested = land.repos[0].tested.clone().unwrap();
    let stand_in = git(rig.repo("web"), &["commit-tree", &format!("{tested}^{{tree}}"), "-p", &tested, "-m", "stand-in"]);
    let mut rec = rig.record(&w.task_id);
    rec.land.as_mut().unwrap().repos[0].tested = Some(stand_in.clone());
    worker::write_worker(&rig.caller.paths.task(&w.task_id), &rec).unwrap();
    let v = approval::approved(&rig.ctx(), &rig.caller, &d, &opts()).await.answer().unwrap();
    assert_eq!(v["state"], STATE_LANDED, "{v}");
    assert_eq!(rig.main_tip("web"), stand_in);
    assert_eq!(rig.record(&w.task_id).phase, worker::PHASE_LANDED);
    let e = &rig.log()[0];
    assert_eq!((e.by.as_str(), e.request.as_deref(), e.kind.as_str()), ("human", Some("r_1"), "land"));
    let settled = rig.seen.lock().unwrap().settled.clone();
    assert_eq!(settled.len(), 1);
    assert!(settled[0].1 && settled[0].0 == "r_1", "{settled:?}");
    assert!(rig.told().iter().any(|t| t.starts_with("⟦HIVE LAND · Title hook⟧ The human approved worker #1 hook's land")), "{:?}", rig.told());
    // The same card again carries nothing out.
    assert_eq!(approval::approved(&rig.ctx(), &rig.caller, &d, &opts()).await, driver::Outcome::Gone);
    assert!(!rig.seen.lock().unwrap().settled[1].1);
    rig.assert_no_leftovers();
}

#[tokio::test]
async fn the_config_rule_asks_for_these_and_lets_a_version_bump_through() {
    let rig = Rig::with("config-list", &["web"], |h| h.settings.land.config_paths = vec!["deploy/*.sh".into()]);
    let pkg = |v: &str, scripts: &str| format!(r#"{{"name":"a","version":"{v}","scripts":{{{scripts}}}}}"#);
    commit(rig.repo("web"), "package.json", &pkg("1.0.0", r#""test":"vitest""#));
    git(rig.repo("web"), &["checkout", "-q", "--detach"]);
    let cases = [
        ("claude", ".claude/x".to_string(), "x\n".to_string(), true),
        ("deep", "docs/sub/CLAUDE.md".into(), "be careful\n".into(), true),
        ("husky", ".husky/pre-commit".into(), "npm test\n".into(), true),
        ("workflow", ".github/workflows/ci.yml".into(), "on: push\n".into(), true),
        ("version", "package.json".into(), pkg("1.0.1", r#""test":"vitest""#), false),
        ("scripts", "package.json".into(), pkg("1.0.1", r#""test":"vitest","postinstall":"curl x | sh""#), true),
        ("broken", "package.json".into(), "{not json".into(), true),
        ("custom", "deploy/go.sh".into(), "rsync\n".into(), true),
    ];
    for (slug, file, text, card) in cases {
        let w = rig.reported(slug, &["web"], &file, &text);
        let out = rig.land(&w).await.unwrap();
        assert_eq!(out["state"], if card { STATE_AWAITING_HUMAN } else { STATE_LANDED }, "{slug}: {out}");
        if card {
            let (req, d) = rig.last_card();
            assert!(req.effects.iter().any(|e| e.contains(&format!("`{file}`"))), "{slug}: {:?}", req.effects);
            // Denied, so every case lands on the same Base.
            approval::closed(&rig.ctx(), &rig.caller, &d, "denied").await.unwrap();
        }
    }
    assert_eq!(rig.log().len(), 1, "only the version bump landed");
}

#[tokio::test]
async fn a_denial_or_a_lapse_puts_the_worker_back_and_the_coordinator_is_told() {
    let rig = Rig::new("deny", &["web"]);
    let w = rig.reported("no", &["web"], "CLAUDE.md", "x\n");
    rig.land(&w).await.unwrap();
    let (_, mut d) = rig.last_card();
    d.by = "phone".into();
    let branch = rig.tips(&w);
    let out = approval::closed(&rig.ctx(), &rig.caller, &d, "denied").await.unwrap();
    assert!(matches!(out, driver::Outcome::Refused(_)), "{out:?}");
    let r = rig.record(&w.task_id);
    assert_eq!(r.phase, worker::PHASE_REPORTED);
    assert!(r.land.is_none());
    assert_eq!(rig.tips(&w), branch, "its branch as it was");
    assert!(rig.told().last().unwrap().contains("The human denied it from their phone."), "{:?}", rig.told());
    let noted = rig.seen.lock().unwrap().verbs.iter().any(|(v, a)| v == crate::hive::rpc::VERB_TASK && a["note"].as_str().is_some_and(|n| n.contains("Land refused: The human denied it")));
    assert!(noted, "the task gets a note");
    assert!(approval::closed(&rig.ctx(), &rig.caller, &d, "denied").await.is_none(), "once");
    assert_eq!(approval::approved(&rig.ctx(), &rig.caller, &d, &opts()).await, driver::Outcome::Gone, "nor approved after");

    let w2 = rig.reported("late", &["web"], "AGENTS.md", "x\n");
    rig.land(&w2).await.unwrap();
    let (_, d2) = rig.last_card();
    approval::closed(&rig.ctx(), &rig.caller, &d2, "expired").await.unwrap();
    assert_eq!(rig.record(&w2.task_id).phase, worker::PHASE_REPORTED);
    assert!(rig.told().last().unwrap().contains("didn't answer its card within 24 hours, so it lapsed"), "{:?}", rig.told());
    assert!(rig.log().is_empty());
    rig.assert_no_leftovers();
}

#[tokio::test]
async fn an_approval_after_base_moved_merges_again_and_a_changed_config_change_asks_again() {
    let rig = Rig::new("moved-card", &["web"]);
    let w = rig.reported("env", &["web"], ".envrc", "export A=1\n\n\n\nexport END=1\n");
    assert_eq!(rig.land(&w).await.unwrap()["state"], STATE_AWAITING_HUMAN);
    let (_, d) = rig.last_card();
    // Base moves under unrelated files: the config change is the one approved, so it lands.
    commit(rig.repo("web"), "unrelated.txt", "u\n");
    let v = approval::approved(&rig.ctx(), &rig.caller, &d, &opts()).await.answer().unwrap();
    assert_eq!(v["state"], STATE_LANDED, "{v}");
    assert_eq!(v["repos"][0]["merged_base"], true, "step 1 ran again on the new Base");
    assert!(rig.repo("web").join("unrelated.txt").exists() && rig.repo("web").join(".envrc").exists());

    // Base changes the config file itself after the card: what lands isn't what was approved.
    let w2 = rig.reported("env2", &["web"], ".envrc", "export A=2\n\n\n\nexport END=1\n");
    assert_eq!(rig.land(&w2).await.unwrap()["state"], STATE_AWAITING_HUMAN);
    let (_, d2) = rig.last_card();
    let base = rig.main_tip("web");
    commit(rig.repo("web"), ".envrc", "export A=1\n\n\n\nexport END=2\n");
    let out = approval::approved(&rig.ctx(), &rig.caller, &d2, &opts()).await;
    assert!(matches!(out, driver::Outcome::Awaiting(_)), "{out:?}");
    assert_ne!(rig.main_tip("web"), base, "the human's commit, nothing more");
    let settled = rig.seen.lock().unwrap().settled.last().cloned().unwrap();
    assert!(!settled.1 && settled.2.contains("no longer the one you approved"), "{settled:?}");
    let (_, d3) = rig.last_card();
    assert_ne!(d3.id, d2.id, "a new card");
    rig.assert_no_leftovers();
    let v = approval::approved(&rig.ctx(), &rig.caller, &d3, &opts()).await.answer().unwrap();
    assert_eq!(v["state"], STATE_LANDED, "{v}");
    assert_eq!(fs::read_to_string(rig.repo("web").join(".envrc")).unwrap(), "export A=2\n\n\n\nexport END=2\n");
}

#[tokio::test]
async fn landing_ask_files_the_card_before_step_1() {
    let rig = Rig::with("ask", &["web"], |h| {
        h.checkpoints.insert("landing".into(), "ask".into());
    });
    let w = rig.reported("asked", &["web"], "a.rs", "a\n");
    let out = rig.land(&w).await.unwrap();
    assert_eq!(out["state"], STATE_AWAITING_HUMAN, "{out}");
    let land = rig.record(&w.task_id).land.unwrap();
    assert_eq!(land.approval.as_deref(), Some("ask"));
    assert!(land.repos[0].tested.is_none(), "no step 1 before the human answers");
    rig.assert_no_leftovers();
    let (req, d) = rig.last_card();
    assert!(req.effects.iter().any(|e| e.contains("Landing checkpoint for this Hive is Ask me")), "{:?}", req.effects);
    assert!(!req.effects.iter().any(|e| e.contains("execution config")), "{:?}", req.effects);
    let v = approval::approved(&rig.ctx(), &rig.caller, &d, &opts()).await.answer().unwrap();
    assert_eq!(v["state"], STATE_LANDED, "{v}");
    assert_eq!(rig.log()[0].by, "human");
    assert_eq!(rig.seen.lock().unwrap().filed.len(), 1, "an approved land isn't asked about again");
    rig.assert_no_leftovers();
}

#[tokio::test]
async fn integration_mode_merges_exactly_the_approved_sha_no_ff_though_the_branch_moved_on() {
    let rig = Rig::with("integration", &["web"], |h| h.landing_mode = LANDING_INTEGRATION.into());
    let repo = rig.repo("web").to_path_buf();
    // The coordinator's read copy holds hive/ship, as the Hive's layout makes it.
    let rc = rig.caller.paths.coordinator_worktree("web");
    fs::create_dir_all(rc.parent().unwrap()).unwrap();
    git(&repo, &["worktree", "add", "-q", "-b", "hive/ship", rc.to_str().unwrap(), "main"]);
    let main0 = rig.main_tip("web");
    let w = rig.reported("feat", &["web"], "feat.rs", "f\n");
    assert_eq!(w.folders[0].target.as_deref(), Some("hive/ship"));
    let pinned = rig.tips(&w)["web"].as_str().unwrap().to_string();
    let out = rig.land(&w).await.unwrap();
    assert_eq!(out["state"], STATE_AWAITING_HUMAN, "{out}");
    rig.assert_no_leftovers();
    let (req, d) = rig.last_card();
    assert_eq!(req.args["why"], "integration");
    assert_eq!(req.summary, "Land “Title feat” into hive/ship in web");
    assert!(req.effects[0].contains("lands into hive/ship only with your approval"), "{:?}", req.effects);
    assert!(req.effects[0].contains("web has no land check") && !req.effects[0].contains("runs its land check"), "never a check it doesn't have: {:?}", req.effects);

    // The worker commits on after the card.
    let wt = PathBuf::from(w.folders[0].worktree.clone().unwrap());
    let newer = commit(&wt, "more.rs", "m\n");
    let v = approval::approved(&rig.ctx(), &rig.caller, &d, &opts()).await.answer().unwrap();
    assert_eq!(v["state"], STATE_LANDED, "{v}");
    let tip = git(&repo, &["rev-parse", "refs/heads/hive/ship"]);
    let parents: Vec<String> = git(&repo, &["rev-list", "--parents", "-n1", &tip]).split_whitespace().map(str::to_string).collect();
    assert_eq!(parents, vec![tip.clone(), main0.clone(), pinned.clone()], "a --no-ff merge of exactly the pin");
    assert!(git(&repo, &["log", "-1", "--format=%s", &tip]).starts_with(&format!("Land Title feat ({}) [maiTerm L-", &pinned[..7])));
    assert_eq!(rig.main_tip("web"), main0, "Base untouched");
    assert_eq!(git(&rc, &["rev-parse", "HEAD"]), tip, "the read copy on hive/ship moved with it");
    assert_eq!(git(&rc, &["symbolic-ref", "HEAD"]), "refs/heads/hive/ship", "and stays on its branch");
    let r = rig.record(&w.task_id);
    assert_eq!(r.phase, worker::PHASE_REPORTED, "not retired: its newer commit needs a land of its own");
    assert_eq!(rig.tips(&w)["web"], newer.as_str(), "its branch isn't moved");
    assert!(v["result"].as_str().unwrap().contains("moved since you named them"), "{v}");
    let e = &rig.log()[0];
    assert_eq!((e.kind.as_str(), e.target.as_str(), e.by.as_str()), ("integration", "hive/ship", "human"));
    assert_eq!((e.merge_commit.as_deref(), e.reviewed_sha.as_str()), (Some(tip.as_str()), pinned.as_str()));
    rig.assert_no_leftovers();
}

#[tokio::test]
async fn a_waiting_card_is_filed_again_after_a_restart_under_its_id_and_deadline() {
    let rig = Rig::new("refile", &["web"]);
    let w = rig.reported("again", &["web"], ".mcp.json", "{}\n");
    rig.land(&w).await.unwrap();
    let (first, d) = rig.last_card();
    let at = rig.record(&w.task_id).land.unwrap().request_at;
    assert!(at > 0);
    assert_eq!(approval::refile_in(&rig.ctx().window, &rig.caller), 1);
    let (again, d2) = rig.last_card();
    assert_eq!(again.keep, Some((d.id.clone(), at)));
    assert_eq!((&again.args, &again.summary, &again.effects, &again.tab_id), (&first.args, &first.summary, &first.effects, &first.tab_id));
    let v = approval::approved(&rig.ctx(), &rig.caller, &d2, &opts()).await.answer().unwrap();
    assert_eq!(v["state"], STATE_LANDED, "{v}");
    assert_eq!(approval::refile_in(&rig.ctx().window, &rig.caller), 0, "nothing waits now");
}

#[tokio::test]
async fn a_worker_stopped_while_its_card_waits_withdraws_the_card() {
    let rig = Rig::new("withdraw", &["web"]);
    let w = rig.reported("halt", &["web"], ".githooks/pre-push", "exit 0\n");
    rig.land(&w).await.unwrap();
    let (_, d) = rig.last_card();
    crate::hive::control::stop_worker(&rig.ctx(), &rig.caller, &json!({ "worker": "halt", "reason": "later" })).await.unwrap();
    assert_eq!(rig.seen.lock().unwrap().withdrawn, vec!["r_1".to_string()]);
    assert_eq!(approval::approved(&rig.ctx(), &rig.caller, &d, &opts()).await, driver::Outcome::Gone);
    let settled = rig.seen.lock().unwrap().settled.clone();
    assert!(!settled[0].1 && settled[0].2.contains("is stopped now"), "{settled:?}");
    assert_eq!(rig.record(&w.task_id).phase, worker::PHASE_STOPPED);
}

#[tokio::test]
async fn a_partly_landed_land_whose_card_goes_with_a_new_report_keeps_what_landed() {
    let rig = Rig::new("partial-card", &["web", "api"]);
    commit(rig.repo("api"), ".envrc", "export A=1\n\n\n\nexport END=1\n");
    let w = rig.reported("split", &["web", "api"], ".envrc", "export A=2\n\n\n\nexport END=1\n");
    let pins = rig.tips(&w);
    assert_eq!(rig.land(&w).await.unwrap()["state"], STATE_AWAITING_HUMAN);
    let (_, d) = rig.last_card();
    // Approved; web lands, then Base changes api's config file under its step 2, so the merged
    // change isn't the approved one and a second card asks with web already landed.
    let api = rig.repo("api").to_path_buf();
    let mut o = opts();
    o.before_advance = Some(Arc::new(move |f: &str| {
        if f == "api" {
            commit(&api, ".envrc", "export A=1\n\n\n\nexport END=2\n");
        }
    }));
    let out = approval::approved(&rig.ctx(), &rig.caller, &d, &o).await;
    assert!(matches!(out, driver::Outcome::Awaiting(_)), "{out:?}");
    let land = rig.record(&w.task_id).land.unwrap();
    assert_eq!((land.state.as_str(), land.landed().len()), (STATE_AWAITING_HUMAN, 1), "{land:?}");
    let web1 = rig.main_tip("web");

    // The worker reports again with a new api commit: the card goes, and the land holds as a
    // denial would have left it, so what landed stays landed.
    let api_wt = PathBuf::from(w.folders.iter().find(|f| f.name == "api").unwrap().worktree.clone().unwrap());
    let new_api = commit(&api_wt, "more.rs", "m\n");
    let mut rec = rig.record(&w.task_id);
    rec.tab_id = Some("wtab".into());
    worker::write_worker(&rig.caller.paths.task(&w.task_id), &rec).unwrap();
    let mut as_worker = rig.caller.clone();
    as_worker.tab_id = "wtab".into();
    crate::hive::report::report(&rig.ctx(), &as_worker, &json!({ "status": "done", "summary": "more", "receipts": { "commits": [new_api] } }))
        .await
        .unwrap();
    assert_eq!(rig.seen.lock().unwrap().withdrawn, vec!["r_2".to_string()]);
    let r = rig.record(&w.task_id);
    assert_eq!(r.phase, worker::PHASE_REPORTED);
    let held_land = r.land.clone().unwrap();
    assert!(held_land.is_held() && held_land.held_reason.as_deref().unwrap().starts_with("Landed web; its card for the rest was withdrawn"), "{held_land:?}");

    let mut wrong = rig.tips(&w);
    wrong["web"] = json!(new_api);
    let e = rig.land_with(&w, json!({ "commits": wrong }), opts()).await.unwrap_err();
    assert!(e.contains("already landed web at"), "{e}");
    // api's new commit still changes .envrc: a new card for it alone, in the same land.
    assert_eq!(rig.land(&w).await.unwrap()["state"], STATE_AWAITING_HUMAN);
    let (req, d) = rig.last_card();
    assert_eq!(req.args["land_id"], land.id.as_str(), "the same land");
    assert!(req.args["repos"].get("web").is_none() && req.effects.iter().any(|e| e == "Already landed: web."), "{:?}", req.effects);
    let v = approval::approved(&rig.ctx(), &rig.caller, &d, &opts()).await.answer().unwrap();
    assert_eq!(v["state"], STATE_LANDED, "{v}");
    assert_eq!(rig.main_tip("web"), web1, "web isn't landed twice");
    let log = rig.log();
    let landed: Vec<(&str, &str)> = log.iter().map(|e| (e.folder.as_str(), e.reviewed_sha.as_str())).collect();
    assert_eq!(landed, vec![("web", pins["web"].as_str().unwrap()), ("api", new_api.as_str())]);
    assert!(log.iter().all(|e| e.land == land.id), "one land");
    rig.assert_no_leftovers();
}

#[tokio::test]
async fn integration_mode_logs_no_merge_commit_for_a_pin_the_branch_already_has_and_says_which_repos_go_unchecked() {
    let rig = Rig::with("integration-noop", &["web", "api"], |h| {
        h.landing_mode = LANDING_INTEGRATION.into();
        h.folders[0].land_check = Some("true".into());
    });
    for name in ["web", "api"] {
        let rc = rig.caller.paths.coordinator_worktree(name);
        fs::create_dir_all(rc.parent().unwrap()).unwrap();
        git(rig.repo(name), &["worktree", "add", "-q", "-b", "hive/ship", rc.to_str().unwrap(), "main"]);
    }
    let ship = |name: &str| git(rig.repo(name), &["rev-parse", "refs/heads/hive/ship"]);
    let checked = || Opts { checker: Arc::new(ShChecker::default()), ..opts() };

    // An earlier land into api: its merge commit is hive/ship's tip there.
    let a = rig.reported("first", &["api"], "a.rs", "a\n");
    rig.land(&a).await.unwrap();
    let (req, d) = rig.last_card();
    assert!(req.effects[0].ends_with("--no-ff. api has no land check, so nothing tests the merge there before it lands."), "{:?}", req.effects);
    assert_eq!(approval::approved(&rig.ctx(), &rig.caller, &d, &opts()).await.answer().unwrap()["state"], STATE_LANDED);
    let m_prev = ship("api");

    // A worker in web and api that committed only in web: its api pin is hive/ship's tip.
    let b = rig.reported("second", &["web", "api"], "b.rs", "b\n");
    let b_api = PathBuf::from(b.folders.iter().find(|f| f.name == "api").unwrap().worktree.clone().unwrap());
    git(&b_api, &["reset", "-q", "--hard", "HEAD~1"]);
    assert_eq!(rig.tips(&b)["api"], m_prev.as_str());
    rig.land_with(&b, json!({}), checked()).await.unwrap();
    let (req, d) = rig.last_card();
    assert!(req.effects[0].contains("api has no land check, so nothing tests the merge there before it lands. The land check runs first in web."), "{:?}", req.effects);
    assert!(req.effects.iter().any(|e| e.starts_with("api: no file changes")), "{:?}", req.effects);
    assert_eq!(approval::approved(&rig.ctx(), &rig.caller, &d, &checked()).await.answer().unwrap()["state"], STATE_LANDED);
    assert_eq!(ship("api"), m_prev, "nothing merged in api");
    let log = rig.log();
    let api_line = log.iter().find(|e| e.task_id == b.task_id && e.folder == "api").unwrap();
    assert_eq!(api_line.merge_commit, None, "the earlier land's merge commit isn't this land's");
    let web_line = log.iter().find(|e| e.task_id == b.task_id && e.folder == "web").unwrap();
    assert_eq!(web_line.merge_commit.as_deref(), Some(ship("web").as_str()));
    rig.assert_no_leftovers();
}
