//! The Files commands (docs/file-tree.md §5.2).
//!
//! Every one is async and does its work under `spawn_blocking`: a listing can hang on a dead
//! mount and a resolve runs git and reads the process table, and none of that may ever run on
//! the main thread (`list_files` and `git_show_file` are the counter-examples). Every path in
//! and out is root-relative POSIX, keyed by the subscription.

use std::sync::Arc;

use tauri::State;

use super::types::{DirReq, Envelope, OpenAck, PlaceHints, SearchResult, StatInfo, TreePlace};
use super::FileTreeState;
use crate::state::AppState;

async fn blocking<T: Send + 'static>(f: impl FnOnce() -> Result<T, String> + Send + 'static) -> Result<T, String> {
    tauri::async_runtime::spawn_blocking(f).await.map_err(|e| e.to_string())?
}

/// Which folder a tab's tree shows (§4.1).
#[tauri::command]
pub async fn ft_resolve_place(state: State<'_, Arc<AppState>>, tab_id: String, hints: PlaceHints) -> Result<TreePlace, String> {
    let st = state.inner().clone();
    blocking(move || super::place::resolve(&st, &tab_id, &hints)).await
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
    let (excludes, git) = {
        let data = state.app_data.read();
        (data.preferences.file_tree_exclude.clone(), data.preferences.file_tree_git)
    };
    let ft = ft.inner().clone();
    let label = window.label().to_string();
    blocking(move || {
        ft.set_git_enabled(git);
        ft.open(&label, &place, restore, Arc::new(on_event), excludes)
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

/// Absolute paths (an editor's file, a hook's path) as rels under the root, spelled as the
/// disk spells them; None for a path outside it.
#[tauri::command]
pub async fn ft_locate(ft: State<'_, Arc<FileTreeState>>, sub_id: u64, abs_paths: Vec<String>) -> Result<Vec<Option<String>>, String> {
    let ft = ft.inner().clone();
    blocking(move || Ok(ft.hub_of(sub_id)?.locate(&abs_paths))).await
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

/// A file's content at "HEAD" or in the "index", for Compare with HEAD. None: the file is new
/// since that base.
#[tauri::command]
pub async fn ft_diff_base(ft: State<'_, Arc<FileTreeState>>, sub_id: u64, rel: String, base: String) -> Result<Option<String>, String> {
    let ft = ft.inner().clone();
    blocking(move || ft.hub_of(sub_id)?.diff_base(&rel, &base)).await
}
