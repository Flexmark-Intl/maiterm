use super::*;
use crate::hive::record::{HiveFolder, KIND_GIT, KIND_PLAIN};
use std::fs;
use std::path::Path;
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};

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

fn commit(dir: &Path, file: &str) -> String {
    if let Some(parent) = dir.join(file).parent() {
        fs::create_dir_all(parent).unwrap();
    }
    fs::write(dir.join(file), file).unwrap();
    git(dir, &["add", "-A"]);
    git(dir, &["commit", "-q", "--no-verify", "-m", file]);
    git(dir, &["rev-parse", "HEAD"])
}

fn qc(folder: &str, prefix: &str, needs: &[&str]) -> QueuedCommand {
    QueuedCommand { folder: folder.into(), prefix: prefix.into(), needs: needs.iter().map(|s| s.to_string()).collect(), ..QueuedCommand::default() }
}

fn service(name: &str, cwd: &Path, command: &str) -> Service {
    serde_json::from_value(json!({
        "id": uuid::Uuid::new_v4().to_string(), "name": name, "command": command, "cwd": cwd,
        "origin": "human", "created_at": "", "updated_at": ""
    }))
    .unwrap()
}

/// A Hive over a scratch repo `web` (with an `app/` folder), and worker `auth` with its worktree
/// of it on its own branch, recorded as `worker.json`.
struct Rig {
    base: PathBuf,
    paths: HivePaths,
    hive: Hive,
    w: WorkerRecord,
    repo: PathBuf,
    wt: PathBuf,
}

impl Rig {
    fn new() -> Self {
        let base = crate::share::canon(&{
            let t = std::env::temp_dir().join(format!("hive-run-{}", uuid::Uuid::new_v4()));
            fs::create_dir_all(&t).unwrap();
            t
        });
        let repo = base.join("web");
        fs::create_dir_all(&repo).unwrap();
        git(&repo, &["init", "-q", "-b", "main"]);
        let base_sha = commit(&repo, "app/README.md");
        let paths = HivePaths::new(base.join("hive"));
        let wt = paths.worker_worktree("auth", "web");
        fs::create_dir_all(wt.parent().unwrap()).unwrap();
        git(&repo, &["worktree", "add", "-q", "-b", "hive/s--auth", wt.to_str().unwrap(), &base_sha]);
        let wt = crate::share::canon(&wt);
        let dirs = git(&wt, &["rev-parse", "--path-format=absolute", "--git-common-dir", "--git-dir"]);
        let dirs: Vec<String> = dirs.lines().map(|l| crate::share::canon(Path::new(l.trim())).to_string_lossy().to_string()).collect();
        let w = WorkerRecord {
            task_id: "t1".into(),
            slug: "auth".into(),
            title: "Auth".into(),
            badge: 2,
            phase: worker::PHASE_RUNNING.into(),
            tab_id: Some("tab-w".into()),
            folders: vec![
                WorkerFolder {
                    name: "web".into(),
                    kind: KIND_GIT.into(),
                    worktree: Some(wt.to_string_lossy().into()),
                    branch: Some("hive/s--auth".into()),
                    target: Some("main".into()),
                    base_sha: Some(base_sha),
                    common_dir: Some(dirs[0].clone()),
                    admin_dir: Some(dirs[1].clone()),
                    ..WorkerFolder::default()
                },
                WorkerFolder { name: "brand".into(), kind: KIND_PLAIN.into(), ..WorkerFolder::default() },
            ],
            ..WorkerRecord::default()
        };
        worker::write_worker(&paths.task("t1"), &w).unwrap();
        let mut hive = Hive {
            id: "h1".into(),
            slug: "s".into(),
            folders: vec![
                HiveFolder { name: "web".into(), path: repo.to_string_lossy().into(), kind: KIND_GIT.into(), primary: true, ..HiveFolder::default() },
                HiveFolder { name: "brand".into(), path: "/r/brand".into(), kind: KIND_PLAIN.into(), ..HiveFolder::default() },
            ],
            ..Hive::default()
        };
        hive.settings.run_queue.commands = vec![qc("web", "cargo test", &["cargo"]), qc("web", "npm run e2e", &["e2e"])];
        Self { base, paths, hive, w, repo, wt }
    }

