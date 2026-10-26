//! Spike 15's acceptance test (docs/hive.md §13 item 15, the table in §13.1, build step 5b): every
//! maiTerm tool a worker tab can use to make something run, called as the worker naming no tab,
//! naming its own, and naming another tab, through the server's checks in the server's order
//! (`refuse_overlord_target`, `refuse_agent_tool`, `queued_verb_caller`, `own_tab_only_refusal`).
//! Each is refused there, or is one of a worker's two ways into the queue (`hiveRun`, a service
//! verb), which either refuses it or comes out as a job line whose command is `sandbox-exec`
//! under a profile drawn from the worker's own write lists. Nothing reaches the window's stores.
//!
//! Against a scratch Hive: nothing touches maiTerm's data folder or a running maiTerm. The service
//! verbs go through the real queue (memory only; with no driver installed nothing starts).

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use parking_lot::RwLock;
use serde_json::{json, Value};

use super::job_sandbox::{PROFILE_FILE, SANDBOX_EXEC};
use super::launch::sh_quote;
use super::paths::HivePaths;
use super::queue::{self, Job, Queue};
use super::record::{Hive, HiveFolder, QueuedCommand, KIND_GIT};
use super::run;
use super::settings::{worker_write_lists, WorkerContext};
use super::tools::Caller;
use super::worker::{self, WorkerFolder, WorkerRecord};
use super::{ROLE_COORDINATOR, ROLE_WORKER};
use crate::state::{AppData, Service, Tab, WindowData, Workspace};

const WORKER_TAB: &str = "tab-w";
const COORD_TAB: &str = "coord";
const PLAIN_TAB: &str = "t1";
/// Over 32 (`slugify` allows 60), so the pilot has to name its instance with `{instance}`.
const SLUG: &str = "auth-session-store-into-redis-cache";
const BRANCH: &str = "hive/s--auth-session-store-into-redis-cache";

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

fn service(name: &str, cwd: &Path, command: &str, origin: &str) -> Service {
    serde_json::from_value(json!({
        "id": uuid::Uuid::new_v4().to_string(), "name": name, "command": command, "cwd": cwd,
        "origin": origin, "created_at": "", "updated_at": ""
    }))
    .unwrap()
}

fn tab(id: &str, role: Option<&str>) -> Tab {
    let mut t = Tab::new(id.to_string());
    t.id = id.to_string();
    t.hive_role = role.map(str::to_string);
    t
}

/// A Hive over scratch repo `web` (an `app/` folder in it), worker `SLUG` on its own branch with
/// its worktree and `worker.json`, the app state that holds it, and its stack.
struct Rig {
    base: PathBuf,
    paths: HivePaths,
    hive: Hive,
    w: WorkerRecord,
    wt: PathBuf,
    bin: PathBuf,
    data: RwLock<AppData>,
}

