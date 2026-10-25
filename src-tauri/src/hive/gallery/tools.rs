//! The gallery's agent tools (docs/hive.md §10.3): `galleryAdd`, `galleryList`, `galleryGet`,
//! `galleryTag`, and the coordinator's `galleryCollect` and `galleryDeliver` (§10.6, step 8e).
//! Under the Finals checkpoint at Ask me, the coordinator's finals are recommendations the
//! human answers by picking in the gallery (`commands::human_tag`), which is how Finals is earned.
//!
//! Who files, for which task and from where comes from the calling tab, never from a tool's
//! arguments: a worker is its `worker.json`, the coordinator its Hive. A path is filed only from
//! the caller's own folders (`agent_roots`), checked whole on canonical paths and again on the
//! open file (`ingest::authorize`). That prevents accidents, not a determined agent (§10.3).
//!
//! Every paid render goes in the ledger, kept or not: a `galleryAdd` with a cost and no path is a
//! spend only, and a duplicate still records its cost (plan D3, D6). Recording is never refused.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde_json::{json, Value};

use super::ingest;
use super::model::*;
use super::Gallery;
use crate::hive::checkpoints;
use crate::hive::decisions::{self, DecisionLine, BY_COORDINATOR as DECIDED_BY_COORDINATOR, EVENT_ANSWERED, EVENT_LAPSED, EVENT_RECOMMENDED, SOURCE_GALLERY};
use crate::hive::deliver::one_line;
use crate::hive::paths::HivePaths;
use crate::hive::record::Hive;
use crate::hive::rpc::HiveWindow;
use crate::hive::tools::{blocking, caller_for_tab, find_worker, Caller, Ctx};
use crate::hive::worker::{self, WorkerRecord};
use crate::hive::{ROLE_COORDINATOR, ROLE_WORKER};
use crate::state::{AppData, AppState};

pub const ADD: &str = "galleryAdd";
pub const LIST: &str = "galleryList";
pub const GET: &str = "galleryGet";
pub const TAG: &str = "galleryTag";
pub const COLLECT: &str = "galleryCollect";
pub const DELIVER: &str = "galleryDeliver";

/// What `tools/list` offers a worker, and a coordinator.
pub const WORKER_GALLERY_TOOLS: [&str; 4] = [ADD, LIST, GET, TAG];
pub const COORDINATOR_GALLERY_TOOLS: [&str; 6] = [ADD, LIST, GET, TAG, COLLECT, DELIVER];
/// Every gallery name, so none falls through to the frontend.
pub const GALLERY_TOOLS: [&str; 6] = [ADD, LIST, GET, TAG, COLLECT, DELIVER];

pub const MAX_ITEMS: usize = 20;
pub const MAX_TITLE: usize = 200;
pub const MAX_CAPTION: usize = 2_000;
pub const MAX_PROMPT: usize = 8_000;
pub const MAX_PARAMS_BYTES: usize = 16 * 1024;
pub const MAX_COST_USD: f64 = 10_000.0;
pub const MAX_PARENTS: usize = 16;
pub const MAX_TAGS: usize = 30;
pub const MAX_TAG: usize = 40;
pub const MAX_URL: usize = 2_000;
pub const MAX_TOOL: usize = 100;
pub const MAX_MODEL: usize = 200;
pub const MAX_COLLECTION: usize = 100;
pub const LIST_DEFAULT: u32 = 50;
pub const LIST_MAX: u32 = 200;
/// Assets one `galleryTag` changes, and one `galleryCollect` list names.
pub const MAX_IDS: usize = 100;
pub const MAX_MEMBERS: usize = 500;
/// A spawn's `inputs` and a message's `assets` (plan D23).
pub const MAX_INPUTS: usize = 20;

/// Collect and deliver are the coordinator's (§10.3, plan D13); the rest every Hive agent's.
pub fn coordinator_only(tool: &str) -> bool {
    tool == COLLECT || tool == DELIVER
}

// ── Schemas ──

