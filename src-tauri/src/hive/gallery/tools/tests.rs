//! The gallery's agent tools against a scratch Hive folder, its own gallery and a stub window.

use std::fs;
use std::sync::{Arc as StdArc, Mutex, MutexGuard};

use parking_lot::RwLock;

use super::*;
use crate::hive::deliver::Sent;
use crate::hive::fuses::{Fuses, FUSES_FILE};
use crate::hive::rpc::{Front, Outcome};
use crate::hive::worker::WorkerFolder;
use crate::state::{Tab, WindowData, Workspace};

#[derive(Default)]
struct WinState {
    gallery_changed: Vec<Vec<String>>,
    decisions_changed: usize,
}

#[derive(Clone, Default)]
struct Win(StdArc<Mutex<WinState>>);

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
    fn deliver(&self, _tab: &str, _text: String, _what: &'static str) -> impl std::future::Future<Output = Sent> + Send {
        async { Sent::Delivered }
    }
    fn front(&self, _tab: &str) -> impl std::future::Future<Output = Front> + Send {
        async { Front::Job }
    }
    fn stop_agent(&self, _tab: &str) -> impl std::future::Future<Output = Result<bool, String>> + Send {
        async { Ok(false) }
    }
    fn decisions_changed(&self, _workspace_id: &str) {
        self.st().decisions_changed += 1;
    }
    fn gallery_changed(&self, _caller: &Caller, ids: &[String]) {
        self.st().gallery_changed.push(ids.to_vec());
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

/// A Hive with a coordinator, two workers (#1 hero, #2 logo, each a scratch and a worktree
/// folder), a human checkout beside it, a temp folder and one output folder the human listed.
struct Rig {
    base: PathBuf,
    data: RwLock<AppData>,
    fuses: &'static Fuses,
    win: Win,
    paths: HivePaths,
    hive_id: String,
    coord: String,
    w1: String,
    w2: String,
    plain: String,
    temp: PathBuf,
    output: PathBuf,
    checkout: PathBuf,
}

impl Rig {
    fn new() -> Self {
        let id = uuid::Uuid::new_v4().simple().to_string();
        let base = std::env::temp_dir().join(format!("hive-gallery-tools-{id}"));
        fs::create_dir_all(&base).unwrap();
        let base = crate::share::canon(&base);
        let hive_id = format!("h-{id}");
        let paths = HivePaths::new(base.join("hive").join(&hive_id));
        let (temp, output, checkout) = (base.join("tmp"), base.join("comfy-out"), base.join("web"));
        for d in [paths.coordinator_home(), paths.coordinator_tmp(), paths.tasks(), temp.clone(), output.clone(), checkout.clone()] {
            fs::create_dir_all(&d).unwrap();
        }
        let (coord, w1, w2, plain) = (format!("coord-{id}"), format!("w1-{id}"), format!("w2-{id}"), format!("plain-{id}"));
        for (task, slug, badge, title, t) in [("t-hero", "hero", 1, "Hero", &w1), ("t-logo", "logo", 2, "Logo", &w2)] {
            let wt = paths.worker_worktree(slug, "web");
            fs::create_dir_all(&wt).unwrap();
            fs::create_dir_all(paths.worker_scratch(slug)).unwrap();
            let w = WorkerRecord {
                task_id: task.into(),
                slug: slug.into(),
                title: title.into(),
                role: "image".into(),
                badge,
                phase: worker::PHASE_RUNNING.into(),
                tab_id: Some(t.clone()),
                folders: vec![WorkerFolder { name: "web".into(), kind: "git".into(), worktree: Some(wt.to_string_lossy().into()), branch: Some(format!("hive/s--{slug}")), ..WorkerFolder::default() }],
                ..WorkerRecord::default()
            };
            let dir = paths.task(task);
            fs::create_dir_all(&dir).unwrap();
            worker::write_worker(&dir, &w).unwrap();
        }
        let mut hive = Hive { id: hive_id.clone(), slug: "s".into(), goal: "Make the launch art".into(), budget_cents: 100, ..Hive::default() };
        hive.checkpoints = crate::hive::create::default_checkpoints(&hive.landing_mode);
        hive.settings.gallery.output_folders = vec![output.to_string_lossy().to_string()];
        let mut ws = Workspace::new("Launch art".into());
        ws.hive = Some(hive);
        ws.panes[0].tabs = vec![tab(&coord, Some(ROLE_COORDINATOR)), tab(&w1, Some(ROLE_WORKER)), tab(&w2, Some(ROLE_WORKER))];
        let mut other = Workspace::new("Plain".into());
        other.panes[0].tabs = vec![tab(&plain, None)];
        let mut win = WindowData::new("main".into());
        win.workspaces = vec![ws, other];
        Self {
            fuses: Box::leak(Box::new(Fuses::at(base.join(FUSES_FILE)))),
            data: RwLock::new(AppData { windows: vec![win], ..AppData::default() }),
            win: Win::default(),
            base,
            paths,
            hive_id,
            coord,
            w1,
            w2,
            plain,
            temp,
            output,
            checkout,
        }
    }

    fn ctx(&self) -> Ctx<'_, Win> {
        Ctx { data: &self.data, fuses: self.fuses, window: self.win.clone(), save: ok_save, preflight: ok_preflight }
    }

    async fn call(&self, tab: &str, tool: &str, args: Value) -> Result<Value, String> {
        let (mut caller, role) = resolve(&self.data.read(), tab)?;
        // The state names the Hive's folder under app data; this one is scratch.
        caller.paths = self.paths.clone();
        super::call(&self.ctx(), caller, role, tool, &args, vec![self.temp.clone()]).await
    }

    fn set_checkpoint(&self, key: &str, value: &str) {
        self.data.write().windows[0].workspaces[0].hive.as_mut().unwrap().checkpoints.insert(key.into(), value.into());
    }

    fn gallery(&self) -> Arc<Gallery> {
        crate::hive::gallery::for_paths(&self.hive_id, &self.paths).unwrap()
    }

    fn scratch(&self, slug: &str) -> PathBuf {
        self.paths.worker_scratch(slug)
    }

    /// A file with these bytes, made at `dir/name`.
    fn make(&self, dir: &Path, name: &str, bytes: &str) -> PathBuf {
        let p = dir.join(name);
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        fs::write(&p, bytes).unwrap();
        p
    }

    fn changed(&self) -> usize {
        self.win.st().gallery_changed.len()
    }
}