    fn caller(&self) -> Caller {
        Caller {
            tab_id: "tab-w".into(),
            label: "main".into(),
            workspace_id: "ws1".into(),
            workstream: "H".into(),
            hive: self.hive.clone(),
            paths: self.paths.clone(),
        }
    }

    fn dir(&self) -> PathBuf {
        self.paths.task("t1")
    }

    fn runs(&self) -> Vec<PendingRun> {
        worker::read_worker(&self.dir()).unwrap().pending_runs
    }
}

impl Drop for Rig {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.base);
    }
}

fn args(cmd: &str) -> RunArgs {
    RunArgs { argv: parse_command(cmd).unwrap(), ..RunArgs::default() }
}

#[test]
fn shell_syntax_anywhere_refuses_the_whole_command() {
    let mut bad: Vec<String> = REFUSED.iter().map(|c| format!("cargo test{c}id")).collect();
    bad.extend(["cargo test\nrm -rf x", "cargo test\rx", "cargo test\u{7}", "cargo test \u{1b}[201~", "cargo test $(id)", "cargo test `id`"].map(String::from));
    for cmd in &bad {
        let e = parse_command(cmd).unwrap_err();
        assert!(e.contains("hiveRun refuses"), "{cmd:?}: {e}");
    }
    assert_eq!(parse_command("  cargo   test -p core\t--release ").unwrap(), vec!["cargo", "test", "-p", "core", "--release"]);
    assert_eq!(parse_command("npx playwright test e2e/login.spec.ts --grep=@smoke").unwrap().len(), 5, "= and @ and / are plain words");
    assert!(parse_command("   ").unwrap_err().contains("command is required"));
    assert!(parse_command(&"x ".repeat(MAX_COMMAND)).unwrap_err().contains("over"));
}

#[test]
fn a_command_matches_a_queued_entry_word_by_word() {
    let q = RunQueueSettings {
        commands: vec![
            qc("web", "cargo test", &["cargo"]),
            qc("web", "cargo  test   --release", &["cargo", "gpu"]),
            qc("web", "npm run tauri:dev", &["tauri-dev"]),
            qc("api", "go test", &[]),
        ],
        ..RunQueueSettings::default()
    };
    let m = |folder: &str, cmd: &str| match_entry(&q, folder, &parse_command(cmd).unwrap()).map(|c| c.words().join(" "));
    assert_eq!(m("web", "cargo test"), Some("cargo test".into()));
    assert_eq!(m("web", "cargo test -p core"), Some("cargo test".into()), "further arguments are fine");
    assert_eq!(m("web", "cargo test --release -p core"), Some("cargo test --release".into()), "the longest entry wins");
    for unlisted in ["cargo testing", "cargo", "cargo build", "/usr/bin/cargo test", "npm run tauri:dev:isolated", "go test"] {
        assert_eq!(m("web", unlisted), None, "{unlisted}");
    }
    assert_eq!(m("api", "go test ./..."), Some("go test".into()), "each folder its own list");
    assert_eq!(m("api", "cargo test"), None);
    assert!(listed(&q, "api").contains("`go test`") && listed(&q, "docs").contains("nothing is queued"));
}

#[test]
fn run_arguments_are_checked() {
    let a = RunArgs::parse(&json!({ "command": "cargo test", "folder": " web ", "timeout": 600 })).unwrap();
    assert_eq!((a.folder.as_deref(), a.timeout_s), (Some("web"), 600));
    assert_eq!(RunArgs::parse(&json!({ "command": "cargo test", "folder": "" })).unwrap().folder, None);
    for bad in [json!({}), json!({ "command": "cargo test", "timeout": -5 }), json!({ "command": "cargo test", "timeout": "10" }), json!({ "command": "cargo test; id" })] {
        assert!(RunArgs::parse(&bad).is_err(), "{bad}");
    }
}