pub fn tool_definitions(role: &str) -> Vec<Value> {
    if role != ROLE_WORKER && role != ROLE_COORDINATOR {
        return Vec::new();
    }
    let tab_id = json!({ "type": "string", "description": "Your own tab (auto-injected)" });
    let ids = json!({ "type": "array", "items": { "type": "string" } });
    let recipe = json!({
        "type": "object",
        "description": "What made it, so it can be made again: without it, nobody can iterate on it",
        "properties": {
            "tool": { "type": "string", "description": "What made it: the generator, MCP server, skill or CLI" },
            "model": { "type": "string" },
            "prompt": { "type": "string", "description": format!("At most {MAX_PROMPT} characters") },
            "negative": { "type": "string" },
            "seed": { "type": "integer" },
            "params": { "type": "object", "description": "The other settings, as the tool took them (at most 16 KB)" },
            "source_url": { "type": "string", "description": "The https URL a generator answered with, after you downloaded it" },
            "cost_usd": { "type": "number", "description": "What this render cost, in dollars, where the tool reports it" }
        },
        "required": ["tool"]
    });
    let item = json!({
        "type": "object",
        "properties": {
            "path": { "type": "string", "description": "The file: absolute, or relative to your working folder. Leave it out to record a spend only" },
            "title": { "type": "string", "description": format!("What it is, at most {MAX_TITLE} characters") },
            "kind": { "type": "string", "enum": KINDS, "description": "Default: from the file's extension" },
            "caption": { "type": "string" },
            "recipe": recipe,
            "parents": { "type": "array", "items": { "type": "string" }, "description": "Asset ids it was made from: a variant, an edit, an upscale, a cut" },
            "tags": { "type": "array", "items": { "type": "string" } },
            "collection": { "type": "string", "description": "A collection (name or id) it joins" }
        },
        "required": ["title"]
    });
    let mut add_props = item["properties"].as_object().cloned().unwrap_or_default();
    add_props.insert("tabId".into(), tab_id.clone());
    add_props.insert("items".into(), json!({ "type": "array", "items": item, "description": format!("Several assets in one call (at most {MAX_ITEMS}), in place of the fields above") }));
    let collection_rule = if role == ROLE_COORDINATOR {
        "A collection that doesn't exist yet is made."
    } else {
        "A collection must already exist: your coordinator makes them."
    };
    let add = json!({
        "name": ADD,
        "description": format!(
            "File what you made into this Hive's gallery: anything a person would look at (renders, video, audio, documents, \
             screenshots, recordings, reports), with the recipe that made it. maiTerm copies the file (the gallery's copy is \
             read-only and outlives your folders) and records who made it and for which task from your tab, never from \
             these arguments. Render into your scratch folder first; a generator that answers with a URL is downloaded \
             there first. A path is taken only from your own folders (scratch, worktrees, your temp folder) or an output \
             folder your human listed. Filing the same bytes again for the same task answers the asset already there. \
             Every paid render is recorded with recipe.cost_usd, kept or not: a discarded or failed one is a galleryAdd \
             with no path. The answer gives each asset's id and its stored path, to use as an input, and the Hive's spend \
             against its budget. {collection_rule}"
        ),
        "inputSchema": { "type": "object", "properties": add_props, "required": [] }
    });
    let list = json!({
        "name": LIST,
        "description": format!(
            "This Hive's gallery as data, newest first: id, title, kind, status, star, tags, who made it, its task, cost, \
             stored path, size or duration, and parent and child counts. Filters narrow; search covers titles, prompts and \
             tags. {LIST_DEFAULT} by default, at most {LIST_MAX}."
        ),
        "inputSchema": {
            "type": "object",
            "properties": {
                "tabId": tab_id,
                "kind": { "type": "string", "enum": KINDS },
                "made_by": { "type": ["string", "number"], "description": "A worker (badge, name or task id), coordinator or human" },
                "task": { "type": ["string", "number"], "description": "A worker (badge or name) or a task id" },
                "collection": { "type": "string", "description": "Name or id" },
                "status": { "type": "string", "enum": STATUSES },
                "tag": { "type": "string" },
                "starred": { "type": "boolean" },
                "query": { "type": "string" },
                "limit": { "type": "integer", "minimum": 1, "maximum": LIST_MAX }
            },
            "required": []
        }
    });
    let get = json!({
        "name": GET,
        "description": "One asset whole: its recipe, who made it and for which task, its parents and children with their titles, \
            its collections, and the stored file's path, which you can read or use as an input (the file is read-only: an \
            edit is a new asset whose parent is this one).",
        "inputSchema": { "type": "object", "properties": { "tabId": tab_id, "id": { "type": "string" } }, "required": ["id"] }
    });
    let status_rule = if role == ROLE_COORDINATOR {
        "status final and rejected are yours to set while your human leaves picking finals to you. Under Ask me they are \
         your human's: a final or rejected you set (or one you move an asset out of) is recorded as your recommendation, \
         shown to your human on that asset in the gallery, and the asset keeps its status until they pick; ask with a \
         decision blocker when you need the answer to go on."
    } else {
        "You may set status picked (or \"\") on an asset that isn't final or rejected; final and rejected are your \
         coordinator's and your human's."
    };
    let tag = json!({
        "name": TAG,
        "description": format!("Tag, untag, star or set the status of assets. Tags are added and removed, never replaced. {status_rule}"),
        "inputSchema": {
            "type": "object",
            "properties": {
                "tabId": tab_id,
                "id": { "type": "string" },
                "ids": ids,
                "tags": { "type": "array", "items": { "type": "string" }, "description": "Tags to add" },
                "untag": { "type": "array", "items": { "type": "string" }, "description": "Tags to remove" },
                "status": { "type": "string", "enum": STATUSES },
                "starred": { "type": "boolean" }
            },
            "required": []
        }
    });
    if role == ROLE_WORKER {
        return vec![add, list, get, tag];
    }
    let collect = json!({
        "name": COLLECT,
        "description": "Build a collection (\"Storyboard v2\", \"Shortlist\"): made by name if it doesn't exist, then assets added, \
            removed, put in order (order lists every member once) and its cover set (\"\" clears it). Members must be in the \
            gallery and out of the trash.",
        "inputSchema": {
            "type": "object",
            "properties": {
                "tabId": tab_id,
                "collection": { "type": "string", "description": "Name or id" },
                "add": ids,
                "remove": ids,
                "order": ids,
                "cover": { "type": "string" }
            },
            "required": ["collection"]
        }
    });
    let deliver = json!({
        "name": DELIVER,
        "description": "Copy assets into one of this Hive's plain folders as new files: the assets you name, or a collection in its \
            order (named 01-…, 02-… unless you give names). folder is a plain folder's name, with an existing subfolder if you \
            like (brand/icons), or an absolute path inside one; maiTerm makes no folders. A name already there gets \" (2)\": \
            nothing is ever overwritten (replacing files is your human's, with Export…). Names default to the titles. A git \
            folder is never written: have a worker put the files in its worktree (hiveSpawnWorker inputs, hiveMessageWorker \
            assets) and land them, so the change is reviewed. Trashed assets aren't delivered. Answers the names written.",
        "inputSchema": {
            "type": "object",
            "properties": {
                "tabId": tab_id,
                "assets": { "type": "array", "items": { "type": "string" }, "description": "Asset ids, in the order to write them" },
                "collection": { "type": "string", "description": "A collection (name or id), in place of assets: its members out of the trash, in order" },
                "folder": { "type": "string", "description": "A plain folder's name, optionally with a subfolder (brand/icons), or an absolute path inside one" },
                "names": { "type": "array", "items": { "type": "string" }, "description": "One file name per asset delivered, in order: one component each, no leading dot. A name with no extension takes the file's" }
            },
            "required": ["folder"]
        }
    });
    vec![add, list, get, tag, collect, deliver]
}

// ── The caller ──

/// A gallery call's caller: the Hive and the calling tab (`caller.tab_id`), its role, and for a
/// worker its record, which is where its task and its folders come from.
#[derive(Debug, Clone)]
pub(crate) struct GalleryCaller {
    pub caller: Caller,
    pub role: &'static str,
    pub worker: Option<WorkerRecord>,
}

impl GalleryCaller {
    fn made_by(&self) -> MadeBy {
        match &self.worker {
            Some(w) => MadeBy { kind: BY_WORKER.into(), tab_id: self.caller.tab_id.clone(), role: w.role.clone(), worker_title: w.title.clone(), badge: w.badge },
            None => MadeBy { kind: BY_COORDINATOR.into(), tab_id: self.caller.tab_id.clone(), role: ROLE_COORDINATOR.into(), ..MadeBy::default() },
        }
    }

    fn label(&self) -> String {
        self.worker.as_ref().map(crate::hive::tools::worker_label).unwrap_or_else(|| "the coordinator".to_string())
    }
}

/// The Hive a live Hive tab is in, as that tab, with its role.
pub(crate) fn resolve(data: &AppData, tab_id: &str) -> Result<(Caller, &'static str), String> {
    let refused = || "Only a Hive's coordinator or its workers can use the gallery, from their own tab.".to_string();
    let (_, tab) = crate::hive::find_hive_tab(data, tab_id).ok_or_else(refused)?;
    let role = match tab.hive_role.as_deref() {
        Some(ROLE_COORDINATOR) => ROLE_COORDINATOR,
        Some(ROLE_WORKER) => ROLE_WORKER,
        _ => return Err(refused()),
    };
    let mut caller = caller_for_tab(data, tab_id).ok_or_else(refused)?;
    caller.tab_id = tab_id.to_string();
    Ok((caller, role))
}

/// The system's temp folders a sandboxed tool writes to (plan D25): Claude's per-user one and
/// maiTerm's own `temp_dir()`. Only those that exist, canonical.
pub fn system_temp_roots() -> Vec<PathBuf> {
    let mut out = vec![std::env::temp_dir()];
    #[cfg(unix)]
    {
        // SAFETY: getuid has no failure mode.
        let uid = unsafe { libc::getuid() };
        out.push(PathBuf::from(if cfg!(target_os = "macos") { format!("/private/tmp/claude-{uid}") } else { format!("/tmp/claude-{uid}") }));
    }
    out.iter().filter(|p| p.is_dir()).map(|p| crate::share::canon(p)).collect()
}

/// The folders `galleryAdd` takes a path from (§10.3): a worker's scratch, its own temp and its
/// worktrees, or the coordinator's home and temp; then the system temp folders and the output
/// folders the human listed. A shared root holding the Hive's own folder is dropped: `tasks/`
/// and the rest are never the caller's to file from.
pub(crate) fn agent_roots(hive: &Hive, paths: &HivePaths, w: Option<&WorkerRecord>, temp: &[PathBuf]) -> Vec<PathBuf> {
    let own: Vec<PathBuf> = match w {
        Some(w) => [paths.worker_scratch(&w.slug), paths.worker_tmp(&w.slug)]
            .into_iter()
            .chain(w.git_folders().filter_map(|f| f.worktree.as_ref().map(PathBuf::from)))
            .collect(),
        None => vec![paths.coordinator_home(), paths.coordinator_tmp()],
    };
    let hive_root = crate::share::canon(&paths.root);
    let shared = temp.iter().cloned().chain(hive.settings.gallery.output_folders.iter().map(PathBuf::from)).filter(|r| {
        let r = crate::share::canon(r);
        let holds_hive = hive_root.starts_with(&r);
        if holds_hive {
            log::warn!("hive {}: {} holds the Hive's folder, so nothing is filed from it", hive.id, r.display());
        }
        !holds_hive
    });
    own.into_iter().chain(shared).filter(|p| p.is_dir()).map(|p| crate::share::canon(&p)).collect()
}

