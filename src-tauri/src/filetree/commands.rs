//! The Files commands (docs/file-tree.md §5.2).
//!
//! Every one is async and does its work under `spawn_blocking`: a listing can hang on a dead
//! mount and a resolve runs git and reads the process table, and none of that may ever run on
//! the main thread (`list_files` and `git_show_file` are the counter-examples). Every path in
//! and out is root-relative POSIX, keyed by the subscription.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use tauri::State;

use super::ops::{CwdHolder, CwdOf};
use super::types::{DeleteTicket, DirReq, Envelope, FileOp, OpResult, OpenAck, PlaceHints, SearchResult, StatInfo, TreePlace};
use super::FileTreeState;
use crate::state::AppState;

async fn blocking<T: Send + 'static>(f: impl FnOnce() -> Result<T, String> + Send + 'static) -> Result<T, String> {
    tauri::async_runtime::spawn_blocking(f).await.map_err(|e| e.to_string())?
}

/// Which folder a tab's tree shows (§4.1). An ssh tab's candidates are checked on the host.
#[tauri::command]
pub async fn ft_resolve_place(state: State<'_, Arc<AppState>>, ft: State<'_, Arc<FileTreeState>>, tab_id: String, hints: PlaceHints) -> Result<TreePlace, String> {
    let (st, ft) = (state.inner().clone(), ft.inner().clone());
    blocking(move || super::place::resolve(&st, &ft, &tab_id, &hints)).await
}

/// Subscribe this webview to a root. `restore` re-opens the folders the view had, with the
/// generations it holds. Events start on `on_event`, possibly before this returns.
#[tauri::command]
pub async fn ft_open(
    window: tauri::Window,
    state: State<'_, Arc<AppState>>,
    ft: State<'_, Arc<FileTreeState>>,
    place: String,
    restore: Vec<DirReq>,
    on_event: tauri::ipc::Channel<Envelope>,
) -> Result<OpenAck, String> {
    let (excludes, git, agents) = {
        let data = state.app_data.read();
        (data.preferences.file_tree_exclude.clone(), data.preferences.file_tree_git, data.preferences.file_tree_agent_activity)
    };
    let ft = ft.inner().clone();
    let label = window.label().to_string();
    let lookup = super::remote::tunnel_lookup(state.inner());
    blocking(move || {
        ft.set_git_enabled(git);
        ft.agents.set_routing(agents);
        ft.open_with(&label, &place, restore, Arc::new(on_event), excludes, Some(lookup))
    })
    .await
}

/// End a subscription (closing the column, or switching roots).
#[tauri::command]
pub async fn ft_close(ft: State<'_, Arc<FileTreeState>>, sub_id: u64) -> Result<(), String> {
    let ft = ft.inner().clone();
    blocking(move || {
        ft.close(sub_id);
        Ok(())
    })
    .await
}

/// `document.visibilitychange`: nothing is sent while paused; changed folders are resent whole
/// on resume.
#[tauri::command]
pub async fn ft_pause(ft: State<'_, Arc<FileTreeState>>, sub_id: u64, paused: bool) -> Result<(), String> {
    let ft = ft.inner().clone();
    blocking(move || {
        ft.hub_of(sub_id)?.pause(sub_id, paused);
        Ok(())
    })
    .await
}

/// The client has applied every message up to `seq` (§5.4).
#[tauri::command]
pub async fn ft_ack(ft: State<'_, Arc<FileTreeState>>, sub_id: u64, seq: u64) -> Result<(), String> {
    let ft = ft.inner().clone();
    blocking(move || {
        ft.hub_of(sub_id)?.ack(sub_id, seq);
        Ok(())
    })
    .await
}

/// Open folders. Each answer arrives on the Channel as `dir` chunks or `not_modified`.
#[tauri::command]
pub async fn ft_expand(ft: State<'_, Arc<FileTreeState>>, sub_id: u64, dirs: Vec<DirReq>) -> Result<(), String> {
    let ft = ft.inner().clone();
    blocking(move || {
        ft.hub_of(sub_id)?.expand(sub_id, dirs);
        Ok(())
    })
    .await
}

/// Close folders: their snapshots and (Linux, shallow) watches go when nobody has them open.
#[tauri::command]
pub async fn ft_collapse(ft: State<'_, Arc<FileTreeState>>, sub_id: u64, dirs: Vec<String>) -> Result<(), String> {
    let ft = ft.inner().clone();
    blocking(move || {
        ft.hub_of(sub_id)?.collapse(sub_id, dirs);
        Ok(())
    })
    .await
}