impl Drop for Rig {
    fn drop(&mut self) {
        crate::hive::gallery::forget(&self.hive_id);
        let _ = fs::remove_dir_all(&self.base);
    }
}

fn first_id(v: &Value) -> String {
    v["assets"][0]["id"].as_str().unwrap_or_else(|| panic!("no asset in {v}")).to_string()
}

#[tokio::test]
async fn only_hive_agents_reach_the_gallery_and_only_the_coordinator_collects() {
    let r = Rig::new();
    let d = r.data.read().clone();
    let refused = |callers: &[&str], tool: &str| crate::hive::refuse_agent_tool(&d, callers, tool);
    for tool in GALLERY_TOOLS {
        assert!(refused(&[&r.plain], tool).is_some(), "an ordinary agent: {tool}");
        assert!(refused(&[], tool).is_some(), "no identity: {tool}");
        assert!(refused(&[&r.coord, &r.w1], tool).is_some(), "two identities that disagree: {tool}");
        assert!(refused(&[&r.coord], tool).is_none(), "the coordinator: {tool}");
    }
    for tool in WORKER_GALLERY_TOOLS {
        assert!(refused(&[&r.w1], tool).is_none(), "a worker: {tool}");
        assert!(refused(&[&r.w1, &r.plain], tool).is_some(), "a worker naming an ordinary tab: {tool}");
    }
    for tool in [COLLECT, DELIVER] {
        assert!(refused(&[&r.w1], tool).unwrap().contains("coordinator's tool"), "{tool}");
    }
    assert!(refused(&[&r.coord], "galleryWipe").unwrap().contains("isn't a maiTerm tool"));
    assert!(crate::hive::settings::worker_denied_mcp_tools().contains(&COLLECT), "a worker's settings deny it too");
    assert!(!crate::hive::settings::worker_denied_mcp_tools().contains(&ADD));

    // tools/list: the gallery only for Hive roles, collect for the coordinator only, deliver not
    // yet for anyone (step 8e).
    let names = |role: &str| tool_definitions(role).iter().map(|t| t["name"].as_str().unwrap().to_string()).collect::<Vec<_>>();
    assert_eq!(names(ROLE_WORKER), WORKER_GALLERY_TOOLS.to_vec());
    assert_eq!(names(ROLE_COORDINATOR), COORDINATOR_GALLERY_TOOLS.to_vec());
    assert!(names("visitor").is_empty());

    // Rust refuses on its own, past the server's check.
    assert!(r.call(&r.plain, LIST, json!({})).await.unwrap_err().contains("coordinator or its workers"));
    assert!(r.call(&r.w1, COLLECT, json!({ "collection": "x" })).await.unwrap_err().contains("coordinator's"));
    assert!(r.call(&r.coord, DELIVER, json!({})).await.unwrap_err().contains("isn't available"));
}