pub(crate) async fn handle(tool: &str, args: &Value, state: &Arc<AppState>, handle: &tauri::AppHandle) -> Value {
    let tab = args.get("tabId").and_then(Value::as_str).unwrap_or_default();
    let resolved = resolve(&state.app_data.read(), tab);
    let (caller, role) = match resolved {
        Ok(r) => r,
        Err(e) => return json!({ "error": e }),
    };
    let ctx = crate::hive::tools::app_ctx(state, handle, &caller);
    let hive_id = caller.hive.id.clone();
    call(&ctx, caller, role, tool, args, system_temp_roots()).await.unwrap_or_else(|e| {
        log::info!("hive {hive_id}: {tool} refused: {e}");
        json!({ "error": e })
    })
}

/// One gallery call as `caller` in `role`; `temp` is the system temp roots (a test's own).
pub(crate) async fn call<W: HiveWindow>(ctx: &Ctx<'_, W>, caller: Caller, role: &'static str, tool: &str, args: &Value, temp: Vec<PathBuf>) -> Result<Value, String> {
    if !GALLERY_TOOLS.contains(&tool) {
        return Err(format!("{tool} isn't a maiTerm tool."));
    }
    if coordinator_only(tool) && role != ROLE_COORDINATOR {
        return Err(format!("{tool} is your coordinator's: ask it in your report or a blocker."));
    }
    let worker = if role == ROLE_WORKER {
        let (paths, tab) = (caller.paths.clone(), caller.tab_id.clone());
        Some(blocking(move || worker::worker_for_tab(&paths, &tab).ok_or_else(|| "No worker record names your tab.".to_string())).await?)
    } else {
        None
    };
    let gc = GalleryCaller { caller, role, worker };
    let (id, paths) = (gc.caller.hive.id.clone(), gc.caller.paths.clone());
    let g = blocking(move || super::for_paths(&id, &paths)).await?;
    match tool {
        ADD => add(ctx, &gc, g, args, temp).await,
        LIST => list(&gc, g, args).await,
        GET => get(g, args).await,
        TAG => tag(ctx, &gc, g, args).await,
        COLLECT => collect(ctx, &gc, g, args).await,
        DELIVER => deliver(&gc, g, args).await,
        t => Err(format!("{t} isn't a maiTerm tool.")),
    }
}

// ── Arguments ──

fn opt_str(v: &Value, k: &str) -> Result<Option<String>, String> {
    match v.get(k) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(s)) => Ok(Some(s.trim().to_string()).filter(|s| !s.is_empty())),
        // A badge, as `made_by: 3`.
        Some(Value::Number(n)) => Ok(Some(n.to_string())),
        Some(_) => Err(format!("{k} must be a string")),
    }
}

fn str_list(v: &Value, k: &str, max: usize) -> Result<Vec<String>, String> {
    match v.get(k) {
        None | Some(Value::Null) => Ok(Vec::new()),
        Some(Value::Array(a)) if a.len() > max => Err(format!("At most {max} {k}.")),
        Some(Value::Array(a)) => {
            a.iter().map(|x| x.as_str().map(|s| s.trim().to_string()).ok_or_else(|| format!("{k} must be a list of strings"))).filter(|r| r.as_ref().map_or(true, |s| !s.is_empty())).collect()
        }
        Some(_) => Err(format!("{k} must be a list")),
    }
}

fn too_long(s: &str, max: usize) -> bool {
    s.chars().count() > max
}

fn asset_ids(v: &Value, k: &str, max: usize) -> Result<Vec<String>, String> {
    let ids = str_list(v, k, max)?;
    if let Some(bad) = ids.iter().find(|id| !is_asset_id(id)) {
        return Err(format!("\"{}\" in {k} isn't an asset id (a- and 12 hex digits).", one_line(bad, 40)));
    }
    let mut out: Vec<String> = Vec::new();
    for id in ids {
        if !out.contains(&id) {
            out.push(id);
        }
    }
    Ok(out)
}

fn tags(v: &Value, k: &str) -> Result<Vec<String>, String> {
    let mut out: Vec<String> = Vec::new();
    for t in str_list(v, k, MAX_TAGS)? {
        if too_long(&t, MAX_TAG) {
            return Err(format!("A tag is at most {MAX_TAG} characters: \"{}…\".", one_line(&t, 20)));
        }
        let t = normalize_tag(&t);
        if !t.is_empty() && !out.contains(&t) {
            out.push(t);
        }
    }
    Ok(out)
}

/// One asset to file, as the agent gave it, checked.
#[derive(Debug, Clone, Default, PartialEq)]
pub(crate) struct AddItem {
    pub path: Option<String>,
    pub title: String,
    pub kind: Option<String>,
    pub caption: Option<String>,
    pub recipe: Option<Recipe>,
    pub parents: Vec<String>,
    pub tags: Vec<String>,
    pub collection: Option<String>,
}

const ITEM_FIELDS: [&str; 8] = ["path", "title", "kind", "caption", "recipe", "parents", "tags", "collection"];

pub(crate) fn parse_add(args: &Value) -> Result<Vec<AddItem>, String> {
    match args.get("items") {
        None | Some(Value::Null) => Ok(vec![parse_item(args)?]),
        Some(Value::Array(_)) if ITEM_FIELDS.iter().any(|k| args.get(*k).is_some_and(|v| !v.is_null())) => {
            Err("Give items, or one asset's fields, not both.".to_string())
        }
        Some(Value::Array(a)) if a.is_empty() => Err("items is empty: give at least one asset.".to_string()),
        Some(Value::Array(a)) if a.len() > MAX_ITEMS => Err(format!("At most {MAX_ITEMS} items in one call.")),
        Some(Value::Array(a)) => a.iter().enumerate().map(|(i, v)| parse_item(v).map_err(|e| format!("Item {}: {e}", i + 1))).collect(),
        Some(_) => Err("items must be a list of assets".to_string()),
    }
}

fn parse_item(v: &Value) -> Result<AddItem, String> {
    if !v.is_object() {
        return Err("each item is an object: {path, title, recipe, …}".to_string());
    }
    let title = opt_str(v, "title")?.ok_or("title is required: what it is, in a few words.")?;
    if too_long(&title, MAX_TITLE) || title.chars().any(char::is_control) {
        return Err(format!("The title is one line of at most {MAX_TITLE} characters."));
    }
    let kind = opt_str(v, "kind")?;
    if let Some(k) = kind.as_deref().filter(|k| !KINDS.contains(k)) {
        return Err(format!("kind \"{}\" isn't one of {}.", one_line(k, 40), KINDS.join(", ")));
    }
    let caption = opt_str(v, "caption")?;
    if caption.as_deref().is_some_and(|c| too_long(c, MAX_CAPTION)) {
        return Err(format!("The caption is at most {MAX_CAPTION} characters."));
    }
    let parents = asset_ids(v, "parents", MAX_PARENTS)?;
    let collection = opt_str(v, "collection")?;
    if collection.as_deref().is_some_and(|c| too_long(c, MAX_COLLECTION) || c.chars().any(char::is_control)) {
        return Err(format!("A collection is named in one line of at most {MAX_COLLECTION} characters."));
    }
    Ok(AddItem { path: opt_str(v, "path")?, title, kind, caption, recipe: parse_recipe(v.get("recipe"))?, parents, tags: tags(v, "tags")?, collection })
}

