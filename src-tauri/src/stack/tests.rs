use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use super::target::{self, Context, GIT_CALLS};
use super::*;
use crate::hive::worker::{WorkerFolder, WorkerRecord};
use crate::share::canon;
use crate::state::workspace::ServiceRef;
use crate::state::{WindowData, Workspace};

fn scratch(tag: &str) -> PathBuf {
    let t = std::env::temp_dir().join(format!("stack-{tag}-{}", uuid::Uuid::new_v4()));
    fs::create_dir_all(&t).unwrap();
    canon(&t)
}

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

fn commit(repo: &Path, file: &str, body: &str) -> String {
    fs::write(repo.join(file), body).unwrap();
    git(repo, &["add", "-A"]);
    git(repo, &["commit", "-q", "--no-verify", "-m", body]);
    git(repo, &["rev-parse", "--short", "HEAD"])
}

/// `<base>/app` on main with a `web/` folder, and a `session-store` branch.
fn repo(base: &Path) -> PathBuf {
    let repo = base.join("app");
    fs::create_dir_all(repo.join("web")).unwrap();
    git(&repo, &["init", "-q", "-b", "main"]);
    fs::write(repo.join("web/package.json"), "{}").unwrap();
    commit(&repo, "README.md", "init");
    git(&repo, &["branch", "session-store"]);
    repo
}

fn service(cwd: &Path, command: &str, env: &[(&str, &str)]) -> Service {
    let mut s: Service = serde_json::from_value(serde_json::json!({
        "id": uuid::Uuid::new_v4().to_string(), "name": "web", "command": command, "cwd": cwd,
        "origin": "human", "created_at": "", "updated_at": ""
    }))
    .unwrap();
    s.env = env.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect();
    s
}

fn request(s: Service, target: &str) -> CopyRequest {
    CopyRequest {
        service: s,
        target: target.to_string(),
        shared_from_workspace: None,
        ports_of: Vec::new(),
        hive_root: None,
        forbidden_ports: ports::forbidden(None, None),
    }
}

fn ctx(base: &Path) -> Context {
    Context { workers: Vec::new(), hives: Some(base.join("hive")), stack_wt: Some(base.join("stack-wt.noindex")) }
}

fn git_calls() -> usize {
    GIT_CALLS.with(|c| c.get())
}

#[test]
fn a_legacy_service_resolves_exactly_as_defined_without_git() {
    // Not even a folder that exists: the shortcut reads nothing.
    let s = service(Path::new("/nowhere/at/all"), "npm run dev -- --open {a,b} ${HOME}", &[("NODE_ENV", "development"), ("weird key", "v")]);
    let before = git_calls();
    let r = resolve(&request(s.clone(), ""), &Context::default()).unwrap();
    assert_eq!(git_calls(), before, "the shortcut never runs git");
    assert_eq!(
        r,
        ResolvedCopy {
            copy_key: s.id.clone(),
            cwd: s.cwd.clone(),
            command: s.command.clone(),
            env: s.env.clone(),
            port: None,
            port2: None,
            branch: None,
            commit: None,
            checkout: None,
            worker: None,
            label: String::new(),
            one_copy: true,
        }
    );
    assert_eq!(copy_key(&s.id, ""), s.id, "its key is the service id the mirror always used");
}

#[test]
fn a_shared_service_or_an_unknown_kind_is_refused() {
    let mut s = service(Path::new("/a"), "x", &[]);
    s.shared_from = Some(ServiceRef { workspace_id: "w".into(), service_id: "s".into() });
    let mut req = request(s, "");
    req.shared_from_workspace = Some("Infra".into());
    assert_eq!(resolve(&req, &Context::default()).unwrap_err(), "web is shared from Infra; it's started there");

    let s = service(Path::new("/a"), "x", &[]);
    assert!(resolve(&request(s, "vm:alpine"), &Context::default()).unwrap_err().contains("isn't a place"));
    assert!(target::parse("checkout:relative/x").is_err());
}