#[tokio::test]
async fn a_worker_files_from_its_own_folders_as_itself_and_nowhere_else() {
    let r = Rig::new();
    let sc = r.scratch("hero");
    r.make(&sc, "renders/hero.png", "png 1");
    let wt = r.paths.worker_worktree("hero", "web");
    r.make(&wt, "shot.png", "png 2");
    let tmp = r.make(&r.temp, "frame.png", "png 3");
    let out = r.make(&r.output, "comfy_0001.png", "png 4");

    // Relative to its working folder (its worktree), and the forged identity is ignored.
    let v = r
        .call(&r.w1, ADD, json!({ "path": "../scratch/renders/hero.png", "title": "Hero v1", "task_id": "t-logo", "made_by": "human", "recipe": { "tool": "fal", "model": "flux", "prompt": "a hero", "seed": 7 } }))
        .await
        .unwrap();
    let hero = first_id(&v);
    let a = r.gallery().get(&hero).unwrap().unwrap();
    assert_eq!(a.task_id.as_deref(), Some("t-hero"), "from the tab, never the arguments");
    assert_eq!((a.made_by.kind.as_str(), a.made_by.badge, a.made_by.worker_title.as_str(), a.made_by.tab_id.as_str()), (BY_WORKER, 1, "Hero", r.w1.as_str()));
    assert_eq!((a.kind.as_str(), a.thumb.as_str()), ("image", THUMB_PENDING));
    assert_eq!(a.recipe.as_ref().unwrap().seed, Some(7));
    assert_eq!(v["assets"][0]["path"].as_str().unwrap(), r.gallery().file_path(&a).to_string_lossy());
    assert_eq!(r.changed(), 1);

    for p in [wt.join("shot.png"), tmp, out] {
        r.call(&r.w1, ADD, json!({ "path": p, "title": "ok" })).await.unwrap_or_else(|e| panic!("{p:?}: {e}"));
    }
    assert_eq!(r.changed(), 4, "one event per call");

    // The human's checkout, another worker's scratch, the gallery itself and tasks/ are not its.
    let theirs = r.make(&r.scratch("logo"), "logo.png", "png 5");
    let human = r.make(&r.checkout, "README.md", "readme");
    let task = r.make(&r.paths.task("t-hero"), "notes.txt", "x");
    let stored = r.gallery().file_path(&a);
    for p in [human, theirs, task, stored.clone()] {
        let e = r.call(&r.w1, ADD, json!({ "path": p, "title": "no" })).await.unwrap_err();
        assert!(e.contains("isn't in a folder") || e.contains("in the gallery already"), "{p:?}: {e}");
    }
    let e = r.call(&r.w1, ADD, json!({ "path": r.checkout.join("README.md"), "title": "no" })).await.unwrap_err();
    assert!(e.contains("You file from:") && e.contains(&*sc.to_string_lossy()), "the refusal names its folders: {e}");
    assert_eq!(r.changed(), 4, "a refused call changes nothing");

    // The coordinator files from its home, not from a worker's scratch, as itself.
    let home = r.make(&r.paths.coordinator_home(), "moodboard.pdf", "%PDF");
    let v = r.call(&r.coord, ADD, json!({ "path": "moodboard.pdf", "title": "Mood" })).await.unwrap();
    let mood = r.gallery().get(&first_id(&v)).unwrap().unwrap();
    assert_eq!((mood.made_by.kind.as_str(), mood.task_id.clone(), mood.kind.as_str()), (BY_COORDINATOR, None, "document"));
    assert!(home.is_file(), "a copy, the source untouched");
    assert!(r.call(&r.coord, ADD, json!({ "path": sc.join("renders/hero.png"), "title": "no" })).await.is_err());
}

