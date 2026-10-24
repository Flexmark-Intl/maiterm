//! The Hive's MCP tools (docs/hive.md §7.2), and who may call what.
//!
//! Every Hive tool is refused unless the calling tab's `hive_role` holds it, and a Hive agent is
//! refused the ordinary tools that reach outside its sandbox (`refuse_agent_tool`), in Rust,
//! whatever its settings say. `tools/list` offers a Hive tool only to the role that holds it, so
//! no other agent carries their schemas.
//!
//! `hiveSpawnWorker` is the riskiest path in the Hive: after its reservation, every failure
//! (the cap or a fuse, a worktree, the preflight, the window's refusal or silence, a save, an
//! expired reservation) takes back everything the spawn made (state rows, the window's half,
//! worktrees and branches, the task folder, the reservation), so nothing is left for a later
//! spawn to trip over.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use parking_lot::RwLock;
use serde_json::{json, Value};

use super::fuses::{Fuses, Reservation};
use super::paths::HivePaths;
use super::record::{Hive, STATUS_ACTIVE};
use super::rpc::{self, HiveWindow};
use super::worker::{self, WorkerFolder, WorkerRecord};
use super::{ROLE_COORDINATOR, ROLE_WORKER};
use crate::state::workspace::TaskNote;
use crate::state::{AppData, AppState, Tab, Task, Workstream};

/// To the Hive's window when its workers change; its board refetches.
pub const WORKERS_CHANGED_EVENT: &str = "hive-workers-changed";
/// A brief is the worker's whole task, like a Hive's Instructions (Projects' 16k).
pub const MAX_BRIEF: usize = 16_000;
pub const MAX_TITLE: usize = 120;
pub const EFFORTS: [&str; 5] = ["low", "medium", "high", "xhigh", "max"];

/// The Hive tools built so far: what `tools/list` offers each role and the handler answers.
/// The rest of the coordinator's (settings' `COORDINATOR_MCP_TOOLS`) are refused as not yet here.
pub const COORDINATOR_TOOLS: [&str; 2] = ["hiveSpawnWorker", "hiveListWorkers"];
/// A worker's own Hive tools (§7.2), built by later steps; the coordinator's are refused to it.
const WORKER_HIVE_TOOLS: [&str; 3] = ["hiveReport", "hiveRun", "hiveVerdict"];

/// Refused to a worker on top of its settings' denies (`worker_denied_mcp_tools`), spike 15's
/// table: rearranging the human's windows, mesh and bridge sends, and moving the human's view.
/// A worker talks to its coordinator, not to other agents (§5).
const WORKER_REFUSED_EXTRA: &[&str] = &[
    "moveTab",
    "moveWorkspace",
    "setMesh",
    "linkMeshes",
    "unlinkMesh",
    "bridgeWith",
    "disconnectBridge",
    "startTopic",
    "completeTopic",
    "switchTab",
    "reloadTab",
    "reloadWindow",
];

/// Which role holds a Hive tool, or None for a name no role holds.
pub fn hive_tool_role(tool: &str) -> Option<&'static str> {
    if super::settings::coordinator_only_mcp_tools().any(|t| t == tool && t.starts_with("hive")) {
        Some(ROLE_COORDINATOR)
    } else if WORKER_HIVE_TOOLS.contains(&tool) {
        Some(ROLE_WORKER)
    } else {
        None
    }
}

fn worker_refused(tool: &str) -> bool {
    super::settings::worker_denied_mcp_tools().contains(&tool) || WORKER_REFUSED_EXTRA.contains(&tool)
}

/// A live tab's `hive_role`, or None when no live tab has that id. A role without its Hive record
/// is still a Hive agent here: it is refused like one.
fn tab_role<'a>(data: &'a AppData, tab_id: &str) -> Option<Option<&'a str>> {
    data.windows
        .iter()
        .flat_map(|w| w.workspaces.iter())
        .flat_map(|ws| ws.panes.iter().flat_map(|p| p.tabs.iter()))
        .find(|t| t.id == tab_id)
        .map(|t| t.hive_role.as_deref())
}

/// Why `tool` is refused to its caller, or None. `callers` are every identity the request carries
/// for itself (the connection's tab, the tab its header names); ids that name no live tab are
/// ignored. A Hive tool needs every one of them to hold its role; a worker anywhere among them is
/// refused what reaches outside its sandbox (§7.2), and a coordinator `ESCAPE_MCP_TOOLS`.
pub fn refuse_agent_tool(data: &AppData, callers: &[&str], tool: &str) -> Option<String> {
    let roles: Vec<Option<&str>> = callers.iter().filter_map(|id| tab_role(data, id)).collect();
    if tool.starts_with("hive") {
        let Some(need) = hive_tool_role(tool) else {
            return Some(format!("{tool} isn't a maiTerm tool."));
        };
        if roles.is_empty() || roles.iter().any(|r| *r != Some(need)) {
            return Some(format!("{tool} is a Hive {need}'s tool: only a Hive's {need} can call it."));
        }
        return None;
    }
    // The Overlord is never a Hive agent: a Hive role among the callers of its tools (close,
    // archive, recover or resume the human's tabs) is one impersonating it.
    if roles.iter().any(Option::is_some) && crate::claude_code::server::OVERLORD_CALLER_TOOLS.contains(&tool) {
        return Some(format!("{tool} refused (reason 'hive'): it is the Overlord's tool, and a Hive agent is never the Overlord."));
    }
    if roles.contains(&Some(ROLE_WORKER)) && worker_refused(tool) {
        return Some(format!(
            "{tool} refused (reason 'hive'): a Hive worker works inside its own folders and sandbox. If you need this, \
             report it to your coordinator."
        ));
    }
    if roles.contains(&Some(ROLE_COORDINATOR)) && super::settings::ESCAPE_MCP_TOOLS.contains(&tool) {
        return Some(format!(
            "{tool} refused (reason 'hive'): a Hive coordinator acts through its Hive tools and its workers, never \
             outside the sandbox."
        ));
    }
    None
}

/// Why `initSession` may not bind this connection to `to`, or None. `own` are the identities the
/// connection already holds for itself (its header, its stated tab): a Hive agent may repair its
/// own identity, never take another tab's, which the Overlord's gate and every own-tab check
/// would then trust.
pub fn refuse_rebind(data: &AppData, own: &[&str], to: &str) -> Option<String> {
    let mine = own.iter().find(|id| **id != to && tab_role(data, id).flatten().is_some())?;
    Some(format!(
        "initSession refused (reason 'hive'): this connection is Hive tab {mine}'s, and a Hive agent can only initSession as \
         its own tab."
    ))
}

/// The role whose Hive tools `tools/list` offers this tab: its `hive_role`, in a Hive.
pub fn listed_role(data: &AppData, tab_id: &str) -> Option<&'static str> {
    let (_, tab) = super::find_hive_tab(data, tab_id)?;
    match tab.hive_role.as_deref()? {
        ROLE_COORDINATOR => Some(ROLE_COORDINATOR),
        ROLE_WORKER => Some(ROLE_WORKER),
        _ => None,
    }
}

/// The Hive tools `tools/list` carries for `role`. Built tools only: a worker has none yet.
pub fn tool_definitions(role: &str) -> Vec<Value> {
    if role != ROLE_COORDINATOR {
        return Vec::new();
    }
    let roles: Vec<&str> = super::roles::catalogue().iter().map(|r| r.name.as_str()).collect();
    let tab_id = json!({ "type": "string", "description": "Your own tab (auto-injected)" });
    let spawn = json!({
        "name": "hiveSpawnWorker",
        "description": format!(
            "Start a worker on one task toward this Hive's goal. maiTerm files the task on the board (the brief is its \
             detail), makes a worktree of each folder on the worker's own branch off its landing target, and starts a \
             sandboxed agent in a hidden tab that reads your brief first. Pick its role from the catalogue in your brief. \
             Refused above the Hive's cap, past the spawn fuses (shared by every Hive on this machine), or while a usage \
             limit or a pause holds; the answer says which. With depends_on, the worker waits as a task and a card, and \
             launches only once every dependency's task is done, branching from what has landed by then. A title whose \
             worker or branch already exists is refused: it names the branch. inputs and reviews aren't available yet. \
             Brief at most {MAX_BRIEF} characters."
        ),
        "inputSchema": {
            "type": "object",
            "properties": {
                "tabId": tab_id,
                "title": { "type": "string", "description": "Short title: the task's title, and the worker's branch name (slugged)" },
                "role": { "type": "string", "enum": roles, "description": "A role from the catalogue" },
                "brief": { "type": "string", "description": "The worker's typed brief: goal, context, checkable acceptance criteria, the evidence wanted, what to do when something is missing" },
                "folders": { "type": "array", "items": { "type": "string" }, "description": "Hive folder names it works in (default: the primary folder). [] gives it only its scratch folder" },
                "depends_on": { "type": "array", "items": { "type": ["string", "number"] }, "description": "Workers of this Hive whose work must be done first: badge (3 or \"#3\"), worker name or task id" },
                "model": { "type": "string", "description": "Model (default: the Hive's worker model)" },
                "effort": { "type": "string", "enum": EFFORTS, "description": "Effort (default: the Hive's worker effort)" },
                "inputs": { "type": "array", "items": { "type": "string" }, "description": "Not available yet (arrives with the gallery)" },
                "reviews": { "type": "string", "description": "Not available yet (arrives with reviews)" }
            },
            "required": ["title", "role", "brief"]
        }
    });
    let list = json!({
        "name": "hiveListWorkers",
        "description": "This Hive's workers as data: badge, name, task, phase, why a waiting one waits, folders with branches and \
            commits ahead of their landing target, model and effort, and the last report. Also your headroom: the cap, live \
            workers, spawns left this hour and today across every Hive, and any usage-limit pause.",
        "inputSchema": { "type": "object", "properties": { "tabId": tab_id }, "required": [] }
    });
    vec![spawn, list]
}