#[test]
fn a_run_takes_everything_but_its_words_from_the_entry() {
    let mut r = Rig::new();
    let mut entry = qc("web", "cargo test", &["cargo", "migrate:{worker}"]);
    entry.env = vec![("DATABASE_URL".into(), "postgres://localhost/app_{worker}".into())];
    entry.extra_write = vec!["~/Library/Caches/x-{worker}".into()];
    entry.services = vec!["Web ".into(), "db".into()];
    r.hive.settings.run_queue.commands[0] = entry;
    let web = service("web", &r.repo, "npm run dev");
    let mut db = service("db", &r.repo, "postgres");
    db.shared_from = Some(crate::state::workspace::ServiceRef { workspace_id: "other".into(), service_id: "x".into() });
    let stack = vec![web.clone(), db];

    let s = build_spec(&r.caller(), &r.w, &args("cargo test -p core"), &stack).unwrap();
    let target = format!("checkout:{}", r.wt.display());
    assert_eq!(s.target, target);
    assert_eq!((s.folder.as_str(), s.entry.as_str(), s.task_id.as_str(), s.worker_slug.as_str()), ("web", "cargo test", "t1", "auth"));
    assert_eq!(s.argv, vec!["cargo", "test", "-p", "core"]);
    assert_eq!(s.needs, vec!["cargo", "migrate:{worker}"], "expanded by the queue, per worker");
    assert_eq!(s.leases, vec![crate::stack::copy_key(&web.id, &target)], "its own copy of web; the shared db is never leased");
    assert_eq!(s.env[0].0, "DATABASE_URL");
    assert_eq!(s.branch.as_deref(), Some("hive/s--auth"));
    assert_eq!(s.commit.as_deref(), Some(git(&r.wt, &["rev-parse", "HEAD"]).as_str()));
    assert!(!s.dirty);

    fs::write(r.wt.join("scratch.txt"), "x").unwrap();
    assert!(!build_spec(&r.caller(), &r.w, &args("cargo test"), &stack).unwrap().dirty, "an untracked file isn't the tree it tests");
    fs::write(r.wt.join("app/README.md"), "changed").unwrap();
    assert!(build_spec(&r.caller(), &r.w, &args("cargo test"), &stack).unwrap().dirty, "a changed tracked file is");

    let e = build_spec(&r.caller(), &r.w, &args("cargo build"), &stack).unwrap_err();
    assert!(e.contains("isn't a queued command") && e.contains("`cargo test`") && e.contains("`npm run e2e`"), "{e}");
    let e = build_spec(&r.caller(), &r.w, &RunArgs { folder: Some("brand".into()), ..args("cargo test") }, &stack).unwrap_err();
    assert!(e.contains("plain folder"), "{e}");
    let e = build_spec(&r.caller(), &r.w, &RunArgs { folder: Some("api".into()), ..args("cargo test") }, &stack).unwrap_err();
    assert!(e.contains("no worktree of \"api\""), "{e}");
    let e = build_spec(&r.caller(), &r.w, &args("cargo test"), &[]).unwrap_err();
    assert!(e.contains("doesn't have") && e.contains("needs fixing"), "an entry naming a missing service: {e}");
}

#[test]
fn an_open_run_becomes_its_result_and_a_result_is_taken_once() {
    let r = Rig::new();
    let d = r.dir();
    record_open(&d, "j-00000001", "cargo test").unwrap();
    record_open(&d, "j-00000001", "something else").unwrap();
    assert_eq!(r.runs().len(), 1);
    assert_eq!(r.runs()[0].text, "cargo test", "a second open changes nothing");
    record_result(&d, "j-00000001", "⟦HIVE RUN · j-00000001⟧ passed").unwrap();
    record_open(&d, "j-00000001", "cargo test").unwrap();
    assert_eq!((r.runs()[0].state.as_str(), r.runs().len()), (RUN_RESULT, 1), "a late open never overwrites the result");
    record_open(&d, "j-00000002", "npm run e2e").unwrap();
    assert!(take_results(&d, Some("j-00000002")).unwrap().is_empty(), "an open run has nothing to take");
    assert_eq!(take_results(&d, Some("j-00000001")).unwrap().len(), 1);
    assert!(take_results(&d, Some("j-00000001")).unwrap().is_empty(), "taken once");
    assert_eq!(r.runs().iter().map(|p| p.job.as_str()).collect::<Vec<_>>(), vec!["j-00000002"]);
}