fn parse_recipe(v: Option<&Value>) -> Result<Option<Recipe>, String> {
    let v = match v {
        None | Some(Value::Null) => return Ok(None),
        Some(v) if v.is_object() => v,
        Some(_) => return Err("recipe must be an object: {tool, model, prompt, seed, params, cost_usd, …}".to_string()),
    };
    let mut r: Recipe = serde_json::from_value(v.clone()).map_err(|e| format!("recipe: {e}"))?;
    let trim = |s: Option<String>| s.map(|s| s.trim().to_string()).filter(|s| !s.is_empty());
    r.tool = r.tool.trim().to_string();
    if r.tool.is_empty() || too_long(&r.tool, MAX_TOOL) {
        return Err(format!("recipe.tool names what made it (a generator, MCP server, skill or CLI), at most {MAX_TOOL} characters."));
    }
    r.model = trim(r.model);
    if r.model.as_deref().is_some_and(|m| too_long(m, MAX_MODEL)) {
        return Err(format!("recipe.model is at most {MAX_MODEL} characters."));
    }
    for (k, s) in [("prompt", &r.prompt), ("negative", &r.negative)] {
        if s.as_deref().is_some_and(|s| too_long(s, MAX_PROMPT)) {
            return Err(format!("recipe.{k} is at most {MAX_PROMPT} characters."));
        }
    }
    if r.params.as_ref().is_some_and(|p| p.to_string().len() > MAX_PARAMS_BYTES) {
        return Err("recipe.params is at most 16 KB of JSON: keep the settings, leave out the data.".to_string());
    }
    r.source_url = trim(r.source_url);
    if let Some(u) = &r.source_url {
        if !u.starts_with("https://") || too_long(u, MAX_URL) || u.chars().any(|c| c.is_whitespace() || c.is_control()) {
            return Err(format!("recipe.source_url is an https:// URL of at most {MAX_URL} characters."));
        }
    }
    if let Some(c) = r.cost_usd {
        if !c.is_finite() || !(0.0..=MAX_COST_USD).contains(&c) {
            return Err(format!("recipe.cost_usd is dollars, from 0 to {MAX_COST_USD}."));
        }
    }
    Ok(Some(r))
}

// ── galleryAdd ──

/// Who files and from where, from the caller's identity.
#[derive(Debug, Clone)]
struct Filer {
    made_by: MadeBy,
    task_id: Option<String>,
    roots: Vec<PathBuf>,
    base: PathBuf,
    may_create_collection: bool,
}

#[derive(Debug)]
enum Filed {
    Asset { asset: Box<Asset>, duplicate: bool, spend: Option<Spend> },
    Spend(Spend),
}

fn find_collection(g: &Gallery, key: &str) -> Result<Option<String>, String> {
    Ok(g.collections()?.into_iter().find(|c| c.id == key || c.name.eq_ignore_ascii_case(key)).map(|c| c.id))
}

fn spend_of(f: &Filer, it: &AddItem, asset_id: Option<&str>) -> Option<NewSpend> {
    let r = it.recipe.as_ref()?;
    let usd = r.cost_usd?;
    Some(NewSpend {
        asset_id: asset_id.map(str::to_string),
        task_id: f.task_id.clone(),
        made_by: f.made_by.clone(),
        cost_micros: usd_to_micros(usd),
        tool: r.tool.clone(),
        model: r.model.clone().unwrap_or_default(),
    })
}

/// Files one item. Everything a refusal depends on is checked before the copy; a store whose row
/// then fails is removed, so `files/` holds only what the index records.
fn file_item(g: &Gallery, f: &Filer, it: &AddItem) -> Result<Filed, String> {
    for p in &it.parents {
        match g.get(p)? {
            None => return Err(format!("No asset {p} to derive from.")),
            Some(a) if a.purged => return Err(format!("{p} was emptied from the trash; it can't be a parent.")),
            Some(_) => {}
        }
    }
    let collection = match it.collection.as_deref() {
        None => None,
        Some(c) => match find_collection(g, c)? {
            Some(id) => Some(Ok(id)),
            None if f.may_create_collection => Some(Err(c.to_string())),
            None => return Err(format!("No collection called \"{}\": only your coordinator makes collections.", one_line(c, 100))),
        },
    };
    let Some(path) = it.path.as_deref() else {
        let spend = spend_of(f, it, None).ok_or("Nothing to file: give a path, or a recipe.cost_usd to record a paid render you didn't keep.")?;
        return Ok(Filed::Spend(g.record_spend(spend)?));
    };
    let src = ingest::authorize(Path::new(path), &f.base, &f.roots, g.root()).map_err(|e| {
        if e.contains("may file from") {
            let roots: Vec<String> = f.roots.iter().map(|r| r.display().to_string()).collect();
            format!("{e} You file from: {}.", roots.join(", "))
        } else {
            e
        }
    })?;
    let id = mint(ASSET_PREFIX);
    let stored = ingest::store_file(&src, &g.files_dir(), &id)?;
    let joined = |g: &Gallery, asset_id: &str| -> Result<Option<String>, String> {
        match &collection {
            None => Ok(None),
            Some(Ok(cid)) => Ok(Some(cid.clone())),
            Some(Err(name)) => {
                let c = g.collect(&CollectChange { collection: name.clone(), create: true, ..CollectChange::default() })?;
                log::info!("gallery {}: collection {} \u{201c}{}\u{201d} made for {asset_id}", g.root().display(), c.id, c.name);
                Ok(Some(c.id))
            }
        }
    };
    if let Some(existing) = g.by_sha(f.task_id.as_deref(), &stored.sha256)? {
        g.discard_file(&stored.file);
        if let Some(cid) = joined(g, &existing.id)? {
            g.collect(&CollectChange { collection: cid, add: vec![existing.id.clone()], ..CollectChange::default() })?;
        }
        let spend = spend_of(f, it, Some(&existing.id)).map(|s| g.record_spend(s)).transpose()?;
        let asset = g.get(&existing.id)?.unwrap_or(existing);
        return Ok(Filed::Asset { asset: Box::new(asset), duplicate: true, spend });
    }
    let inserted = joined(g, &id).and_then(|collection| {
        g.insert_asset(NewAsset {
            id: id.clone(),
            kind: it.kind.clone().unwrap_or_else(|| ingest::kind_for(&src.ext).to_string()),
            file: stored.file.clone(),
            mime: ingest::mime_for(&src.ext).to_string(),
            bytes: stored.bytes,
            sha256: stored.sha256.clone(),
            title: it.title.clone(),
            caption: it.caption.clone(),
            made_by: f.made_by.clone(),
            task_id: f.task_id.clone(),
            source_path: src.canonical.to_string_lossy().to_string(),
            recipe: it.recipe.clone(),
            parents: it.parents.clone(),
            tags: it.tags.clone(),
            collection,
            thumb: THUMB_PENDING.to_string(),
            ..NewAsset::default()
        })
    });
    let asset = match inserted {
        Ok(a) => a,
        Err(e) => {
            g.discard_file(&stored.file);
            return Err(e);
        }
    };
    let spend = spend_of(f, it, Some(&asset.id)).map(|s| g.record_spend(s)).transpose()?;
    Ok(Filed::Asset { asset: Box::new(asset), duplicate: false, spend })
}