/// A full `dir` for `rel`, or for every open folder with None. What a client does when a
/// delta's `from` isn't the generation it holds, or on `resync`.
#[tauri::command]
pub async fn ft_refresh(ft: State<'_, Arc<FileTreeState>>, sub_id: u64, rel: Option<String>) -> Result<(), String> {
    let ft = ft.inner().clone();
    blocking(move || {
        ft.hub_of(sub_id)?.refresh(sub_id, rel);
        Ok(())
    })
    .await
}

/// A remote place's Retry, when opening it failed (§15.3): the host's helper, stuck on "needs
/// key-based ssh", "no python3" or a reconnect backoff, tries again now. A subscription's
/// Refresh does this through `ft_refresh`, but an open that failed has no subscription.
#[tauri::command]
pub async fn ft_retry_host(state: State<'_, Arc<AppState>>, ft: State<'_, Arc<FileTreeState>>, place: String) -> Result<(), String> {
    let ft = ft.inner().clone();
    let lookup = super::remote::tunnel_lookup(state.inner());
    blocking(move || {
        let (host, _) = super::place::split_remote(place.trim()).ok_or_else(|| format!("Not a folder on an ssh host: {place}"))?;
        ft.remotes.conn(&host, Some(lookup)).retry();
        Ok(())
    })
    .await
}

/// This computer's name, for the wrong-host guard (§15.3): OSC 7 naming it is the local shell's
/// report from before ssh started, not a third machine.
#[tauri::command]
pub async fn ft_local_hostname() -> Result<String, String> {
    blocking(|| Ok(sysinfo::System::host_name().unwrap_or_default())).await
}

/// Absolute paths (an editor's file, a hook's path) as rels under the root, spelled as the
/// disk spells them; None for a path outside it.
#[tauri::command]
pub async fn ft_locate(ft: State<'_, Arc<FileTreeState>>, sub_id: u64, abs_paths: Vec<String>) -> Result<Vec<Option<String>>, String> {
    let ft = ft.inner().clone();
    blocking(move || Ok(ft.hub_of(sub_id)?.locate(&abs_paths))).await
}

/// Whether each absolute path is on disk: with a subscription, on its root's machine (an ssh
/// host is asked, for a path under the root); without one, on this computer. None where it
/// can't be told. `revealInTree` asks this first, so a path that isn't there is answered as
/// such and never raises a chip (§9.6).
#[tauri::command]
pub async fn ft_exists(ft: State<'_, Arc<FileTreeState>>, sub_id: Option<u64>, abs_paths: Vec<String>) -> Result<Vec<Option<bool>>, String> {
    let ft = ft.inner().clone();
    blocking(move || match sub_id {
        Some(id) => Ok(ft.hub_of(id)?.exists(&abs_paths)),
        None => Ok(abs_paths.iter().map(|p| super::paths::on_disk(std::path::Path::new(p))).collect()),
    })
    .await
}

/// Size, mtime, exec bit, link target and (for a changed file) its line counts against HEAD,
/// for the hover tooltip only.
#[tauri::command]
pub async fn ft_stat(ft: State<'_, Arc<FileTreeState>>, sub_id: u64, rel: String) -> Result<StatInfo, String> {
    let ft = ft.inner().clone();
    blocking(move || ft.hub_of(sub_id)?.stat(&rel)).await
}

/// Find (§8): the best `limit` (≤500) paths for `query`. `seq` orders one subscription's
/// queries: an older one still scanning stops when a newer one from the same view arrives. `index_shallow`: the human asked to index a
/// home or volume root.
#[tauri::command]
pub async fn ft_search(
    ft: State<'_, Arc<FileTreeState>>,
    sub_id: u64,
    query: String,
    limit: u32,
    seq: u64,
    index_shallow: Option<bool>,
) -> Result<SearchResult, String> {
    let ft = ft.inner().clone();
    blocking(move || Ok(ft.hub_of(sub_id)?.search(sub_id, &query, limit as usize, seq, index_shallow.unwrap_or(false)))).await
}