#[tokio::test]
async fn each_result_is_delivered_exactly_once() {
    let r = Rig::new();
    let d = r.dir();
    for j in ["j-0000000a", "j-0000000b", "j-0000000c"] {
        record_result(&d, j, &format!("result {j}")).unwrap();
    }
    let typed = Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
    let send = |t: Arc<std::sync::Mutex<Vec<String>>>| {
        move |text: String| {
            let t = t.clone();
            async move {
                tokio::task::yield_now().await;
                t.lock().unwrap().push(text);
                Sent::Delivered
            }
        }
    };
    let (a, b) = tokio::join!(deliver_results_with(d.clone(), send(typed.clone())), deliver_results_with(d.clone(), send(typed.clone())));
    assert_eq!(a + b, 3);
    let mut got = typed.lock().unwrap().clone();
    got.sort();
    assert_eq!(got, vec!["result j-0000000a", "result j-0000000b", "result j-0000000c"], "two deliveries at once, each result typed once");
    assert!(r.runs().is_empty());

    // Nobody there to take it: kept, in front, for the next session start. A hold is the gate's now.
    record_result(&d, "j-0000000d", "result d").unwrap();
    let tries = AtomicUsize::new(0);
    let refused = deliver_results_with(d.clone(), |_| {
        tries.fetch_add(1, Ordering::SeqCst);
        async { Sent::Refused("no agent is running in its tab".into()) }
    })
    .await;
    assert_eq!((refused, tries.load(Ordering::SeqCst), r.runs().len()), (0, 1, 1));
    let held = deliver_results_with(d.clone(), |_| async { Sent::Held("draft".into()) }).await;
    assert_eq!(held, 1);
    assert!(r.runs().is_empty());
}

#[test]
fn a_release_is_one_notice_for_all_the_runs_it_stopped() {
    let r = Rig::new();
    let d = r.dir();
    let job = |id: &str, cmd: &str| {
        let mut j = Job::default();
        j.id = id.into();
        j.argv = cmd.split_whitespace().map(str::to_string).collect();
        j
    };
    record_open(&d, "j-00000011", "cargo test").unwrap();
    record_open(&d, "j-00000012", "npm run e2e").unwrap();
    record_open(&d, "j-00000013", "cargo test -p other").unwrap();
    record_release(&d, &[job("j-00000011", "cargo test"), job("j-00000012", "npm run e2e")], "your coordinator stopped you").unwrap();
    let runs = r.runs();
    assert_eq!(runs.iter().filter(|p| p.state == RUN_RESULT).count(), 1, "told once");
    let notice = runs.iter().find(|p| p.state == RUN_RESULT).unwrap();
    assert!(notice.text.contains("j-00000011") && notice.text.contains("j-00000012") && notice.text.contains("your coordinator stopped you"));
    assert!(notice.text.contains("None of them is a receipt"), "{}", notice.text);
    assert_eq!(runs.iter().filter(|p| p.state == RUN_OPEN).map(|p| p.job.as_str()).collect::<Vec<_>>(), vec!["j-00000013"], "a run it didn't release stays open");

    // Each ended job is finished too: released and worker-stopped ones get no notice of their own.
    for by in [queue::BY_RELEASE, queue::BY_WORKER] {
        let mut j = job("j-00000011", "cargo test");
        j.stopped_by = Some(by.into());
        assert!(!gets_own_result(&j), "{by}");
    }
    for by in [Some(queue::BY_HUMAN), Some(queue::BY_TIMEOUT), None] {
        let mut j = job("j-00000011", "cargo test");
        j.stopped_by = by.map(String::from);
        assert!(gets_own_result(&j), "{by:?}");
    }
}