#[test]
fn branch_names_with_shell_in_them_are_refused_before_git_sees_them() {
    let base = scratch("evil");
    let repo = repo(&base);
    // git itself takes `$`, `(`, `)` and backticks in a ref name.
    for evil in ["x$(id>p)", "x`id>p`", "a;b", "a'b", "a\"b"] {
        git(&repo, &["branch", evil]);
    }
    let s = service(&repo.join("web"), "npm run dev", &[]);
    for evil in ["x$(id>p)", "x`id>p`", "a;b", "a'b", "a\"b", "-rf", "a b", "x$(touch p)"] {
        let err = resolve(&request(s.clone(), &format!("branch:{evil}")), &ctx(&base)).unwrap_err();
        assert!(err.contains("can't be a branch"), "{evil}: {err}");
        assert!(target::target_for(evil, None, &ctx(&base)).is_err(), "{evil}");
    }
    assert!(!repo.join("p").exists() && !repo.join("web/p").exists());
    let _ = fs::remove_dir_all(&base);
}

#[test]
fn a_checkout_target_reroots_the_folder_and_refuses_one_that_isnt_there() {
    let base = scratch("checkout");
    let repo = repo(&base);
    let wt = base.join("wt-session");
    git(&repo, &["worktree", "add", "-q", wt.to_str().unwrap(), "session-store"]);
    let s = service(&repo.join("web"), "npm run dev", &[]);
    let wt = canon(&wt);

    let r = resolve(&request(s.clone(), &format!("checkout:{}", wt.display())), &ctx(&base)).unwrap();
    assert_eq!(PathBuf::from(&r.cwd), wt.join("web"));
    assert_eq!(r.branch.as_deref(), Some("session-store"));
    assert_eq!(r.label, "session-store");
    assert_eq!(r.commit.as_deref().map(str::len), Some(git(&wt, &["rev-parse", "--short", "HEAD"]).len()));
    assert_eq!(r.command, "npm run dev", "no placeholders, nothing filled");
    assert!(r.one_copy);

    // The branch never had web/: refused and named.
    git(&wt, &["rm", "-rq", "web"]);
    git(&wt, &["-c", "user.email=t@example.com", "-c", "user.name=t", "commit", "-q", "--no-verify", "-m", "drop web"]);
    let err = resolve(&request(s.clone(), &format!("checkout:{}", wt.display())), &ctx(&base)).unwrap_err();
    assert_eq!(err, "web/ doesn't exist on session-store");

    // A folder that isn't a worktree of this repo.
    let other = base.join("other");
    fs::create_dir_all(&other).unwrap();
    git(&other, &["init", "-q"]);
    assert!(resolve(&request(s, &format!("checkout:{}", canon(&other).display())), &ctx(&base)).unwrap_err().contains("isn't a worktree"));
    let _ = fs::remove_dir_all(&base);
}

#[test]
fn a_branch_that_is_checked_out_somewhere_is_used_as_it_is() {
    let base = scratch("reuse");
    let repo = repo(&base);
    let wt = base.join("human-wt");
    git(&repo, &["worktree", "add", "-q", wt.to_str().unwrap(), "session-store"]);
    // Tracked changes in the human's checkout are theirs: used as it is, never refused or moved.
    fs::write(wt.join("README.md"), "edited\n").unwrap();
    let s = service(&repo.join("web"), "npm run dev", &[]);
    let r = resolve(&request(s, "branch:session-store"), &ctx(&base)).unwrap();
    assert_eq!(PathBuf::from(&r.cwd), canon(&wt).join("web"));
    assert!(!base.join("stack-wt.noindex").exists(), "no checkout of maiTerm's own was made");
    let _ = fs::remove_dir_all(&base);
}

#[test]
fn a_branch_with_no_checkout_gets_a_detached_one_brought_to_its_tip() {
    let base = scratch("detached");
    let repo = repo(&base);
    let s = service(&repo.join("web"), "npm run dev", &[]);
    let c = ctx(&base);

    let first = resolve(&request(s.clone(), "branch:session-store"), &c).unwrap();
    let root = PathBuf::from(first.checkout.clone().unwrap());
    assert!(root.starts_with(canon(&base.join("stack-wt.noindex"))));
    assert_eq!(PathBuf::from(&first.cwd), root.join("web"));
    assert_eq!(git(&root, &["rev-parse", "--abbrev-ref", "HEAD"]), "HEAD", "detached");
    assert_eq!(git(&repo, &["rev-parse", "--abbrev-ref", "HEAD"]), "main", "the human's checkout never moved");

    // The branch moves on: the next start runs its new tip, in the same folder.
    git(&repo, &["checkout", "-q", "session-store"]);
    let tip = commit(&repo, "web/app.js", "new work");
    git(&repo, &["checkout", "-q", "main"]);
    let second = resolve(&request(s.clone(), "branch:session-store"), &c).unwrap();
    assert_eq!(second.checkout, first.checkout);
    assert_eq!(second.commit.as_deref(), Some(tip.as_str()));
    assert!(root.join("web/app.js").exists());

    // Tracked changes there refuse the start rather than being checked out over.
    fs::write(root.join("web/app.js"), "local edit").unwrap();
    let err = resolve(&request(s.clone(), "branch:session-store"), &c).unwrap_err();
    assert!(err.contains("changes to tracked files"), "{err}");
    // Untracked build output doesn't.
    git(&root, &["checkout", "--", "web/app.js"]);
    fs::write(root.join("web/dist.js"), "built").unwrap();
    assert!(resolve(&request(s.clone(), "branch:session-store"), &c).is_ok());

    assert!(resolve(&request(s, "branch:nope"), &c).unwrap_err().contains("no branch nope"));
    let _ = fs::remove_dir_all(&base);
}