impl Rig {
    fn new() -> Self {
        let base = crate::share::canon(&{
            let t = std::env::temp_dir().join(format!("hive-spike15-{}", uuid::Uuid::new_v4()));
            fs::create_dir_all(&t).unwrap();
            t
        });
        let repo = base.join("web");
        fs::create_dir_all(repo.join("app")).unwrap();
        git(&repo, &["init", "-q", "-b", "main"]);
        fs::write(repo.join("app/README.md"), "app").unwrap();
        git(&repo, &["add", "-A"]);
        git(&repo, &["commit", "-q", "--no-verify", "-m", "base"]);
        let base_sha = git(&repo, &["rev-parse", "HEAD"]);
        let paths = HivePaths::new(base.join("hive"));
        let wt = paths.worker_worktree(SLUG, "web");
        fs::create_dir_all(wt.parent().unwrap()).unwrap();
        git(&repo, &["worktree", "add", "-q", "-b", BRANCH, wt.to_str().unwrap(), &base_sha]);
        let wt = crate::share::canon(&wt);
        let dirs = git(&wt, &["rev-parse", "--path-format=absolute", "--git-common-dir", "--git-dir"]);
        let dirs: Vec<String> = dirs.lines().map(|l| crate::share::canon(Path::new(l.trim())).to_string_lossy().to_string()).collect();
        let w = WorkerRecord {
            task_id: "t-auth".into(),
            slug: SLUG.into(),
            title: "Auth".into(),
            badge: 2,
            phase: worker::PHASE_RUNNING.into(),
            tab_id: Some(WORKER_TAB.into()),
            folders: vec![WorkerFolder {
                name: "web".into(),
                kind: KIND_GIT.into(),
                worktree: Some(wt.to_string_lossy().into()),
                branch: Some(BRANCH.into()),
                target: Some("main".into()),
                base_sha: Some(base_sha),
                common_dir: Some(dirs[0].clone()),
                admin_dir: Some(dirs[1].clone()),
                ..WorkerFolder::default()
            }],
            ..WorkerRecord::default()
        };
        worker::write_worker(&paths.task(&w.task_id), &w).unwrap();

        let mut hive = Hive {
            // Its own id: the queue is the process's, shared with other tests.
            id: format!("spike15-{}", uuid::Uuid::new_v4().simple()),
            slug: "s".into(),
            folders: vec![HiveFolder { name: "web".into(), path: repo.to_string_lossy().into(), kind: KIND_GIT.into(), primary: true, ..HiveFolder::default() }],
            ..Hive::default()
        };
        let entry = |prefix: &str, needs: &[&str]| QueuedCommand {
            folder: "web".into(),
            prefix: prefix.into(),
            needs: needs.iter().map(|s| s.to_string()).collect(),
            ..QueuedCommand::default()
        };
        let mut tests = entry("cargo test", &["cargo"]);
        tests.env = vec![("DATABASE_URL".into(), "postgres://localhost/app_{worker}".into())];
        tests.extra_write = vec!["~/Library/Caches/spike15-{worker}".into()];
        let mut pilot = entry("npm run tauri:dev:isolated", &["tauri-dev", "cargo"]);
        pilot.env = queue::PILOT_ENV.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect();
        pilot.extra_write = queue::pilot_extra_write();
        hive.settings.run_queue.commands = vec![tests, pilot, entry("npm run dev", &[])];

        let mut shared = service("db", &repo, "postgres", "human");
        shared.shared_from = Some(crate::state::workspace::ServiceRef { workspace_id: "elsewhere".into(), service_id: "x".into() });
        let mut edited = service("docs", &repo.join("app"), "mkdocs serve --dev-addr {port}", "human");
        edited.agent_edited = true;
        let stack = vec![
            service("web", &repo.join("app"), "npm run dev -- --port {port}", "human"),
            service("fixed", &repo.join("app"), "npm run dev", "human"),
            service("made", &repo.join("app"), "node api.js --port {port}", "agent"),
            edited,
            shared,
        ];
        let mut ws = Workspace::new("Hive".into());
        ws.id = "ws1".into();
        ws.hive = Some(hive.clone());
        ws.stack = stack;
        ws.panes[0].tabs = vec![tab(COORD_TAB, Some(ROLE_COORDINATOR)), tab(WORKER_TAB, Some(ROLE_WORKER))];
        let mut plain = Workspace::new("Plain".into());
        plain.panes[0].tabs = vec![tab(PLAIN_TAB, None)];
        let mut win = WindowData::new("main".into());
        win.workspaces = vec![ws, plain];
        let data = RwLock::new(AppData { windows: vec![win], ..AppData::default() });
        Rig { bin: base.join("bin"), base, paths, hive, w, wt, data }
    }

    fn caller(&self) -> Caller {
        Caller { tab_id: WORKER_TAB.into(), label: "main".into(), workspace_id: "ws1".into(), workstream: "Hive".into(), hive: self.hive.clone(), paths: self.paths.clone() }
    }

    fn stack(&self) -> Vec<Service> {
        self.data.read().windows[0].workspaces[0].stack.clone()
    }
}

impl Drop for Rig {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.base);
    }
}