/// Every tab working in some folder on this computer, for the live-cwd guard (§10.3): each
/// live terminal's shell (its cwd read at the moment of the op), each agent session in a live
/// local tab, and each running stack service's folder. An agent in an ssh tab works on the
/// remote host: its cwd names a folder there, not here.
fn live_cwd_holders(state: &AppState) -> Vec<CwdHolder> {
    let (names, services): (HashMap<String, String>, HashMap<String, (String, String)>) = {
        let data = state.app_data.read();
        let mut names = HashMap::new();
        let mut services = HashMap::new();
        for ws in data.windows.iter().flat_map(|w| w.workspaces.iter()) {
            for t in ws.panes.iter().flat_map(|p| p.tabs.iter()) {
                names.insert(t.id.clone(), t.name.clone());
                if let Some(sid) = &t.service_id {
                    if let Some(svc) = ws.stack.iter().find(|s| &s.id == sid) {
                        services.insert(sid.clone(), (t.id.clone(), svc.cwd.clone()));
                    }
                }
            }
        }
        (names, services)
    };
    let name = |id: &str| names.get(id).cloned().unwrap_or_else(|| id.to_string());
    let mut out = Vec::new();
    let live: Vec<(String, u32)> = {
        let tabs = state.tab_pty_map.read();
        let ptys = state.pty_registry.read();
        tabs.iter().filter_map(|(tab, pty)| ptys.get(pty).and_then(|h| h.child_pid).map(|pid| (tab.clone(), pid))).collect()
    };
    for (tab, pid) in &live {
        out.push(CwdHolder { tab_id: tab.clone(), tab_name: name(tab), cwd: CwdOf::Pid(*pid) });
    }
    let ssh_tabs: HashSet<String> = state.ssh_tunnels.read().values().flat_map(|t| t.tab_ids.iter().cloned()).collect();
    for s in state.agent_sessions.read().values() {
        let live_local = live.iter().any(|(t, _)| *t == s.tab_id) && !ssh_tabs.contains(&s.tab_id);
        if let (true, Some(cwd)) = (live_local, &s.cwd) {
            out.push(CwdHolder { tab_id: s.tab_id.clone(), tab_name: name(&s.tab_id), cwd: CwdOf::Path(cwd.into()) });
        }
    }
    for row in state.stack_runtime.read().values() {
        if !matches!(row.status.as_str(), "starting" | "running" | "ready") {
            continue;
        }
        if let Some((tab, cwd)) = services.get(&row.service_id) {
            out.push(CwdHolder { tab_id: tab.clone(), tab_name: name(tab), cwd: CwdOf::Path(cwd.into()) });
        }
    }
    out
}

/// Every agent session working on the ssh host `host` (a tunnel's connect args): (tab id, tab
/// name, its cwd there), for a remote root's live-cwd guard. Only the agent's own folder is
/// known for an ssh tab: the remote shell's cwd isn't readable from here.
fn remote_cwd_holders(state: &AppState, host: &str) -> Vec<(String, String, String)> {
    let tabs: HashSet<String> = state
        .ssh_tunnels
        .read()
        .values()
        .filter(|t| crate::watch::connect_args(&t.host_key) == host)
        .flat_map(|t| t.tab_ids.iter().cloned())
        .collect();
    if tabs.is_empty() {
        return Vec::new();
    }
    let names: HashMap<String, String> = {
        let data = state.app_data.read();
        data.windows.iter().flat_map(|w| w.workspaces.iter()).flat_map(|ws| ws.panes.iter()).flat_map(|p| p.tabs.iter()).map(|t| (t.id.clone(), t.name.clone())).collect()
    };
    state
        .agent_sessions
        .read()
        .values()
        .filter(|s| tabs.contains(&s.tab_id))
        .filter_map(|s| s.cwd.clone().map(|c| (s.tab_id.clone(), names.get(&s.tab_id).cloned().unwrap_or_else(|| s.tab_id.clone()), c)))
        .collect()
}

/// A file operation on the subscription's root (§10). Errors refuse the whole op before
/// anything changed; a guard the human can answer comes back as `needs`.
#[tauri::command]
pub async fn ft_op(state: State<'_, Arc<AppState>>, ft: State<'_, Arc<FileTreeState>>, sub_id: u64, op: FileOp) -> Result<OpResult, String> {
    let (st, ft) = (state.inner().clone(), ft.inner().clone());
    blocking(move || ft.op_with(sub_id, &op, &|| live_cwd_holders(&st), &|host| remote_cwd_holders(&st, host))).await
}

/// A single-use ticket, good for 60 s, for permanently deleting exactly `rels`, with what that
/// removes. `ft_op { kind: "delete" }` refuses anything without one.
#[tauri::command]
pub async fn ft_prepare_delete(ft: State<'_, Arc<FileTreeState>>, sub_id: u64, rels: Vec<String>) -> Result<DeleteTicket, String> {
    let ft = ft.inner().clone();
    blocking(move || ft.prepare_delete(sub_id, &rels)).await
}

/// A file's content at "HEAD" or in the "index", for Compare with HEAD. None: the file is new
/// since that base.
#[tauri::command]
pub async fn ft_diff_base(ft: State<'_, Arc<FileTreeState>>, sub_id: u64, rel: String, base: String) -> Result<Option<String>, String> {
    let ft = ft.inner().clone();
    blocking(move || ft.hub_of(sub_id)?.diff_base(&rel, &base)).await
}