#[test]
fn copies_starting_together_share_one_checkout_and_neither_destroys_it() {
    let base = scratch("race");
    let repo = repo(&base);
    let c = ctx(&base);
    let starts: Vec<_> = ["web", "api", "worker", "db"]
        .into_iter()
        .map(|name| {
            let mut s = service(&repo.join("web"), "npm run dev", &[]);
            s.name = name.to_string();
            let c = c.clone();
            std::thread::spawn(move || resolve(&request(s, "branch:session-store"), &c))
        })
        .collect();
    let roots: HashSet<String> = starts.into_iter().map(|t| t.join().unwrap().unwrap().checkout.unwrap()).collect();
    assert_eq!(roots.len(), 1, "{roots:?}");
    let root = PathBuf::from(roots.into_iter().next().unwrap());
    assert!(root.join("web/package.json").exists(), "the checkout is still there");
    assert_eq!(git(&repo, &["worktree", "list", "--porcelain"]).matches("worktree ").count(), 2, "registered once");
    let _ = fs::remove_dir_all(&base);
}

#[test]
fn placeholders_are_filled_per_copy_and_two_copies_get_two_ports() {
    let base = scratch("ph");
    let repo = repo(&base);
    let mut s = service(&repo.join("web"), "vite --port {port} --strictPort # {branch}", &[("DB", "postgres://localhost/app_{worker}"), ("ROOT", "{checkout}"), ("API", "http://localhost:{port:api}")]);
    s.port = Some(15173);
    let mut req = request(s.clone(), "");
    req.ports_of = vec![("api".into(), Some(18080))];
    let c = ctx(&base);

    let own = resolve(&req, &c).unwrap();
    let mut on_branch = req.clone();
    on_branch.target = "branch:session-store".into();
    let copy = resolve(&on_branch, &c).unwrap();

    assert!(!own.one_copy && !copy.one_copy);
    let (p1, p2) = (own.port.unwrap(), copy.port.unwrap());
    assert_ne!(p1, p2);
    assert!(p2 > 15173, "a copy never takes the own copy's address");
    assert_eq!(own.command, format!("vite --port {p1} --strictPort # main"));
    assert_eq!(copy.command, format!("vite --port {p2} --strictPort # session-store"));
    let env = |r: &ResolvedCopy, k: &str| r.env.iter().find(|(n, _)| n == k).unwrap().1.clone();
    assert_eq!(env(&own, "DB"), "postgres://localhost/app_main");
    assert_eq!(env(&copy, "DB"), "postgres://localhost/app_session-store");
    assert_eq!(env(&own, "ROOT"), canon(&repo).display().to_string());
    assert_eq!(env(&copy, "ROOT"), copy.checkout.clone().unwrap());
    assert_eq!(env(&copy, "API"), "http://localhost:18080");

    // A restart keeps its port; a release frees it.
    assert_eq!(resolve(&on_branch, &c).unwrap().port, Some(p2));
    assert!(ports::release(&copy.copy_key));
    assert!(ports::release(&own.copy_key));

    // {checkout} in a command, a bad env name, and an unknown or portless {port:x} are refused.
    let refused = |s: Service, ports_of: Vec<(String, Option<u16>)>| {
        let mut r = request(s, "");
        r.ports_of = ports_of;
        resolve(&r, &c).unwrap_err()
    };
    assert!(refused(service(&repo, "ls {checkout}", &[]), vec![]).contains("only be used in env"));
    assert!(refused(service(&repo, "x {port}", &[("A-B", "1")]), vec![]).contains("env name"));
    assert!(refused(service(&repo, "x {port:db}", &[]), vec![]).contains("no service called db"));
    assert!(refused(service(&repo, "x {port:db}", &[]), vec![("db".into(), None)]).contains("no port yet"));
    let _ = fs::remove_dir_all(&base);
}