#[tokio::test]
async fn spends_duplicates_and_limits() {
    let r = Rig::new();
    let sc = r.scratch("hero");
    r.make(&sc, "a.png", "same bytes");

    // A discarded render: a ledger row, no asset.
    let v = r.call(&r.w1, ADD, json!({ "title": "Rejected take", "recipe": { "tool": "kling", "cost_usd": 0.25 } })).await.unwrap();
    assert_eq!(v["assets"], json!([]));
    assert_eq!(v["spends"][0]["cost_usd"], json!(0.25));
    assert_eq!(v["budget"]["spent_usd"], json!(0.25));
    assert_eq!(v["budget"]["budget_usd"], json!(1.0));
    assert!(r.gallery().list(&ListFilter::default()).unwrap().is_empty());
    assert!(r.call(&r.w1, ADD, json!({ "title": "Nothing" })).await.unwrap_err().contains("Nothing to file"));

    // The same bytes again for the same task: the asset already there, its cost still recorded.
    let first = first_id(&r.call(&r.w1, ADD, json!({ "path": sc.join("a.png"), "title": "A", "recipe": { "tool": "fal", "cost_usd": 0.5 } })).await.unwrap());
    r.make(&sc, "b.png", "same bytes");
    let v = r.call(&r.w1, ADD, json!({ "path": sc.join("b.png"), "title": "B", "recipe": { "tool": "fal", "cost_usd": 0.5 } })).await.unwrap();
    assert_eq!((first_id(&v), v["assets"][0]["duplicate"].clone()), (first.clone(), json!(true)));
    assert_eq!(fs::read_dir(r.gallery().files_dir()).unwrap().count(), 1, "the duplicate's copy is removed");
    assert_eq!(r.gallery().spent_micros().unwrap(), 1_250_000, "every render counts, kept or not");
    assert_eq!(v["budget"]["over"], json!(true));
    assert!(v["budget"]["rule"].as_str().unwrap().contains("no paid render"));
    // Another task's identical bytes are its own asset.
    r.make(&r.scratch("logo"), "a.png", "same bytes");
    assert_ne!(first_id(&r.call(&r.w2, ADD, json!({ "path": r.scratch("logo").join("a.png"), "title": "A" })).await.unwrap()), first);

    // Limits, before anything is filed.
    let many: Vec<Value> = (0..=MAX_ITEMS).map(|i| json!({ "title": format!("t{i}") })).collect();
    assert!(r.call(&r.w1, ADD, json!({ "items": many })).await.unwrap_err().contains("At most 20 items"));
    let bad = |args: Value, says: &'static str| {
        let e = parse_add(&args).unwrap_err();
        assert!(e.contains(says), "{says}: {e}");
    };
    bad(json!({ "path": "x" }), "title is required");
    bad(json!({ "title": "x".repeat(MAX_TITLE + 1) }), "one line of at most 200");
    bad(json!({ "title": "x", "kind": "hologram" }), "isn't one of");
    bad(json!({ "title": "x", "recipe": { "model": "m" } }), "recipe.tool");
    bad(json!({ "title": "x", "recipe": { "tool": "t", "prompt": "p".repeat(MAX_PROMPT + 1) } }), "recipe.prompt");
    bad(json!({ "title": "x", "recipe": { "tool": "t", "params": { "blob": "b".repeat(MAX_PARAMS_BYTES) } } }), "16 KB");
    bad(json!({ "title": "x", "recipe": { "tool": "t", "cost_usd": -1 } }), "cost_usd");
    bad(json!({ "title": "x", "recipe": { "tool": "t", "cost_usd": 20000 } }), "cost_usd");
    bad(json!({ "title": "x", "recipe": { "tool": "t", "source_url": "http://fal.media/x.png" } }), "https://");
    bad(json!({ "title": "x", "parents": vec!["a-0123456789ab"; MAX_PARENTS + 1] }), "At most 16");
    bad(json!({ "title": "x", "parents": ["hero.png"] }), "isn't an asset id");
    bad(json!({ "title": "x", "tags": vec!["t"; MAX_TAGS + 1] }), "At most 30");
    bad(json!({ "title": "x", "tags": ["t".repeat(MAX_TAG + 1)] }), "at most 40");
    bad(json!({ "title": "x", "items": [{ "title": "y" }] }), "not both");

    // Some items refused, the rest filed: one event, and the answer says which.
    let before = r.changed();
    let v = r
        .call(&r.w1, ADD, json!({ "items": [{ "path": sc.join("a.png"), "title": "again" }, { "path": "missing.png", "title": "gone" }, { "title": "x", "parents": ["a-0123456789ab"], "recipe": { "tool": "t", "cost_usd": 1 } }] }))
        .await
        .unwrap();
    assert_eq!(v["refused"], json!(2));
    assert_eq!(v["assets"][1]["item"], json!(2));
    assert!(v["assets"][2]["error"].as_str().unwrap().contains("No asset a-0123456789ab"), "{v}");
    assert_eq!(r.changed(), before + 1);
    let e = r.call(&r.w1, ADD, json!({ "items": [{ "path": "missing.png", "title": "a" }, { "path": "gone.png", "title": "b" }] })).await.unwrap_err();
    assert!(e.starts_with("Nothing was filed"), "{e}");
}