/// The coordinator a tool call comes from, with its Hive, cloned out of the state lock.
#[derive(Debug, Clone)]
pub(crate) struct Caller {
    pub tab_id: String,
    pub label: String,
    pub workspace_id: String,
    /// Its workers' tasks are filed under the Hive's name (§3).
    pub workstream: String,
    pub hive: Hive,
    pub paths: HivePaths,
}

fn resolve_caller(data: &AppData, tab_id: &str) -> Result<Caller, String> {
    for w in &data.windows {
        for ws in &w.workspaces {
            let Some(hive) = &ws.hive else { continue };
            let is_coordinator =
                ws.panes.iter().flat_map(|p| p.tabs.iter()).any(|t| t.id == tab_id && t.hive_role.as_deref() == Some(ROLE_COORDINATOR));
            if !is_coordinator {
                continue;
            }
            let root = super::paths::hive_root(&hive.id).ok_or_else(|| format!("\"{}\" can't name a Hive folder", hive.id))?;
            return Ok(Caller {
                tab_id: tab_id.to_string(),
                label: w.label.clone(),
                workspace_id: ws.id.clone(),
                workstream: ws.name.clone(),
                hive: hive.clone(),
                paths: HivePaths::new(root),
            });
        }
    }
    Err("Only a Hive's coordinator can call this, from its own tab.".to_string())
}

/// What a spawn needs besides its caller: production's state, fuses, window and checks, or a
/// test's.
pub(crate) struct Ctx<'a, W: HiveWindow> {
    pub data: &'a RwLock<AppData>,
    pub fuses: &'static Fuses,
    pub window: W,
    pub save: fn(&AppData) -> Result<(), String>,
    /// Checks the launch would make (Claude's version, repo trust), before the tab exists.
    pub preflight: fn(&HivePaths, &WorkerRecord) -> Result<(), String>,
}

pub async fn handle(tool: &str, args: &Value, state: &Arc<AppState>, handle: &tauri::AppHandle) -> Value {
    let caller_tab = args.get("tabId").and_then(Value::as_str).unwrap_or_default();
    let caller = match resolve_caller(&state.app_data.read(), caller_tab) {
        Ok(c) => c,
        Err(e) => return json!({ "error": e }),
    };
    let res = match tool {
        "hiveSpawnWorker" => {
            let window = rpc::AppWindow { app: state.clone(), handle: handle.clone(), label: caller.label.clone() };
            let ctx = Ctx {
                data: &state.app_data,
                fuses: super::fuses::book(),
                window,
                save: crate::state::save_state,
                preflight: super::launch::worker_preflight,
            };
            spawn_worker(&ctx, &caller, args).await
        }
        "hiveListWorkers" => list_workers_tool(&state.app_data, super::fuses::book(), &caller).await,
        t => Err(format!("{t} isn't available in this build of maiTerm yet.")),
    };
    res.unwrap_or_else(|e| {
        log::info!("hive {}: {tool} refused: {e}", caller.hive.id);
        json!({ "error": e })
    })
}

async fn blocking<T: Send + 'static>(f: impl FnOnce() -> Result<T, String> + Send + 'static) -> Result<T, String> {
    tokio::task::spawn_blocking(f).await.map_err(|e| format!("maiTerm's worker thread failed: {e}"))?
}

// ── hiveSpawnWorker ──

#[derive(Debug, Clone, Default)]
pub(crate) struct SpawnArgs {
    pub title: String,
    pub role: String,
    pub brief: String,
    pub folders: Option<Vec<String>>,
    pub depends_on: Vec<String>,
    pub model: Option<String>,
    pub effort: Option<String>,
}

fn present(v: Option<&Value>) -> bool {
    match v {
        None | Some(Value::Null) => false,
        Some(Value::String(s)) => !s.trim().is_empty(),
        Some(Value::Array(a)) => !a.is_empty(),
        Some(_) => true,
    }
}

impl SpawnArgs {
    pub(crate) fn parse(args: &Value) -> Result<Self, String> {
        let s = |k: &str| args.get(k).and_then(Value::as_str).map(str::trim).filter(|s| !s.is_empty()).map(str::to_string);
        if present(args.get("inputs")) {
            return Err("inputs arrive with the gallery, which this build doesn't have yet. Put any file paths the worker starts from in its brief.".to_string());
        }
        if present(args.get("reviews")) {
            return Err("reviews arrive with reviewers, which this build doesn't have yet. Spawn a code-reviewer with the branch named in its brief.".to_string());
        }
        let strings = |k: &str| -> Result<Option<Vec<String>>, String> {
            match args.get(k) {
                None | Some(Value::Null) => Ok(None),
                Some(Value::Array(a)) => a
                    .iter()
                    .map(|v| match v {
                        Value::String(s) => Ok(s.trim().to_string()),
                        Value::Number(n) if k == "depends_on" => Ok(format!("#{n}")),
                        _ => Err(format!("{k} must be a list of names")),
                    })
                    .collect::<Result<Vec<_>, _>>()
                    .map(Some),
                Some(_) => Err(format!("{k} must be a list")),
            }
        };
        Ok(Self {
            title: s("title").ok_or("title is required")?,
            role: s("role").ok_or("role is required")?,
            brief: args.get("brief").and_then(Value::as_str).map(str::trim).unwrap_or_default().to_string(),
            folders: strings("folders")?,
            depends_on: strings("depends_on")?.unwrap_or_default(),
            model: s("model"),
            effort: s("effort"),
        })
    }
}

/// A spawn's arguments, checked against the Hive and its workers.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Plan {
    pub title: String,
    pub slug: String,
    pub role: String,
    pub brief: String,
    pub folders: Vec<String>,
    /// Task ids.
    pub depends_on: Vec<String>,
    /// As given; empty takes the Hive's worker default at each launch.
    pub model: String,
    pub effort: String,
}

/// A worker of this Hive by badge (`3`, `#3`), name or task id.
fn find_worker<'a>(workers: &'a [WorkerRecord], name: &str) -> Option<&'a WorkerRecord> {
    let badge = name.strip_prefix('#').unwrap_or(name).parse::<u32>().ok();
    workers
        .iter()
        .find(|w| badge.is_some_and(|b| w.badge == b))
        .or_else(|| workers.iter().find(|w| w.slug == name || w.task_id == name))
}

pub(crate) fn validate(hive: &Hive, workers: &[WorkerRecord], a: SpawnArgs) -> Result<Plan, String> {
    let title: String = a.title.split_whitespace().collect::<Vec<_>>().join(" ");
    if title.chars().count() > MAX_TITLE {
        return Err(format!("The title is too long: keep it under {MAX_TITLE} characters (it names the branch)."));
    }
    let slug = worker::worker_slug(&title)?;
    if let Some(w) = workers.iter().find(|w| w.slug == slug) {
        return Err(format!(
            "Worker #{} is already called \"{slug}\" ({}), and the name is its branch's. Give this one another title.",
            w.badge, w.phase
        ));
    }
    if super::roles::role(&a.role).is_none() {
        let names: Vec<&str> = super::roles::catalogue().iter().map(|r| r.name.as_str()).collect();
        return Err(format!("There's no role \"{}\". The roles are: {}.", a.role, names.join(", ")));
    }
    if a.brief.is_empty() {
        return Err("The brief is empty: it's all the worker is told about its task.".to_string());
    }
    if a.brief.chars().count() > MAX_BRIEF {
        return Err(format!("The brief is over {MAX_BRIEF} characters: split the task, or move detail into a file in its folder."));
    }
    let folders = match a.folders {
        None => hive.primary().map(|f| vec![f.name.clone()]).unwrap_or_default(),
        Some(names) => {
            let mut out: Vec<String> = Vec::new();
            for n in names {
                if !hive.folders.iter().any(|f| f.name == n) {
                    let have: Vec<&str> = hive.folders.iter().map(|f| f.name.as_str()).collect();
                    return Err(format!("This Hive has no folder \"{n}\". Its folders are: {}.", have.join(", ")));
                }
                if out.contains(&n) {
                    return Err(format!("\"{n}\" is named twice in folders."));
                }
                out.push(n);
            }
            out
        }
    };
    let mut depends_on: Vec<String> = Vec::new();
    for d in &a.depends_on {
        let w = find_worker(workers, d).ok_or_else(|| format!("depends_on: this Hive has no worker \"{d}\" (use its badge, name or task id)."))?;
        if !depends_on.contains(&w.task_id) {
            depends_on.push(w.task_id.clone());
        }
    }
    let model = a.model.unwrap_or_default();
    if !model.is_empty() && (model.len() > 64 || !model.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'))) {
        return Err(format!("\"{model}\" isn't a model name: letters, digits, '.', '_' and '-' only."));
    }
    let effort = a.effort.unwrap_or_default();
    if !effort.is_empty() && !EFFORTS.contains(&effort.as_str()) {
        return Err(format!("effort must be one of {}.", EFFORTS.join(", ")));
    }
    Ok(Plan { title, slug, role: a.role, brief: a.brief, folders, depends_on, model, effort })
}

/// Badges and slugs are taken under this, from the records on disk, so two spawns at once never
/// share either. Held over small file reads and one write, never across an await.
static SPAWN_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// A worker on disk, `waiting` until its launch: its task folder and `worker.json`.
#[derive(Debug, Clone)]
pub(crate) struct Registered {
    pub worker: WorkerRecord,
    pub task_dir: PathBuf,
    pub brief: String,
    /// The dependencies not yet done, as the coordinator names them.
    pub unmet: Vec<String>,
}