fn usd(micros: i64) -> f64 {
    micros as f64 / 1_000_000.0
}

/// The Hive's spend against its budget (§10.3). Self-reported: a rule agents follow and a total
/// the human watches.
pub(crate) fn budget_view(g: &Gallery, hive: &Hive) -> Result<Value, String> {
    let spent = g.spent_micros()?;
    let budget = i64::try_from(hive.budget_cents).unwrap_or(i64::MAX / 10_000).saturating_mul(10_000);
    let over = spent > 0 && spent >= budget;
    let mut v = json!({ "spent_usd": usd(spent), "budget_usd": usd(budget), "remaining_usd": usd((budget - spent).max(0)), "over": over });
    if over {
        v["rule"] = json!("This Hive's budget for paid generation is spent: start no paid render until your human raises it.");
    }
    Ok(v)
}

async fn add<W: HiveWindow>(ctx: &Ctx<'_, W>, gc: &GalleryCaller, g: Arc<Gallery>, args: &Value, temp: Vec<PathBuf>) -> Result<Value, String> {
    let items = parse_add(args)?;
    let hive = gc.caller.hive.clone();
    let paths = &gc.caller.paths;
    let filer = Filer {
        made_by: gc.made_by(),
        task_id: gc.worker.as_ref().map(|w| w.task_id.clone()),
        roots: agent_roots(&hive, paths, gc.worker.as_ref(), &temp),
        base: gc.worker.as_ref().map_or_else(|| paths.coordinator_home(), |w| w.cwd(paths)),
        may_create_collection: gc.role == ROLE_COORDINATOR,
    };
    let (results, budget) = blocking(move || {
        let results: Vec<Result<Filed, String>> = items.iter().map(|it| file_item(&g, &filer, it)).collect();
        let assets: Vec<Value> = results
            .iter()
            .enumerate()
            .map(|(i, r)| match r {
                Ok(Filed::Asset { asset, duplicate, spend }) => {
                    let mut v = json!({ "id": asset.id, "path": g.file_path(asset), "kind": asset.kind, "title": asset.title });
                    if *duplicate {
                        v["duplicate"] = json!(true);
                    }
                    if let Some(s) = spend {
                        v["spend"] = json!({ "id": s.id, "cost_usd": usd(s.cost_micros) });
                    }
                    (i, Ok(v))
                }
                Ok(Filed::Spend(s)) => (i, Ok(json!({ "spend": { "id": s.id, "cost_usd": usd(s.cost_micros) } }))),
                Err(e) => (i, Err(e.clone())),
            })
            .map(|(i, r)| match r {
                Ok(v) => v,
                Err(e) => json!({ "item": i + 1, "error": e }),
            })
            .collect();
        let ids: Vec<String> = results.iter().filter_map(|r| match r {
            Ok(Filed::Asset { asset, .. }) => Some(asset.id.clone()),
            _ => None,
        }).collect();
        let recorded = results.iter().filter(|r| r.is_ok()).count();
        let errors: Vec<String> = results.iter().filter_map(|r| r.as_ref().err().cloned()).collect();
        Ok(((assets, ids, recorded, errors), budget_view(&g, &hive)?))
    })
    .await?;
    let (answers, ids, recorded, errors) = results;
    if recorded == 0 {
        return Err(if errors.len() == 1 { errors[0].clone() } else { format!("Nothing was filed: {}", errors.join(" ")) });
    }
    // One event per call, whatever it filed: a spend moves the header's total too.
    ctx.window.gallery_changed(&gc.caller, &ids);
    super::thumbs::enqueue(&gc.caller.hive.id, ids.clone());
    log::info!("hive {}: {} filed {} asset(s), {} item(s) recorded, {} refused", gc.caller.hive.id, gc.label(), ids.len(), recorded, errors.len());
    let (filed, spends): (Vec<Value>, Vec<Value>) = answers.into_iter().partition(|v| v.get("id").is_some() || v.get("error").is_some());
    let mut out = json!({ "assets": filed, "budget": budget });
    if !spends.is_empty() {
        out["spends"] = json!(spends.into_iter().map(|v| v["spend"].clone()).collect::<Vec<_>>());
    }
    if !errors.is_empty() {
        out["refused"] = json!(errors.len());
    }
    Ok(out)
}

// ── galleryList, galleryGet ──

/// Who made an asset, as an agent reads it: `#3 Title`, `coordinator`, `human`.
fn by_label(m: &MadeBy) -> String {
    match m.kind.as_str() {
        BY_WORKER => format!("#{} {}", m.badge, one_line(&m.worker_title, 120)),
        "" => "unknown".to_string(),
        k => k.to_string(),
    }
}

/// The filter `galleryList` asks for, workers resolved to their task. None: it can match nothing
/// (a worker's assets for another worker's task).
pub(crate) fn list_filter(args: &Value, workers: &[WorkerRecord]) -> Result<Option<ListFilter>, String> {
    let mut f = ListFilter { include_trash: false, ..ListFilter::default() };
    let worker_task = |name: &str| find_worker(workers, name).map(|w| w.task_id.clone());
    if let Some(m) = opt_str(args, "made_by")? {
        match m.to_ascii_lowercase().as_str() {
            BY_COORDINATOR | BY_HUMAN | BY_WORKER => f.made_by = Some(m.to_ascii_lowercase()),
            _ => {
                let t = worker_task(&m).ok_or_else(|| format!("No worker \"{}\" in this Hive: made_by is a worker (badge, name or task id), coordinator or human.", one_line(&m, 40)))?;
                f.made_by = Some(BY_WORKER.to_string());
                f.task_id = Some(t);
            }
        }
    }
    if let Some(t) = opt_str(args, "task")? {
        let t = worker_task(&t).unwrap_or(t);
        if f.task_id.as_ref().is_some_and(|mine| *mine != t) {
            return Ok(None);
        }
        f.task_id = Some(t);
    }
    f.kind = opt_str(args, "kind")?;
    if let Some(k) = f.kind.as_deref().filter(|k| !KINDS.contains(k)) {
        return Err(format!("kind \"{}\" isn't one of {}.", one_line(k, 40), KINDS.join(", ")));
    }
    f.collection = opt_str(args, "collection")?;
    f.status = match args.get("status") {
        None | Some(Value::Null) => None,
        Some(Value::String(s)) if STATUSES.contains(&s.trim()) => Some(s.trim().to_string()),
        Some(_) => return Err("status is \"\", picked, final or rejected.".to_string()),
    };
    f.tag = opt_str(args, "tag")?;
    f.starred = match args.get("starred") {
        None | Some(Value::Null) => None,
        Some(Value::Bool(b)) => Some(*b),
        Some(_) => return Err("starred is true or false.".to_string()),
    };
    f.query = opt_str(args, "query")?;
    if f.query.as_deref().is_some_and(|q| too_long(q, 200)) {
        return Err("A search is at most 200 characters.".to_string());
    }
    let limit = match args.get("limit") {
        None | Some(Value::Null) => LIST_DEFAULT,
        Some(v) => v.as_u64().filter(|n| (1..=u64::from(LIST_MAX)).contains(n)).ok_or_else(|| format!("limit is 1 to {LIST_MAX}."))? as u32,
    };
    f.limit = Some(limit);
    Ok(Some(f))
}