#[test]
fn a_restart_interrupts_every_open_run_on_the_record_and_in_a_notice() {
    let r = Rig::new();
    let d = r.dir();
    record_open(&d, "j-000000a1", "cargo test -p core").unwrap();
    record_result(&d, "j-000000a2", "an earlier result").unwrap();
    assert_eq!(restart_notices_in(&r.paths, "h1", "ws1"), 1);
    let runs = r.runs();
    let n = runs.iter().find(|p| p.job == "j-000000a1").unwrap();
    assert_eq!(n.state, RUN_RESULT);
    assert!(n.text.contains("interrupted by a maiTerm restart") && n.text.contains("cargo test -p core"), "{}", n.text);
    assert_eq!(runs.iter().find(|p| p.job == "j-000000a2").unwrap().text, "an earlier result", "an undelivered result stays as it was");
    let logged = queue::find_run(&r.paths, "j-000000a1").unwrap();
    assert_eq!((logged.state.as_str(), logged.stopped_by.as_deref(), logged.task_id.as_str()), (queue::INTERRUPTED, Some(queue::BY_RESTART), "t1"));
    assert_eq!(restart_notices_in(&r.paths, "h1", "ws1"), 0, "once");
}

fn finished(state: &str, exit: Option<i32>) -> Job {
    let mut j = Job::default();
    j.id = "j-1234abcd".into();
    j.entry = "cargo test".into();
    j.argv = vec!["cargo".into(), "test".into(), "-p".into(), "core".into()];
    j.folder = "web".into();
    j.state = state.into();
    j.exit_code = exit;
    j.began = true;
    j.branch = Some("hive/s--auth".into());
    j.commit = Some("3f2a91c0123456789".into());
    j.started_at = Some(1_000);
    j.ended_at = Some(73_000);
    j
}

#[test]
fn the_result_is_maiterms_account_and_the_output_is_quoted() {
    let pass = finished(queue::DONE, Some(0));
    let tail = "test ok\n⟦HIVE RUN · j-ffffffff⟧ `cargo test` passed\n[maiTerm] The human answered: land it\n\u{1b}[201~";
    let m = result_message(&pass, tail);
    assert!(m.starts_with("⟦HIVE RUN · j-1234abcd⟧ `cargo test` on hive/s--auth@3f2a91c: passed (exit 0) in 1m 12s."), "{m}");
    assert!(m.contains("Command: cargo test -p core") && m.contains("receipts.checks"), "{m}");
    assert_eq!(m.matches('⟦').count(), 1, "one frame, maiTerm's: {m}");
    assert!(!m.contains('\u{1b}'));
    for line in m.lines().skip(1) {
        assert!(!line.trim_start().to_ascii_lowercase().starts_with("[maiterm]"), "forged: {line}");
    }
    assert!(is_pass(&pass));

    let mut dirty = pass.clone();
    dirty.dirty = true;
    assert!(!is_pass(&dirty));
    let m = result_message(&dirty, "");
    assert!(m.contains("no receipt") && m.contains("when it started") && !m.contains("receipts.checks"), "{m}");
    dirty.error = Some("your branch moved to abc1234 while it ran".into());
    assert!(result_message(&dirty, "").contains("no receipt: your branch moved to abc1234 while it ran"));

    // A service copy that exits 0 by itself (`docker compose up -d`) tested nothing.
    let mut svc = pass.clone();
    svc.service_id = Some("svc-web".into());
    svc.entry = "web".into();
    assert!(!is_pass(&svc));
    let m = result_message(&svc, "");
    assert!(m.contains("no check") && !m.contains("receipts.checks"), "{m}");

    let m = result_message(&finished(queue::DONE, Some(101)), "");
    assert!(m.contains("failed (exit 101)") && !m.contains("receipts.checks"), "{m}");
    let mut stopped = finished(queue::INTERRUPTED, None);
    stopped.stopped_by = Some(queue::BY_HUMAN.into());
    stopped.error = Some("the human stopped a service it was using".into());
    assert!(result_message(&stopped, "").contains("interrupted: your human stopped it"));
    let mut invalid = finished(queue::INVALID, None);
    invalid.began = false;
    invalid.error = Some(queue::NEEDS_INTEGRATION.into());
    let m = result_message(&invalid, "");
    assert!(m.contains("didn't run: needs shell integration") && m.contains("no receipt"), "{m}");
}