fn worker_label(w: &WorkerRecord) -> String {
    format!("#{} {}", w.badge, w.slug)
}

pub(crate) fn register(hive: &Hive, paths: &HivePaths, a: SpawnArgs, tasks: &[Task], now: &str) -> Result<Registered, String> {
    let _held = SPAWN_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let workers = worker::list_workers(paths);
    let plan = validate(hive, &workers, a)?;
    if std::fs::symlink_metadata(paths.worker_dir(&plan.slug)).is_ok() {
        return Err(format!("A worker called \"{}\" already has folders here. Give this one another title.", plan.slug));
    }
    // "Met" is the dependency's task being done: landing sets that (step 6), and so can the human.
    let unmet: Vec<String> = plan
        .depends_on
        .iter()
        .filter(|id| tasks.iter().find(|t| &t.id == *id).is_none_or(|t| t.status != "done"))
        .filter_map(|id| workers.iter().find(|w| &w.task_id == id).map(worker_label))
        .collect();
    let task_id = uuid::Uuid::new_v4().to_string();
    let task_dir = worker::task_dir(paths, &task_id)?;
    let folders = plan
        .folders
        .iter()
        .map(|n| {
            let kind = hive.folders.iter().find(|f| f.name == *n).map(|f| f.kind.clone()).unwrap_or_default();
            WorkerFolder { name: n.clone(), kind, ..WorkerFolder::default() }
        })
        .collect();
    let w = WorkerRecord {
        task_id,
        slug: plan.slug,
        title: plan.title,
        role: plan.role,
        badge: worker::next_badge(&workers),
        phase: worker::PHASE_WAITING.to_string(),
        folders,
        model: plan.model,
        effort: plan.effort,
        depends_on: plan.depends_on,
        wait_reason: (!unmet.is_empty()).then(|| format!("waits on {}", unmet.join(", "))),
        created_at: now.to_string(),
        ..WorkerRecord::default()
    };
    worker::write_worker(&task_dir, &w).inspect_err(|_| {
        let _ = std::fs::remove_dir_all(&task_dir);
    })?;
    Ok(Registered { worker: w, task_dir, brief: plan.brief, unmet })
}

/// The worker's row on the board: assigned to its tab once it has one.
fn worker_task(w: &WorkerRecord, brief: &str, tab_id: Option<&str>, now: &str) -> Task {
    let note = match tab_id {
        Some(_) => format!("Worker #{} ({}) started by the coordinator", w.badge, w.role),
        None => format!("Worker #{} ({}) {}", w.badge, w.role, w.wait_reason.as_deref().unwrap_or("waiting")),
    };
    Task {
        id: w.task_id.clone(),
        title: w.title.clone(),
        normalized_title: Task::normalize_title(&w.title),
        detail: Some(brief.to_string()),
        status: if tab_id.is_some() { "active" } else { "todo" }.to_string(),
        tab_id: tab_id.map(str::to_string),
        blocked_by: w.depends_on.clone(),
        origin: "agent".to_string(),
        created_at: now.to_string(),
        updated_at: now.to_string(),
        workstream_id: None,
        notes: vec![TaskNote { at: now.to_string(), text: note, by: "agent".to_string() }],
        blocker: None,
    }
}

/// What a spawn put in the state, so a rollback takes exactly that out.
#[derive(Debug, Clone)]
struct Rows {
    task_id: String,
    tab_id: Option<String>,
    /// A workstream this spawn created, removed again when nothing else points at it.
    new_workstream: Option<String>,
}

fn workspace_mut<'a>(data: &'a mut AppData, caller: &Caller) -> Result<&'a mut crate::state::Workspace, String> {
    data.window_mut(&caller.label)
        .and_then(|w| w.workspaces.iter_mut().find(|ws| ws.id == caller.workspace_id))
        .filter(|ws| ws.hive.as_ref().is_some_and(|h| h.id == caller.hive.id))
        .ok_or_else(|| "This Hive is gone, so nothing was started.".to_string())
}

fn workspace_tasks(data: &RwLock<AppData>, caller: &Caller) -> Result<Vec<Task>, String> {
    let d = data.read();
    d.window(&caller.label)
        .and_then(|w| w.workspaces.iter().find(|ws| ws.id == caller.workspace_id))
        .map(|ws| ws.tasks.clone())
        .ok_or_else(|| "This Hive is gone.".to_string())
}

/// The task row (filed under the Hive's workstream) and the hidden tab, unsaved. The tab goes in
/// the first pane and is never made active: a Hive's pane has none (§5). Returns the rows as put
/// and the workstream, for the window.
fn insert_rows(data: &RwLock<AppData>, caller: &Caller, mut task: Task, tab: Option<&Tab>) -> Result<(Rows, Task, Workstream), String> {
    let mut d = data.write();
    let ws = workspace_mut(&mut d, caller)?;
    if ws.panes.is_empty() && tab.is_some() {
        return Err("This Hive has no pane to hold a worker's tab.".to_string());
    }
    let key = Workstream::normalize_name(&caller.workstream);
    let (stream, new_workstream) = match ws.workstreams.iter().find(|s| s.normalized_name == key) {
        Some(s) => (s.clone(), None),
        None => {
            let s = Workstream {
                id: uuid::Uuid::new_v4().to_string(),
                name: caller.workstream.trim().to_string(),
                normalized_name: key,
                created_at: task.created_at.clone(),
                updated_at: task.created_at.clone(),
            };
            ws.workstreams.push(s.clone());
            (s.clone(), Some(s.id))
        }
    };
    task.workstream_id = Some(stream.id.clone());
    ws.tasks.retain(|t| t.id != task.id);
    ws.tasks.push(task.clone());
    if let Some(tab) = tab {
        ws.panes[0].tabs.push(tab.clone());
    }
    Ok((Rows { task_id: task.id.clone(), tab_id: tab.map(|t| t.id.clone()), new_workstream }, task, stream))
}

fn remove_rows(data: &RwLock<AppData>, caller: &Caller, rows: &Rows) {
    let mut d = data.write();
    let Ok(ws) = workspace_mut(&mut d, caller) else { return };
    ws.tasks.retain(|t| t.id != rows.task_id);
    if let Some(tab) = &rows.tab_id {
        for p in &mut ws.panes {
            p.tabs.retain(|t| t.id != *tab);
            if p.active_tab_id.as_deref() == Some(tab.as_str()) {
                p.active_tab_id = None;
            }
        }
    }
    if let Some(id) = &rows.new_workstream {
        if !ws.tasks.iter().any(|t| t.workstream_id.as_deref() == Some(id.as_str())) {
            ws.workstreams.retain(|s| s.id != *id);
        }
    }
}

fn save<W: HiveWindow>(ctx: &Ctx<'_, W>) -> Result<(), String> {
    let snapshot = ctx.data.read().clone();
    (ctx.save)(&snapshot).map_err(|e| format!("maiTerm couldn't save the new worker ({e}), so it was taken back."))
}

/// Everything a spawn has made so far, taken back in reverse.
struct Undo {
    task_dir: PathBuf,
    task_id: String,
    slug: String,
    worktrees: Option<Vec<WorkerFolder>>,
    rows: Option<Rows>,
    /// Set once `hive.spawn` went out: whether the window answered it.
    sent: Option<bool>,
    reservation: Option<Reservation<'static>>,
}

impl Undo {
    fn new(r: &Registered) -> Self {
        Self {
            task_dir: r.task_dir.clone(),
            task_id: r.worker.task_id.clone(),
            slug: r.worker.slug.clone(),
            worktrees: None,
            rows: None,
            sent: None,
            reservation: None,
        }
    }

    /// Rolls the spawn back and returns `why`, with a note when something couldn't be removed.
    async fn run<W: HiveWindow>(self, ctx: &Ctx<'_, W>, caller: &Caller, why: String) -> String {
        let mut stayed: Vec<String> = Vec::new();
        if let Some(rows) = &self.rows {
            remove_rows(ctx.data, caller, rows);
            // Another write may have saved the rows meanwhile: the state on disk loses them too.
            let snapshot = ctx.data.read().clone();
            if let Err(e) = (ctx.save)(&snapshot) {
                log::warn!("hive {}: worker {} taken out of the state, not saved: {e}", caller.hive.id, self.slug);
            }
        }
        if let Some(answered) = self.sent {
            let args = json!({
                "workspace_id": caller.workspace_id,
                "task_id": self.task_id,
                "tab_id": self.rows.as_ref().and_then(|r| r.tab_id.clone()),
            });
            // A window that just answered takes the unspawn now. One that didn't answer may wake
            // later: it then handles the spawn and this after it, in order, so this isn't waited on.
            if answered {
                if let Err(e) = rpc::accepted(ctx.window.request(rpc::VERB_UNSPAWN, args).await) {
                    log::warn!("hive {}: worker {} unspawn: {e}", caller.hive.id, self.slug);
                }
            } else {
                let window = ctx.window.clone();
                tokio::spawn(async move {
                    let _ = window.request(rpc::VERB_UNSPAWN, args).await;
                });
            }
            ctx.window.workers_changed(&caller.workspace_id);
        }
        if let Some(folders) = self.worktrees {
            let (hive, paths, slug) = (caller.hive.clone(), caller.paths.clone(), self.slug.clone());
            if let Err(e) = blocking(move || super::worktree::remove_worker_worktrees(&hive, &paths, &slug, &folders)).await {
                log::warn!("hive {}: worker {} rollback: {e}", caller.hive.id, self.slug);
                stayed.push("some of its worktrees or branches (the log names them)".to_string());
            }
        }
        match std::fs::remove_dir_all(&self.task_dir) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => {
                log::warn!("hive {}: task folder {} left: {e}", caller.hive.id, self.task_dir.display());
                stayed.push("its task folder".to_string());
            }
        }
        drop(self.reservation);
        log::info!("hive {}: spawn of worker {} rolled back: {why}", caller.hive.id, self.slug);
        if stayed.is_empty() {
            why
        } else {
            format!("{why} (maiTerm couldn't remove {})", stayed.join(" or "))
        }
    }
}

