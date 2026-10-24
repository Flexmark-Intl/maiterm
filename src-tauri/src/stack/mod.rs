//! Any service, on any branch (docs/hive.md §6.9): the Rust side of a stack copy.
//!
//! A copy is a service run on one checkout, keyed by `(service_id, target)`. Before the store
//! types a copy's start line it asks `stack_resolve_copy`, which answers the folder, command and
//! env to type: re-rooted onto the target's checkout (`target`), placeholders filled with values
//! checked against strict charsets (`placeholders`), and ports from the in-memory book (`ports`).
//! A service on its own folder with no placeholders comes back exactly as defined, without
//! running git: the stack as it was before copies existed.

pub mod placeholders;
pub mod ports;
pub mod target;

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde::Serialize;
use tauri::State;

use crate::state::app_state::StackRuntimeRow;
use crate::state::{AppData, AppState, Service, Workspace};
use placeholders::{Needs, Values};
use target::{Context, Target};

/// The mirror's and the port book's key for a copy: the service id for its own folder, else
/// `<id>@<target>` (ids are uuids, so the first `@` splits it).
pub fn copy_key(service_id: &str, target: &str) -> String {
    if target.is_empty() {
        service_id.to_string()
    } else {
        format!("{service_id}@{target}")
    }
}

/// What the store types for one copy, and what it says about it.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct ResolvedCopy {
    pub copy_key: String,
    pub cwd: String,
    pub command: String,
    pub env: Vec<(String, String)>,
    /// The ports maiTerm booked for `{port}` and `{port2}`; None when it booked none.
    pub port: Option<u16>,
    pub port2: Option<u16>,
    pub branch: Option<String>,
    /// The short commit the checkout is at, for `web · on session-store@3f2a91c`.
    pub commit: Option<String>,
    /// The checkout's root; None on the own folder.
    pub checkout: Option<String>,
    /// The worker's slug, when it runs in a worker's worktree.
    pub worker: Option<String>,
    /// What a row calls the copy; empty for the own folder.
    pub label: String,
    /// No `{port}`: only one copy of it can run at a time.
    pub one_copy: bool,
}

/// Everything a resolve reads from the state, cloned out of the lock.
#[derive(Debug, Clone)]
pub struct CopyRequest {
    pub service: Service,
    pub target: String,
    /// The workspace a `shared_from` service is really in, for the refusal.
    pub shared_from_workspace: Option<String>,
    /// This workspace's services by normalized name → their port, through `shared_from`.
    pub ports_of: Vec<(String, Option<u16>)>,
    /// This workspace's Hive folder, whose workers' records name their worktrees.
    pub hive_root: Option<PathBuf>,
    pub forbidden_ports: Vec<u16>,
}

fn find_workspace<'a>(data: &'a AppData, id: &str) -> Option<&'a Workspace> {
    data.windows.iter().flat_map(|w| w.workspaces.iter()).find(|ws| ws.id == id)
}

/// The service a `shared_from` names, and the workspace it is in.
pub(crate) fn shared_source<'a>(data: &'a AppData, r: &crate::state::workspace::ServiceRef) -> Option<(&'a Workspace, &'a Service)> {
    let ws = find_workspace(data, &r.workspace_id)?;
    ws.stack.iter().find(|s| s.id == r.service_id).map(|s| (ws, s))
}

pub fn request_for(data: &AppData, label: &str, workspace_id: &str, service_id: &str, target: &str) -> Result<CopyRequest, String> {
    let ws = data
        .window(label)
        .and_then(|w| w.workspaces.iter().find(|ws| ws.id == workspace_id))
        .ok_or("Workspace not found")?;
    let service = ws.stack.iter().find(|s| s.id == service_id).ok_or("Service not found in this workspace")?.clone();
    let shared_from_workspace = service.shared_from.as_ref().map(|r| {
        find_workspace(data, &r.workspace_id).map(|w| w.name.clone()).unwrap_or_else(|| "a workspace that no longer exists".to_string())
    });
    let ports_of = ws
        .stack
        .iter()
        .filter(|s| !s.is_job())
        .map(|s| {
            let port = match &s.shared_from {
                Some(r) => shared_source(data, r).and_then(|(_, src)| src.port),
                None => s.port,
            };
            (Service::normalize_name(&s.name), port)
        })
        .collect();
    let hive_root = ws.hive.as_ref().and_then(|h| crate::hive::paths::hive_root(&h.id));
    Ok(CopyRequest { service, target: target.to_string(), shared_from_workspace, ports_of, hive_root, forbidden_ports: Vec::new() })
}