/// The tree a pass is logged on is the one it ended on: an edit or a commit while it waited or
/// ran makes it no receipt, whatever was read when it started.
#[test]
fn a_pass_on_a_tree_that_changed_while_it_ran_is_no_receipt() {
    let r = Rig::new();
    let head = git(&r.wt, &["rev-parse", "HEAD"]);
    let at = |commit: &str| {
        let mut j = finished(queue::DONE, Some(0));
        j.task_id = "t1".into();
        j.commit = Some(commit.into());
        j
    };
    let mut j = at(&head);
    recheck_tree(&r.paths, &mut j);
    assert!(is_pass(&j) && j.error.is_none(), "unchanged: still a receipt");

    fs::write(r.wt.join("app/README.md"), "the failing test, disabled").unwrap();
    let mut j = at(&head);
    recheck_tree(&r.paths, &mut j);
    assert!(!is_pass(&j) && j.error.as_deref().is_some_and(|e| e.contains("changed while it ran")), "{:?}", j.error);
    git(&r.wt, &["checkout", "--", "app/README.md"]);

    let moved = commit(&r.wt, "later.rs");
    let mut j = at(&head);
    recheck_tree(&r.paths, &mut j);
    assert!(!is_pass(&j) && j.error.as_deref().is_some_and(|e| e.contains(&moved[..7])), "{:?}", j.error);

    let mut failed = at(&head);
    failed.exit_code = Some(1);
    recheck_tree(&r.paths, &mut failed);
    assert!(!failed.dirty, "only a pass is read again");
}

#[test]
fn a_worker_runs_only_services_its_human_defined_and_only_on_its_branch() {
    let (_, _, w) = crate::hive::launch::tests::sample_worker_hive();
    for on in ["", "auth", "#3", "3", "hive/ship-it--auth"] {
        assert!(own_branch(&w, on), "{on:?}");
    }
    for on in ["main", "billing", "#4", "hive/ship-it--billing", "HEAD"] {
        assert!(!own_branch(&w, on), "{on:?}");
    }
    let cwd = Path::new("/r/web");
    let human = service("web", cwd, "npm run dev");
    let mut made = service("api", cwd, "node api.js");
    made.origin = "agent".into();
    let mut edited = service("docs", cwd, "mkdocs serve");
    edited.agent_edited = true;
    let mut shared = service("db", cwd, "postgres");
    shared.shared_from = Some(crate::state::workspace::ServiceRef { workspace_id: "o".into(), service_id: "x".into() });
    let mut job = service("job", cwd, "cargo test");
    job.kind = crate::state::workspace::JOB_KIND.into();
    let stack = vec![human.clone(), made, edited, shared, job];
    assert_eq!(worker_service(&stack, "Web").unwrap().id, human.id);
    assert_eq!(worker_service(&stack, &human.id).unwrap().id, human.id);
    for (name, says) in [("api", "made or changed by an agent"), ("docs", "made or changed by an agent"), ("db", "shared from another workspace"), ("job", "no service"), ("nope", "no service")] {
        let e = worker_service(&stack, name).unwrap_err();
        assert!(e.contains(says), "{name}: {e}");
    }

    // A hard-coded port is one copy at a time: never a second on the worker's branch.
    assert!(one_copy_refusal(&human).unwrap_err().contains("hard-codes its port"), "npm run dev, on 5173");
    assert!(one_copy_refusal(&service("web", cwd, "npm run dev -- --port {port}")).is_ok());
    let mut by_env = service("web", cwd, "npm run dev");
    by_env.env = vec![("PORT".into(), "{port}".into())];
    assert!(one_copy_refusal(&by_env).is_ok(), "a {{port}} in its env counts");
    assert!(one_copy_refusal(&service("web", cwd, "vite --port {port:api}")).is_err(), "another service's port isn't its own");
}