#[tokio::test]
async fn collections_are_the_coordinators_to_make_and_anyones_to_join() {
    let r = Rig::new();
    let sc = r.scratch("hero");
    r.make(&sc, "a.png", "a");
    r.make(&sc, "b.png", "b");
    let e = r.call(&r.w1, ADD, json!({ "path": sc.join("a.png"), "title": "A", "collection": "Shortlist" })).await.unwrap_err();
    assert!(e.contains("only your coordinator makes collections"), "{e}");
    assert!(r.gallery().list(&ListFilter::default()).unwrap().is_empty(), "nothing stored on a refusal");
    assert_eq!(fs::read_dir(r.gallery().files_dir()).unwrap().count(), 0);

    r.make(&r.paths.coordinator_home(), "ref.png", "ref");
    let refv = r.call(&r.coord, ADD, json!({ "path": "ref.png", "title": "Ref", "collection": "Shortlist" })).await.unwrap();
    let a = first_id(&r.call(&r.w1, ADD, json!({ "path": sc.join("a.png"), "title": "A", "collection": "shortlist", "parents": [first_id(&refv)] })).await.unwrap());
    let cols = r.gallery().collections().unwrap();
    assert_eq!(cols.len(), 1);
    assert_eq!(cols[0].asset_ids, vec![first_id(&refv), a.clone()], "a worker joins one that exists, by any case");

    let b = first_id(&r.call(&r.w1, ADD, json!({ "path": sc.join("b.png"), "title": "B" })).await.unwrap());
    let v = r.call(&r.coord, COLLECT, json!({ "collection": "Shortlist", "add": [b.clone()], "remove": [first_id(&refv)], "cover": b })).await.unwrap();
    assert_eq!(v["collection"]["assets"].as_array().unwrap().iter().map(|x| x["id"].as_str().unwrap().to_string()).collect::<Vec<_>>(), vec![a.clone(), b.clone()]);
    assert_eq!(v["collection"]["cover"], json!(b));
    let v = r.call(&r.coord, COLLECT, json!({ "collection": "Shortlist", "order": [b.clone(), a.clone()] })).await.unwrap();
    assert_eq!(v["collection"]["assets"][0]["id"], json!(b));
    assert!(r.call(&r.coord, COLLECT, json!({ "collection": "Shortlist", "order": [a.clone()] })).await.unwrap_err().contains("each of the collection's"));
    let v = r.call(&r.coord, COLLECT, json!({ "collection": "Storyboard v2", "add": [a] })).await.unwrap();
    assert_eq!(v["collection"]["name"], json!("Storyboard v2"), "made by name");
    r.gallery().trash(std::slice::from_ref(&b)).unwrap();
    assert!(r.call(&r.coord, COLLECT, json!({ "collection": "Storyboard v2", "add": [b] })).await.unwrap_err().contains("trash"));
}