/// The workers of the Hive at `hive_root` and the folders a resolve guards. Reads disk.
pub fn context_for(hive_root: Option<&Path>) -> Context {
    Context {
        workers: hive_root.map(|r| crate::hive::worker::list_workers(&crate::hive::paths::HivePaths::new(r))).unwrap_or_default(),
        hives: crate::hive::paths::hives_dir(),
        stack_wt: target::stack_wt_root(),
    }
}

/// Work out one copy. Blocks on git and the filesystem.
pub fn resolve(req: &CopyRequest, ctx: &Context) -> Result<ResolvedCopy, String> {
    let s = &req.service;
    // Before the shortcut: a shared service is never started here, on any target. A legacy
    // service never has one.
    if s.shared_from.is_some() {
        let from = req.shared_from_workspace.as_deref().unwrap_or("another workspace");
        return Err(format!("{} is shared from {from}; it's started there", s.name));
    }
    let target = target::parse(&req.target)?;
    let key = copy_key(&s.id, &req.target);
    let needs = Needs::of(&s.command, &s.env);
    if target == Target::Own && !needs.any() {
        // Exactly what the store typed before copies existed: no git, nothing re-rooted or filled.
        return Ok(ResolvedCopy {
            copy_key: key,
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
        });
    }
    if let Some((k, _)) = s.env.iter().find(|(k, _)| !placeholders::valid_env_key(k)) {
        return Err(format!("env name {k:?} isn't one a shell takes (letters, digits and _, not starting with a digit)"));
    }
    placeholders::check(&Needs { checkout_in_command: needs.checkout_in_command, ..Needs::default() }, &Values::default())?;

    let mut values = Values::default();
    let (cwd, commit, checkout, worker, label);
    match &target {
        Target::Own => {
            let repo = if needs.branch || needs.checkout { Some(target::repo_of(Path::new(&s.cwd), ctx)?) } else { None };
            if needs.branch {
                values.branch = repo.as_ref().and_then(target::own_branch);
            }
            values.checkout = repo.as_ref().map(|r| r.top.display().to_string());
            values.worker = Some("main".to_string());
            (cwd, commit, checkout, worker, label) = (s.cwd.clone(), None, None, None, String::new());
        }
        Target::Checkout(_) | Target::Branch(_) => {
            let repo = target::repo_of(Path::new(&s.cwd), ctx)?;
            let place = match &target {
                Target::Checkout(p) => target::checkout_place(&repo, p, ctx)?,
                Target::Branch(b) => target::branch_place(&repo, b, ctx)?.0,
                Target::Own => unreachable!(),
            };
            let at = place.root.join(&repo.rel);
            if !at.is_dir() {
                let rel = repo.rel.display().to_string().replace('\\', "/");
                return Err(format!("{rel}/ doesn't exist on {}", place.label()));
            }
            values.branch = place.branch.clone();
            values.checkout = Some(place.root.display().to_string());
            values.worker = place.worker.clone().or_else(|| {
                let slug = crate::hive::create::slugify(place.branch.as_deref().unwrap_or(&place.label()));
                (!slug.is_empty()).then_some(slug)
            });
            let head = place.head_short();
            if let Err(e) = &head {
                log::debug!("stack: no commit for {}: {e}", place.root.display());
            }
            (cwd, commit, checkout, worker, label) =
                (at.display().to_string(), head.ok(), Some(place.root.display().to_string()), place.worker.clone(), place.label());
        }
    }
    for name in &needs.port_of {
        let want = Service::normalize_name(name);
        match req.ports_of.iter().find(|(n, _)| *n == want) {
            Some((_, Some(p))) => values.port_of.push((name.clone(), *p)),
            Some((_, None)) => return Err(format!("{{port:{name}}}: {name} has no port yet; start it first")),
            None => return Err(format!("{{port:{name}}}: there is no service called {name} in this workspace")),
        }
    }
    placeholders::check(&needs, &values)?;
    // Last, so no refusal above leaves a port booked.
    if needs.port || needs.port2 {
        let n = if needs.port2 { 2 } else { 1 };
        let base = s.port.unwrap_or(ports::DEFAULT_BASE);
        let got = ports::take(&key, n, base, target == Target::Own, &req.forbidden_ports)?;
        values.port = got.first().copied();
        values.port2 = got.get(1).copied().filter(|_| needs.port2);
    }
    let command = placeholders::fill(&s.command, &values)?;
    let env = s.env.iter().map(|(k, v)| Ok((k.clone(), placeholders::fill(v, &values)?))).collect::<Result<Vec<_>, String>>()?;
    Ok(ResolvedCopy {
        copy_key: key,
        cwd,
        command,
        env,
        port: values.port.filter(|_| needs.port || needs.port2),
        port2: values.port2,
        branch: values.branch,
        commit,
        checkout,
        worker,
        label,
        one_copy: needs.one_copy(),
    })
}