/// `hiveSpawnWorker` (§7.2): register the worker, then launch it, or with unmet dependencies
/// file it as waiting (no reservation: a waiting worker holds no slot, §3.1).
pub(crate) async fn spawn_worker<W: HiveWindow>(ctx: &Ctx<'_, W>, caller: &Caller, args: &Value) -> Result<Value, String> {
    let a = SpawnArgs::parse(args)?;
    let tasks = workspace_tasks(ctx.data, caller)?;
    let (hive, paths) = (caller.hive.clone(), caller.paths.clone());
    let now = crate::commands::workspace::iso_now();
    let reg = blocking(move || register(&hive, &paths, a, &tasks, &now)).await?;
    if reg.unmet.is_empty() {
        launch_new_worker(ctx, caller, reg).await
    } else {
        file_waiting(ctx, caller, reg).await
    }
}

async fn file_waiting<W: HiveWindow>(ctx: &Ctx<'_, W>, caller: &Caller, reg: Registered) -> Result<Value, String> {
    let mut undo = Undo::new(&reg);
    let now = crate::commands::workspace::iso_now();
    let (rows, task, stream) = match insert_rows(ctx.data, caller, worker_task(&reg.worker, &reg.brief, None, &now), None) {
        Ok(r) => r,
        Err(e) => return Err(undo.run(ctx, caller, e).await),
    };
    undo.rows = Some(rows);
    let outcome = ctx
        .window
        .request(rpc::VERB_SPAWN, json!({ "workspace_id": caller.workspace_id, "task": task, "workstream": stream }))
        .await;
    undo.sent = Some(rpc::window_answered(&outcome));
    if let Err(e) = rpc::accepted(outcome).and_then(|_| save(ctx)) {
        return Err(undo.run(ctx, caller, e).await);
    }
    ctx.window.workers_changed(&caller.workspace_id);
    let w = &reg.worker;
    log::info!("hive {}: worker {} waits on {}", caller.hive.id, worker_label(w), reg.unmet.join(", "));
    Ok(json!({
        "worker": w.slug,
        "badge": w.badge,
        "task_id": w.task_id,
        "state": "waiting",
        "waits_on": reg.unmet,
        "note": "It launches once every dependency's task is done. Its worktrees are made then, from what has landed.",
    }))
}

/// A registered worker's first launch: reserve, worktrees, records, the hidden tab, the window,
/// then save and commit. Any failure takes all of it back (`Undo`).
async fn launch_new_worker<W: HiveWindow>(ctx: &Ctx<'_, W>, caller: &Caller, reg: Registered) -> Result<Value, String> {
    let mut undo = Undo::new(&reg);
    let mut w = reg.worker.clone();

    let (hive, paths, fuses, task_id) = (caller.hive.clone(), caller.paths.clone(), ctx.fuses, w.task_id.clone());
    let reserved = blocking(move || fuses.reserve(&hive, &task_id, || live_task_ids(&paths), super::fuses::now_ms()))
    .await;
    match reserved {
        Ok(r) => undo.reservation = Some(r),
        Err(e) => return Err(undo.run(ctx, caller, e).await),
    }

    // `create_worker_worktrees` takes back its own half-made folders when it fails.
    let (hive, paths, slug) = (caller.hive.clone(), caller.paths.clone(), w.slug.clone());
    let names: Vec<String> = w.folders.iter().map(|f| f.name.clone()).collect();
    match blocking(move || super::worktree::create_worker_worktrees(&hive, &paths, &slug, &names)).await {
        Ok(folders) => {
            undo.worktrees = Some(folders.clone());
            w.folders = folders;
        }
        Err(e) => return Err(undo.run(ctx, caller, e).await),
    }

    let mut tab = Tab::new(w.title.clone());
    tab.custom_name = true;
    tab.hive_role = Some(ROLE_WORKER.to_string());
    let now = crate::commands::workspace::iso_now();
    w.phase = worker::PHASE_LAUNCHING.to_string();
    w.tab_id = Some(tab.id.clone());
    w.launched_at = Some(now.clone());
    w.wait_reason = None;
    // Written `launching` before the commit, so the slot moves from pending to live with no gap.
    // The reservation is keyed by the task id, so meanwhile the worker counts once.
    let (hive, paths, task_dir, record, preflight) = (caller.hive.clone(), caller.paths.clone(), reg.task_dir.clone(), w.clone(), ctx.preflight);
    let written = blocking(move || {
        preflight(&paths, &record)?;
        super::launch::write_record(&task_dir, &super::launch::worker_record(&hive, &paths, &record))?;
        worker::write_worker(&task_dir, &record)
    })
    .await;
    if let Err(e) = written {
        return Err(undo.run(ctx, caller, e).await);
    }

    let (rows, task, stream) = match insert_rows(ctx.data, caller, worker_task(&w, &reg.brief, Some(&tab.id), &now), Some(&tab)) {
        Ok(r) => r,
        Err(e) => return Err(undo.run(ctx, caller, e).await),
    };
    undo.rows = Some(rows);
    let outcome = ctx
        .window
        .request(rpc::VERB_SPAWN, json!({ "workspace_id": caller.workspace_id, "task": task, "workstream": stream, "tab": tab }))
        .await;
    undo.sent = Some(rpc::window_answered(&outcome));
    if let Err(e) = rpc::accepted(outcome).and_then(|_| save(ctx)) {
        return Err(undo.run(ctx, caller, e).await);
    }
    let committed = undo.reservation.take().map_or(Ok(()), |r| r.commit(super::fuses::now_ms()));
    if let Err(e) = committed {
        return Err(undo.run(ctx, caller, e).await);
    }
    ctx.window.workers_changed(&caller.workspace_id);
    log::info!("hive {}: worker {} launching in tab {}", caller.hive.id, worker_label(&w), tab.id);
    let folders: Vec<Value> = w
        .folders
        .iter()
        .map(|f| json!({ "name": f.name, "kind": f.kind, "branch": f.branch, "worktree": f.worktree, "target": f.target }))
        .collect();
    Ok(json!({
        "worker": w.slug,
        "badge": w.badge,
        "task_id": w.task_id,
        "state": "launching",
        "folders": folders,
        "note": "It reads its brief first and reports with hiveReport. Its card shows its progress.",
    }))
}

// ── hiveListWorkers ──

/// The task ids of the Hive's workers in `LIVE_PHASES`, as the fuses count them. Blocks.
fn live_task_ids(paths: &HivePaths) -> Vec<String> {
    worker::list_workers(paths).into_iter().filter(|w| w.is_live()).map(|w| w.task_id).collect()
}

async fn list_workers_tool(data: &RwLock<AppData>, fuses: &'static Fuses, caller: &Caller) -> Result<Value, String> {
    let tasks = workspace_tasks(data, caller)?;
    let (hive, paths) = (caller.hive.clone(), caller.paths.clone());
    blocking(move || Ok(list_view(&hive, &paths, &tasks, fuses, super::fuses::now_ms()))).await
}

/// Commits on the worker's branch that its landing target doesn't have, or None when its
/// worktree or branch is gone.
fn commits_ahead(f: &WorkerFolder) -> Option<u64> {
    let (wt, branch) = (f.worktree.as_deref()?, f.branch.as_deref()?);
    if !Path::new(wt).is_dir() {
        return None;
    }
    let base = match (&f.target, &f.base_sha) {
        (Some(t), _) => format!("refs/heads/{t}"),
        (None, Some(sha)) => sha.clone(),
        (None, None) => return None,
    };
    let range = format!("{base}..refs/heads/{branch}");
    let out = super::git::run(super::git::git_in(Path::new(wt)).args(["rev-list", "--count", &range]), super::git::QUERY_TIMEOUT);
    out.ok()?.trim().parse().ok()
}

/// The workers as `hiveListWorkers` answers (§7.2) and the headroom it reports with them (§7.8).
/// Blocks: worker records and one git call per worktree.
pub(crate) fn list_view(hive: &Hive, paths: &HivePaths, tasks: &[Task], fuses: &Fuses, now: i64) -> Value {
    let workers = worker::list_workers(paths);
    let live: Vec<String> = workers.iter().filter(|w| w.is_live()).map(|w| w.task_id.clone()).collect();
    let by_task: HashMap<&str, &WorkerRecord> = workers.iter().map(|w| (w.task_id.as_str(), w)).collect();
    let or = |mine: &str, hive: &str| if mine.is_empty() { hive.to_string() } else { mine.to_string() };
    let rows: Vec<Value> = workers
        .iter()
        .map(|w| {
            let folders: Vec<Value> = w
                .folders
                .iter()
                .map(|f| json!({ "name": f.name, "kind": f.kind, "branch": f.branch, "target": f.target, "ahead": commits_ahead(f) }))
                .collect();
            let deps: Vec<String> = w.depends_on.iter().map(|id| by_task.get(id.as_str()).map_or_else(|| id.clone(), |d| worker_label(d))).collect();
            json!({
                "badge": w.badge,
                "worker": w.slug,
                "title": w.title,
                "task_id": w.task_id,
                "task_status": tasks.iter().find(|t| t.id == w.task_id).map(|t| t.status.clone()),
                "role": w.role,
                "phase": w.phase,
                "wait_reason": w.wait_reason,
                "depends_on": deps,
                "folders": folders,
                "model": or(&w.model, &hive.models.worker.model),
                "effort": or(&w.effort, &hive.models.worker.effort),
                "last_report": w.last_report,
            })
        })
        .collect();
    let h = fuses.headroom(hive, &live, now);
    json!({
        "workers": rows,
        "headroom": {
            "cap": h.cap,
            "live": h.live,
            "starting": h.pending,
            "spawns_left_this_hour": h.hour_left,
            "spawns_left_today": h.day_left,
            "usage_limit_until": h.breaker_until.map(super::fuses::reset_hhmm),
            "usage_limit_reason": h.breaker_reason,
        },
        "hive_status": hive.status,
        "starts_workers": hive.status == STATUS_ACTIVE && h.breaker_until.is_none(),
    })
}