/// A worker's worktree as `hiveSpawnWorker` makes it: under the Hive's folder, on its branch,
/// with its git dirs recorded.
fn worker(base: &Path, repo: &Path, slug: &str, badge: u32) -> WorkerRecord {
    let wt = base.join("hive/h1/wt.noindex").join(slug).join("app");
    fs::create_dir_all(wt.parent().unwrap()).unwrap();
    let branch = format!("hive/s--{slug}");
    git(repo, &["worktree", "add", "-q", "-b", &branch, wt.to_str().unwrap(), "main"]);
    let dirs = git(&wt, &["rev-parse", "--path-format=absolute", "--git-common-dir", "--git-dir"]);
    let dirs: Vec<String> = dirs.lines().map(|l| canon(Path::new(l.trim())).to_string_lossy().to_string()).collect();
    WorkerRecord {
        task_id: format!("t-{slug}"),
        slug: slug.into(),
        title: format!("Worker {slug}"),
        badge,
        folders: vec![WorkerFolder {
            name: "app".into(),
            kind: "git".into(),
            worktree: Some(canon(&wt).to_string_lossy().into()),
            branch: Some(branch),
            common_dir: Some(dirs[0].clone()),
            admin_dir: Some(dirs[1].clone()),
            ..WorkerFolder::default()
        }],
        ..WorkerRecord::default()
    }
}

#[test]
fn a_workers_worktree_runs_on_its_recorded_git_dir_never_its_git_file() {
    let base = scratch("worker");
    let repo = repo(&base);
    let w = worker(&base, &repo, "auth", 2);
    let wt = PathBuf::from(w.folders[0].worktree.clone().unwrap());
    let real_head = git(&wt, &["rev-parse", "--short", "HEAD"]);
    // The worker points its `.git` at a repo of its own, where HEAD is something else.
    let fake = base.join("fake");
    fs::create_dir_all(&fake).unwrap();
    git(&fake, &["init", "-q", "-b", "main"]);
    commit(&fake, "f", "forged");
    fs::write(wt.join(".git"), format!("gitdir: {}\n", canon(&fake.join(".git")).display())).unwrap();

    let mut c = ctx(&base);
    c.workers = vec![w.clone()];
    let mut s = service(&repo.join("web"), "vite --port {port} # {worker}", &[]);
    s.port = Some(25173);
    let target = target::target_for("auth", Some(&target::repo_of(&repo, &c).unwrap()), &c).unwrap();
    assert_eq!(target, format!("checkout:{}", wt.display()));
    assert_eq!(target::target_for("#2", Some(&target::repo_of(&repo, &c).unwrap()), &c).unwrap(), target, "by badge");
    let r = resolve(&request(s, &target), &c).unwrap();
    assert_eq!(r.commit.as_deref(), Some(real_head.as_str()), "the recorded git dir, not the forged one");
    assert_eq!(r.worker.as_deref(), Some("auth"));
    assert!(r.command.ends_with("# auth"));
    ports::release(&r.copy_key);

    // Not this workspace's worker: a Hive folder is refused outright, never run by path.
    let stranger = service(&repo.join("web"), "x", &[]);
    let err = resolve(&request(stranger.clone(), &format!("checkout:{}", wt.display())), &ctx(&base)).unwrap_err();
    assert!(err.contains("belongs to a Hive"), "{err}");
    // Nor does a service whose own folder is inside one get git run there.
    assert!(resolve(&request(service(&wt, "x {branch}", &[]), ""), &ctx(&base)).unwrap_err().contains("inside a Hive"));
    let _ = fs::remove_dir_all(&base);
}