/// What a priming row calls a copy whose store gave no label.
fn target_label(target: &str) -> String {
    match target::parse(target) {
        Ok(Target::Branch(b)) => b,
        Ok(Target::Checkout(p)) => p.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default(),
        _ => target.to_string(),
    }
}

/// `name (status[, endpoint])`, as the priming has always written a service.
fn priming_entry(name: &str, status: &str, url: Option<&str>, port: Option<u16>) -> String {
    let mut d = format!("{name} ({status}");
    // The endpoint is remembered across runs (listStack shows it as `stale`), but
    // "dev (stopped, http://localhost:5174/)" reads as an invitation to curl it.
    let up = matches!(status, "running" | "ready");
    if let (true, Some(url)) = (up, url) {
        d.push_str(&format!(", {url}"));
    } else if let (true, Some(port)) = (up, port) {
        d.push_str(&format!(", :{port}"));
    }
    d.push(')');
    d
}

/// The priming's stack list for one workspace (docs/stack.md §6.2): each service's own copy as
/// before, then one entry per other copy that isn't stopped, and a shared service with its
/// source's status.
pub fn priming_entries(data: &AppData, ws: &Workspace, runtime: &HashMap<String, StackRuntimeRow>) -> Vec<String> {
    let status_of = |key: &str| runtime.get(key).map(|r| r.status.as_str()).unwrap_or("stopped");
    let mut parts = Vec::new();
    // A run-queue job is the queue's to report, never a service the priming offers (§6.8).
    for s in ws.stack.iter().filter(|s| !s.is_job()) {
        if let Some(r) = &s.shared_from {
            parts.push(match shared_source(data, r) {
                Some((src_ws, src)) => {
                    // A workspace name is typed by a human, and the hook echoes this single-quoted.
                    let from: String = src_ws.name.chars().filter(|c| !c.is_control()).collect::<String>().replace('\'', "\u{2019}");
                    priming_entry(&format!("{} from {from}", s.name), status_of(&src.id), src.url.as_deref(), src.port)
                }
                None => format!("{} (shared from a workspace that no longer has it)", s.name),
            });
            continue;
        }
        parts.push(priming_entry(&s.name, status_of(&s.id), s.url.as_deref(), s.port));
        let mut copies: Vec<&StackRuntimeRow> =
            runtime.values().filter(|r| r.service_id == s.id && !r.target.is_empty() && r.status != "stopped").collect();
        copies.sort_by(|a, b| a.target.cmp(&b.target));
        for r in copies {
            let label = if r.label.is_empty() { target_label(&r.target) } else { r.label.clone() };
            parts.push(priming_entry(&format!("{} on {label}", s.name), &r.status, None, None));
        }
    }
    parts
}

/// A `shared_from` service's source, as a window that doesn't hold it sees it (docs/hive.md
/// §6.8): the store reads this every few seconds rather than asking the other window.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct SharedRow {
    pub workspace_name: String,
    pub service_name: String,
    /// The source's own folder copy, "stopped" when nothing published one.
    pub status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub since_ms: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub port: Option<u16>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
}