// ── The Hive view's mount ──

/// The worker tabs the Hive view mounts (restart, reopen): those whose worker is in a live phase.
/// A stopped or failed one stays down until `hiveRestartWorker`; the builder refuses it anyway.
#[tauri::command]
pub async fn hive_live_worker_tabs(
    window: tauri::Window,
    state: tauri::State<'_, Arc<AppState>>,
    workspace_id: String,
) -> Result<Vec<String>, String> {
    let (hive_id, tabs) = {
        let data = state.app_data.read();
        let ws = data
            .window(window.label())
            .and_then(|w| w.workspaces.iter().find(|w| w.id == workspace_id))
            .ok_or("Workspace not found")?;
        let Some(hive) = &ws.hive else { return Ok(Vec::new()) };
        let tabs: Vec<String> = ws
            .panes
            .iter()
            .flat_map(|p| p.tabs.iter())
            .filter(|t| t.hive_role.as_deref() == Some(ROLE_WORKER))
            .map(|t| t.id.clone())
            .collect();
        (hive.id.clone(), tabs)
    };
    if tabs.is_empty() {
        return Ok(Vec::new());
    }
    let root = super::paths::hive_root(&hive_id).ok_or("No Hive folder")?;
    blocking(move || Ok(live_worker_tabs(&HivePaths::new(root), &tabs))).await
}

pub(crate) fn live_worker_tabs(paths: &HivePaths, tabs: &[String]) -> Vec<String> {
    worker::list_workers(paths)
        .into_iter()
        .filter(|w| w.is_live())
        .filter_map(|w| w.tab_id)
        .filter(|t| tabs.contains(t))
        .collect()
}

/// A reload's new tab takes over its worker (§11.2): `worker.json` names the tab, so it follows.
/// Blocks: reads the Hive's worker records.
pub fn remap_worker_tab(hive: &Hive, from: &str, to: &str) {
    let Some(root) = super::paths::hive_root(&hive.id) else { return };
    remap_worker_tab_in(&HivePaths::new(root), from, to);
}