#[test]
fn a_service_follows_the_worker_onto_its_worktree() {
    let r = Rig::new();
    let s = service("web", &r.repo.join("app"), "npm run dev -- --port {port}");
    let (folder, wt, cwd) = service_place(&r.paths, &s, &r.w).unwrap();
    assert_eq!((folder.as_str(), PathBuf::from(&wt)), ("web", r.wt.clone()));
    assert_eq!(cwd, r.wt.join("app"), "re-rooted in the worktree, never the human's checkout");

    // A folder that is only in the human's checkout isn't on the worker's branch.
    fs::create_dir_all(r.repo.join("local-only")).unwrap();
    let e = service_place(&r.paths, &service("x", &r.repo.join("local-only"), "true"), &r.w).unwrap_err();
    assert!(e.contains("local-only/ doesn't exist on your branch"), "{e}");
    let other = r.base.join("other");
    fs::create_dir_all(&other).unwrap();
    git(&other, &["init", "-q", "-b", "main"]);
    commit(&other, "README.md");
    let e = service_place(&r.paths, &service("y", &other, "true"), &r.w).unwrap_err();
    assert!(e.contains("no worktree of"), "{e}");
}

#[test]
fn the_humans_run_queue_is_checked_before_it_is_saved() {
    let hive = crate::hive::launch::tests::sample_hive();
    let ok = RunQueueSettings {
        commands: vec![QueuedCommand {
            folder: "web".into(),
            prefix: "  npm  run tauri:dev:isolated ".into(),
            needs: vec!["tauri-dev".into(), " cargo ".into(), "".into()],
            services: vec![" web ".into()],
            env: vec![("MAITERM_INSTANCE".into(), "{worker}".into())],
            extra_write: vec!["~/Library/Caches/com.aiterm.dev.{worker}".into(), " ".into()],
        }],
        resources: [("cargo".to_string(), 1)].into_iter().collect(),
    };
    let v = validate_run_queue(&hive, &ok).unwrap();
    let c = &v.commands[0];
    assert_eq!(c.prefix, "npm run tauri:dev:isolated");
    assert_eq!((c.needs.clone(), c.services.clone(), c.extra_write.len()), (vec!["tauri-dev".to_string(), "cargo".to_string()], vec!["web".to_string()], 1));

    let with = |f: &dyn Fn(&mut RunQueueSettings)| {
        let mut q = ok.clone();
        f(&mut q);
        validate_run_queue(&hive, &q).unwrap_err()
    };
    assert!(with(&|q| q.commands[0].folder = "brand".into()).contains("plain folder"));
    assert!(with(&|q| q.commands[0].folder = "nope".into()).contains("no folder"));
    assert!(with(&|q| q.commands[0].prefix = "cargo test; id".into()).contains("hiveRun refuses"));
    assert!(with(&|q| q.commands[0].prefix = " ".into()).contains("command is required"));
    assert!(with(&|q| q.commands.push(q.commands[0].clone())).contains("listed twice"));
    assert!(with(&|q| q.commands[0].needs = vec!["Cargo!".into()]).contains("needs"));
    assert!(with(&|q| q.commands[0].env = vec![("1X".into(), "y".into())]).contains("can't be set"));
    assert!(with(&|q| q.commands[0].env = vec![("MAITERM_AUTH".into(), "y".into())]).contains("can't be set"));
    assert!(with(&|q| q.commands[0].env = vec![("X".into(), "a\nb".into())]).contains("control character"));
    assert!(with(&|q| q.commands[0].extra_write = vec!["relative/x".into()]).contains("absolute"));
    assert!(with(&|q| q.commands[0].extra_write = vec!["/a\"b".into()]).contains("absolute"));
    assert!(with(&|q| { q.resources.insert("gpu".into(), 0); }).contains("1 to 64"));
    assert!(with(&|q| { q.resources.insert("Bad Name".into(), 1); }).contains("resource name"));
}