fn row_json(g: &Gallery, r: &AssetRow) -> Value {
    json!({
        "id": r.id,
        "title": r.title,
        "kind": r.kind,
        "status": r.status,
        "starred": r.starred,
        "tags": r.tags,
        "by": by_label(&r.made_by),
        "task_id": r.task_id,
        "created_at": r.created_at,
        "cost_usd": (r.cost_micros > 0).then(|| usd(r.cost_micros)),
        "path": g.files_dir().join(&r.file),
        "width": r.width,
        "height": r.height,
        "duration_ms": r.duration_ms,
        "parents": r.parents.len(),
        "children": r.children,
    })
}

async fn list(gc: &GalleryCaller, g: Arc<Gallery>, args: &Value) -> Result<Value, String> {
    let (args, paths) = (args.clone(), gc.caller.paths.clone());
    blocking(move || {
        let Some(mut f) = list_filter(&args, &worker::list_workers(&paths))? else {
            return Ok(json!({ "assets": [], "more": false }));
        };
        // The trash is the human's: never listed to an agent.
        f.include_trash = false;
        let limit = f.limit.unwrap_or(LIST_DEFAULT) as usize;
        f.limit = Some(limit as u32 + 1);
        let rows = g.list(&f)?;
        let more = rows.len() > limit;
        Ok(json!({ "assets": rows.iter().take(limit).map(|r| row_json(&g, r)).collect::<Vec<_>>(), "more": more }))
    })
    .await
}

/// An asset an agent may act on: filed, and out of the trash.
fn live_asset(g: &Gallery, id: &str) -> Result<Asset, String> {
    if !is_asset_id(id) {
        return Err(format!("\"{}\" isn't an asset id (a- and 12 hex digits).", one_line(id, 40)));
    }
    match g.get(id)? {
        None => Err(format!("No asset {id} in this Hive's gallery.")),
        Some(a) if a.purged => Err(format!("{id} was emptied from the trash: only its record is left.")),
        Some(a) if a.deleted_at.is_some() => Err(format!("{id} is in the trash: your human put it there.")),
        Some(a) => Ok(a),
    }
}

async fn get(g: Arc<Gallery>, args: &Value) -> Result<Value, String> {
    let id = opt_str(args, "id")?.ok_or("id is required: an asset id from galleryList or galleryAdd.")?;
    blocking(move || {
        let a = live_asset(&g, &id)?;
        let titled = |id: &str| -> Result<Value, String> {
            Ok(match g.get(id)? {
                Some(p) => json!({ "id": p.id, "title": p.title, "in_trash": p.in_trash() }),
                None => json!({ "id": id }),
            })
        };
        let parents = a.parents.iter().map(|p| titled(p)).collect::<Result<Vec<_>, _>>()?;
        let children: Vec<Value> = g.children_of(&a.id)?.into_iter().map(|(id, title)| json!({ "id": id, "title": title })).collect();
        let collections: Vec<Value> =
            g.collections()?.into_iter().filter(|c| a.collections.contains(&c.id)).map(|c| json!({ "id": c.id, "name": c.name })).collect();
        Ok(json!({
            "id": a.id,
            "title": a.title,
            "kind": a.kind,
            "mime": a.mime,
            "bytes": a.bytes,
            "sha256": a.sha256,
            "width": a.width,
            "height": a.height,
            "duration_ms": a.duration_ms,
            "caption": a.caption,
            "by": by_label(&a.made_by),
            "made_by": a.made_by,
            "task_id": a.task_id,
            "recipe": a.recipe,
            "status": a.status,
            "starred": a.starred,
            "tags": a.tags,
            "collections": collections,
            "parents": parents,
            "children": children,
            "path": g.file_path(&a),
            "created_at": a.created_at,
        }))
    })
    .await
}

// ── galleryTag ──

pub(crate) fn is_finals_status(s: &str) -> bool {
    s == STATUS_FINAL || s == STATUS_REJECTED
}

/// Into final or rejected, or out of one: a finals call. Picked to none isn't.
pub(crate) fn is_finals_move(from: &str, to: &str) -> bool {
    from != to && (is_finals_status(to) || is_finals_status(from))
}

/// Who may set what (plan D12): a worker tags, stars and picks, never over a final or rejected.
/// The coordinator sets anything; under the Finals checkpoint at Ask me its finals moves become
/// recommendations (`tag`), so nothing here refuses it.
pub(crate) fn tag_refusal(role: &str, targets: &[Asset], status: Option<&str>) -> Option<String> {
    let status = status?;
    if role != ROLE_WORKER {
        return None;
    }
    if is_finals_status(status) {
        return Some(format!("Status {status} is your coordinator's and your human's to set; you may set picked."));
    }
    targets
        .iter()
        .find(|a| is_finals_status(&a.status) && a.status != status)
        .map(|a| format!("{} is {}: only your coordinator or your human changes that.", a.id, a.status))
}

/// A status as an answer reads: `no status` for none.
pub(crate) fn status_label(s: &str) -> String {
    if s.is_empty() { "no status".to_string() } else { s.to_string() }
}

/// A finals call on one asset in maiTerm's words: the question, its options and the answer `to`
/// gives it. Un-finalizing is as much a finals call as finalizing.
pub(crate) fn finals_question(a: &Asset, to: &str) -> (String, Vec<String>, String) {
    let title = one_line(&a.title, 120);
    if is_finals_status(to) {
        (format!("Is \u{201c}{title}\u{201d} ({}, {}) a final?", a.kind, a.id), vec![STATUS_FINAL.to_string(), STATUS_REJECTED.to_string()], to.to_string())
    } else {
        let to = status_label(to);
        (format!("Is \u{201c}{title}\u{201d} ({}, {}) still {}?", a.kind, a.id, a.status), vec![a.status.clone(), to.clone()], to)
    }
}

/// A coordinator's finals line: a decision carries its answer, a recommendation carries it as
/// `recommended`.
fn finals_base(hive_id: &str, a: &Asset, to: &str, id: String, event: &str) -> DecisionLine {
    let (question, options, answer) = finals_question(a, to);
    let recommends = event == EVENT_RECOMMENDED;
    DecisionLine {
        id,
        event: event.to_string(),
        hive_id: hive_id.to_string(),
        category: checkpoints::FINALS.to_string(),
        source: SOURCE_GALLERY.to_string(),
        task_id: a.task_id.clone(),
        key: Some(a.id.clone()),
        question,
        options: options.into_iter().map(|label| decisions::DecisionOption { recommended: recommends && label == answer, label }).collect(),
        recommended: recommends.then(|| answer.clone()),
        answer: (!recommends).then_some(answer),
        by: DECIDED_BY_COORDINATOR.to_string(),
        at: crate::commands::workspace::iso_now(),
        ..DecisionLine::default()
    }
}

/// The coordinator's finals move under its Finals checkpoint: its own decision, one per asset
/// whose status it changed, which the human can override (and so hand Finals back, §7.6).
fn finals_line(hive_id: &str, a: &Asset, status: &str) -> DecisionLine {
    finals_base(hive_id, a, status, decisions::new_id(), EVENT_ANSWERED)
}

/// The asset's finals recommendation still waiting on the human: the coordinator's, under Ask
/// me, with no answer and not withdrawn.
pub(crate) fn open_recommendation(lines: &[DecisionLine], asset_id: &str) -> Option<decisions::Decision> {
    let mine: Vec<DecisionLine> =
        lines.iter().filter(|l| l.key.as_deref() == Some(asset_id) && l.source == SOURCE_GALLERY && l.category == checkpoints::FINALS).cloned().collect();
    decisions::fold(&mine).into_iter().find(|d| d.recommended.is_some() && d.answered.is_none() && d.lapsed.is_none())
}

