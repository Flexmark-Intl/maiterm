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
fn ok_preflight(_: &HivePaths, _: &WorkerRecord, _: &crate::state::Preferences) -> Result<(), String> {
    Ok(())
}

/// Never asked: these lands have no land check.
struct NoChecker;
impl Checker for NoChecker {
    fn start(&self, req: &CheckRequest) -> Result<CheckStart, String> {
        panic!("no land check was set for {}", req.folder)
    }
}

/// Runs the check where the queue would, to its end, and counts its runs by kind.
#[derive(Default)]
struct ShChecker(Mutex<Vec<(&'static str, String)>>);
impl ShChecker {
    fn runs(&self, kind: &str) -> usize {
        self.0.lock().unwrap().iter().filter(|(k, _)| *k == kind).count()
    }
}
impl Checker for ShChecker {
    fn start(&self, req: &CheckRequest) -> Result<CheckStart, String> {
        self.0.lock().unwrap().push((req.kind, req.folder.clone()));
        let out = Command::new("/bin/sh").arg("-c").arg(&req.command).current_dir(&req.tmp.path).output().map_err(|e| e.to_string())?;
        let tail = format!("{}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr));
        Ok(CheckStart::Done { passed: out.status.success(), tail })
    }
}

fn opts() -> Opts {
    Opts { lock_wait: Duration::from_secs(20), checker: Arc::new(NoChecker), before_advance: None, after_advance: None }
}

/// The land a test's `hold_lock` stands in for.
const HOLDER: &str = "L-h01d0000";

/// Each check made into the job the queue would run (kept, to count), then run where it would.
#[derive(Default)]
struct QueueCounter {
    jobs: Mutex<Vec<crate::hive::queue::Job>>,
    sh: ShChecker,
}
impl Checker for QueueCounter {
    fn start(&self, req: &CheckRequest) -> Result<CheckStart, String> {
        let job = crate::hive::queue::Queue::default().enqueue(driver::check_spec(req), 0)?;
        self.jobs.lock().unwrap().push(job);
        self.sh.start(req)
    }
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
            // The helper ignores system config and maiTerm's git doesn't: Windows git's system
            // `core.autocrlf=true` would check out CRLF that the helper then commits back.
            git(&r, &["config", "core.autocrlf", "false"]);
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

    /// The coordinator's `hiveUndoLand`.
    async fn undo(&self, line: &str) -> Result<Value, String> {
        undo::undo_tool_with(&self.ctx(), &self.caller, &json!({ "land": line, "reason": "Broke it." }), &opts()).await
    }

    /// The land log line a single-repo land wrote.
    fn line_of(out: &Value, folder: &str) -> String {
        format!("{}/{folder}", out["land"].as_str().expect("a land id"))
    }

    /// Another land holds `folder`'s lock, so one-repo lands there queue for a batch. Its common
    /// dir, for `lock::release(.., HOLDER)`.
    fn hold_lock(&self, folder: &str) -> PathBuf {
        let common = git::common_dir(self.repo(folder)).unwrap();
        assert_eq!(lock::try_take(&common, &lock::LockInfo::ours(&self.caller.hive.id, HOLDER)).unwrap(), lock::Taken::Taken);
        common
    }

    /// `hiveLand` of each, every one answering that it waits for a batch.
    async fn queue_all(&self, ws: &[WorkerRecord], o: &Opts) {
        for w in ws {
            let out = self.land_with(w, json!({}), o.clone()).await.unwrap();
            assert_eq!(out["state"], STATE_QUEUED_BATCH, "{out}");
            assert_eq!(self.record(&w.task_id).land.unwrap().state, STATE_QUEUED_BATCH);
        }
    }

    /// The repo's batch queue run to empty, as `continue_batch` does outside a test.
    async fn run_batch(&self, folder: &str, o: &Opts) {
        batch::run(&self.ctx(), &self.caller, folder, o, &|| {}).await;
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
    // A failed check runs the post-land check on Base alone (§8.6): main has no verdict either,
    // so Base fails too and web is suspect, though no land of the Hive's caused it.
    assert_eq!(checker.runs(CHECK_BASE), 1);
    let folded = log::folded(&rig.caller.paths);
    assert!(folded.repos["web"].suspect, "{folded:?}");
    let told = rig.told().join("\n");
    assert!(told.contains("⟦HIVE BASE CHECK · web⟧ main fails web's land check alone") && told.contains("No land of this Hive's has landed there"), "{told}");

    let good = rig.reported("good", &["web"], "verdict.txt", "ok\n");
    let e = rig.land_with(&good, json!({}), o.clone()).await.unwrap_err();
    assert!(e.contains("web is suspect") && e.contains("fix_forward: true"), "{e}");
    // Without the worker's installed state the check has nothing to run against.
    let e = rig.land_with(&good, json!({ "fix_forward": true }), o.clone()).await.unwrap_err();
    assert!(e.contains("FAIL: verdict is not ok") && !e.contains("the setup ran"), "{e}");
    let wt = installed(&good);
    let out = rig.land_with(&good, json!({ "fix_forward": true }), o).await.unwrap();
    assert_eq!(out["state"], STATE_LANDED, "{out}");
    assert_eq!(checker.runs(CHECK_LAND), 3, "one check per land");
    assert_eq!(checker.runs(CHECK_BASE), 2, "and Base alone after each failure");
    assert_eq!(rig.log().iter().filter(|e| e.kind == "land").map(|e| e.check.as_str()).collect::<Vec<_>>(), vec!["passed"]);
    assert!(!log::folded(&rig.caller.paths).repos["web"].suspect, "a fix forward whose check passed clears it");
    assert!(!rig.repo("web").join("node_modules").exists(), "seeded into the temporary worktree, not the checkout");
    assert!(!wt.exists() || wt.join("node_modules/dep/index.js").exists(), "copied, not moved");
    rig.assert_no_leftovers();
}

/// The post-land check judges Base alone, often against the very worker whose check just failed:
/// nothing that worker's agent left in its worktree (ignored files) reaches it.
#[cfg(unix)]
#[tokio::test]
async fn the_post_land_check_is_never_seeded_from_the_failing_workers_worktree() {
    let rig = Rig::with("baseseed", &["web"], |h| {
        h.folders[0].land_check = Some("test ! -e node_modules/poison && grep -q ok verdict.txt || { echo 'FAIL'; exit 3; }".into());
    });
    commit(rig.repo("web"), ".gitignore", "node_modules/\n");
    commit(rig.repo("web"), "verdict.txt", "ok\n");
    let checker = Arc::new(ShChecker::default());
    let mut o = opts();
    o.checker = checker.clone();
    let bad = rig.reported("bad", &["web"], "verdict.txt", "nope\n");
    let wt = PathBuf::from(bad.folders[0].worktree.clone().unwrap());
    fs::create_dir_all(wt.join("node_modules")).unwrap();
    fs::write(wt.join("node_modules/poison"), "x").unwrap();
    rig.land_with(&bad, json!({}), o).await.unwrap_err();
    assert_eq!(checker.runs(CHECK_BASE), 1);
    let base = rig.log().into_iter().find(|e| e.kind == "base_check").expect("a base_check line");
    assert_eq!(base.check, "passed", "Base alone has no poison: {base:?}");
    assert!(!log::folded(&rig.caller.paths).repos.get("web").is_some_and(|r| r.suspect));
    let told = rig.told().join("\n");
    assert!(told.contains("main passes web's land check alone"), "{told}");
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
    let seeded = git::seed_ignored(&w.folders[0], &tmp, &[]);
    assert_eq!(seeded, vec!["cache".to_string()], "{seeded:?}");
    assert_eq!(fs::read_to_string(tmp.join("cache/a")).unwrap(), "a");
    assert!(fs::read_dir(&elsewhere).unwrap().next().is_none(), "nothing went through the link");
}

/// The third live test logged `seeded web from worker …'s worktree: .claude`. Agent config the
/// config rule lists, the human's extra paths included, never reaches a check's tree.
#[cfg(unix)] // seeding copies with /bin/cp
#[test]
fn seeding_skips_agent_config() {
    let rig = Rig::new("seedconfig", &["web"]);
    let w = rig.reported("seedcfg", &["web"], "s.rs", "s\n");
    let wt = PathBuf::from(w.folders[0].worktree.clone().unwrap());
    fs::write(wt.join(".gitignore"), "node_modules/\n.claude/\n.mcp.json\n.envrc\nscripts-local/\n").unwrap();
    for d in ["node_modules", ".claude", "scripts-local"] {
        fs::create_dir_all(wt.join(d)).unwrap();
        fs::write(wt.join(d).join("f"), "x").unwrap();
    }
    fs::write(wt.join(".mcp.json"), "{}").unwrap();
    fs::write(wt.join(".envrc"), "export X=1").unwrap();
    let tmp = rig.base.join("tmpcfg");
    fs::create_dir_all(&tmp).unwrap();
    let seeded = git::seed_ignored(&w.folders[0], &tmp, &["scripts-local/**".to_string()]);
    assert_eq!(seeded, vec!["node_modules".to_string()], "{seeded:?}");
    for gone in [".claude", ".mcp.json", ".envrc", "scripts-local"] {
        assert!(fs::symlink_metadata(tmp.join(gone)).is_err(), "{gone} seeded");
    }
}

#[tokio::test]
async fn a_queued_check_pauses_the_land_and_its_end_carries_it_on() {
    struct Queued(Mutex<Vec<String>>);
    impl Checker for Queued {
        fn start(&self, req: &CheckRequest) -> Result<CheckStart, String> {
            // The post-land check after a failure: Base passes alone.
            if req.kind == CHECK_BASE {
                return Ok(CheckStart::Done { passed: true, tail: String::new() });
            }
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
    let told = rig.told().join("\n");
    assert!(told.contains("main passes web's land check alone") && told.contains("failed on its land's own change"), "{told}");
    assert!(rig.log().last().is_some_and(|e| e.kind == "base_check" && e.check == "passed"));
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
            if req.kind == CHECK_BASE {
                return Ok(CheckStart::Done { passed: true, tail: String::new() });
            }
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
        decision: Some("D-c4a5e000".into()),
        ..LandState::default()
    });
    worker::write_worker(&rig.caller.paths.task(&w.task_id), &rec).unwrap();
    crate::hive::decisions::land_decided(&rig.caller.paths, &rig.caller.hive.id, &rec, rec.land.as_ref().unwrap());
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
    assert_eq!(log[0].decision.as_deref(), Some("D-c4a5e000"), "the decision that landed it");
    // The coordinator's decision hears the hold, rather than reading "running" for good.
    let d = crate::hive::decisions::find(&crate::hive::decisions::read(&rig.caller.paths), "D-c4a5e000").unwrap();
    assert!(d.outcome.as_deref().is_some_and(|o| o.starts_with("held: landed web; interrupted by a maiTerm restart")), "{:?}", d.outcome);
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
    l.state = STATE_QUEUED_BATCH.into();
    assert_eq!(describe(&l), "waiting to land in a batch");
    assert!(l.in_progress(), "a place in a batch queue is a land under way: a restart holds it");
    l.state = STATE_CHECKING.into();
    l.batch = Some("Q-1a2b3c4d".into());
    assert_eq!((describe(&l).as_str(), LandView::of(&l).batch.as_deref()), ("batch land check running", Some("Q-1a2b3c4d")));
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

#[cfg(unix)]
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

// ── Undo (plan 6e, §8.6) ──

#[tokio::test]
async fn undo_reverts_a_fast_forward_land_on_a_moved_base_and_refuses_one_later_work_builds_on() {
    let rig = Rig::new("undo-ff", &["web"]);
    let w = rig.reported("feat", &["web"], "feat.rs", "fn feat() {}\n");
    let id = Rig::line_of(&rig.land(&w).await.unwrap(), "web");
    // The human moves on meanwhile: the revert goes on top of their commit.
    let human = commit(rig.repo("web"), "docs.md", "docs\n");
    let v = rig.undo(&id).await.unwrap();
    assert_eq!(v["state"], undo::STATE_DONE, "{v}");
    let tip = rig.main_tip("web");
    assert_eq!(git(rig.repo("web"), &["rev-parse", &format!("{tip}^")]), human, "a commit on top: nothing reset");
    assert_eq!(git(rig.repo("web"), &["rev-parse", "HEAD"]), tip, "the checkout fast-forwarded");
    assert!(!rig.repo("web").join("feat.rs").exists() && rig.repo("web").join("docs.md").exists());
    let msg = git(rig.repo("web"), &["log", "-1", "--format=%s", &tip]);
    assert!(msg.starts_with(&format!("Revert land {id} (Title feat) [maiTerm U-")), "{msg}");
    let log = rig.log();
    let u = log.last().unwrap();
    assert_eq!((u.kind.as_str(), u.undoes.as_deref(), u.by.as_str()), ("undo", Some(id.as_str()), "coordinator"));
    assert_eq!((u.base_before.as_str(), u.base_after.as_str(), u.summary.as_str()), (human.as_str(), tip.as_str(), "Broke it."));
    assert_eq!(log::folded(&rig.caller.paths).get(&id).unwrap().undone_by.as_deref(), Some(u.id.as_str()));
    let r = rig.record(&w.task_id);
    assert!(r.undone && r.phase == worker::PHASE_LANDED, "marked undone, not reopened");
    assert!(rig.undo(&id).await.unwrap_err().contains("already undone"));
    assert!(rig.undo(&u.id).await.unwrap_err().contains("is an undo itself"));
    // Workers waiting on it stay blocked, though its task is done.
    let waiter = WorkerRecord { task_id: "t-next".into(), slug: "next".into(), depends_on: vec![w.task_id.clone()], phase: worker::PHASE_WAITING.into(), ..Default::default() };
    let row: crate::state::Task = serde_json::from_value(json!({ "id": w.task_id, "title": "t", "status": "done", "origin": "agent", "created_at": "", "updated_at": "" })).unwrap();
    let s = crate::hive::waiters::standing(&waiter, &worker::list_workers(&rig.caller.paths), &[row]);
    assert_eq!(s, crate::hive::waiters::Standing::Blocked("blocked: #1 feat's land was undone".into()));
    let board = crate::hive::tools::board_view(&rig.caller.hive, &rig.caller.paths, rig.fuses, crate::hive::fuses::now_ms(), false);
    assert!(board.workers.iter().any(|c| c.task_id == w.task_id && c.undone));
    rig.assert_no_leftovers();

    // Later work changes the same lines: a revert can't take it back.
    let w2 = rig.reported("lines", &["web"], "shared.txt", "one\nworker\nthree\n");
    let id2 = Rig::line_of(&rig.land(&w2).await.unwrap(), "web");
    commit(rig.repo("web"), "shared.txt", "one\nworker and human\nthree\n");
    let before = rig.main_tip("web");
    let p = undo::preview(&rig.caller.hive, &rig.caller.paths, &id2).unwrap();
    assert!(p.refusal.as_deref().is_some_and(|r| r.contains("conflicts with later work")), "{p:?}");
    let e = rig.undo(&id2).await.unwrap_err();
    assert!(e.contains("conflicts with later work") && e.contains("`shared.txt`") && e.contains("fix forward"), "{e}");
    assert!(e.contains("Nothing was written to main in web"), "{e}");
    assert_eq!(rig.main_tip("web"), before);
    assert!(!rig.record(&w2.task_id).undone);
    rig.assert_no_leftovers();
}

/// Base moves between the undo's fast-forward and the read of what landed: that tree is nothing
/// a check passed, so the post-land check judges it, as after a land (§8.6).
#[cfg(unix)]
#[tokio::test]
async fn an_undo_whose_landed_tree_isnt_its_tested_one_gets_the_post_land_check() {
    let rig = Rig::with("undo-moved", &["web"], |h| h.folders[0].land_check = Some("true".into()));
    let checker = Arc::new(ShChecker::default());
    let mut o = Opts { checker: checker.clone(), ..opts() };
    let w = rig.reported("feat", &["web"], "feat.rs", "fn feat() {}\n");
    let id = Rig::line_of(&rig.land_with(&w, json!({}), o.clone()).await.unwrap(), "web");
    assert_eq!(checker.runs(CHECK_BASE), 0, "the land landed what it tested");
    let web = rig.repo("web").to_path_buf();
    o.after_advance = Some(Arc::new(move |_: &str| {
        commit(&web, "late.md", "late\n");
    }));
    let v = undo::undo_tool_with(&rig.ctx(), &rig.caller, &json!({ "land": id, "reason": "r" }), &o).await.unwrap();
    assert_eq!(v["state"], undo::STATE_DONE, "{v}");
    let log = rig.log();
    let u = log.iter().find(|e| e.kind == "undo").unwrap();
    assert_ne!(u.tree_landed, u.tree_tested);
    let base = log.iter().find(|e| e.kind == "base_check").expect("the post-land check ran");
    assert!(base.summary.contains(&format!("{} landed a tree other than the one its check passed", u.land)) && base.check == "passed", "{base:?}");
    assert_eq!(checker.runs(CHECK_BASE), 1);
    rig.assert_no_leftovers();
}

#[tokio::test]
async fn a_land_in_several_repos_is_undone_one_repo_at_a_time() {
    let rig = Rig::new("undo-multi", &["web", "api"]);
    let w = rig.reported("pair", &["web", "api"], "pair.txt", "pair\n");
    let out = rig.land(&w).await.unwrap();
    let land = out["land"].as_str().unwrap().to_string();
    let e = rig.undo(&land).await.unwrap_err();
    assert!(e.contains("landed in several repos") && e.contains(&format!("{land}/web")) && e.contains(&format!("{land}/api")), "{e}");
    assert!(rig.undo("L-00000000/web").await.unwrap_err().contains("No land L-00000000/web"));
    let api0 = rig.main_tip("api");
    assert_eq!(rig.undo(&format!("{land}/web")).await.unwrap()["state"], undo::STATE_DONE);
    assert!(!rig.repo("web").join("pair.txt").exists() && rig.repo("api").join("pair.txt").exists());
    assert_eq!(rig.main_tip("api"), api0, "api untouched");
}

#[tokio::test]
async fn a_dirty_base_checkout_holds_the_undo_and_undoing_again_resumes_it() {
    let rig = Rig::new("undo-dirty", &["web"]);
    let w = rig.reported("touch", &["web"], "shared.txt", "one\nTWO\nthree\n");
    let id = Rig::line_of(&rig.land(&w).await.unwrap(), "web");
    let landed = rig.main_tip("web");
    fs::write(rig.repo("web").join("shared.txt"), "the human's\n").unwrap();
    let v = rig.undo(&id).await.unwrap();
    assert_eq!(v["state"], STATE_HELD, "{v}");
    assert!(v["reason"].as_str().unwrap().contains("uncommitted changes to `shared.txt`"), "{v}");
    assert_eq!(rig.main_tip("web"), landed, "nothing written");
    assert_eq!(fs::read_to_string(rig.repo("web").join("shared.txt")).unwrap(), "the human's\n", "nothing stashed or overwritten");
    assert!(!rig.record(&w.task_id).undone);
    rig.assert_no_leftovers();
    let view = undo::log_view(&rig.caller.hive, &rig.caller.paths);
    assert_eq!((view.undos.len(), view.undos[0].state.as_str()), (1, STATE_HELD));

    git(rig.repo("web"), &["checkout", "--", "shared.txt"]);
    let v = rig.undo(&id).await.unwrap();
    assert_eq!(v["state"], undo::STATE_DONE, "{v}");
    assert_eq!(v["undo"], view.undos[0].id.as_str(), "the held undo resumed");
    assert_eq!(fs::read_to_string(rig.repo("web").join("shared.txt")).unwrap(), "one\ntwo\nthree\n");
    assert!(undo::log_view(&rig.caller.hive, &rig.caller.paths).undos.is_empty());
    rig.assert_no_leftovers();
}

/// A Done Hive's report proved its lands: its coordinator can't take one back, even in a call that
/// raced the finish past the tool gate. The human's own Undo stays theirs.
#[tokio::test]
async fn a_done_hives_coordinator_undoes_nothing_and_the_human_still_can() {
    let rig = Rig::new("undo-done", &["web"]);
    let w = rig.reported("feat", &["web"], "feat.rs", "fn feat() {}\n");
    let id = Rig::line_of(&rig.land(&w).await.unwrap(), "web");
    let landed = rig.main_tip("web");
    rig.data.write().windows[0].workspaces[0].hive.as_mut().unwrap().status = crate::hive::record::STATUS_DONE.into();
    let e = rig.undo(&id).await.unwrap_err();
    assert!(e.contains("This Hive is done") && e.contains("Nothing was written to main in web"), "{e}");
    assert_eq!(rig.main_tip("web"), landed);
    assert!(undo::read_all(&rig.caller.paths).iter().all(|u| u.state == undo::STATE_REFUSED), "settled, so nothing is under way");
    rig.assert_no_leftovers();
    let p = undo::preview(&rig.caller.hive, &rig.caller.paths, &id).unwrap();
    assert_eq!(undo::human_undo(&rig.ctx(), &rig.caller, &id, p.config_fp.clone(), &opts()).await.unwrap()["state"], undo::STATE_DONE);
}

#[tokio::test]
async fn the_coordinators_undo_of_config_files_a_card_and_the_humans_own_undo_goes_on_what_it_showed() {
    let rig = Rig::new("undo-config", &["web"]);
    git(rig.repo("web"), &["checkout", "-q", "--detach"]);
    let w = rig.reported("hook", &["web"], ".claude/settings.json", "{}\n");
    assert_eq!(rig.land(&w).await.unwrap()["state"], STATE_AWAITING_HUMAN);
    let (_, d) = rig.last_card();
    approval::approved(&rig.ctx(), &rig.caller, &d, &opts()).await.answer().unwrap();
    let id = rig.log()[0].id.clone();
    let before = rig.main_tip("web");

    let v = rig.undo(&id).await.unwrap();
    assert_eq!(v["state"], STATE_AWAITING_HUMAN, "{v}");
    assert_eq!(rig.main_tip("web"), before, "nothing written while the human decides");
    rig.assert_no_leftovers();
    let (req, d) = rig.last_card();
    assert_eq!((req.kind, req.tab_id.as_str(), req.reason.as_deref()), (undo::KIND, "coord", Some("Broke it.")));
    assert_eq!(req.summary, "Undo land “Title hook” on main in web");
    assert!(req.effects.iter().any(|e| e == "Changes agent or execution config in web: `.claude/settings.json`."), "{:?}", req.effects);
    assert!(req.effects.iter().any(|e| e.starts_with(&format!("Reverts land {id} (1 file changed"))), "{:?}", req.effects);
    assert!(rig.undo(&id).await.unwrap_err().contains("waits for the human's approval"));
    // A restart files it again under its id.
    assert_eq!(undo::refile_in(&rig.ctx().window, &rig.caller), 1);
    assert_eq!(rig.last_card().0.keep.map(|k| k.0), Some(d.id.clone()));

    let v = approval::approved(&rig.ctx(), &rig.caller, &d, &opts()).await.answer().unwrap();
    assert_eq!(v["state"], undo::STATE_DONE, "{v}");
    assert!(git(rig.repo("web"), &["ls-tree", "--name-only", "-r", "refs/heads/main"]).lines().all(|l| l != ".claude/settings.json"));
    let u = rig.log().last().cloned().unwrap();
    assert_eq!((u.by.as_str(), u.request.as_deref()), ("human", Some(d.id.as_str())));
    assert!(rig.seen.lock().unwrap().settled.iter().any(|(i, ok, _)| *i == d.id && *ok));
    assert!(rig.told().iter().any(|t| t.starts_with("⟦HIVE UNDO · Title hook⟧ The human approved undoing")), "{:?}", rig.told());

    // The human's own Undo: no card, but only on the config files its confirmation showed.
    let w2 = rig.reported("mcp", &["web"], ".mcp.json", "{}\n");
    assert_eq!(rig.land(&w2).await.unwrap()["state"], STATE_AWAITING_HUMAN);
    let (_, d2) = rig.last_card();
    approval::approved(&rig.ctx(), &rig.caller, &d2, &opts()).await.answer().unwrap();
    let id2 = rig.log().iter().rev().find(|e| e.kind == "land").unwrap().id.clone();
    let p = undo::preview(&rig.caller.hive, &rig.caller.paths, &id2).unwrap();
    assert_eq!((p.config_files.clone(), p.method.as_str(), p.refusal.clone()), (vec![".mcp.json".to_string()], "fast-forward", None));
    assert_eq!((p.commits.len(), p.commits[0].subject.as_str()), (1, "edit .mcp.json"));
    let filed = rig.seen.lock().unwrap().filed.len();
    let e = undo::human_undo(&rig.ctx(), &rig.caller, &id2, None, &opts()).await.unwrap_err();
    assert!(e.contains("your confirmation didn't show"), "{e}");
    let v = undo::human_undo(&rig.ctx(), &rig.caller, &id2, p.config_fp.clone(), &opts()).await.unwrap();
    assert_eq!(v["state"], undo::STATE_DONE, "{v}");
    assert_eq!(rig.seen.lock().unwrap().filed.len(), filed, "the human's Undo never files a card");
    assert_eq!(rig.log().last().unwrap().by, "human");
    assert!(rig.told().iter().any(|t| t.contains("The human started undoing")), "{:?}", rig.told());
    rig.assert_no_leftovers();
}

#[tokio::test]
async fn an_integration_land_is_undone_by_the_human_with_revert_m1_and_never_by_the_coordinator() {
    let rig = Rig::with("undo-integration", &["web"], |h| h.landing_mode = LANDING_INTEGRATION.into());
    let repo = rig.repo("web").to_path_buf();
    let rc = rig.caller.paths.coordinator_worktree("web");
    fs::create_dir_all(rc.parent().unwrap()).unwrap();
    git(&repo, &["worktree", "add", "-q", "-b", "hive/ship", rc.to_str().unwrap(), "main"]);
    let w = rig.reported("feat", &["web"], "feat.rs", "f\n");
    rig.land(&w).await.unwrap();
    let (_, d) = rig.last_card();
    approval::approved(&rig.ctx(), &rig.caller, &d, &opts()).await.answer().unwrap();
    let line = rig.log()[0].clone();
    let merge = line.merge_commit.clone().expect("an integration land's own merge commit");

    let e = rig.undo(&line.id).await.unwrap_err();
    assert!(e.contains("Undo is theirs"), "{e}");
    let p = undo::preview(&rig.caller.hive, &rig.caller.paths, &line.id).unwrap();
    assert_eq!((p.method.as_str(), p.target.as_str(), p.refusal.clone()), ("merge", "hive/ship", None));
    assert!(p.commits.iter().any(|c| c.subject == "edit feat.rs"), "{:?}", p.commits);
    let v = undo::human_undo(&rig.ctx(), &rig.caller, &line.id, p.config_fp.clone(), &opts()).await.unwrap();
    assert_eq!(v["state"], undo::STATE_DONE, "{v}");
    let tip = git(&repo, &["rev-parse", "refs/heads/hive/ship"]);
    assert_eq!(git(&repo, &["rev-parse", &format!("{tip}^")]), merge, "a revert on top of the merge");
    assert!(git(&repo, &["ls-tree", "--name-only", &tip, "feat.rs"]).is_empty());
    assert_eq!(git(&rc, &["rev-parse", "HEAD"]), tip, "the read copy on hive/ship moved with it");
    assert!(git(&repo, &["log", "-1", "--format=%s", &tip]).starts_with(&format!("Revert land {} (Title feat) [maiTerm U-", line.id)));
    rig.assert_no_leftovers();
}

#[tokio::test]
async fn reconcile_settles_an_undo_caught_in_step_2() {
    let rig = Rig::new("undo-reconcile", &["web"]);
    git(rig.repo("web"), &["checkout", "-q", "--detach"]);
    let w = rig.reported("crash", &["web"], "c.rs", "c\n");
    let id = Rig::line_of(&rig.land(&w).await.unwrap(), "web");
    let e = rig.log()[0].clone();
    // The revert made and moved onto main, then maiTerm died before its bookkeeping.
    let tip = rig.main_tip("web");
    let revert = git(rig.repo("web"), &["commit-tree", &format!("{}^{{tree}}", e.base_before), "-p", &tip, "-m", "revert"]);
    git(rig.repo("web"), &["update-ref", "refs/heads/main", &revert]);
    let mut u = undo::UndoState {
        id: "U-0000000c".into(),
        undoes: id.clone(),
        land: e.land.clone(),
        folder: "web".into(),
        target: "main".into(),
        task_id: w.task_id.clone(),
        worker: w.slug.clone(),
        title: w.title.clone(),
        by: "coordinator".into(),
        reason: "r".into(),
        state: STATE_ADVANCING.into(),
        tip: Some(tip.clone()),
        revert: Some(revert.clone()),
        ..undo::UndoState::default()
    };
    let p = rig.caller.paths.undos().join("U-0000000c.json");
    fs::create_dir_all(p.parent().unwrap()).unwrap();
    fs::write(&p, serde_json::to_vec(&u).unwrap()).unwrap();
    // And one caught before its step 2: it holds.
    u.id = "U-0000000d".into();
    u.state = STATE_CHECKING.into();
    u.undoes = "L-99999999/web".into();
    fs::write(rig.caller.paths.undos().join("U-0000000d.json"), serde_json::to_vec(&u).unwrap()).unwrap();

    assert_eq!(undo::reconcile_in(&rig.caller.hive, &rig.caller.paths), 2);
    assert_eq!(undo::read_one(&rig.caller.paths, "U-0000000c").unwrap().state, undo::STATE_DONE);
    assert_eq!(undo::read_one(&rig.caller.paths, "U-0000000d").unwrap().state, STATE_HELD);
    let last = rig.log().last().cloned().unwrap();
    assert_eq!((last.kind.as_str(), last.base_after.as_str()), ("undo", revert.as_str()));
    assert!(rig.record(&w.task_id).undone);
    assert_eq!(undo::reconcile_in(&rig.caller.hive, &rig.caller.paths), 0, "settled once");
}

#[tokio::test]
async fn the_humans_retry_resumes_a_held_land_with_its_own_commits() {
    let rig = Rig::new("retry", &["web"]);
    let w = rig.reported("held", &["web"], "shared.txt", "one\nTWO\nthree\n");
    fs::write(rig.repo("web").join("shared.txt"), "the human's\n").unwrap();
    assert_eq!(rig.land(&w).await.unwrap()["state"], STATE_HELD);
    assert!(retry(&rig.ctx(), &rig.caller, "t-nobody", opts()).await.is_err());
    git(rig.repo("web"), &["checkout", "--", "shared.txt"]);
    let v = retry(&rig.ctx(), &rig.caller, &w.task_id, opts()).await.unwrap();
    assert_eq!(v["state"], STATE_LANDED, "{v}");
    assert!(rig.told().iter().any(|t| t.starts_with("⟦HIVE LAND · Title held⟧ The human retried worker #1 held's held land L-")), "{:?}", rig.told());
    assert!(retry(&rig.ctx(), &rig.caller, &w.task_id, opts()).await.unwrap_err().contains("no held land"));
}

// ── The post-land check (§8.6) ──

#[cfg(unix)]
#[tokio::test]
async fn a_base_that_fails_alone_makes_the_lands_since_it_passed_suspect_until_a_fix_passes() {
    let rig = Rig::new("suspect", &["web"]);
    let checked = {
        let mut c = rig.caller.clone();
        c.hive.folders[0].land_check = Some("test ! -f broken.txt || { echo 'FAIL: broken'; exit 1; }".into());
        c
    };
    let checker = Arc::new(ShChecker::default());
    let o = Opts { checker: checker.clone(), ..opts() };
    let land = |w: &WorkerRecord, extra: Value| {
        let mut args = json!({ "worker": w.slug, "summary": "s", "commits": rig.tips(w) });
        for (k, v) in extra.as_object().unwrap() {
            args[k] = v.clone();
        }
        let (ctx, c, o) = (rig.ctx(), checked.clone(), o.clone());
        async move { land_tool_with(&ctx, &c, &args, o).await }
    };
    // Landed before the folder had a land check: nothing proved what it left.
    let w1 = rig.reported("breaks", &["web"], "broken.txt", "x\n");
    let id1 = Rig::line_of(&rig.land(&w1).await.unwrap(), "web");

    // An innocent land's check fails on the merge; Base alone fails too: the land since Base last
    // passed is suspect, and so is the repo.
    let w2 = rig.reported("innocent", &["web"], "ok.rs", "ok\n");
    let e = land(&w2, json!({})).await.unwrap_err();
    assert!(e.contains("land check failed in web"), "{e}");
    let folded = log::folded(&rig.caller.paths);
    assert_eq!(folded.repos["web"].suspects, vec![id1.clone()], "{folded:?}");
    assert!(folded.get(&id1).unwrap().suspect);
    let told = rig.told().join("\n");
    assert!(told.contains(&format!("The lands since it last passed are suspect: {id1} (breaks, “Title breaks”)")) && told.contains("FAIL: broken"), "{told}");
    let board = crate::hive::tools::board_view(&checked.hive, &rig.caller.paths, rig.fuses, crate::hive::fuses::now_ms(), false);
    assert_eq!(board.suspect_repos, vec!["web".to_string()]);
    assert!(board.workers.iter().any(|c| c.task_id == w1.task_id && c.suspect));

    // Every land there is held but the fix.
    let fix = rig.reported("fix", &["web"], "fix.rs", "fix\n");
    let wt = PathBuf::from(fix.folders[0].worktree.clone().unwrap());
    fs::remove_file(wt.join("broken.txt")).unwrap();
    git(&wt, &["add", "-A"]);
    git(&wt, &["commit", "-q", "--no-verify", "-m", "remove broken.txt"]);
    let e = land(&fix, json!({})).await.unwrap_err();
    assert!(e.contains("web is suspect") && e.contains("fix_forward: true"), "{e}");
    let out = land(&fix, json!({ "fix_forward": true })).await.unwrap();
    assert_eq!(out["state"], STATE_LANDED, "{out}");
    let folded = log::folded(&rig.caller.paths);
    assert!(!folded.repos["web"].suspect && !folded.get(&id1).unwrap().suspect, "a fix whose check passed on what landed clears it");
    assert_eq!(checker.runs(CHECK_BASE), 1, "only the failure ran Base alone");
    assert_eq!(land(&w2, json!({})).await.unwrap()["state"], STATE_LANDED, "lands go on as before");
    rig.assert_no_leftovers();
}

/// An undo's check is a job under the land profile: the temporary worktree and its temp folder,
/// no worker's folders, the repo's config denied, its own cargo target.
#[cfg(target_os = "macos")]
#[tokio::test]
async fn an_undo_check_runs_under_the_land_profile_and_its_end_lands_the_revert() {
    struct Capture(Mutex<Vec<crate::hive::queue::JobSpec>>);
    impl Checker for Capture {
        fn start(&self, req: &CheckRequest) -> Result<CheckStart, String> {
            if req.kind == CHECK_LAND {
                return Ok(CheckStart::Done { passed: true, tail: String::new() });
            }
            self.0.lock().unwrap().push(driver::check_spec(req));
            Ok(CheckStart::Queued("j-0000000e".into()))
        }
    }
    let rig = Rig::with("undo-job", &["web"], |h| h.folders[0].land_check = Some("cargo test".into()));
    let cap = Arc::new(Capture(Mutex::new(Vec::new())));
    let o = Opts { checker: cap.clone(), ..opts() };
    let w = rig.reported("job", &["web"], "j.rs", "j\n");
    let id = Rig::line_of(&rig.land_with(&w, json!({}), o.clone()).await.unwrap(), "web");
    let before = rig.main_tip("web");
    let v = undo::undo_tool_with(&rig.ctx(), &rig.caller, &json!({ "land": id, "reason": "r" }), &o).await.unwrap();
    assert_eq!(v["state"], STATE_CHECKING, "{v}");
    assert_eq!(rig.main_tip("web"), before);
    let common = git::common_dir(rig.repo("web")).unwrap();
    assert!(lock::lock_path(&common).exists(), "the lock is held across the check");
    let spec = cap.0.lock().unwrap()[0].clone();
    let run = spec.land.clone().unwrap();
    assert_eq!((run.kind.as_str(), run.profile.as_str(), spec.entry.as_str()), (CHECK_UNDO, "land", "undo check"));
    let job = crate::hive::queue::Queue::default().enqueue(spec, 0).unwrap();
    let bin = rig.base.join("bin");
    fs::create_dir_all(&bin).unwrap();
    let p = crate::hive::queue::prepare_in(&rig.data, &job, &rig.caller.paths, &bin, &[]);
    crate::stack::ports::release(&job.id);
    let p = p.unwrap();
    let tmp = PathBuf::from(&run.worktree);
    let land_tmp = rig.caller.paths.land_tmp(&run.land_id);
    assert!(p.line.starts_with(&format!("cd {} ", crate::hive::launch::sh_quote(&tmp.to_string_lossy()))), "{}", p.line);
    let target_dir = land_tmp.join(crate::hive::queue::LAND_CARGO_TARGET);
    assert!(p.line.contains(&format!("CARGO_TARGET_DIR={}", crate::hive::launch::sh_quote(&target_dir.to_string_lossy()))), "{}", p.line);
    let profile = fs::read_to_string(rig.caller.paths.run(&job.id).join(crate::hive::job_sandbox::PROFILE_FILE)).unwrap();
    let (allow, deny) = profile.split_once("(deny file-write*\n").unwrap();
    let canon = |p: &Path| crate::share::canon(&fs::canonicalize(p).unwrap()).to_string_lossy().to_string();
    assert!(allow.contains(&format!("(subpath \"{}\")", canon(&tmp))), "{profile}");
    assert!(!allow.contains(&canon(&rig.caller.paths.worker_dir(&w.slug))) && !allow.contains("objects"), "no worker's folders: {profile}");
    assert!(deny.contains(&format!("(literal \"{}\")", canon(&common.join("config")))), "{profile}");

    let mut j = crate::hive::queue::Job::default();
    j.id = "j-0000000e".into();
    j.task_id = w.task_id.clone();
    j.state = crate::hive::queue::DONE.into();
    j.began = true;
    j.exit_code = Some(0);
    let out = undo::on_check_end(&rig.ctx(), &rig.caller, &j, &run, "", &o).await.unwrap();
    assert!(matches!(out, driver::Outcome::Landed(_)), "{out:?}");
    assert!(!rig.repo("web").join("j.rs").exists());
    assert_eq!(rig.log().last().unwrap().check, "passed");
    assert!(rig.told().iter().any(|t| t.starts_with("⟦HIVE UNDO · Title job⟧ Land ")), "{:?}", rig.told());
    rig.assert_no_leftovers();
}

// ── Batched land checks (§6.8) ──

#[cfg(unix)]
#[tokio::test]
async fn three_waiting_lands_cost_one_check_and_base_moves_to_their_chain_tip() {
    let rig = Rig::with("batch3", &["web"], |h| h.folders[0].land_check = Some("test -f a.rs && test -f b.rs && test -f c.rs".into()));
    let counter = Arc::new(QueueCounter::default());
    let o = Opts { checker: counter.clone(), ..opts() };
    let ws: Vec<WorkerRecord> = ["a", "b", "c"].iter().map(|s| rig.reported(s, &["web"], &format!("{s}.rs"), "x\n")).collect();
    let pins: Vec<String> = ws.iter().map(|w| rig.tips(w)["web"].as_str().unwrap().to_string()).collect();
    let before = rig.main_tip("web");
    let common = rig.hold_lock("web");
    rig.queue_all(&ws, &o).await;
    assert_eq!(rig.main_tip("web"), before, "nothing lands while the repo is busy");
    lock::release(&common, HOLDER);
    rig.run_batch("web", &o).await;

    let jobs = counter.jobs.lock().unwrap().clone();
    assert_eq!(jobs.len(), 1, "three lands, one queued check");
    let run = jobs[0].land.clone().unwrap();
    assert_eq!((run.kind.as_str(), run.profile.as_str(), jobs[0].needs.clone()), (CHECK_BATCH, "land", vec!["land:web".to_string()]));
    assert_eq!((counter.sh.runs(CHECK_BATCH), counter.sh.runs(CHECK_LAND)), (1, 0));

    let tip = rig.main_tip("web");
    let log = rig.log();
    assert_eq!(log.len(), 3, "{log:?}");
    let mut parent = before.clone();
    for (i, e) in log.iter().enumerate() {
        let merge = e.merge_commit.clone().expect("a member's own merge commit");
        assert_eq!((e.kind.as_str(), e.task_id.as_str(), e.reviewed_sha.as_str()), ("batch_member", ws[i].task_id.as_str(), pins[i].as_str()));
        assert_eq!((e.tested_sha.as_str(), e.base_before.as_str(), e.base_after.as_str(), e.check.as_str()), (tip.as_str(), before.as_str(), tip.as_str(), "passed"));
        assert_eq!(e.tree_tested, e.tree_landed);
        git(rig.repo("web"), &["merge-base", "--is-ancestor", &merge, &tip]);
        assert_eq!(git(rig.repo("web"), &["rev-parse", &format!("{merge}^1")]), parent, "merged in hiveLand order");
        assert_eq!(git(rig.repo("web"), &["rev-parse", &format!("{merge}^2")]), pins[i], "--no-ff of exactly the pinned commit");
        let subject = git(rig.repo("web"), &["log", "-1", "--format=%s", &merge]);
        assert_eq!(subject, format!("Land Title {} ({}) [maiTerm {}]", ws[i].slug, &pins[i][..7], e.land));
        parent = merge;
    }
    assert_eq!(parent, tip, "Base is exactly the chain's tip");
    for (w, pin) in ws.iter().zip(&pins) {
        let r = rig.record(&w.task_id);
        assert_eq!((r.phase.as_str(), r.land.as_ref().unwrap().state.as_str()), (worker::PHASE_LANDED, STATE_LANDED));
        assert_eq!(&git(rig.repo("web"), &["rev-parse", &format!("refs/heads/hive/ship--{}", w.slug)]), pin, "a member's branch isn't moved");
    }
    let told = rig.told().join("\n");
    for w in &ws {
        assert!(told.contains(&format!("⟦HIVE LAND · Title {}⟧ Worker", w.slug)) && told.contains("It landed in batch Q-"), "{told}");
    }
    assert!(log::folded(&rig.caller.paths).suspect_folders().is_empty());
    rig.assert_no_leftovers();
}

#[cfg(unix)]
#[tokio::test]
async fn a_failing_batch_is_split_until_the_bad_land_stands_alone_and_the_others_land() {
    let rig = Rig::with("bisect", &["web"], |h| h.folders[0].land_check = Some("test ! -e bad.txt || { echo 'FAIL: bad.txt'; exit 3; }".into()));
    let checker = Arc::new(ShChecker::default());
    let o = Opts { checker: checker.clone(), ..opts() };
    let a = rig.reported("a", &["web"], "a.rs", "a\n");
    let b = rig.reported("b", &["web"], "bad.txt", "bad\n");
    let c = rig.reported("c", &["web"], "c.rs", "c\n");
    let common = rig.hold_lock("web");
    rig.queue_all(&[a.clone(), b.clone(), c.clone()], &o).await;
    lock::release(&common, HOLDER);
    rig.run_batch("web", &o).await;

    // abc fails; ab fails; a passes; b fails alone; c passes.
    assert_eq!(checker.runs(CHECK_BATCH), 5);
    assert_eq!(checker.runs(CHECK_BASE), 1, "Base alone, once, after the culprit is found");
    for (f, there) in [("a.rs", true), ("c.rs", true), ("bad.txt", false)] {
        assert_eq!(git(rig.repo("web"), &["ls-tree", "--name-only", "refs/heads/main", f]).is_empty(), !there, "{f}");
    }
    assert_eq!((rig.record(&a.task_id).phase.as_str(), rig.record(&c.task_id).phase.as_str()), (worker::PHASE_LANDED, worker::PHASE_LANDED));
    let r = rig.record(&b.task_id);
    assert_eq!(r.phase, worker::PHASE_REPORTED);
    assert!(r.land.is_none());
    let members: Vec<String> = rig.log().iter().filter(|e| e.kind == "batch_member").map(|e| e.worker.clone()).collect();
    assert_eq!(members, vec!["a", "c"]);
    let told = rig.told().join("\n");
    assert!(told.contains("Worker #2 b didn't land") && told.contains("split down to this land alone") && told.contains("FAIL: bad.txt"), "{told}");
    assert!(told.contains("main passes web's land check alone"), "{told}");
    rig.assert_no_leftovers();
}

#[cfg(unix)]
#[tokio::test]
async fn a_conflicting_member_and_a_config_member_leave_the_batch_and_land_alone() {
    let rig = Rig::with("leave", &["web"], |h| h.folders[0].land_check = Some("true".into()));
    let checker = Arc::new(ShChecker::default());
    let o = Opts { checker: checker.clone(), ..opts() };
    let a = rig.reported("a", &["web"], "shared.txt", "one\nA\nthree\n");
    let b = rig.reported("b", &["web"], "shared.txt", "one\nB\nthree\n");
    let c = rig.reported("c", &["web"], ".claude/settings.json", "{}\n");
    let d = rig.reported("d", &["web"], "d.rs", "d\n");
    let common = rig.hold_lock("web");
    rig.queue_all(&[a.clone(), b.clone(), c.clone(), d.clone()], &o).await;
    lock::release(&common, HOLDER);
    rig.run_batch("web", &o).await;

    assert_eq!(checker.runs(CHECK_BATCH), 1, "a and d, checked once");
    let members: Vec<String> = rig.log().iter().filter(|e| e.kind == "batch_member").map(|e| e.worker.clone()).collect();
    assert_eq!(members, vec!["a", "d"]);
    // b, alone, merges the Base a left into its commit: the conflict is its own to resolve.
    let rb = rig.record(&b.task_id);
    assert_eq!(rb.phase, worker::PHASE_REPORTED);
    let told = rig.told().join("\n");
    assert!(told.contains("Worker #2 b didn't land") && told.contains("conflicts in web: `shared.txt`"), "{told}");
    // c, alone, runs its own check and goes to the human's card.
    assert_eq!(checker.runs(CHECK_LAND), 1);
    let rc = rig.record(&c.task_id);
    assert_eq!((rc.phase.as_str(), rc.land.as_ref().unwrap().state.as_str()), (worker::PHASE_LANDING, STATE_AWAITING_HUMAN));
    let filed = rig.seen.lock().unwrap().filed.clone();
    assert_eq!(filed.len(), 1);
    assert!(filed[0].effects.iter().any(|e| e.contains(".claude/settings.json")), "{:?}", filed[0].effects);
    assert!(!git(rig.repo("web"), &["ls-tree", "--name-only", "-r", "refs/heads/main"]).contains(".claude"), "config never lands without the human");
    rig.assert_no_leftovers();
}

#[cfg(unix)]
#[tokio::test]
async fn base_moving_before_the_fast_forward_rebuilds_the_batch_then_holds_and_a_held_member_resumes_alone() {
    let rig = Rig::with("batchmoved", &["web"], |h| h.folders[0].land_check = Some("true".into()));
    let checker = Arc::new(ShChecker::default());
    let repo = rig.repo("web").to_path_buf();
    let side = rig.base.join("side");
    git(&repo, &["checkout", "-q", "--detach"]);
    git(&repo, &["worktree", "add", "-q", side.to_str().unwrap(), "main"]);
    let a = rig.reported("a", &["web"], "a.rs", "a\n");
    let b = rig.reported("b", &["web"], "b.rs", "b\n");
    let (s, n) = (side.clone(), Arc::new(Mutex::new(0)));
    let once = n.clone();
    let o = Opts {
        checker: checker.clone(),
        before_advance: Some(Arc::new(move |_f: &str| {
            let mut n = n.lock().unwrap();
            if *n == 0 {
                commit(&s, "side-0.txt", "side\n");
            }
            *n += 1;
        })),
        ..opts()
    };
    let common = rig.hold_lock("web");
    rig.queue_all(&[a.clone(), b.clone()], &o).await;
    lock::release(&common, HOLDER);
    rig.run_batch("web", &o).await;
    assert_eq!((*once.lock().unwrap(), checker.runs(CHECK_BATCH)), (2, 2), "rebuilt and checked again once");
    let moved = git(&repo, &["log", "-1", "--format=%H", "refs/heads/main", "--", "side-0.txt"]);
    let log = rig.log();
    assert!(log.iter().all(|e| e.kind == "batch_member" && e.base_before == moved), "rebuilt from the moved Base: {log:?}");
    for f in ["side-0.txt", "a.rs", "b.rs"] {
        assert!(!git(&repo, &["ls-tree", "--name-only", "refs/heads/main", f]).is_empty(), "{f}");
    }

    // Base moving every time: three tries, then both held, their chain forgotten.
    let c = rig.reported("c", &["web"], "c.rs", "c\n");
    let d = rig.reported("d", &["web"], "d.rs", "d\n");
    let (s, n) = (side.clone(), Arc::new(Mutex::new(0)));
    let tries = n.clone();
    let o2 = Opts {
        checker: checker.clone(),
        before_advance: Some(Arc::new(move |_f: &str| {
            let mut n = n.lock().unwrap();
            git(&s, &["reset", "-q", "--hard", "main"]);
            commit(&s, &format!("again-{}.txt", *n), "x\n");
            *n += 1;
        })),
        ..opts()
    };
    let common = rig.hold_lock("web");
    rig.queue_all(&[c.clone(), d.clone()], &o2).await;
    lock::release(&common, HOLDER);
    rig.run_batch("web", &o2).await;
    assert_eq!((*tries.lock().unwrap(), checker.runs(CHECK_BATCH)), (3, 5));
    for w in [&c, &d] {
        let l = rig.record(&w.task_id).land.unwrap();
        assert!(l.is_held() && l.batch.is_none(), "{l:?}");
        assert!(l.held_reason.as_deref().unwrap().contains("main keeps moving (3 tries)"), "{l:?}");
        assert!(l.repos[0].tested.is_none() && l.repos[0].chain_tip.is_none(), "a held member never lands its chain merge alone: {l:?}");
    }
    // The repo free, hiveLand again lands it alone, from its pin.
    let out = rig.land_with(&c, json!({}), Opts { checker: checker.clone(), ..opts() }).await.unwrap();
    assert_eq!(out["state"], STATE_LANDED, "{out}");
    assert_eq!(rig.log().last().unwrap().kind, "land");
    assert!(git(&repo, &["ls-tree", "--name-only", "refs/heads/main", "d.rs"]).is_empty(), "only c's work");
    rig.assert_no_leftovers();
}

#[cfg(unix)]
#[tokio::test]
async fn a_multi_repo_worker_never_batches_nor_does_a_land_that_must_stand_alone() {
    let rig = Rig::with("batchmulti", &["web", "api"], |h| {
        h.folders[0].land_check = Some("true".into());
        h.folders[1].land_check = Some("true".into());
    });
    let both = rig.reported("both", &["web", "api"], "b.rs", "b\n");
    let common = rig.hold_lock("web");
    let o = Opts { lock_wait: Duration::from_millis(300), checker: Arc::new(ShChecker::default()), ..opts() };
    let out = rig.land_with(&both, json!({}), o).await.unwrap();
    assert_eq!(out["state"], STATE_LOCKING, "it waits for the lock, alone: {out}");
    lock::release(&common, HOLDER);

    let one = rig.reported("one", &["web"], "o.rs", "o\n");
    let land = LandState { repos: vec![LandRepo { folder: "web".into(), status: REPO_PENDING.into(), ..LandRepo::default() }], ..LandState::default() };
    let hive = rig.caller.hive.clone();
    assert!(batch::eligible(&hive, &one, &land));
    assert!(!batch::eligible(&hive, &both, &land), "two repos");
    let not = |edit: &dyn Fn(&mut Hive, &mut LandState)| {
        let (mut h, mut l) = (hive.clone(), land.clone());
        edit(&mut h, &mut l);
        !batch::eligible(&h, &one, &l)
    };
    assert!(not(&|h, _| h.settings.land.batch = false), "batching off");
    assert!(not(&|h, _| h.folders[0].land_check = None), "no land check to share");
    assert!(not(&|h, _| h.landing_mode = LANDING_INTEGRATION.into()), "the approval mode never batches");
    assert!(not(&|h, _| {
        h.checkpoints.insert("landing".into(), "ask".into());
    }), "Landing = Ask me");
    assert!(not(&|_, l| l.fix_forward = true), "a fix forward");
    assert!(not(&|_, l| l.approved_by = Some("desktop".into())), "an approved land");
    assert!(not(&|_, l| l.repos[0].status = REPO_TESTED.into()), "its step 1 started");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_queued_batch_check_is_awaited_and_a_member_is_undone_by_reverting_its_merge() {
    struct Queued;
    impl Checker for Queued {
        fn start(&self, req: &CheckRequest) -> Result<CheckStart, String> {
            if req.kind != CHECK_BATCH {
                return Ok(CheckStart::Done { passed: true, tail: String::new() });
            }
            let id = format!("j-{}", &uuid::Uuid::new_v4().simple().to_string()[..8]);
            let (job_id, batch_id, task) = (id.clone(), req.land_id.clone(), req.worker.task_id.clone());
            std::thread::spawn(move || {
                std::thread::sleep(Duration::from_millis(100));
                let mut j = crate::hive::queue::Job::default();
                j.id = job_id;
                j.task_id = task;
                j.state = crate::hive::queue::DONE.into();
                j.began = true;
                j.exit_code = Some(0);
                j.land = Some(crate::hive::queue::LandRun { land_id: batch_id, kind: CHECK_BATCH.into(), ..Default::default() });
                batch::on_check_end(&j, "");
            });
            Ok(CheckStart::Queued(id))
        }
    }
    let rig = Rig::with("batchqueued", &["web"], |h| h.folders[0].land_check = Some("cargo test".into()));
    let o = Opts { checker: Arc::new(Queued), ..opts() };
    let a = rig.reported("a", &["web"], "a.rs", "a\n");
    let b = rig.reported("b", &["web"], "b.rs", "b\n");
    let common = rig.hold_lock("web");
    rig.queue_all(&[a.clone(), b.clone()], &o).await;
    lock::release(&common, HOLDER);
    rig.run_batch("web", &o).await;
    assert_eq!((rig.record(&a.task_id).phase.as_str(), rig.record(&b.task_id).phase.as_str()), (worker::PHASE_LANDED, worker::PHASE_LANDED));

    // Undo of a member reverts its own merge (`revert -m 1`), leaving the other's work.
    let line = rig.log().into_iter().find(|e| e.worker == "a").unwrap().id;
    let v = undo::undo_tool_with(&rig.ctx(), &rig.caller, &json!({ "land": line, "reason": "Broke it." }), &o).await.unwrap();
    assert_eq!(v["state"], undo::STATE_DONE, "{v}");
    let main = git(rig.repo("web"), &["ls-tree", "--name-only", "refs/heads/main"]);
    assert!(!main.contains("a.rs") && main.contains("b.rs"), "{main}");
    rig.assert_no_leftovers();
}

#[cfg(unix)]
#[tokio::test]
async fn a_restart_holds_queued_and_mid_batch_members_and_never_reuses_a_chain_merge() {
    let rig = Rig::with("batchrestart", &["web"], |h| h.folders[0].land_check = Some("true".into()));
    let o = Opts { checker: Arc::new(ShChecker::default()), ..opts() };
    let a = rig.reported("a", &["web"], "a.rs", "a\n");
    let b = rig.reported("b", &["web"], "b.rs", "b\n");
    let common = rig.hold_lock("web");
    rig.queue_all(&[a.clone(), b.clone()], &o).await;
    lock::release(&common, HOLDER);
    // b was in a batch's check when maiTerm stopped.
    let main = rig.main_tip("web");
    worker::update_worker(&rig.caller.paths.task(&b.task_id), |cur| {
        let l = cur.land.as_mut().unwrap();
        l.state = STATE_CHECKING.into();
        l.batch = Some("Q-dead0000".into());
        let r = &mut l.repos[0];
        r.status = REPO_CHECKING.into();
        r.tested = Some(main.clone());
        r.chain_tip = Some(main.clone());
        r.base_before = Some(main.clone());
        r.job = Some("j-dead0000".into());
        true
    })
    .unwrap();
    // Its chain's temporary worktree, left behind.
    let chain = rig.caller.paths.land_worktree("Q-dead0000", "web");
    fs::create_dir_all(chain.parent().unwrap()).unwrap();
    git(rig.repo("web"), &["worktree", "add", "-q", "--detach", chain.to_str().unwrap(), &main]);

    assert_eq!(reconcile_in(&rig.caller.hive, &rig.caller.paths), 2);
    batch::sweep(&rig.caller.hive, &rig.caller.paths);
    let la = rig.record(&a.task_id).land.unwrap();
    assert!(la.is_held() && la.held_reason.as_deref().unwrap().contains("interrupted by a maiTerm restart"), "{la:?}");
    let lb = rig.record(&b.task_id).land.unwrap();
    assert!(lb.is_held() && lb.batch.is_none(), "{lb:?}");
    assert!(lb.repos[0].tested.is_none() && lb.repos[0].chain_tip.is_none() && lb.repos[0].status == REPO_PENDING, "{lb:?}");

    // Resumed with the repo free, each lands alone from its pin.
    for w in [&a, &b] {
        let out = rig.land_with(w, json!({}), o.clone()).await.unwrap();
        assert_eq!(out["state"], STATE_LANDED, "{out}");
    }
    assert!(rig.log().iter().all(|e| e.kind == "land"));
    rig.assert_no_leftovers();
}

/// A restart between a batch's fast-forward and its members' records: a merge in the target is
/// logged as `land_members` would log it (its check passed on the tree that landed), and one not in
/// it goes back to waiting from its pin.
#[cfg(unix)]
#[tokio::test]
async fn a_restart_mid_advance_logs_a_landed_member_as_checked_and_detaches_one_that_didnt_land() {
    let rig = Rig::with("batchadv", &["web"], |h| h.folders[0].land_check = Some("true".into()));
    let checker = Arc::new(ShChecker::default());
    let repo = rig.repo("web").to_path_buf();
    // The restart, just before step 2: Base already at the chain's tip (the fast-forward done), or
    // moved by someone else (it wasn't). Reconcile's log lines and records are read there.
    let restart = |ff: bool| -> (Opts, Arc<Mutex<Vec<log::LandLogEntry>>>) {
        let (hive, paths, r) = (rig.caller.hive.clone(), rig.caller.paths.clone(), repo.clone());
        let seen = Arc::new(Mutex::new(Vec::new()));
        let s = seen.clone();
        let o = Opts {
            checker: checker.clone(),
            before_advance: Some(Arc::new(move |_f: &str| {
                let tip = worker::list_workers(&paths)
                    .into_iter()
                    .filter_map(|w| w.land.filter(|l| l.state == STATE_ADVANCING).and_then(|l| l.repos[0].chain_tip.clone()))
                    .next()
                    .expect("a member advancing");
                if ff {
                    git(&r, &["merge", "--ff-only", "-q", &tip]);
                } else {
                    commit(&r, "other.txt", "x\n");
                }
                reconcile_in(&hive, &paths);
                *s.lock().unwrap() = log::read(&paths);
            })),
            ..opts()
        };
        (o, seen)
    };

    let a = rig.reported("a", &["web"], "a.rs", "a\n");
    let b = rig.reported("b", &["web"], "b.rs", "b\n");
    let before = rig.main_tip("web");
    let (o, seen) = restart(true);
    let common = rig.hold_lock("web");
    rig.queue_all(&[a.clone(), b.clone()], &o).await;
    lock::release(&common, HOLDER);
    rig.run_batch("web", &o).await;
    let tip = rig.main_tip("web");
    let lines = seen.lock().unwrap().clone();
    assert_eq!(lines.len(), 2, "{lines:?}");
    for (e, w) in lines.iter().zip([&a, &b]) {
        assert_eq!((e.kind.as_str(), e.task_id.as_str(), e.check.as_str()), ("batch_member", w.task_id.as_str(), "passed"), "{e:?}");
        assert_eq!((e.base_before.as_str(), e.base_after.as_str(), e.tested_sha.as_str()), (before.as_str(), tip.as_str(), tip.as_str()));
        assert!(!e.tree_tested.is_empty() && e.tree_tested == e.tree_landed, "{e:?}");
    }
    assert!(log::folded(&rig.caller.paths).suspect_folders().is_empty());
    for w in [&a, &b] {
        let l = rig.record(&w.task_id).land.unwrap();
        assert!(l.is_held() && l.repos[0].status == REPO_LANDED, "{l:?}");
        let again = Opts { checker: checker.clone(), ..opts() };
        assert_eq!(rig.land_with(w, json!({}), again).await.unwrap()["state"], STATE_LANDED, "hiveLand again retires it");
    }
    assert_eq!(rig.log().len(), 2, "logged once");

    let c = rig.reported("c", &["web"], "c.rs", "c\n");
    let d = rig.reported("d", &["web"], "d.rs", "d\n");
    let (o, seen) = restart(false);
    let common = rig.hold_lock("web");
    rig.queue_all(&[c.clone(), d.clone()], &o).await;
    lock::release(&common, HOLDER);
    rig.run_batch("web", &o).await;
    assert_eq!(seen.lock().unwrap().len(), 2, "nothing logged for merges not in the target");
    for w in [&c, &d] {
        let l = rig.record(&w.task_id).land.unwrap();
        assert!(l.is_held() && l.batch.is_none(), "{l:?}");
        let r = &l.repos[0];
        assert!(r.status == REPO_PENDING && r.tested.is_none() && r.chain_tip.is_none() && !r.checked, "{l:?}");
    }
    rig.assert_no_leftovers();
}

/// A member that left its batch lands alone once, however often Base moved under the batch: a
/// second drive would detach the first one's queued check and remake its temporary worktree.
#[cfg(unix)]
#[tokio::test]
async fn a_member_that_left_its_batch_lands_alone_once_however_often_the_batch_is_rebuilt() {
    struct QueueLand(Mutex<Vec<String>>);
    impl Checker for QueueLand {
        fn start(&self, req: &CheckRequest) -> Result<CheckStart, String> {
            if req.kind != CHECK_LAND {
                return Ok(CheckStart::Done { passed: true, tail: String::new() });
            }
            let id = format!("j-{}", &uuid::Uuid::new_v4().simple().to_string()[..8]);
            self.0.lock().unwrap().push(id.clone());
            Ok(CheckStart::Queued(id))
        }
    }
    let rig = Rig::with("batchonce", &["web"], |h| h.folders[0].land_check = Some("true".into()));
    let repo = rig.repo("web").to_path_buf();
    let side = rig.base.join("side");
    git(&repo, &["checkout", "-q", "--detach"]);
    git(&repo, &["worktree", "add", "-q", side.to_str().unwrap(), "main"]);
    let a = rig.reported("a", &["web"], "shared.txt", "one\nA\nthree\n");
    let b = rig.reported("b", &["web"], "shared.txt", "one\nB\nthree\n");
    let c = rig.reported("c", &["web"], "c.rs", "c\n");
    let checker = Arc::new(QueueLand(Mutex::new(Vec::new())));
    let (s, n) = (side.clone(), Arc::new(Mutex::new(0)));
    let o = Opts {
        checker: checker.clone(),
        before_advance: Some(Arc::new(move |_f: &str| {
            let mut n = n.lock().unwrap();
            git(&s, &["reset", "-q", "--hard", "main"]);
            commit(&s, &format!("again-{}.txt", *n), "x\n");
            *n += 1;
        })),
        ..opts()
    };
    let common = rig.hold_lock("web");
    rig.queue_all(&[a.clone(), b.clone(), c.clone()], &o).await;
    lock::release(&common, HOLDER);
    rig.run_batch("web", &o).await;

    for w in [&a, &c] {
        assert!(rig.record(&w.task_id).land.unwrap().held_reason.unwrap().contains("keeps moving"));
    }
    let jobs = checker.0.lock().unwrap().clone();
    assert_eq!(jobs.len(), 1, "b's own check queued once: {jobs:?}");
    let lb = rig.record(&b.task_id).land.unwrap();
    assert_eq!((lb.state.as_str(), lb.repos[0].job.as_deref()), (STATE_CHECKING, Some(jobs[0].as_str())), "{lb:?}");
    let tmp = lb.repos[0].tmp.clone().expect("its temporary worktree");
    assert!(Path::new(&tmp).exists());
    let told = rig.told().join("\n");
    assert_eq!(told.matches("⟦HIVE LAND · Title b⟧").count(), 0, "a running check says nothing yet: {told}");
}

// ── The decision log (step 7d) ──

fn decisions_of(rig: &Rig) -> Vec<crate::hive::decisions::Decision> {
    crate::hive::decisions::fold(&crate::hive::decisions::read(&rig.caller.paths))
}

fn precedent(rig: &Rig, id: &str) {
    let p = crate::hive::precedents::Precedent { id: id.into(), category: "landing".into(), ..Default::default() };
    crate::hive::precedents::save_in(&rig.caller.paths.precedents(), &p).unwrap();
}

#[tokio::test]
async fn the_coordinators_land_is_its_decision_with_the_outcome_and_the_citation_counted() {
    let rig = Rig::new("decided", &["web"]);
    let w = rig.reported("touch", &["web"], "shared.txt", "one\nTWO\nthree\n");
    // An unknown precedent is refused before anything is written.
    let e = rig.land_with(&w, json!({ "precedent": "P-00000bad" }), opts()).await.unwrap_err();
    assert!(e.contains("no precedent P-00000bad"), "{e}");
    assert_eq!(rig.record(&w.task_id).phase, worker::PHASE_REPORTED);
    assert!(decisions_of(&rig).is_empty());

    precedent(&rig, "P-0000c001");
    fs::write(rig.repo("web").join("shared.txt"), "the human's\n").unwrap();
    let out = rig.land_with(&w, json!({ "precedent": "P-0000c001" }), opts()).await.unwrap();
    assert_eq!(out["state"], STATE_HELD, "{out}");
    let land = rig.record(&w.task_id).land.unwrap();
    let ds = decisions_of(&rig);
    assert_eq!(ds.len(), 1);
    let a = ds[0].answered.clone().unwrap();
    assert_eq!((a.by.as_str(), a.category.as_str(), a.source.as_str(), a.key.as_deref()), ("coordinator", "landing", "land", Some(land.id.as_str())));
    assert_eq!((a.answer.as_deref(), a.precedent.as_deref(), a.task_id.as_deref()), (Some("Land"), Some("P-0000c001"), Some(w.task_id.as_str())));
    assert!(a.question.contains("Title touch") && a.question.contains("(web)"), "{}", a.question);
    assert_eq!(land.decision.as_deref(), Some(ds[0].id.as_str()));
    assert!(ds[0].outcome.as_deref().is_some_and(|o| o.starts_with("held: ") && o.contains("shared.txt")), "{:?}", ds[0].outcome);
    assert_eq!(crate::hive::precedents::get(&rig.caller.paths, "P-0000c001").unwrap().cited, 1);

    // Resumed with the same commits: the same decision, its outcome now landed, cited once.
    fs::write(rig.repo("web").join("shared.txt"), "one\ntwo\nthree\n").unwrap();
    assert_eq!(rig.land_with(&w, json!({ "precedent": "P-0000c001" }), opts()).await.unwrap()["state"], STATE_LANDED);
    let ds = decisions_of(&rig);
    assert_eq!(ds.len(), 1, "one decision per land");
    assert_eq!(ds[0].outcome.as_deref(), Some("landed"));
    assert_eq!(crate::hive::precedents::get(&rig.caller.paths, "P-0000c001").unwrap().cited, 1);
    let v = crate::hive::decisions::view(&rig.caller.paths);
    assert_eq!((v[0].by_label.as_str(), v[0].can_override), ("coordinator \u{b7} by precedent", true));
}

#[tokio::test]
async fn land_cards_are_decisions_categorized_by_why_and_a_lapse_isnt_a_denial() {
    // Landing = Ask me: the human's decision, the coordinator recommending the land.
    let rig = Rig::with("card-ask", &["web"], |h| {
        h.checkpoints.insert("landing".into(), "ask".into());
    });
    let w = rig.reported("asked", &["web"], "a.rs", "a\n");
    rig.land(&w).await.unwrap();
    let ds = decisions_of(&rig);
    assert_eq!(ds.len(), 1, "no decision of the coordinator's own");
    let asked = ds[0].asked.clone().unwrap();
    assert_eq!((asked.category.as_str(), asked.recommended.as_deref(), asked.question.as_str()), ("landing", Some("Land"), "Land \u{201c}Title asked\u{201d} into main in web"));
    assert!(asked.context.as_deref().is_some_and(|c| c.contains("Ask me")), "{:?}", asked.context);
    let (_, d) = rig.last_card();
    approval::approved(&rig.ctx(), &rig.caller, &d, &opts()).await.answer().unwrap();
    let ds = decisions_of(&rig);
    let a = ds[0].answered.clone().unwrap();
    assert_eq!((a.by.as_str(), a.answer.as_deref(), a.recommended.as_deref(), a.category.as_str()), ("human", Some("Land"), Some("Land"), "landing"));
    assert_eq!(ds[0].outcome.as_deref(), Some("landed"));
    assert!(!ds[0].can_override(), "the human's own");

    // The config rule on the coordinator's own land: its decision waits, the card is `config`.
    let rig = Rig::new("card-config", &["web"]);
    let w = rig.reported("hook", &["web"], ".claude/settings.json", "{}\n");
    rig.land(&w).await.unwrap();
    let land = rig.record(&w.task_id).land.unwrap();
    let ds = decisions_of(&rig);
    assert_eq!(ds.len(), 2);
    let coord = ds.iter().find(|d| Some(&d.id) == land.decision.as_ref()).unwrap();
    let card = ds.iter().find(|d| Some(&d.id) == land.card_decision.as_ref()).unwrap();
    assert_eq!((coord.category(), coord.outcome.as_deref()), ("landing", Some("waiting for the human's approval (config)")));
    assert_eq!(card.category(), "config");
    let (_, d) = rig.last_card();
    approval::closed(&rig.ctx(), &rig.caller, &d, "denied").await.unwrap();
    let ds = decisions_of(&rig);
    let card = ds.iter().find(|x| Some(&x.id) == land.card_decision.as_ref()).unwrap();
    let coord = ds.iter().find(|x| Some(&x.id) == land.decision.as_ref()).unwrap();
    assert_eq!((card.answered_by(), card.answered.as_ref().unwrap().answer.as_deref()), (Some("human"), Some("Don\u{2019}t land")));
    for x in [card, coord] {
        assert!(x.outcome.as_deref().is_some_and(|o| o.starts_with("refused: The human denied it")), "{:?}", x.outcome);
    }

    // A card nobody answered lapses: no answer is logged.
    let w2 = rig.reported("late", &["web"], "AGENTS.md", "x\n");
    rig.land(&w2).await.unwrap();
    let card_id = rig.record(&w2.task_id).land.unwrap().card_decision.unwrap();
    let (_, d2) = rig.last_card();
    approval::closed(&rig.ctx(), &rig.caller, &d2, "expired").await.unwrap();
    let card = decisions_of(&rig).into_iter().find(|x| x.id == card_id).unwrap();
    assert!(card.answered.is_none() && card.lapsed.as_ref().is_some_and(|l| l.by == "maiterm"), "{card:?}");

    // The integration mode's cards are `integration`, never earned.
    let rig = Rig::with("card-integration", &["web"], |h| h.landing_mode = LANDING_INTEGRATION.into());
    let rc = rig.caller.paths.coordinator_worktree("web");
    fs::create_dir_all(rc.parent().unwrap()).unwrap();
    git(rig.repo("web"), &["worktree", "add", "-q", "-b", "hive/ship", rc.to_str().unwrap(), "main"]);
    let w = rig.reported("feat", &["web"], "feat.rs", "f\n");
    rig.land(&w).await.unwrap();
    assert_eq!(decisions_of(&rig)[0].category(), "integration");
}

#[tokio::test]
async fn the_humans_undo_of_the_coordinators_land_overrides_it_and_the_coordinator_hears() {
    let rig = Rig::new("undo-override", &["web"]);
    let w = rig.reported("feat", &["web"], "feat.rs", "fn feat() {}\n");
    precedent(&rig, "P-0000d001");
    let id = Rig::line_of(&rig.land_with(&w, json!({ "precedent": "P-0000d001" }), opts()).await.unwrap(), "web");
    let p = undo::preview(&rig.caller.hive, &rig.caller.paths, &id).unwrap();
    assert_eq!(undo::human_undo(&rig.ctx(), &rig.caller, &id, p.config_fp.clone(), &opts()).await.unwrap()["state"], undo::STATE_DONE);
    let d = decisions_of(&rig).into_iter().next().unwrap();
    let o = d.overridden.clone().expect("overridden");
    assert_eq!((o.by.as_str(), o.reason.as_deref()), ("human", Some("Undone by your human")));
    assert_eq!(crate::hive::precedents::get(&rig.caller.paths, "P-0000d001").unwrap().overridden, 1);
    assert!(rig.told().iter().any(|t| t.starts_with(&format!("[maiTerm] Your human overrode your decision {}: Undone by your human", d.id))), "{:?}", rig.told());

    // The coordinator's own undo is no override.
    let w2 = rig.reported("again", &["web"], "again.rs", "a\n");
    let id2 = Rig::line_of(&rig.land(&w2).await.unwrap(), "web");
    rig.undo(&id2).await.unwrap();
    let key = rig.record(&w2.task_id).land.unwrap().decision.unwrap();
    assert!(decisions_of(&rig).into_iter().find(|x| x.id == key).unwrap().overridden.is_none());
}

#[tokio::test]
async fn the_humans_undo_overrides_the_decision_that_landed_that_repo_when_a_resume_made_a_second() {
    let rig = Rig::new("undo-two-decisions", &["web", "api"]);
    let w = rig.reported("split", &["web", "api"], "shared.txt", "one\nworker\nthree\n");
    precedent(&rig, "P-0000e001");
    precedent(&rig, "P-0000e002");
    // The human's edit in api's checkout is in the way: web lands under D1, api holds.
    fs::write(rig.repo("api").join("shared.txt"), "one\nhuman\nthree\n").unwrap();
    let out = rig.land_with(&w, json!({ "precedent": "P-0000e001" }), opts()).await.unwrap();
    assert_eq!((out["state"].as_str(), out["landed"].clone()), (Some(STATE_HELD), json!(["web"])), "{out}");
    let land = out["land"].as_str().unwrap().to_string();
    let d1 = rig.record(&w.task_id).land.unwrap().decision.unwrap();

    // The worker commits more in api: resumed on its new pin, the same land under a new decision D2.
    git(rig.repo("api"), &["checkout", "-q", "--", "shared.txt"]);
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
    let out = rig.land_with(&w, json!({ "precedent": "P-0000e002" }), opts()).await.unwrap();
    assert_eq!((out["state"].as_str(), out["land"].as_str()), (Some(STATE_LANDED), Some(land.as_str())), "{out}");
    let d2 = rig.record(&w.task_id).land.unwrap().decision.unwrap();
    assert_ne!(d1, d2);
    let log = rig.log();
    let named: Vec<(&str, Option<&str>)> = log.iter().map(|e| (e.folder.as_str(), e.decision.as_deref())).collect();
    assert_eq!(named, vec![("web", Some(d1.as_str())), ("api", Some(d2.as_str()))]);

    // Undoing web overrides D1, the decision that landed web, and counts against its precedent.
    let undo_of = |folder: &str| format!("{land}/{folder}");
    let p = undo::preview(&rig.caller.hive, &rig.caller.paths, &undo_of("web")).unwrap();
    assert_eq!(undo::human_undo(&rig.ctx(), &rig.caller, &undo_of("web"), p.config_fp.clone(), &opts()).await.unwrap()["state"], undo::STATE_DONE);
    let overridden = |id: &str| decisions_of(&rig).into_iter().find(|d| d.id == id).unwrap().overridden.is_some();
    assert!(overridden(&d1) && !overridden(&d2));
    let counted = |id: &str| crate::hive::precedents::get(&rig.caller.paths, id).unwrap().overridden;
    assert_eq!((counted("P-0000e001"), counted("P-0000e002")), (1, 0));
    assert!(rig.told().iter().any(|t| t.starts_with(&format!("[maiTerm] Your human overrode your decision {d1}:"))), "{:?}", rig.told());

    // And undoing api overrides D2.
    let p = undo::preview(&rig.caller.hive, &rig.caller.paths, &undo_of("api")).unwrap();
    assert_eq!(undo::human_undo(&rig.ctx(), &rig.caller, &undo_of("api"), p.config_fp.clone(), &opts()).await.unwrap()["state"], undo::STATE_DONE);
    assert!(overridden(&d2));
    assert_eq!((counted("P-0000e001"), counted("P-0000e002")), (1, 1));
}

#[tokio::test]
async fn a_denied_card_replaced_when_its_held_land_resumes_isnt_said_to_have_been_approved() {
    let rig = Rig::new("card-denied-resumed", &["web", "api"]);
    commit(rig.repo("api"), ".envrc", "export A=1\n\n\n\nexport END=1\n");
    let w = rig.reported("split", &["web", "api"], ".envrc", "export A=2\n\n\n\nexport END=1\n");
    assert_eq!(rig.land(&w).await.unwrap()["state"], STATE_AWAITING_HUMAN);
    let c1 = rig.record(&w.task_id).land.unwrap().card_decision.unwrap();
    let (_, d) = rig.last_card();
    // Approved; web lands, then Base changes api's config file under its step 2: a second card.
    let api = rig.repo("api").to_path_buf();
    let mut o = opts();
    o.before_advance = Some(Arc::new(move |f: &str| {
        if f == "api" {
            commit(&api, ".envrc", "export A=1\n\n\n\nexport END=2\n");
        }
    }));
    assert!(matches!(approval::approved(&rig.ctx(), &rig.caller, &d, &o).await, driver::Outcome::Awaiting(_)));
    let c2 = rig.record(&w.task_id).land.unwrap().card_decision.unwrap();
    assert_ne!(c1, c2);
    let outcome = |id: &str| decisions_of(&rig).into_iter().find(|x| x.id == id).unwrap().outcome;
    assert_eq!(outcome(&c1).as_deref(), Some("asked again on a new card: the config change was no longer the one approved"));

    // The human denies the second card: web landed, so the land holds, and is resumed on the same pins.
    let (_, d2) = rig.last_card();
    approval::closed(&rig.ctx(), &rig.caller, &d2, "denied").await.unwrap();
    assert!(rig.record(&w.task_id).land.unwrap().is_held());
    assert_eq!(rig.land(&w).await.unwrap()["state"], STATE_AWAITING_HUMAN);
    let c3 = rig.record(&w.task_id).land.unwrap().card_decision.unwrap();
    assert!(c3 != c2 && c3 != c1);
    let denied = decisions_of(&rig).into_iter().find(|x| x.id == c2).unwrap();
    assert_eq!(denied.answered.as_ref().and_then(|l| l.answer.as_deref()), Some("Don\u{2019}t land"));
    assert_eq!(denied.outcome.as_deref(), Some("superseded by a new card when the land resumed"));
}