#[test]
fn a_worker_claiming_another_branch_is_not_taken_for_it() {
    let base = scratch("claim");
    let repo = repo(&base);
    let w = worker(&base, &repo, "auth", 1);
    // The worker rewrites its own HEAD (its admin dir is writable to it) to claim session-store.
    let admin = PathBuf::from(w.folders[0].admin_dir.clone().unwrap());
    fs::write(admin.join("HEAD"), "ref: refs/heads/session-store\n").unwrap();
    let mut c = ctx(&base);
    c.workers = vec![w.clone()];
    let s = service(&repo.join("web"), "npm run dev", &[]);
    let r = resolve(&request(s, "branch:session-store"), &c).unwrap();
    let root = PathBuf::from(r.checkout.unwrap());
    assert!(root.starts_with(canon(&base.join("stack-wt.noindex"))), "maiTerm's own checkout, not the worker's: {}", root.display());
    let _ = fs::remove_dir_all(&base);
}

#[test]
fn run_on_lists_own_worktrees_workers_and_branches() {
    let base = scratch("targets");
    let repo = repo(&base);
    let human = base.join("human-wt");
    git(&repo, &["worktree", "add", "-q", "-b", "feature", human.to_str().unwrap(), "main"]);
    let w = worker(&base, &repo, "auth", 3);
    // A read copy: under the Hive, not a worker's. Left out.
    let read_copy = base.join("hive/h1/wt.noindex/coordinator/app");
    git(&repo, &["worktree", "add", "-q", "--detach", read_copy.to_str().unwrap(), "main"]);
    let mut c = ctx(&base);
    c.workers = vec![w];
    // maiTerm's own branch checkout: left out (it's offered as its branch).
    resolve(&request(service(&repo.join("web"), "x {port}", &[]), "branch:session-store"), &c).map(|r| ports::release(&r.copy_key)).unwrap();

    let list = target::copy_targets(Some(&target::repo_of(&repo.join("web"), &c).unwrap()), &c);
    let kinds: Vec<(&str, &str)> = list.iter().map(|t| (t.kind.as_str(), t.label.as_str())).collect();
    assert_eq!(list[0].target, "");
    assert_eq!(list[0].branch.as_deref(), Some("main"));
    assert!(kinds.contains(&("worktree", "feature")), "{kinds:?}");
    assert!(kinds.contains(&("worktree", "auth (worker 3)")), "{kinds:?}");
    assert_eq!(list.iter().filter(|t| t.kind == "worktree").count(), 2, "{kinds:?}");
    for b in ["main", "session-store", "feature", "hive/s--auth"] {
        assert!(list.iter().any(|t| t.kind == "branch" && t.target == format!("branch:{b}")), "{b}: {kinds:?}");
    }
    assert_eq!(target::target_for("main", None, &c).unwrap(), "");
    assert_eq!(target::target_for("session-store", None, &c).unwrap(), "branch:session-store");
    let _ = fs::remove_dir_all(&base);
}

#[test]
fn prune_removes_only_clean_untargeted_checkouts_of_deleted_branches() {
    let base = scratch("prune");
    let repo = repo(&base);
    let all = ["gone-clean", "gone-dirty", "gone-untracked", "gone-targeted", "alive"];
    for b in all {
        git(&repo, &["branch", b]);
    }
    fs::write(repo.join(".git/info/exclude"), "dist/\n").unwrap();
    let c = ctx(&base);
    let s = service(&repo.join("web"), "x", &[]);
    let root = |b: &str| PathBuf::from(resolve(&request(s.clone(), &format!("branch:{b}")), &c).unwrap().checkout.unwrap());
    let roots: HashMap<&str, PathBuf> = all.into_iter().map(|b| (b, root(b))).collect();
    fs::write(roots["gone-dirty"].join("README.md"), "edited").unwrap();
    // A file the human made there is work, whatever git tracks; ignored build output isn't.
    fs::write(roots["gone-untracked"].join(".env.local"), "SECRET=1").unwrap();
    fs::create_dir_all(roots["gone-clean"].join("dist")).unwrap();
    fs::write(roots["gone-clean"].join("dist/app.js"), "built").unwrap();
    for b in ["gone-clean", "gone-dirty", "gone-untracked", "gone-targeted"] {
        git(&repo, &["branch", "-D", b]);
    }
    let keep: HashSet<String> = ["branch:gone-targeted".to_string()].into();
    assert_eq!(target::prune(c.stack_wt.as_deref().unwrap(), &keep), 1);
    assert!(!roots["gone-clean"].exists());
    assert!(roots["gone-dirty"].exists() && roots["gone-targeted"].exists() && roots["alive"].exists());
    assert_eq!(fs::read_to_string(roots["gone-untracked"].join(".env.local")).unwrap(), "SECRET=1");
    assert!(!git(&repo, &["worktree", "list"]).contains("gone-clean"));
    let _ = fs::remove_dir_all(&base);
}