#[tokio::test]
async fn who_may_set_what_status() {
    let r = Rig::new();
    let sc = r.scratch("hero");
    r.make(&sc, "a.png", "a");
    r.make(&sc, "b.png", "b");
    let a = first_id(&r.call(&r.w1, ADD, json!({ "path": sc.join("a.png"), "title": "A", "tags": ["Hero"] })).await.unwrap());
    let b = first_id(&r.call(&r.w1, ADD, json!({ "path": sc.join("b.png"), "title": "B" })).await.unwrap());

    // A worker tags, stars and picks any asset, another worker's too.
    let v = r.call(&r.w2, TAG, json!({ "id": a, "tags": ["Warm"], "untag": ["hero"], "starred": true, "status": "picked" })).await.unwrap();
    assert_eq!((v["assets"][0]["tags"].clone(), v["assets"][0]["status"].clone(), v["assets"][0]["starred"].clone()), (json!(["warm"]), json!("picked"), json!(true)));
    for s in ["final", "rejected"] {
        assert!(r.call(&r.w1, TAG, json!({ "id": a, "status": s })).await.unwrap_err().contains("you may set picked"), "{s}");
    }
    assert!(r.call(&r.w1, TAG, json!({ "id": a })).await.unwrap_err().contains("Nothing to change"));

    // The coordinator sets finals while Finals is its checkpoint: each change is its own decision.
    let decided = r.win.st().decisions_changed;
    r.call(&r.coord, TAG, json!({ "ids": [a.clone(), b.clone()], "status": "final" })).await.unwrap();
    assert_eq!(r.win.st().decisions_changed, decided + 1);
    let lines = decisions::read(&r.paths);
    let finals: Vec<&DecisionLine> = lines.iter().filter(|l| l.category == "finals").collect();
    assert_eq!(finals.len(), 2, "{lines:?}");
    assert!(finals.iter().all(|l| l.by == "coordinator" && l.answer.as_deref() == Some("final") && l.event == "answered"));
    assert!(decisions::fold(&lines).iter().all(|d| d.can_override()), "the human can override each");
    r.call(&r.coord, TAG, json!({ "id": a, "status": "final" })).await.unwrap();
    assert_eq!(decisions::read(&r.paths).len(), 2, "no change, no new decision");

    // A worker can't move a final, but may still tag it.
    assert!(r.call(&r.w1, TAG, json!({ "id": a, "status": "picked" })).await.unwrap_err().contains("is final"));
    r.call(&r.w1, TAG, json!({ "id": a, "tags": ["cover"] })).await.unwrap();

    // Moving an asset out of final is a finals decision too; moving between non-finals isn't.
    let decided = r.win.st().decisions_changed;
    r.call(&r.coord, TAG, json!({ "id": b, "status": "picked" })).await.unwrap();
    assert_eq!(r.win.st().decisions_changed, decided + 1);
    let lines = decisions::read(&r.paths);
    assert_eq!(lines.len(), 3, "{lines:?}");
    let out = lines.last().unwrap();
    assert_eq!((out.category.as_str(), out.by.as_str(), out.key.as_deref(), out.answer.as_deref()), ("finals", "coordinator", Some(b.as_str()), Some("picked")));
    assert!(out.question.contains("still final"), "{}", out.question);
    r.call(&r.coord, TAG, json!({ "id": b, "status": "" })).await.unwrap();
    assert_eq!(decisions::read(&r.paths).len(), 3, "picked to none is no finals call");
    r.call(&r.coord, TAG, json!({ "id": b, "status": "rejected" })).await.unwrap();
    r.call(&r.coord, TAG, json!({ "id": b, "status": "" })).await.unwrap();
    let lines = decisions::read(&r.paths);
    assert_eq!(lines.len(), 5, "{lines:?}");
    assert_eq!(lines.last().unwrap().answer.as_deref(), Some("no status"));
    assert!(decisions::fold(&lines).iter().all(|d| d.can_override()));

    // Under Ask me, finals are the human's: the coordinator neither sets nor moves one.
    r.set_checkpoint("finals", "ask");
    let e = r.call(&r.coord, TAG, json!({ "id": b, "status": "rejected" })).await.unwrap_err();
    assert!(e.contains("Picking finals is your human's in this Hive: ask with a decision blocker"), "{e}");
    assert!(r.call(&r.coord, TAG, json!({ "id": a, "status": "picked" })).await.is_err());
    r.call(&r.coord, TAG, json!({ "id": a, "starred": false })).await.unwrap();

    // Unknown and trashed assets refuse the whole change.
    assert!(r.call(&r.coord, TAG, json!({ "ids": [a.clone(), "a-0123456789ab"], "starred": true })).await.unwrap_err().contains("No asset"));
    r.gallery().trash(std::slice::from_ref(&b)).unwrap();
    assert!(r.call(&r.w1, TAG, json!({ "id": b, "starred": true })).await.unwrap_err().contains("in the trash"));
}