/// Every open finals recommendation in the log: asset id → the recommended status (`""` none).
pub(crate) fn open_recommendations(lines: &[DecisionLine]) -> std::collections::BTreeMap<String, String> {
    let mine: Vec<DecisionLine> = lines.iter().filter(|l| l.source == SOURCE_GALLERY && l.category == checkpoints::FINALS).cloned().collect();
    decisions::fold(&mine)
        .into_iter()
        .filter(|d| d.answered.is_none() && d.lapsed.is_none())
        .filter_map(|d| {
            let r = d.recommended?;
            let status = r.recommended.filter(|s| s != "no status").unwrap_or_default();
            Some((r.key?, status))
        })
        .collect()
}

/// The coordinator's finals move under Ask me (§7.4): recorded as its recommendation for the
/// human, under the asset's open recommendation when it has one (the newest wins).
fn recommendation_line(lines: &[DecisionLine], hive_id: &str, a: &Asset, status: &str) -> DecisionLine {
    let id = open_recommendation(lines, &a.id).map_or_else(decisions::new_id, |d| d.id);
    finals_base(hive_id, a, status, id, EVENT_RECOMMENDED)
}

/// A recommendation the coordinator then decided itself, once Finals was its: withdrawn, so the
/// human's next move on the asset is an override, not an answer.
fn withdrawn_line(hive_id: &str, d: &decisions::Decision) -> DecisionLine {
    let r = d.recommended.clone().unwrap_or_default();
    DecisionLine {
        id: d.id.clone(),
        event: EVENT_LAPSED.to_string(),
        hive_id: hive_id.to_string(),
        category: checkpoints::FINALS.to_string(),
        source: SOURCE_GALLERY.to_string(),
        task_id: r.task_id,
        key: r.key,
        question: r.question,
        outcome: Some("withdrawn: the coordinator decided it once Finals was its".to_string()),
        by: decisions::BY_MAITERM.to_string(),
        at: crate::commands::workspace::iso_now(),
        ..DecisionLine::default()
    }
}

async fn tag<W: HiveWindow>(ctx: &Ctx<'_, W>, gc: &GalleryCaller, g: Arc<Gallery>, args: &Value) -> Result<Value, String> {
    let mut ids = asset_ids(args, "ids", MAX_IDS)?;
    if let Some(id) = opt_str(args, "id")? {
        if !ids.contains(&id) {
            ids.insert(0, id);
        }
    }
    if ids.is_empty() {
        return Err("Name the assets: id, or ids.".to_string());
    }
    let change = TagChange {
        tags: tags(args, "tags")?,
        untag: tags(args, "untag")?,
        status: match args.get("status") {
            None | Some(Value::Null) => None,
            Some(Value::String(s)) if STATUSES.contains(&s.trim()) => Some(s.trim().to_string()),
            Some(_) => return Err("status is \"\", picked, final or rejected.".to_string()),
        },
        starred: match args.get("starred") {
            None | Some(Value::Null) => None,
            Some(Value::Bool(b)) => Some(*b),
            Some(_) => return Err("starred is true or false.".to_string()),
        },
    };
    if change.tags.is_empty() && change.untag.is_empty() && change.status.is_none() && change.starred.is_none() {
        return Err("Nothing to change: give tags, untag, status or starred.".to_string());
    }
    let (role, hive, paths) = (gc.role, gc.caller.hive.clone(), gc.caller.paths.clone());
    let (after, decided, recommended) = blocking(move || {
        let before = ids.iter().map(|id| live_asset(&g, id)).collect::<Result<Vec<_>, _>>()?;
        if let Some(e) = tag_refusal(role, &before, change.status.as_deref()) {
            return Err(e);
        }
        let coordinator = role == ROLE_COORDINATOR;
        let asks = checkpoints::asks(&hive, checkpoints::FINALS);
        let status = change.status.clone();
        let finals: Vec<&Asset> = match status.as_deref() {
            Some(s) if coordinator => before.iter().filter(|a| is_finals_move(&a.status, s)).collect(),
            _ => Vec::new(),
        };
        let lines = if finals.is_empty() { Vec::new() } else { decisions::read(&paths) };
        let mut recommended: Vec<(String, String)> = Vec::new();
        let mut decided = 0;
        if asks && !finals.is_empty() {
            // Under Ask me a finals move is the coordinator's recommendation: the asset keeps its
            // status until the human picks. Everything else in the change still applies.
            let s = status.clone().unwrap_or_default();
            let direct: Vec<String> = before.iter().filter(|a| !is_finals_move(&a.status, &s)).map(|a| a.id.clone()).collect();
            g.tag(&ids, &TagChange { status: None, ..change.clone() })?;
            if !direct.is_empty() {
                g.tag(&direct, &TagChange { status: Some(s.clone()), ..TagChange::default() })?;
            }
            for a in &finals {
                decisions::append(&paths, &recommendation_line(&lines, &hive.id, a, &s))?;
                recommended.push((a.id.clone(), s.clone()));
            }
        } else {
            g.tag(&ids, &change)?;
            if let Some(s) = status.as_deref() {
                for a in &finals {
                    if let Some(open) = open_recommendation(&lines, &a.id) {
                        decisions::record(&paths, &withdrawn_line(&hive.id, &open));
                    }
                    decisions::record(&paths, &finals_line(&hive.id, a, s));
                    decided += 1;
                }
            }
        }
        let after = ids.iter().map(|id| g.get(id)?.ok_or_else(|| format!("{id} is gone from the gallery."))).collect::<Result<Vec<_>, _>>()?;
        Ok((after, decided, recommended))
    })
    .await?;
    let ids: Vec<String> = after.iter().map(|a| a.id.clone()).collect();
    ctx.window.gallery_changed(&gc.caller, &ids);
    if decided > 0 || !recommended.is_empty() {
        ctx.window.decisions_changed(&gc.caller.workspace_id);
    }
    let mut out = json!({
        "assets": after.iter().map(|a| json!({ "id": a.id, "title": a.title, "status": a.status, "starred": a.starred, "tags": a.tags })).collect::<Vec<_>>()
    });
    if !recommended.is_empty() {
        out["recommended"] = json!(recommended.iter().map(|(id, s)| json!({ "id": id, "status": s })).collect::<Vec<_>>());
        out["note"] = json!(
            "Picking finals is your human's in this Hive: maiTerm recorded these as your recommendation, which your human sees on \
             each asset in the gallery; the assets keep their status until they pick. Ask with a decision blocker when you need \
             the answer to go on."
        );
    }
    Ok(out)
}

// ── galleryCollect ──

async fn collect<W: HiveWindow>(ctx: &Ctx<'_, W>, gc: &GalleryCaller, g: Arc<Gallery>, args: &Value) -> Result<Value, String> {
    let collection = opt_str(args, "collection")?.ok_or("collection is required: a name (made if new) or an id.")?;
    let order = match args.get("order") {
        None | Some(Value::Null) => None,
        Some(_) => Some(asset_ids(args, "order", MAX_MEMBERS)?),
    };
    let cover = match args.get("cover") {
        None | Some(Value::Null) => None,
        Some(Value::String(s)) if s.trim().is_empty() => Some(String::new()),
        Some(Value::String(s)) if is_asset_id(s.trim()) => Some(s.trim().to_string()),
        Some(_) => return Err("cover is a member's asset id, or \"\" to clear it.".to_string()),
    };
    let change = CollectChange { collection, create: true, add: asset_ids(args, "add", MAX_MEMBERS)?, remove: asset_ids(args, "remove", MAX_MEMBERS)?, order, cover };
    let touched: Vec<String> = change.add.iter().chain(&change.remove).cloned().collect();
    let c = blocking(move || {
        let c = g.collect(&change)?;
        let members = c
            .asset_ids
            .iter()
            .map(|id| Ok(g.get(id)?.map(|a| json!({ "id": a.id, "title": a.title, "in_trash": a.in_trash() })).unwrap_or_else(|| json!({ "id": id }))))
            .collect::<Result<Vec<_>, String>>()?;
        Ok(json!({ "id": c.id, "name": c.name, "cover": c.cover, "assets": members }))
    })
    .await?;
    ctx.window.gallery_changed(&gc.caller, &touched);
    Ok(json!({ "collection": c }))
}