/// Where the server sends a call, by its checks in its order (`claude_code::server`).
#[derive(Debug, PartialEq)]
enum Route {
    Refused(String),
    /// `worker_service_verb`, as this worker.
    Queue(String),
    /// The Hive tools' own handler.
    HiveTool,
    /// The window's stores: what a worker's call that runs something must never reach.
    Window,
}

fn route(data: &AppData, callers: &[&str], tool: &str, args: &Value) -> Route {
    if let Some(r) = super::refuse_overlord_target(data, tool, args) {
        return Route::Refused(r);
    }
    if let Some(r) = super::tools::refuse_agent_tool(data, callers, tool) {
        return Route::Refused(r);
    }
    let queued = run::queued_verb_caller(data, callers, tool);
    let named = args.get("tabId").and_then(Value::as_str).filter(|s| !s.is_empty());
    if let Some(r) = crate::claude_code::server::own_tab_only_refusal(tool, named, callers.first().map(|c| (*c, true))) {
        return Route::Refused(r);
    }
    match queued {
        Some(tab) => Route::Queue(tab),
        None if tool.starts_with("hive") => Route::HiveTool,
        None => Route::Window,
    }
}

/// `'…'` words, as `sh_quote` writes them, and bare ones.
fn words(s: &str) -> Vec<String> {
    let (mut out, mut cur, mut quoted, mut any) = (Vec::new(), String::new(), false, false);
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        match c {
            '\'' => {
                quoted = !quoted;
                any = true;
            }
            c if c.is_whitespace() && !quoted => {
                if any || !cur.is_empty() {
                    out.push(std::mem::take(&mut cur));
                }
                any = false;
            }
            c => cur.push(c),
        }
    }
    if any || !cur.is_empty() {
        out.push(cur);
    }
    out
}

/// The quoted strings of one `(<head>` block of a profile.
fn block(profile: &str, head: &str) -> Vec<String> {
    let Some(start) = profile.find(head) else { return Vec::new() };
    let body = &profile[start + head.len()..];
    let end = body.find("\n(").unwrap_or(body.len());
    body[..end].split('"').skip(1).step_by(2).map(str::to_string).collect()
}