fn row(sid: &str, target: &str, status: &str, label: &str) -> StackRuntimeRow {
    serde_json::from_value(serde_json::json!({"service_id": sid, "target": target, "status": status, "label": label})).unwrap()
}

#[test]
fn the_priming_lists_one_entry_per_copy_and_is_unchanged_for_a_legacy_stack() {
    let mut data = AppData::default();
    let mut win = WindowData::new("main".into());
    let mut infra = Workspace::new("Infra's db".into());
    let mut db = service(Path::new("/db"), "postgres", &[]);
    db.name = "postgres".into();
    db.port = Some(5432);
    infra.stack.push(db.clone());
    let mut ws = Workspace::new("proj".into());
    let mut web = service(Path::new("/a"), "x", &[]);
    web.url = Some("http://localhost:5173".into());
    let mut api = service(Path::new("/a"), "y", &[]);
    api.name = "api".into();
    ws.stack.push(web.clone());
    ws.stack.push(api.clone());
    win.workspaces.push(infra.clone());
    win.workspaces.push(ws.clone());
    data.windows.push(win);

    // What a store that knows nothing of copies publishes: the line it always was.
    let mut runtime: HashMap<String, StackRuntimeRow> = HashMap::new();
    runtime.insert(web.id.clone(), row(&web.id, "", "ready", ""));
    assert_eq!(priming_entries(&data, &ws, &runtime).join(", "), "web (ready, http://localhost:5173), api (stopped)");

    runtime.insert(copy_key(&web.id, "branch:session-store"), row(&web.id, "branch:session-store", "running", "session-store"));
    runtime.insert(copy_key(&web.id, "branch:old"), row(&web.id, "branch:old", "stopped", ""));
    runtime.insert(copy_key(&api.id, "checkout:/x/wt.noindex/auth/app"), row(&api.id, "checkout:/x/wt.noindex/auth/app", "crashed", "auth"));
    runtime.insert(db.id.clone(), row(&db.id, "", "ready", ""));
    let mut shared = service(Path::new("/a"), "", &[]);
    shared.name = "db".into();
    shared.shared_from = Some(ServiceRef { workspace_id: infra.id.clone(), service_id: db.id.clone() });
    let mut ws2 = ws.clone();
    ws2.stack.push(shared);
    assert_eq!(
        priming_entries(&data, &ws2, &runtime).join(", "),
        "web (ready, http://localhost:5173), web on session-store (running), api (stopped), api on auth (crashed), db from Infra\u{2019}s db (ready, :5432)"
    );
}

#[test]
fn a_request_reads_ports_through_shared_from() {
    let mut data = AppData::default();
    let mut win = WindowData::new("main".into());
    let mut infra = Workspace::new("Infra".into());
    let mut db = service(Path::new("/db"), "postgres", &[]);
    db.port = Some(5432);
    infra.stack.push(db.clone());
    let mut ws = Workspace::new("proj".into());
    let mut shared = service(Path::new("/a"), "", &[]);
    shared.name = "DB".into();
    shared.port = Some(1);
    shared.shared_from = Some(ServiceRef { workspace_id: infra.id.clone(), service_id: db.id.clone() });
    let web = service(Path::new("/a"), "x {port:db}", &[]);
    ws.stack.push(shared.clone());
    ws.stack.push(web.clone());
    let ws_id = ws.id.clone();
    win.workspaces.push(infra);
    win.workspaces.push(ws);
    data.windows.push(win);

    let req = request_for(&data, "main", &ws_id, &web.id, "").unwrap();
    assert_eq!(req.ports_of.iter().find(|(n, _)| n == "db").map(|(_, p)| *p), Some(Some(5432)), "the source's port, not the reference's");
    assert_eq!(resolve(&req, &Context::default()).unwrap().command, "x 5432");
    let req = request_for(&data, "main", &ws_id, &shared.id, "").unwrap();
    assert_eq!(req.shared_from_workspace.as_deref(), Some("Infra"));
    assert!(request_for(&data, "main", &ws_id, "nope", "").is_err());
    assert!(request_for(&data, "other", &ws_id, &web.id, "").is_err());
}