// ── galleryDeliver (§10.6) ──

/// `names` as given: a list of strings, none dropped, so a count that doesn't match is said.
fn names_arg(args: &Value) -> Result<Option<Vec<String>>, String> {
    match args.get("names") {
        None | Some(Value::Null) => Ok(None),
        Some(Value::Array(a)) if a.len() > super::deliver::MAX_DELIVER => Err(format!("At most {} names.", super::deliver::MAX_DELIVER)),
        Some(Value::Array(a)) => a.iter().map(|v| v.as_str().map(|s| s.trim().to_string()).ok_or_else(|| "names must be a list of file names".to_string())).collect::<Result<_, _>>().map(Some),
        Some(_) => Err("names must be a list of file names".to_string()),
    }
}

/// What a delivery or an export writes: the assets named (each out of the trash), or a
/// collection's members out of the trash in order (the trashed ones counted, not written).
/// Answers the assets, how many of a collection were skipped, and whether names are numbered.
pub(crate) fn delivery_assets(g: &Gallery, ids: &[String], collection: Option<&str>) -> Result<(Vec<Asset>, usize, bool), String> {
    match (ids.is_empty(), collection) {
        (false, Some(_)) => Err("Give assets or a collection, not both.".to_string()),
        (true, None) => Err("Name what to deliver: assets (ids) or a collection.".to_string()),
        (false, None) => Ok((ids.iter().map(|id| live_asset(g, id)).collect::<Result<Vec<_>, _>>()?, 0, false)),
        (true, Some(key)) => {
            let c = g
                .collections()?
                .into_iter()
                .find(|c| c.id == key || c.name.eq_ignore_ascii_case(key))
                .ok_or_else(|| format!("No collection called \"{}\".", one_line(key, 100)))?;
            let mut live = Vec::new();
            let mut skipped = 0;
            for id in &c.asset_ids {
                match g.get(id)? {
                    Some(a) if !a.in_trash() => live.push(a),
                    _ => skipped += 1,
                }
            }
            if live.is_empty() {
                return Err(format!("\u{201c}{}\u{201d} has nothing out of the trash to deliver.", one_line(&c.name, 100)));
            }
            Ok((live, skipped, true))
        }
    }
}

async fn deliver(gc: &GalleryCaller, g: Arc<Gallery>, args: &Value) -> Result<Value, String> {
    let folder = opt_str(args, "folder")?.ok_or("folder is required: a plain folder's name (with a subfolder: brand/icons), or a path inside one.")?;
    let ids = asset_ids(args, "assets", super::deliver::MAX_DELIVER)?;
    let collection = opt_str(args, "collection")?;
    let names = names_arg(args)?;
    let (hive, paths) = (gc.caller.hive.clone(), gc.caller.paths.clone());
    let out = blocking(move || {
        // Every check before the first byte (§7.3).
        let (dir, plain) = super::deliver::resolve_target(&hive, &folder)?;
        let (assets, skipped, numbered) = delivery_assets(&g, &ids, collection.as_deref())?;
        let names = match names {
            Some(n) => super::deliver::given_names(&assets, &n)?,
            None => super::deliver::derived_names(&assets, numbered),
        };
        super::deliver::check_config_names(&hive, &dir, &names)?;
        let items: Vec<(PathBuf, String)> = assets.iter().zip(&names).map(|(a, n)| (g.file_path(a), n.clone())).collect();
        let written = super::deliver::log_run(&paths, BY_COORDINATOR, &dir, &assets, super::deliver::deliver_files(&dir, &items))?;
        let mut v = json!({
            "folder": dir,
            "plain_folder": plain,
            "delivered": assets.iter().zip(&written).map(|(a, n)| json!({ "asset": a.id, "title": a.title, "name": n })).collect::<Vec<_>>(),
        });
        if skipped > 0 {
            v["skipped_in_trash"] = json!(skipped);
        }
        Ok(v)
    })
    .await?;
    log::info!("hive {}: the coordinator delivered {} file(s) into {}", gc.caller.hive.id, out["delivered"].as_array().map_or(0, Vec::len), out["folder"]);
    Ok(out)
}

// ── Assets handed to a worker: spawn `inputs`, message `assets` (§7.2, plan D23) ──

/// Asset ids under `k`: at most 20, each an id, repeats dropped.
pub(crate) fn input_ids(args: &Value, k: &str) -> Result<Vec<String>, String> {
    asset_ids(args, k, MAX_INPUTS)
}

/// Each asset filed in this Hive's gallery and out of the trash, with its stored path.
pub(crate) fn live_inputs(hive_id: &str, paths: &HivePaths, ids: &[String]) -> Result<Vec<(Asset, PathBuf)>, String> {
    if ids.is_empty() {
        return Ok(Vec::new());
    }
    let g = super::for_paths(hive_id, paths)?;
    ids.iter()
        .map(|id| {
            let a = live_asset(&g, id)?;
            let p = g.file_path(&a);
            Ok((a, p))
        })
        .collect()
}

/// maiTerm's own lines naming attached assets as paths, the titles escaped.
pub(crate) fn attached_lines(items: &[(Asset, PathBuf)]) -> String {
    let mut s = "[maiTerm] Attached from the gallery:".to_string();
    for (a, p) in items {
        s.push_str(&format!("\n- {} ({}): {}", one_line(&a.title, 200), one_line(&a.kind, 20), one_line(&p.to_string_lossy(), 1000)));
    }
    s
}

/// A worker's inputs as its brief lists them, read at each launch: one that has since gone to
/// the trash or out of the gallery says so.
pub(crate) fn brief_inputs(hive_id: &str, paths: &HivePaths, ids: &[String]) -> Vec<crate::hive::brief::GalleryInput> {
    if ids.is_empty() {
        return Vec::new();
    }
    let g = super::view::existing(hive_id, paths);
    ids.iter()
        .map(|id| {
            let a = g.as_deref().and_then(|g| g.get(id).ok().flatten());
            let mut input = crate::hive::brief::GalleryInput { id: id.clone(), ..Default::default() };
            let Some(a) = a else {
                input.note = Some("no longer in the gallery".to_string());
                return input;
            };
            input.path = (!a.purged).then(|| g.as_deref().map(|g| g.file_path(&a).to_string_lossy().to_string())).flatten();
            input.note = if a.purged { Some("emptied from the trash: only its record is left".to_string()) } else { a.deleted_at.as_ref().map(|_| "in the trash now".to_string()) };
            input.prompt = a.recipe.as_ref().and_then(|r| r.prompt.as_deref()).and_then(|p| p.lines().find(|l| !l.trim().is_empty())).map(str::to_string);
            input.title = a.title;
            input.kind = a.kind;
            input
        })
        .collect()
}

#[cfg(test)]
pub(crate) mod tests;