/// What a job the worker queued comes out as: `cd <its folder> && env -u … K=V …` and then
/// `sandbox-exec -f <profile> <its argv>`, the profile being exactly the worker's own lists (its
/// allow list, its job temp and the entry's extra writes; its deny list), with no network but
/// loopback to the ports the job was booked. `want_argv` is its argv given those ports. Off
/// macOS, refused. Answers the profile.
fn assert_sandboxed(rig: &Rig, job: &Job, want_argv: &dyn Fn(&[u16]) -> Vec<String>) -> Option<String> {
    let res = queue::prepare_in(&rig.data, job, &rig.paths, &rig.bin, &[]);
    crate::stack::ports::release(&job.id);
    super::registry_proxy::close(&job.id);
    if !cfg!(target_os = "macos") {
        let e = res.err().expect("no sandbox, no job");
        assert!(e.contains("only on macOS"), "{e}");
        return None;
    }
    let p = res.unwrap_or_else(|e| panic!("{}: {e}", job.argv.join(" ")));
    let profile_file = rig.paths.run(&job.id).join(PROFILE_FILE);
    let w = words(&p.line);
    assert_eq!(w[0], "cd", "{}", p.line);
    let cwd = job.cwd.clone().map(PathBuf::from).unwrap_or_else(|| rig.wt.clone());
    assert_eq!(fs::canonicalize(&w[1]).unwrap(), fs::canonicalize(&cwd).unwrap(), "in the worker's worktree: {}", p.line);
    assert_eq!((w[2].as_str(), w[3].as_str()), ("&&", "env"));
    // Between `env` and the program: only removals and assignments.
    let at = w.iter().position(|x| x == SANDBOX_EXEC).unwrap_or_else(|| panic!("no sandbox-exec: {}", p.line));
    let mut i = 4;
    while i < at {
        if w[i] == "-u" {
            i += 2;
        } else {
            assert!(w[i].split_once('=').is_some_and(|(k, _)| crate::stack::placeholders::valid_env_key(k)), "{:?} before sandbox-exec: {}", w[i], p.line);
            i += 1;
        }
    }
    assert_eq!(i, at);
    for scrubbed in super::job_sandbox::SCRUBBED {
        assert!(p.line.contains(&format!(" -u {scrubbed} ")), "{scrubbed} is removed: {}", p.line);
    }
    assert_eq!(w[at + 1], "-f");
    assert_eq!(PathBuf::from(&w[at + 2]), profile_file);
    assert!(p.line.contains(&format!(" {SANDBOX_EXEC} -f {} ", sh_quote(&profile_file.to_string_lossy()))));
    let end = w.iter().skip(at).position(|x| x == "&&").map(|e| e + at).unwrap_or_else(|| panic!("no pass mark: {}", p.line));
    assert_eq!(w[at + 3..end].to_vec(), want_argv(&p.ports), "{}", p.line);
    assert!(!p.line[..p.line.find(SANDBOX_EXEC).unwrap()].contains("sh -c"), "nothing runs a shell outside the sandbox: {}", p.line);

    // The profile is the worker's own lists, nothing more.
    let profile = fs::read_to_string(&profile_file).unwrap();
    let job_rec = worker::read_worker(&rig.paths.task(&job.task_id)).unwrap();
    let lists = worker_write_lists(&rig.hive, &job_rec, &rig.paths, &WorkerContext::gather(&rig.hive, &rig.paths, &job_rec), &rig.bin).unwrap();
    let home = dirs::home_dir().unwrap();
    let extra: Vec<PathBuf> = job
        .extra_write
        .iter()
        .map(|p| {
            let values = crate::stack::placeholders::Values { worker: Some(job.worker_slug.clone()), ..Default::default() };
            let filled = crate::stack::placeholders::fill(p, &values).unwrap();
            filled.strip_prefix("~/").map(|r| home.join(r)).unwrap_or_else(|| PathBuf::from(&filled))
        })
        .collect();
    let writable: Vec<PathBuf> = lists.allow.iter().cloned().chain([rig.paths.worker_job_tmp(&job.worker_slug)]).chain(extra.iter().cloned()).collect();
    let named = |p: &Path| super::job_sandbox::profile_path(p, &writable).unwrap().to_string_lossy().to_string();
    let mut want_allow: Vec<String> = writable.iter().map(|p| named(p)).collect();
    want_allow.sort();
    want_allow.dedup();
    let mut got_allow = block(&profile, "(allow file-write*\n");
    assert_eq!((got_allow.remove(0), got_allow.remove(0)), ("/dev/null".to_string(), "^/dev/tty".to_string()), "{profile}");
    got_allow.sort();
    assert_eq!(got_allow, want_allow, "{profile}");
    let mut want_deny: Vec<String> = lists.deny.iter().map(|d| named(&d.path)).collect();
    want_deny.sort();
    let mut got_deny = block(&profile, "(deny file-write*\n");
    got_deny.sort();
    assert_eq!(got_deny, want_deny, "the worker's deny list, whole: {profile}");
    assert!(lists.deny.iter().any(|d| d.path == rig.paths.runs()), "the runs folder (its marks and profiles) is denied");
    assert!(profile.contains("(deny network*)\n") && profile.contains("(deny file-ioctl (ioctl-command TIOCSTI))"), "{profile}");
    // An install's one way out: maiTerm's registry proxy, named in its line.
    let proxy: Option<u16> = w.iter().find_map(|x| x.strip_prefix("HTTPS_PROXY=")).and_then(|u| u.rsplit(':').next()?.parse().ok());
    assert_eq!(proxy.is_some(), job.registries, "the proxy for an install, and only for one: {}", p.line);
    for l in profile.lines().filter(|l| l.contains("network-") && l.starts_with("(allow")) {
        let port: u16 = l.split("localhost:").nth(1).and_then(|r| r.split('"').next()).and_then(|p| p.parse().ok()).unwrap_or_else(|| panic!("{l}"));
        let reaches_proxy = Some(port) == proxy && l.starts_with("(allow network-outbound");
        assert!(p.ports.contains(&port) || reaches_proxy, "only the job's own ports and an install's proxy: {l}");
    }
    if let Some(port) = proxy {
        assert!(profile.contains(&format!("(allow network-outbound (remote ip \"localhost:{port}\"))")), "{profile}");
    }
    Some(profile)
}