#[tokio::test]
async fn list_and_get_read_the_gallery_without_its_trash() {
    let r = Rig::new();
    r.make(&r.scratch("hero"), "a.png", "a");
    r.make(&r.scratch("logo"), "l.png", "l");
    let a = first_id(&r.call(&r.w1, ADD, json!({ "path": r.scratch("hero").join("a.png"), "title": "Hero sunrise", "recipe": { "tool": "fal", "prompt": "a sunrise over 100% blue", "cost_usd": 0.1 } })).await.unwrap());
    r.make(&r.scratch("hero"), "a2.png", "a2");
    let child = first_id(&r.call(&r.w1, ADD, json!({ "path": r.scratch("hero").join("a2.png"), "title": "Hero warmer", "parents": [a.clone()] })).await.unwrap());
    let logo = first_id(&r.call(&r.w2, ADD, json!({ "path": r.scratch("logo").join("l.png"), "title": "Logo" })).await.unwrap());

    let ids = |v: &Value| v["assets"].as_array().unwrap().iter().map(|x| x["id"].as_str().unwrap().to_string()).collect::<Vec<_>>();
    let all = r.call(&r.coord, LIST, json!({})).await.unwrap();
    assert_eq!(ids(&all), vec![logo.clone(), child.clone(), a.clone()], "newest first");
    assert_eq!(all["assets"][2]["by"], json!("#1 Hero"));
    assert_eq!(all["assets"][2]["cost_usd"], json!(0.1));
    assert_eq!(all["assets"][2]["children"], json!(1));
    assert_eq!(ids(&r.call(&r.w2, LIST, json!({ "made_by": "#1" })).await.unwrap()), vec![child.clone(), a.clone()]);
    assert_eq!(ids(&r.call(&r.w2, LIST, json!({ "made_by": 2 })).await.unwrap()), vec![logo.clone()]);
    assert_eq!(ids(&r.call(&r.w2, LIST, json!({ "task": "logo" })).await.unwrap()), vec![logo.clone()]);
    assert_eq!(ids(&r.call(&r.w2, LIST, json!({ "task": "t-hero", "made_by": "hero" })).await.unwrap()).len(), 2);
    assert!(ids(&r.call(&r.w2, LIST, json!({ "task": "logo", "made_by": "hero" })).await.unwrap()).is_empty(), "a contradiction matches nothing");
    assert_eq!(ids(&r.call(&r.w2, LIST, json!({ "query": "100%" })).await.unwrap()), vec![a.clone()], "the prompt, wildcards escaped");
    let one = r.call(&r.w2, LIST, json!({ "limit": 1 })).await.unwrap();
    assert_eq!((ids(&one).len(), one["more"].clone()), (1, json!(true)));
    assert!(r.call(&r.w2, LIST, json!({ "made_by": "#9" })).await.unwrap_err().contains("No worker"));
    assert!(r.call(&r.w2, LIST, json!({ "limit": 500 })).await.unwrap_err().contains("limit is 1 to 200"));

    let g = r.call(&r.w2, GET, json!({ "id": a })).await.unwrap();
    assert_eq!(g["recipe"]["prompt"], json!("a sunrise over 100% blue"));
    assert_eq!(g["children"], json!([{ "id": child, "title": "Hero warmer" }]));
    assert!(Path::new(g["path"].as_str().unwrap()).is_file());
    let c = r.call(&r.w2, GET, json!({ "id": child })).await.unwrap();
    assert_eq!(c["parents"][0]["title"], json!("Hero sunrise"));

    r.gallery().trash(std::slice::from_ref(&logo)).unwrap();
    assert!(!ids(&r.call(&r.coord, LIST, json!({ "include_trash": true })).await.unwrap()).contains(&logo), "never the trash");
    assert!(r.call(&r.coord, GET, json!({ "id": logo })).await.unwrap_err().contains("in the trash"));
    assert!(r.call(&r.coord, GET, json!({ "id": "../x" })).await.unwrap_err().contains("isn't an asset id"));
}

#[test]
fn roots_are_the_callers_own_and_never_a_folder_holding_the_hive() {
    let r = Rig::new();
    let hive = r.data.read().windows[0].workspaces[0].hive.clone().unwrap();
    let w = worker::worker_for_tab(&r.paths, &r.w1).unwrap();
    let roots = agent_roots(&hive, &r.paths, Some(&w), &[r.temp.clone(), r.base.clone()]);
    let c = |p: PathBuf| crate::share::canon(&p);
    for must in [c(r.scratch("hero")), c(r.paths.worker_worktree("hero", "web")), c(r.temp.clone()), c(r.output.clone())] {
        assert!(roots.contains(&must), "{must:?} in {roots:?}");
    }
    assert!(!roots.contains(&c(r.base.clone())), "a temp root holding the Hive's folder is dropped");
    assert!(!roots.contains(&c(r.scratch("logo"))));
    let coord = agent_roots(&hive, &r.paths, None, &[]);
    assert!(coord.contains(&c(r.paths.coordinator_home())) && !coord.contains(&c(r.scratch("hero"))));
}