/// The source `workspace_id`/`service_id` names, in any window; None when it is gone. A source
/// that is itself shared is followed no further: sharing names the workspace that runs it.
pub fn shared_row(data: &AppData, runtime: &HashMap<String, StackRuntimeRow>, workspace_id: &str, service_id: &str) -> Option<SharedRow> {
    let r = crate::state::workspace::ServiceRef { workspace_id: workspace_id.to_string(), service_id: service_id.to_string() };
    let (ws, src) = shared_source(data, &r)?;
    if src.shared_from.is_some() {
        return None;
    }
    let row = runtime.get(&src.id);
    Some(SharedRow {
        workspace_name: ws.name.clone(),
        service_name: src.name.clone(),
        status: row.map(|r| r.status.clone()).unwrap_or_else(|| "stopped".to_string()),
        note: row.and_then(|r| r.note.clone()),
        since_ms: row.and_then(|r| r.since_ms),
        port: src.port,
        url: src.url.clone(),
    })
}

#[tauri::command]
pub fn get_stack_runtime_row(state: State<'_, Arc<AppState>>, workspace_id: String, service_id: String) -> Result<Option<SharedRow>, String> {
    let data = state.app_data.read();
    let runtime = state.stack_runtime.read();
    Ok(shared_row(&data, &runtime, &workspace_id, &service_id))
}

/// Remove maiTerm's branch checkouts nothing needs any more (`target::prune`). At startup and
/// after each retarget.
pub fn prune_worktrees(state: &AppState) -> usize {
    let keep = target::targeted(&state.app_data.read());
    match target::stack_wt_root() {
        Some(base) => target::prune(&base, &keep),
        None => 0,
    }
}

fn forbidden_ports(state: &AppState) -> Vec<u16> {
    ports::forbidden(*state.mcp_port.read(), state.mailink_info.read().as_ref().map(|(_, p)| *p))
}

/// The folder, command and env to type for one copy of a service (docs/hive.md §6.9).
#[tauri::command]
pub async fn stack_resolve_copy(
    window: tauri::Window,
    state: State<'_, Arc<AppState>>,
    workspace_id: String,
    service_id: String,
    target: Option<String>,
) -> Result<ResolvedCopy, String> {
    let mut req = request_for(&state.app_data.read(), window.label(), &workspace_id, &service_id, &target.unwrap_or_default())?;
    req.forbidden_ports = forbidden_ports(&state);
    crate::hive::tools::blocking(move || resolve(&req, &context_for(req.hive_root.as_deref()))).await
}

/// Free a stopped copy's ports. False if it held none.
#[tauri::command]
pub fn stack_release_copy(copy_key: String) -> Result<bool, String> {
    Ok(ports::release(&copy_key))
}

/// The "Run on ▸" list for one service.
#[tauri::command]
pub async fn stack_copy_targets(
    window: tauri::Window,
    state: State<'_, Arc<AppState>>,
    workspace_id: String,
    service_id: String,
) -> Result<Vec<target::CopyTarget>, String> {
    let req = request_for(&state.app_data.read(), window.label(), &workspace_id, &service_id, "")?;
    if req.service.shared_from.is_some() {
        return Err(format!("{} is shared from {}; it runs there", req.service.name, req.shared_from_workspace.unwrap_or_default()));
    }
    crate::hive::tools::blocking(move || {
        let ctx = context_for(req.hive_root.as_deref());
        let repo = target::repo_of(Path::new(&req.service.cwd), &ctx).ok();
        Ok(target::copy_targets(repo.as_ref(), &ctx))
    })
    .await
}

/// An agent's `on` as a target: `"main"` → `""`, a worker → its worktree, else a branch.
#[tauri::command]
pub async fn stack_target_for(
    window: tauri::Window,
    state: State<'_, Arc<AppState>>,
    workspace_id: String,
    service_id: String,
    on: String,
) -> Result<String, String> {
    let req = request_for(&state.app_data.read(), window.label(), &workspace_id, &service_id, "")?;
    crate::hive::tools::blocking(move || {
        let ctx = context_for(req.hive_root.as_deref());
        // Only a worker needs the repo, and a branch name needs no git at all.
        let repo = if ctx.workers.is_empty() { None } else { target::repo_of(Path::new(&req.service.cwd), &ctx).ok() };
        target::target_for(&on, repo.as_ref(), &ctx)
    })
    .await
}

/// Remove maiTerm's branch checkouts whose branch is gone and that nothing targets.
#[tauri::command]
pub async fn stack_prune_worktrees(state: State<'_, Arc<AppState>>) -> Result<usize, String> {
    let state = state.inner().clone();
    crate::hive::tools::blocking(move || Ok(prune_worktrees(&state))).await
}

#[cfg(test)]
mod tests;