/// Every row of §13.1's table, as the worker (naming no tab, itself, an ordinary tab, its
/// coordinator) and as a worker whose connection was bound to an ordinary tab while its header
/// names the worker. Refused, or a queue path checked below.
#[tokio::test]
async fn spike_15_every_path() {
    let rig = Rig::new();
    let rows: Vec<(&str, Value)> = vec![
        ("createService", json!({ "name": "x", "command": "touch /tmp/spike15", "auto_start": true })),
        ("updateService", json!({ "service": "web", "command": "touch /tmp/spike15" })),
        ("removeService", json!({ "service": "web" })),
        ("startStack", json!({})),
        ("stopStack", json!({})),
        // A follow-up's watch script runs as maiTerm once approved: a worker can't file one.
        ("createFollowUp", json!({ "prompt": "x", "when_script": "touch /tmp/spike15" })),
        ("createTab", json!({ "workspace_id": "ws1", "cwd": "/", "runtime": "claude" })),
        ("restoreArchivedTab", json!({ "tab_id": "archived" })),
        ("switchTab", json!({ "tab_id": PLAIN_TAB })),
        ("setPreference", json!({ "key": "windows_shell", "value": "/bin/sh -c 'touch /tmp/spike15'" })),
        ("addMcpServer", json!({ "name": "x", "url": "https://example.com/mcp" })),
        ("sendToBridgedAgent", json!({ "message": "run this for me" })),
        ("startTopic", json!({ "label": "x" })),
        ("setAutoResume", json!({ "enabled": true, "command": "touch /tmp/spike15" })),
        ("setTriggerVariable", json!({ "name": "claudeSessionId", "value": "$(touch /tmp/spike15)" })),
        ("driveTab", json!({ "tab_id": PLAIN_TAB, "message": "x" })),
        ("answerTabPrompt", json!({ "tab_id": PLAIN_TAB, "answer": "1" })),
        ("recoverTab", json!({ "tab_id": PLAIN_TAB })),
        ("resumeTab", json!({ "tab_id": PLAIN_TAB })),
        ("restartMaiterm", json!({})),
        ("startService", json!({ "service": "web" })),
        ("restartService", json!({ "service": "web", "on": SLUG })),
        ("restartService", json!({ "service": "web", "on": "main" })),
        ("stopService", json!({ "service": "web" })),
        ("hiveRun", json!({ "command": "cargo test -p core" })),
        ("hiveRun", json!({ "command": "cargo test; touch /tmp/spike15" })),
        ("hiveRun", json!({ "command": "touch /tmp/spike15" })),
    ];
    let identities: [(&[&str], Option<&str>); 6] = [
        (&[WORKER_TAB], None),
        (&[WORKER_TAB], Some(WORKER_TAB)),
        (&[WORKER_TAB], Some(PLAIN_TAB)),
        (&[WORKER_TAB], Some(COORD_TAB)),
        (&[PLAIN_TAB, WORKER_TAB], None),
        (&[PLAIN_TAB, WORKER_TAB], Some(PLAIN_TAB)),
    ];
    let mut reached: Vec<(String, Route)> = Vec::new();
    for (tool, base_args) in &rows {
        for (callers, named) in identities {
            let mut args = base_args.clone();
            if let Some(t) = named {
                args["tabId"] = json!(t);
            }
            let r = route(&rig.data.read(), callers, tool, &args);
            assert_ne!(r, Route::Window, "{tool} {args} from {callers:?} reached the window's stores");
            let own = named.is_none_or(|t| t == WORKER_TAB) && callers.len() == 1 && callers[0] == WORKER_TAB;
            match &r {
                Route::Refused(_) => {}
                Route::Queue(tab) => {
                    assert_eq!(tab, WORKER_TAB, "{tool}: as the worker, whichever tab it was bound to");
                    assert!(named.is_none_or(|t| t == callers[0]), "{tool}: a named tab that isn't the caller's is refused first");
                }
                Route::HiveTool => assert!(own && *tool == "hiveRun", "{tool} {args} from {callers:?}"),
                Route::Window => unreachable!(),
            }
            reached.push((format!("{tool} {base_args} {callers:?} {named:?}"), r));
        }
    }
    // The rows a worker reaches the queue by, and only those.
    for (what, r) in &reached {
        let reaches = !matches!(r, Route::Refused(_));
        let queue_path = ["startService", "restartService", "stopService", "hiveRun"].iter().any(|t| what.starts_with(&format!("{t} ")));
        assert!(!reaches || queue_path, "{what}: {r:?}");
    }

    // The Overlord's tools aimed at a Hive tab, from an ordinary agent too (§7.3).
    for tool in ["driveTab", "answerTabPrompt", "recoverTab", "resumeTab"] {
        let r = route(&rig.data.read(), &[PLAIN_TAB], tool, &json!({ "tab_id": WORKER_TAB }));
        assert!(matches!(&r, Route::Refused(m) if m.contains("reason 'hive'")), "{tool}: {r:?}");
    }

    // ── hiveRun: its own validation, then the job line ──
    let stack = rig.stack();
    let run_job = |cmd: &str| -> Result<Job, String> {
        let a = run::RunArgs::parse(&json!({ "command": cmd }))?;
        let spec = run::build_spec(&rig.caller(), &rig.w, &a, &stack)?;
        Queue::default().enqueue(spec, 0)
    };
    for bad in ["cargo test; touch /tmp/spike15", "cargo test $(touch /tmp/spike15)", "cargo test `id`", "cargo test > /etc/x", "cargo test\ntouch /tmp/x"] {
        assert!(run_job(bad).unwrap_err().contains("hiveRun refuses"), "{bad:?}");
    }
    for unlisted in ["touch /tmp/spike15", "/usr/bin/cargo test", "cargo testx", "npm run tauri:dev", "sh -c cargo"] {
        assert!(run_job(unlisted).unwrap_err().contains("isn't a queued command"), "{unlisted:?}");
    }
    let job = run_job("cargo test -p core").unwrap();
    let words_of = |s: &'static str| move |_: &[u16]| s.split_whitespace().map(str::to_string).collect::<Vec<_>>();
    if let Some(profile) = assert_sandboxed(&rig, &job, &words_of("cargo test -p core")) {
        assert!(!profile.contains("network-bind"), "cargo test was booked no port: {profile}");
        assert!(profile.contains(&format!("/Library/Caches/spike15-{SLUG}\"")), "the entry's extra write, filled for the worker: {profile}");
    }
    // The pilot: a worker's own isolated maiTerm, on its own ports and folders, never dev's, named
    // so `dev-isolated.mjs` takes it although the slug is over 32.
    let job = run_job("npm run tauri:dev:isolated").unwrap();
    let instance = crate::instance::name_for_worker(SLUG);
    assert!(crate::instance::valid_name(&instance), "{instance}");
    if let Some(profile) = assert_sandboxed(&rig, &job, &words_of("npm run tauri:dev:isolated")) {
        for d in ["Application Support", "WebKit", "Caches", "Logs"] {
            assert!(profile.contains(&format!("/Library/{d}/com.aiterm.dev.{instance}\"")), "{d}: {profile}");
        }
        assert!(!profile.contains(&format!("com.aiterm.dev.{SLUG}")), "never a folder for an instance that can't exist: {profile}");
        assert!(!profile.contains("com.aiterm.dev\""), "never dev's own folders: {profile}");
        assert_eq!(profile.matches("network-bind").count(), 2, "its vite and MCP ports: {profile}");
    }
    // A package install the human's lockfile names (first real use): queued for the worker
    // although the human queued none, run in its profile with its manager's caches and the
    // registry proxy, and refused in its own shell, which can't install.
    assert!(run_job("pnpm install").unwrap_err().contains("isn't a queued command"), "no lockfile, no install");
    fs::write(rig.base.join("web/pnpm-lock.yaml"), "").unwrap();
    let job = run_job("pnpm install --frozen-lockfile").unwrap();
    assert!(job.registries);
    if let Some(profile) = assert_sandboxed(&rig, &job, &words_of("pnpm install --frozen-lockfile")) {
        assert!(profile.contains("deps-cache"), "its own cache folder: {profile}");
        assert!(!profile.contains("/Library/pnpm") && !profile.contains("/Library/Caches/pnpm"), "never the human's pnpm caches: {profile}");
        assert!(!profile.contains("network-bind"), "{profile}");
    }
    assert!(run::queued_prefixes(&rig.hive).contains(&vec!["pnpm".to_string(), "install".to_string()]), "the worker's shell sends it to hiveRun");
    assert!(!run_job("cargo test -p core").unwrap().registries, "a test run reaches no registry");
    fs::remove_file(rig.base.join("web/pnpm-lock.yaml")).unwrap();

    // ── A worker's service verbs: through the real routing into the queue ──
    let verb = |tool: &'static str, args: Value| {
        let (c, data) = (rig.caller(), &rig.data);
        async move { run::service_verb(tool, &args, &c, data).await }
    };
    for (name, says) in [("fixed", "hard-codes its port"), ("made", "made or changed by an agent"), ("docs", "made or changed by an agent"), ("db", "shared from another workspace"), ("nope", "no service")] {
        let e = verb("startService", json!({ "service": name })).await.unwrap_err();
        assert!(e.contains(says), "{name}: {e}");
    }
    for on in ["main", "hive/s--other", "#3"] {
        let e = verb("restartService", json!({ "service": "web", "on": on })).await.unwrap_err();
        assert!(e.contains("only on its own branch"), "{on}: {e}");
    }
    let e = verb("stopService", json!({ "service": "web" })).await.unwrap_err();
    assert!(e.contains("no copy of web running"), "{e}");

    let started = verb("startService", json!({ "service": "web" })).await.unwrap();
    let id = started["job"].as_str().unwrap().to_string();
    let job = queue::job(&id).unwrap();
    assert_eq!(job.cwd.as_deref().map(PathBuf::from), Some(rig.wt.join("app")), "re-rooted in the worker's worktree, never the human's checkout");
    assert_sandboxed(&rig, &job, &|ports: &[u16]| {
        assert_eq!(ports.len(), 1, "its {{port}}");
        vec!["/bin/sh".into(), "-c".into(), format!("npm run dev -- --port {}", ports[0])]
    });

    let e = verb("startService", json!({ "service": "web" })).await.unwrap_err();
    assert!(e.contains("already") && e.contains(&id), "{e}");
    let restarted = verb("restartService", json!({ "service": "web", "on": SLUG })).await.unwrap();
    let id2 = restarted["job"].as_str().unwrap().to_string();
    assert_ne!(id2, id, "a restart is a new job");
    assert!(!queue::job(&id).is_some_and(|j| j.is_active()), "the old copy's job is stopped");
    let stopped = verb("stopService", json!({ "service": "web", "on": BRANCH })).await.unwrap();
    assert_eq!((stopped["job"].as_str(), stopped["stopped"].as_bool()), (Some(id2.as_str()), Some(true)));
    assert!(!queue::job(&id2).is_some_and(|j| j.is_active()));

    // maiTerm's own git on worker content runs no hook and no fsmonitor, on the git dir it recorded.
    let args = |c: &Command| c.get_args().map(|a| a.to_string_lossy().to_string()).collect::<Vec<_>>();
    let a = args(&super::git::git_in(&rig.wt));
    assert!(a.windows(2).any(|p| p == ["-c", "core.fsmonitor=false"]) && a.windows(2).any(|p| p == ["-c", "core.hooksPath=/dev/null"]), "{a:?}");
    let a = args(&super::git::git_in_worktree(&rig.w.folders[0]).unwrap());
    assert!(a.contains(&format!("--git-dir={}", rig.w.folders[0].admin_dir.as_deref().unwrap())), "{a:?}");
}