pub(crate) fn remap_worker_tab_in(paths: &HivePaths, from: &str, to: &str) {
    let Some(w) = worker::worker_for_tab(paths, from) else { return };
    let Ok(dir) = worker::task_dir(paths, &w.task_id) else { return };
    let res = worker::update_worker(&dir, |w| {
        let mine = w.tab_id.as_deref() == Some(from);
        if mine {
            w.tab_id = Some(to.to_string());
        }
        mine
    });
    match res {
        Ok(_) => log::info!("hive: worker {} moved from tab {from} to {to} (reload)", w.slug),
        Err(e) => log::warn!("hive: worker {} not moved to tab {to}, so its relaunch will be refused: {e}", w.slug),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hive::fuses::{FUSES_FILE, PER_HOUR};
    use crate::hive::record::{HiveFolder, KIND_GIT, KIND_PLAIN};
    use crate::hive::rpc::Outcome;
    use crate::state::{WindowData, Workspace};
    use std::fs;
    use std::process::Command;
    use std::sync::Mutex;

    // ── Who may call what ──

    fn tab(id: &str, role: Option<&str>) -> Tab {
        let mut t = Tab::new(id.to_string());
        t.id = id.to_string();
        t.hive_role = role.map(str::to_string);
        t
    }

    fn roles_app() -> AppData {
        let mut h = Workspace::new("Hive".into());
        h.hive = Some(Hive { id: "h1".into(), slug: "s".into(), ..Hive::default() });
        h.panes[0].tabs = vec![tab("coord", Some(ROLE_COORDINATOR)), tab("w1", Some(ROLE_WORKER))];
        let mut plain = Workspace::new("Plain".into());
        plain.panes[0].tabs = vec![tab("t1", None)];
        let mut win = WindowData::new("main".into());
        win.workspaces = vec![h, plain];
        AppData { windows: vec![win], ..AppData::default() }
    }

    /// Spike 15's table and §7.2's limits, every role against every tool named there.
    #[test]
    fn refusals_by_role() {
        let d = roles_app();
        let refused = |callers: &[&str], tool: &str| refuse_agent_tool(&d, callers, tool).is_some();
        let worker_refused = [
            "createService", "updateService", "removeService", "startService", "restartService", "startStack", "stopStack",
            "stopService", "replyToOverlord", "createTab", "moveTab", "moveWorkspace", "setMesh", "linkMeshes", "unlinkMesh",
            "bridgeWith", "disconnectBridge", "sendToBridgedAgent", "startTopic", "completeTopic", "createFollowUp",
            "setPreference", "addMcpServer", "restoreArchivedTab", "switchTab", "setAutoResume", "setTriggerVariable",
            "driveTab", "answerTabPrompt", "restartMaiterm", "createBackup", "readLogs", "getTabContext",
        ];
        for tool in worker_refused {
            assert!(refused(&["w1"], tool), "a worker: {tool}");
            assert!(refused(&["w1", "t1"], tool), "a worker naming an ordinary tab is still a worker: {tool}");
            assert!(!refused(&["t1"], tool), "an ordinary agent: {tool}");
        }
        for tool in ["listTasks", "createTasks", "updateTasks", "getTabNotes", "setTabNotes", "writeWorkspaceNote"] {
            assert!(!refused(&["w1"], tool), "a worker keeps {tool}");
            assert!(!refused(&["coord"], tool), "the coordinator keeps {tool}");
        }
        for tool in super::super::settings::ESCAPE_MCP_TOOLS {
            assert!(refused(&["coord"], tool), "the coordinator: {tool}");
            assert!(!refused(&["t1"], tool), "an ordinary agent: {tool}");
        }
        assert!(!refused(&["coord"], "stopService"), "not an escape: Rust leaves it to the coordinator's allow list");
        // The Overlord's tools close, archive, recover and resume the human's tabs: a Hive role
        // among the callers is impersonation, whichever tab the connection was bound to.
        for tool in crate::claude_code::server::OVERLORD_CALLER_TOOLS {
            assert!(refused(&["w1"], tool) && refused(&["coord"], tool), "{tool}");
            assert!(refused(&["t1", "w1"], tool), "a worker bound to another tab: {tool}");
            assert!(refused(&["t1", "coord"], tool), "{tool}");
            assert!(!refused(&["t1"], tool), "an ordinary agent is left to the Overlord gate: {tool}");
        }

        for tool in COORDINATOR_TOOLS.iter().chain(&["hiveLand", "hiveStopWorker", "hiveFinish"]) {
            assert!(!refused(&["coord"], tool), "{tool}");
            assert!(refused(&["w1"], tool), "a worker can't spawn or land: {tool}");
            assert!(refused(&["t1"], tool), "an ordinary agent: {tool}");
            assert!(refused(&[], tool), "no identity at all: {tool}");
            assert!(refused(&["coord", "w1"], tool), "two identities that disagree: {tool}");
            assert!(refused(&["coord", "t1"], tool), "{tool}");
        }
        assert!(!refused(&["coord", "gone"], "hiveSpawnWorker"), "an id naming no tab (a stale header) is ignored");
        for tool in WORKER_HIVE_TOOLS {
            assert!(!refused(&["w1"], tool) && refused(&["coord"], tool) && refused(&["t1"], tool), "{tool}");
        }
        assert!(refused(&["coord"], "hiveAnything"), "an unknown hive name never reaches the frontend");
        assert_eq!(hive_tool_role("hiveSpawnWorker"), Some(ROLE_COORDINATOR));
        assert_eq!(hive_tool_role("galleryDeliver"), None, "the gallery's names aren't Hive tools here");
    }

    /// A worker that names the Overlord's tab (from listWorkspaces) in initSession would have the
    /// Overlord gate trust it: a Hive agent may only bind as itself.
    #[test]
    fn a_hive_agent_can_only_initsession_as_itself() {
        let d = roles_app();
        for own in ["w1", "coord"] {
            let e = refuse_rebind(&d, &[own], "t1").expect("a header naming a Hive tab");
            assert!(e.contains("only initSession as its own tab"), "{e}");
            assert!(refuse_rebind(&d, &["t1", own], "t1").is_some(), "a stated tab and a Hive header: {own}");
            assert!(refuse_rebind(&d, &[own], own).is_none(), "repairing its own identity: {own}");
        }
        assert!(refuse_rebind(&d, &["w1"], "coord").is_some(), "a worker can't become its coordinator");
        assert!(refuse_rebind(&d, &["t1"], "coord").is_none(), "an ordinary agent is left to the usual checks");
        assert!(refuse_rebind(&d, &[], "t1").is_none());
        assert!(refuse_rebind(&d, &["gone"], "t1").is_none(), "an id naming no tab is no Hive identity");
    }

    #[test]
    fn the_tools_list_role_comes_from_a_hive_tab() {
        let mut d = roles_app();
        assert_eq!(listed_role(&d, "coord"), Some(ROLE_COORDINATOR));
        assert_eq!(listed_role(&d, "w1"), Some(ROLE_WORKER));
        assert_eq!(listed_role(&d, "t1"), None);
        assert_eq!(listed_role(&d, "gone"), None);
        d.windows[0].workspaces[0].hive = None;
        assert_eq!(listed_role(&d, "coord"), None, "a role without its Hive lists nothing");
        let defs = tool_definitions(ROLE_COORDINATOR);
        let names: Vec<&str> = defs.iter().map(|t| t["name"].as_str().unwrap()).collect();
        assert_eq!(names, COORDINATOR_TOOLS.to_vec());
        let roles = defs[0]["inputSchema"]["properties"]["role"]["enum"].as_array().unwrap();
        assert!(roles.iter().any(|r| r == "backend"));
        assert!(tool_definitions(ROLE_WORKER).is_empty());
    }

    // ── Arguments ──

    fn arg_hive() -> Hive {
        Hive {
            id: "h1".into(),
            slug: "ship".into(),
            folders: vec![
                HiveFolder { name: "web".into(), path: "/r/web".into(), kind: KIND_GIT.into(), primary: true, ..HiveFolder::default() },
                HiveFolder { name: "api".into(), path: "/r/api".into(), kind: KIND_GIT.into(), ..HiveFolder::default() },
                HiveFolder { name: "brand".into(), path: "/r/brand".into(), kind: KIND_PLAIN.into(), ..HiveFolder::default() },
            ],
            ..Hive::default()
        }
    }

    fn existing(task: &str, badge: u32, slug: &str) -> WorkerRecord {
        WorkerRecord { task_id: task.into(), badge, slug: slug.into(), phase: worker::PHASE_RUNNING.into(), ..WorkerRecord::default() }
    }

    fn check(args: Value) -> Result<Plan, String> {
        let workers = vec![existing("t-auth", 1, "auth"), existing("t-db", 2, "db")];
        SpawnArgs::parse(&args).and_then(|a| validate(&arg_hive(), &workers, a))
    }

    #[test]
    fn spawn_arguments_are_checked() {
        let ok = check(json!({ "title": "  Login   page ", "role": "frontend", "brief": "Build it" })).unwrap();
        assert_eq!((ok.title.as_str(), ok.slug.as_str()), ("Login page", "login-page"));
        assert_eq!(ok.folders, vec!["web"], "the primary by default");
        assert!(ok.model.is_empty() && ok.effort.is_empty(), "the Hive's defaults, decided at launch");

        let ok = check(json!({ "title": "x", "role": "backend", "brief": "b", "folders": ["api", "brand"], "depends_on": [2, "#1", "auth", "t-db"], "model": "claude-sonnet-5.1", "effort": "xhigh" })).unwrap();
        assert_eq!(ok.folders, vec!["api", "brand"]);
        assert_eq!(ok.depends_on, vec!["t-db", "t-auth"], "badge, #badge, name and task id, each once");
        assert_eq!((ok.model.as_str(), ok.effort.as_str()), ("claude-sonnet-5.1", "xhigh"));
        assert!(check(json!({ "title": "x", "role": "backend", "brief": "b", "folders": [] })).unwrap().folders.is_empty(), "scratch only");

        let refused = |args: Value, says: &str| {
            let e = check(args).unwrap_err();
            assert!(e.contains(says), "{e}");
        };
        refused(json!({ "role": "backend", "brief": "b" }), "title is required");
        refused(json!({ "title": "x", "brief": "b" }), "role is required");
        refused(json!({ "title": "x", "role": "wizard", "brief": "b" }), "no role \"wizard\"");
        refused(json!({ "title": "x", "role": "backend", "brief": "  " }), "brief is empty");
        refused(json!({ "title": "x", "role": "backend", "brief": "a".repeat(MAX_BRIEF + 1) }), "over 16000");
        assert!(check(json!({ "title": "x", "role": "backend", "brief": "a".repeat(MAX_BRIEF) })).is_ok());
        refused(json!({ "title": "x", "role": "backend", "brief": "b", "folders": ["nope"] }), "no folder \"nope\"");
        refused(json!({ "title": "x", "role": "backend", "brief": "b", "folders": ["web", "web"] }), "named twice");
        refused(json!({ "title": "x", "role": "backend", "brief": "b", "folders": "web" }), "must be a list");
        refused(json!({ "title": "x", "role": "backend", "brief": "b", "depends_on": ["#9"] }), "no worker \"#9\"");
        refused(json!({ "title": "x", "role": "backend", "brief": "b", "effort": "extreme" }), "effort must be one of");
        refused(json!({ "title": "x", "role": "backend", "brief": "b", "model": "opus; rm -rf /" }), "isn't a model name");
        refused(json!({ "title": "x", "role": "backend", "brief": "b", "inputs": ["a1"] }), "gallery");
        refused(json!({ "title": "x", "role": "backend", "brief": "b", "reviews": "auth" }), "reviewers");
        refused(json!({ "title": "Auth", "role": "backend", "brief": "b" }), "already called \"auth\"");
        refused(json!({ "title": "Coordinator", "role": "backend", "brief": "b" }), "reserved");
        refused(json!({ "title": "!!!", "role": "backend", "brief": "b" }), "letter or digit");
        refused(json!({ "title": "x".repeat(MAX_TITLE + 1), "role": "backend", "brief": "b" }), "too long");
        assert!(check(json!({ "title": "x", "role": "backend", "brief": "b", "inputs": [], "reviews": null })).is_ok(), "empty is absent");
    }

    // ── The spawn, against scratch repos and a stub window ──

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

    fn repo(base: &Path, name: &str) -> PathBuf {
        let r = base.join(name);
        fs::create_dir_all(&r).unwrap();
        git(&r, &["init", "-q", "-b", "main"]);
        fs::write(r.join("README.md"), "r\n").unwrap();
        git(&r, &["add", "-A"]);
        git(&r, &["commit", "-q", "--no-verify", "-m", "init"]);
        r
    }

    type Answer = fn(&str, &Value) -> Outcome;

    #[derive(Clone)]
    struct Stub {
        answer: Answer,
        calls: Arc<Mutex<Vec<(String, Value)>>>,
    }

    impl HiveWindow for Stub {
        fn request(&self, verb: &'static str, args: Value) -> impl std::future::Future<Output = Outcome> + Send {
            let o = (self.answer)(verb, &args);
            self.calls.lock().unwrap().push((verb.to_string(), args));
            async move { o }
        }
        fn workers_changed(&self, _workspace_id: &str) {}
    }

    fn accept(_: &str, _: &Value) -> Outcome {
        Outcome::Answered(json!({ "ok": true }))
    }
    fn refuse_spawn(verb: &str, _: &Value) -> Outcome {
        if verb == rpc::VERB_SPAWN { Outcome::Answered(json!({ "error": "no such workspace here" })) } else { accept(verb, &Value::Null) }
    }
    fn silent(_: &str, _: &Value) -> Outcome {
        Outcome::Timeout
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
    fn untrusted(_: &HivePaths, _: &WorkerRecord) -> Result<(), String> {
        Err("Claude Code doesn't trust the repo behind \"web\"".into())
    }

    struct Rig {
        base: PathBuf,
        web: PathBuf,
        api: PathBuf,
        data: RwLock<AppData>,
        fuses: &'static Fuses,
        caller: Caller,
        calls: Arc<Mutex<Vec<(String, Value)>>>,
    }

    impl Rig {
        fn new(tag: &str, cap: u32) -> Self {
            let base = crate::share::canon(&{
                let t = std::env::temp_dir().join(format!("hive-tools-{tag}-{}", uuid::Uuid::new_v4()));
                fs::create_dir_all(&t).unwrap();
                t
            });
            let (web, api) = (repo(&base, "web"), repo(&base, "api"));
            let folder = |name: &str, p: &Path, primary: bool| HiveFolder {
                name: name.into(),
                path: p.to_string_lossy().into(),
                kind: KIND_GIT.into(),
                base: Some("main".into()),
                primary,
                ..HiveFolder::default()
            };
            let hive = Hive { id: "h1".into(), slug: "ship".into(), cap, folders: vec![folder("web", &web, true), folder("api", &api, false)], ..Hive::default() };
            let mut ws = Workspace::new("Ship it".into());
            ws.id = "ws".into();
            ws.hive = Some(hive.clone());
            ws.panes[0].tabs = vec![tab("coord", Some(ROLE_COORDINATOR))];
            ws.panes[0].active_tab_id = None;
            let mut win = WindowData::new("main".into());
            win.workspaces = vec![ws];
            let caller = Caller {
                tab_id: "coord".into(),
                label: "main".into(),
                workspace_id: "ws".into(),
                workstream: "Ship it".into(),
                hive,
                paths: HivePaths::new(base.join("hive/h1")),
            };
            Self {
                fuses: Box::leak(Box::new(Fuses::at(base.join(FUSES_FILE)))),
                data: RwLock::new(AppData { windows: vec![win], ..AppData::default() }),
                base,
                web,
                api,
                caller,
                calls: Arc::new(Mutex::new(Vec::new())),
            }
        }

        fn ctx(&self, answer: Answer, save: fn(&AppData) -> Result<(), String>, preflight: fn(&HivePaths, &WorkerRecord) -> Result<(), String>) -> Ctx<'_, Stub> {
            Ctx { data: &self.data, fuses: self.fuses, window: Stub { answer, calls: self.calls.clone() }, save, preflight }
        }

        async fn spawn(&self, answer: Answer, args: Value) -> Result<Value, String> {
            spawn_worker(&self.ctx(answer, ok_save, ok_preflight), &self.caller, &args).await
        }

        fn ws(&self) -> Workspace {
            self.data.read().windows[0].workspaces[0].clone()
        }

        fn verbs(&self) -> Vec<String> {
            self.calls.lock().unwrap().iter().map(|(v, _)| v.clone()).collect()
        }

        fn headroom(&self) -> crate::hive::fuses::Headroom {
            self.fuses.headroom(&self.caller.hive, &live_task_ids(&self.caller.paths), crate::hive::fuses::now_ms())
        }

        /// Nothing a spawn made is left: no worker record or task folder, no worktree or branch, no
        /// tab or task row, no slot held, and the human's checkouts as they were.
        fn assert_clean(&self) {
            assert!(worker::list_workers(&self.caller.paths).is_empty(), "no worker.json");
            let tasks = self.caller.paths.tasks();
            assert!(fs::read_dir(&tasks).map_or(true, |mut d| d.next().is_none()), "no task folder in {}", tasks.display());
            assert!(fs::read_dir(self.caller.paths.worktrees()).map_or(true, |mut d| d.next().is_none()), "no worker folder");
            for r in [&self.web, &self.api] {
                assert!(git(r, &["branch", "--list", "hive/*"]).is_empty(), "no branch in {}", r.display());
                assert!(!git(r, &["worktree", "list"]).contains("wt.noindex"));
                assert_eq!(git(r, &["rev-parse", "--abbrev-ref", "HEAD"]), "main");
            }
            let ws = self.ws();
            assert_eq!(ws.panes[0].tabs.iter().map(|t| t.id.as_str()).collect::<Vec<_>>(), vec!["coord"], "no worker tab");
            assert!(ws.tasks.is_empty() && ws.workstreams.is_empty(), "no task row, no workstream");
            let h = self.headroom();
            assert_eq!((h.live, h.pending, h.hour_left), (0, 0, PER_HOUR), "the reservation was given back, nothing counted");
        }
    }

    impl Drop for Rig {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.base);
        }
    }

    fn args(title: &str) -> Value {
        json!({ "title": title, "role": "backend", "brief": "Add the login endpoint.", "folders": ["web", "api"] })
    }

    #[tokio::test]
    async fn a_spawn_launches_a_hidden_worker_and_counts_once() {
        let rig = Rig::new("ok", 4);
        let out = rig.spawn(accept, args("Login API")).await.unwrap();
        assert_eq!((out["worker"].as_str(), out["badge"].as_u64(), out["state"].as_str()), (Some("login-api"), Some(1), Some("launching")));

        let w = worker::list_workers(&rig.caller.paths).pop().unwrap();
        assert_eq!(w.phase, worker::PHASE_LAUNCHING, "live before the commit, so the slot never gaps");
        let tab_id = w.tab_id.clone().unwrap();
        assert_eq!(w.folders.iter().map(|f| f.branch.as_deref()).collect::<Vec<_>>(), vec![Some("hive/ship--login-api"); 2]);
        for f in &w.folders {
            let wt = PathBuf::from(f.worktree.as_ref().unwrap());
            assert_eq!(git(&wt, &["rev-parse", "--abbrev-ref", "HEAD"]), "hive/ship--login-api");
        }
        let task_dir = rig.caller.paths.task(&w.task_id);
        let rec = crate::hive::launch::read_record(&task_dir).expect("no record, no launch: the spawn wrote it");
        assert_eq!((rec.role.as_str(), rec.task_id.as_str()), (ROLE_WORKER, w.task_id.as_str()));

        let ws = rig.ws();
        let t = ws.panes[0].tabs.iter().find(|t| t.id == tab_id).expect("the worker's tab");
        assert_eq!(t.hive_role.as_deref(), Some(ROLE_WORKER));
        assert_eq!(ws.panes[0].active_tab_id, None, "never made active");
        let row = ws.tasks.iter().find(|t| t.id == w.task_id).unwrap();
        assert_eq!((row.status.as_str(), row.tab_id.as_deref(), row.origin.as_str()), ("active", Some(tab_id.as_str()), "agent"));
        assert_eq!(row.detail.as_deref(), Some("Add the login endpoint."));
        assert_eq!(ws.workstreams.len(), 1);
        assert_eq!((ws.workstreams[0].name.as_str(), row.workstream_id.as_deref()), ("Ship it", Some(ws.workstreams[0].id.as_str())));

        let calls = rig.calls.lock().unwrap().clone();
        assert_eq!(calls.len(), 1);
        let (verb, sent) = &calls[0];
        assert_eq!(verb, rpc::VERB_SPAWN);
        assert_eq!((sent["workspace_id"].as_str(), sent["tab"]["id"].as_str(), sent["task"]["id"].as_str()), (Some("ws"), Some(tab_id.as_str()), Some(w.task_id.as_str())));
        assert_eq!(sent["workstream"]["id"], ws.workstreams[0].id.as_str());

        let h = rig.headroom();
        assert_eq!((h.live, h.pending, h.hour_left), (1, 0, PER_HOUR - 1));

        // A second worker joins the same workstream, takes the next badge, and its title is its own.
        let out = rig.spawn(accept, args("Login UI")).await.unwrap();
        assert_eq!(out["badge"], 2);
        assert_eq!(rig.ws().workstreams.len(), 1);
        let e = rig.spawn(accept, args("Login API")).await.unwrap_err();
        assert!(e.contains("already called \"login-api\""), "{e}");
        assert_eq!(rig.headroom().hour_left, PER_HOUR - 2, "a refused spawn counts nothing");
    }

    #[tokio::test]
    async fn a_refused_window_leaves_nothing_behind() {
        let rig = Rig::new("refused", 4);
        let e = rig.spawn(refuse_spawn, args("Login API")).await.unwrap_err();
        assert!(e.contains("refused it: no such workspace here"), "{e}");
        assert_eq!(rig.verbs(), vec![rpc::VERB_SPAWN, rpc::VERB_UNSPAWN], "the window undoes its half");
        let unspawn = rig.calls.lock().unwrap()[1].1.clone();
        assert!(unspawn["task_id"].is_string() && unspawn["tab_id"].is_string());
        rig.assert_clean();
        // Nothing is left to trip over: the same spawn goes through once the window takes it.
        rig.spawn(accept, args("Login API")).await.unwrap();
        assert_eq!(worker::list_workers(&rig.caller.paths)[0].badge, 1, "a rolled-back spawn used no badge");
    }

    #[tokio::test]
    async fn a_silent_window_is_rolled_back_without_waiting_on_it() {
        let rig = Rig::new("silent", 4);
        let e = rig.spawn(silent, args("Login API")).await.unwrap_err();
        assert!(e.contains("didn't confirm"), "{e}");
        rig.assert_clean();
    }

    #[tokio::test]
    async fn a_failed_save_after_the_window_answered_is_rolled_back() {
        let rig = Rig::new("save", 4);
        let e = spawn_worker(&rig.ctx(accept, bad_save, ok_preflight), &rig.caller, &args("Login API")).await.unwrap_err();
        assert!(e.contains("couldn't save") && e.contains("disk full"), "{e}");
        assert_eq!(rig.verbs(), vec![rpc::VERB_SPAWN, rpc::VERB_UNSPAWN]);
        rig.assert_clean();
    }

    #[tokio::test]
    async fn a_launch_the_builder_would_refuse_refuses_the_spawn() {
        let rig = Rig::new("preflight", 4);
        let e = spawn_worker(&rig.ctx(accept, ok_save, untrusted), &rig.caller, &args("Login API")).await.unwrap_err();
        assert!(e.contains("doesn't trust"), "{e}");
        assert!(rig.verbs().is_empty(), "refused before the tab existed: the window was never asked");
        rig.assert_clean();
    }

    /// A window that holds its answer to `hive.spawn` until released, as a sleeping screen does.
    #[derive(Clone)]
    struct Parked {
        asked: Arc<tokio::sync::Notify>,
        release: Arc<tokio::sync::Notify>,
    }

    impl HiveWindow for Parked {
        fn request(&self, _verb: &'static str, _args: Value) -> impl std::future::Future<Output = Outcome> + Send {
            let (asked, release) = (self.asked.clone(), self.release.clone());
            async move {
                asked.notify_one();
                release.notified().await;
                Outcome::Answered(json!({ "ok": true }))
            }
        }
        fn workers_changed(&self, _workspace_id: &str) {}
    }

    /// Spawn A is `launching` on disk and still holds its reservation while its window answers:
    /// it counts once, so B fits under a cap of 2 meanwhile.
    #[tokio::test]
    async fn a_spawn_waiting_on_its_window_counts_once_against_the_cap() {
        let rig = Rig::new("parallel", 2);
        let parked = Parked { asked: Arc::new(tokio::sync::Notify::new()), release: Arc::new(tokio::sync::Notify::new()) };
        let a_ctx = Ctx { data: &rig.data, fuses: rig.fuses, window: parked.clone(), save: ok_save, preflight: ok_preflight };
        let a_args = args("One");
        let a = spawn_worker(&a_ctx, &rig.caller, &a_args);
        let b = async {
            parked.asked.notified().await;
            let h = rig.headroom();
            assert_eq!((h.live, h.pending), (1, 0), "A is live on disk, not starting as well");
            let out = rig.spawn(accept, args("Two")).await;
            parked.release.notify_one();
            out
        };
        let (a, b) = tokio::join!(a, b);
        a.unwrap();
        b.expect("one worker of two was live: B fits");
        let h = rig.headroom();
        assert_eq!((h.live, h.pending, h.hour_left), (2, 0, PER_HOUR - 2));
    }

    #[tokio::test]
    async fn a_count_that_cannot_be_saved_rolls_the_spawn_back() {
        let rig = Rig::new("commit", 4);
        // fuses.json as a folder with something in it: the commit's write can't replace it.
        let fuses_json = rig.base.join(FUSES_FILE);
        fs::create_dir_all(&fuses_json).unwrap();
        fs::write(fuses_json.join("x"), "x").unwrap();
        let e = rig.spawn(accept, args("Login API")).await.unwrap_err();
        assert!(e.contains("spawn count couldn't be saved"), "{e}");
        assert_eq!(rig.verbs(), vec![rpc::VERB_SPAWN, rpc::VERB_UNSPAWN], "the window had mounted it: it undoes its half");
        rig.assert_clean();
    }

    #[tokio::test]
    async fn a_hive_gone_before_its_rows_rolls_the_spawn_back() {
        let rig = Rig::new("gone", 4);
        // Worktrees, the launch record and a `launching` worker.json exist when the rows fail.
        rig.data.write().windows[0].workspaces[0].hive = None;
        let e = rig.spawn(accept, args("Login API")).await.unwrap_err();
        assert!(e.contains("This Hive is gone"), "{e}");
        assert!(rig.verbs().is_empty(), "no tab was put, so the window was never asked");
        rig.assert_clean();
    }

    #[tokio::test]
    async fn the_cap_refuses_before_any_worktree() {
        let rig = Rig::new("cap", 1);
        rig.spawn(accept, args("One")).await.unwrap();
        let e = rig.spawn(accept, args("Two")).await.unwrap_err();
        assert!(e.contains("cap is reached: 1 of 1"), "{e}");
        let workers = worker::list_workers(&rig.caller.paths);
        assert_eq!(workers.iter().map(|w| w.slug.as_str()).collect::<Vec<_>>(), vec!["one"], "no record for the refused one");
        assert!(!rig.caller.paths.worker_dir("two").exists());
        assert!(git(&rig.web, &["branch", "--list", "hive/ship--two"]).is_empty());
        assert_eq!(rig.ws().tasks.len(), 1);
    }

    #[tokio::test]
    async fn a_worker_branch_taken_by_someone_else_rolls_the_spawn_back() {
        let rig = Rig::new("branch", 4);
        git(&rig.api, &["branch", "hive/ship--login-api"]);
        let e = rig.spawn(accept, args("Login API")).await.unwrap_err();
        assert!(e.starts_with("api: a branch named"), "{e}");
        assert!(worker::list_workers(&rig.caller.paths).is_empty());
        assert!(!git(&rig.api, &["branch", "--list", "hive/*"]).is_empty(), "the existing branch is the human's: it stays");
        assert_eq!(rig.headroom().pending, 0);
    }

    #[tokio::test]
    async fn a_waiting_worker_takes_no_reservation_and_no_worktree() {
        let rig = Rig::new("waiting", 1);
        rig.spawn(accept, args("Schema")).await.unwrap();
        let before = rig.headroom();
        // The cap is full, but a waiting worker holds no slot (§3.1).
        let out = rig.spawn(accept, json!({ "title": "Queries", "role": "backend", "brief": "After the schema.", "depends_on": ["schema"] })).await.unwrap();
        assert_eq!(out["state"], "waiting");
        assert_eq!(out["waits_on"], json!(["#1 schema"]));
        let w = worker::list_workers(&rig.caller.paths).into_iter().find(|w| w.slug == "queries").unwrap();
        assert_eq!((w.phase.as_str(), w.tab_id.as_ref(), w.badge), (worker::PHASE_WAITING, None, 2));
        assert_eq!(w.wait_reason.as_deref(), Some("waits on #1 schema"));
        assert_eq!(w.folders.iter().map(|f| (f.name.as_str(), f.worktree.is_none())).collect::<Vec<_>>(), vec![("web", true)]);
        assert!(!rig.caller.paths.worker_dir("queries").exists(), "worktrees come at launch, from what has landed");
        let after = rig.headroom();
        assert_eq!((after.live, after.pending, after.hour_left), (before.live, 0, before.hour_left), "no slot and no spawn counted");
        let ws = rig.ws();
        let row = ws.tasks.iter().find(|t| t.id == w.task_id).unwrap();
        assert_eq!((row.status.as_str(), row.tab_id.as_ref()), ("todo", None));
        let dep = ws.tasks.iter().find(|t| t.title == "Schema").unwrap();
        assert_eq!(row.blocked_by, vec![dep.id.clone()]);
        assert_eq!(ws.panes[0].tabs.len(), 2, "no tab for it");
        assert!(rig.calls.lock().unwrap().last().unwrap().1.get("tab").is_none());

        // A dependency already done launches at once (here, refused by the full cap).
        rig.data.write().windows[0].workspaces[0].tasks.iter_mut().find(|t| t.title == "Schema").unwrap().status = "done".into();
        let e = rig.spawn(accept, json!({ "title": "Views", "role": "backend", "brief": "b", "depends_on": [1] })).await.unwrap_err();
        assert!(e.contains("cap is reached"), "{e}");
    }

    #[tokio::test]
    async fn a_refused_waiting_worker_is_rolled_back_too() {
        let rig = Rig::new("waiting-refused", 4);
        rig.spawn(accept, args("Schema")).await.unwrap();
        let e = rig.spawn(refuse_spawn, json!({ "title": "Queries", "role": "backend", "brief": "b", "depends_on": ["schema"] })).await.unwrap_err();
        assert!(e.contains("refused it"), "{e}");
        assert_eq!(worker::list_workers(&rig.caller.paths).len(), 1);
        assert_eq!(rig.ws().tasks.len(), 1);
        assert_eq!(rig.verbs().last().map(String::as_str), Some(rpc::VERB_UNSPAWN));
    }

    #[tokio::test]
    async fn the_list_reports_workers_branches_and_headroom() {
        let rig = Rig::new("list", 4);
        rig.spawn(accept, args("Login API")).await.unwrap();
        rig.spawn(accept, json!({ "title": "Docs", "role": "docs", "brief": "b", "depends_on": [1], "effort": "low" })).await.unwrap();
        let w = worker::list_workers(&rig.caller.paths).into_iter().find(|w| w.slug == "login-api").unwrap();
        let wt = PathBuf::from(w.folders[0].worktree.as_ref().unwrap());
        fs::write(wt.join("a.txt"), "a").unwrap();
        git(&wt, &["add", "-A"]);
        git(&wt, &["commit", "-q", "--no-verify", "-m", "work"]);

        let tasks = rig.ws().tasks;
        let v = list_view(&rig.caller.hive, &rig.caller.paths, &tasks, rig.fuses, crate::hive::fuses::now_ms());
        let rows = v["workers"].as_array().unwrap();
        assert_eq!(rows.len(), 2);
        assert_eq!((rows[0]["worker"].as_str(), rows[0]["phase"].as_str(), rows[0]["task_status"].as_str()), (Some("login-api"), Some("launching"), Some("active")));
        assert_eq!(rows[0]["folders"][0]["ahead"], 1, "web has one commit Base doesn't");
        assert_eq!(rows[0]["folders"][1]["ahead"], 0);
        assert_eq!(rows[0]["folders"][0]["target"], "main");
        assert_eq!((rows[0]["model"].as_str(), rows[0]["effort"].as_str()), (Some("sonnet"), Some("high")), "the Hive's defaults");
        assert_eq!((rows[1]["phase"].as_str(), rows[1]["effort"].as_str()), (Some("waiting"), Some("low")));
        assert_eq!(rows[1]["depends_on"], json!(["#1 login-api"]));
        assert!(rows[1]["folders"][0]["ahead"].is_null(), "no worktree yet");
        let h = &v["headroom"];
        assert_eq!((h["cap"].as_u64(), h["live"].as_u64(), h["spawns_left_this_hour"].as_u64()), (Some(4), Some(1), Some(PER_HOUR as u64 - 1)));
        assert_eq!(v["starts_workers"], true);
    }

    #[tokio::test]
    async fn the_view_mounts_live_workers_only_and_a_reload_moves_the_record() {
        let rig = Rig::new("mount", 4);
        rig.spawn(accept, args("One")).await.unwrap();
        rig.spawn(accept, args("Two")).await.unwrap();
        let p = &rig.caller.paths;
        let ws = worker::list_workers(p);
        let (one, two) = (ws[0].tab_id.clone().unwrap(), ws[1].tab_id.clone().unwrap());
        let tabs = vec![one.clone(), two.clone(), "coord".to_string()];
        assert_eq!(live_worker_tabs(p, &tabs), vec![one.clone(), two.clone()]);
        worker::update_worker(&p.task(&ws[1].task_id), |w| {
            w.phase = worker::PHASE_STOPPED.into();
            true
        })
        .unwrap();
        assert_eq!(live_worker_tabs(p, &tabs), vec![one.clone()], "a stopped worker stays down");
        assert!(live_worker_tabs(p, &[two]).is_empty());

        remap_worker_tab_in(p, &one, "one-reloaded");
        assert_eq!(worker::worker_for_tab(p, "one-reloaded").map(|w| w.slug), Some("one".into()));
        assert!(worker::worker_for_tab(p, &one).is_none());
        remap_worker_tab_in(p, "not-a-worker", "x");
    }
}
