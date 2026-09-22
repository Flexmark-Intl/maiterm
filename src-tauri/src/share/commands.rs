//! Tauri commands for Workspace Share. Anything that runs git is async + `spawn_blocking`.

use std::path::PathBuf;
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use tauri::State;

use super::git::{self, DirVerdict, Probe};
use super::{ExportOptions, ExportPreview, ImportOptions, ImportResult, RootKind, ShareFile, SharedRoot, TabShareContext};
use crate::state::{save_state, AppState, Workspace};

fn find_workspace(state: &AppState, label: &str, workspace_id: &str) -> Result<Workspace, String> {
    let data = state.app_data.read();
    data.window(label)
        .and_then(|w| w.workspaces.iter().find(|ws| ws.id == workspace_id))
        .cloned()
        .ok_or_else(|| "Workspace not found".to_string())
}

async fn blocking<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> Result<T, String> {
    tauri::async_runtime::spawn_blocking(f).await.map_err(|e| format!("share task failed: {e}"))
}

#[tauri::command]
pub async fn share_export_preview(
    window: tauri::Window,
    state: State<'_, Arc<AppState>>,
    workspace_id: String,
    contexts: Vec<TabShareContext>,
) -> Result<ExportPreview, String> {
    let ws = find_workspace(&state, window.label(), &workspace_id)?;
    blocking(move || super::export_preview(&ws, &contexts)).await
}

#[tauri::command]
pub async fn share_export_write(
    window: tauri::Window,
    state: State<'_, Arc<AppState>>,
    workspace_id: String,
    contexts: Vec<TabShareContext>,
    options: ExportOptions,
    path: String,
) -> Result<(), String> {
    let ws = find_workspace(&state, window.label(), &workspace_id)?;
    blocking(move || {
        let file = super::export_file(&ws, &contexts, &options);
        super::write_file(&file, &PathBuf::from(&path))?;
        log::info!("share: exported workspace '{}' to {}", ws.name, path);
        Ok(())
    })
    .await?
}

/// A root as the import wizard first sees it: where the sender had it, what that is here.
#[derive(Debug, Clone, Serialize)]
pub struct RootCheck {
    pub root_id: String,
    /// The recorded path with `~` expanded for THIS machine.
    pub local_path: String,
    /// The directory name "Create all in…" uses.
    pub repo_name: Option<String>,
    #[serde(flatten)]
    pub verdict: DirVerdict,
}

#[derive(Debug, Clone, Serialize)]
pub struct ImportPreview {
    pub file: ShareFile,
    pub roots: Vec<RootCheck>,
}

/// The one rule (§4), for either kind of root. A plain root has nothing to clone or match:
/// any existing directory will do.
fn check(root: &SharedRoot, dir: &std::path::Path) -> DirVerdict {
    match root.kind {
        RootKind::Git => git::classify_git_dir(dir, &root.remotes),
        RootKind::Plain if dir.is_dir() => DirVerdict::Use,
        RootKind::Plain => DirVerdict::Reject { reason: "it doesn't exist on this computer".to_string() },
    }
}

#[tauri::command]
pub async fn share_read_file(path: String) -> Result<ImportPreview, String> {
    blocking(move || {
        let file = super::read_file(&PathBuf::from(&path))?;
        let roots = file
            .roots
            .iter()
            .map(|r| {
                let local = super::expand_tilde(&r.path);
                RootCheck {
                    root_id: r.id.clone(),
                    local_path: local.to_string_lossy().to_string(),
                    repo_name: r.clone_url().map(|u| git::repo_name(u)),
                    verdict: check(r, &local),
                }
            })
            .collect();
        Ok(ImportPreview { file, roots })
    })
    .await?
}

#[derive(Debug, Clone, Deserialize)]
pub struct DirCheck {
    pub root: SharedRoot,
    pub dir: String,
}

#[tauri::command]
pub async fn share_check_dirs(checks: Vec<DirCheck>) -> Result<Vec<DirVerdict>, String> {
    blocking(move || checks.iter().map(|c| check(&c.root, &super::expand_tilde(&c.dir))).collect()).await
}

#[derive(Debug, Clone, Deserialize)]
pub struct ProbeRequest {
    pub url: String,
    #[serde(default)]
    pub branch: Option<String>,
}

/// `git ls-remote` every repo at once — each can take its full timeout, and there is no
/// reason for a slow host to delay the others.
#[tauri::command]
pub async fn share_probe(requests: Vec<ProbeRequest>) -> Result<Vec<Probe>, String> {
    blocking(move || {
        let handles: Vec<_> = requests
            .into_iter()
            .map(|r| std::thread::spawn(move || git::probe(&r.url, r.branch.as_deref())))
            .collect();
        handles
            .into_iter()
            .map(|h| h.join().unwrap_or(Probe::Unverified { message: "probe crashed".to_string() }))
            .collect()
    })
    .await
}

fn sh_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
}

/// The line typed into the clone tab (§4 step 2). Built here so quoting has one owner.
#[tauri::command]
pub fn share_clone_command(url: String, dest: String, branch: Option<String>) -> String {
    let branch = branch.map(|b| format!(" --branch {}", sh_quote(&b))).unwrap_or_default();
    format!("git clone{branch} -- {} {}", sh_quote(&url), sh_quote(&dest))
}

/// Build the workspace and put it in this window, after the active one, active (§4 step 3).
#[tauri::command]
pub async fn share_import_build(
    window: tauri::Window,
    state: State<'_, Arc<AppState>>,
    path: String,
    options: ImportOptions,
) -> Result<ImportResult, String> {
    let result = blocking(move || -> Result<ImportResult, String> {
        let file = super::read_file(&PathBuf::from(&path))?;
        Ok(super::build_workspace(&file, &options))
    })
    .await??;
    let label = window.label().to_string();
    let data = {
        let mut app_data = state.app_data.write();
        let win = app_data.window_mut(&label).ok_or("Window not found")?;
        let at = win
            .active_workspace_id
            .as_ref()
            .and_then(|id| win.workspaces.iter().position(|w| &w.id == id))
            .map(|i| i + 1)
            .unwrap_or(win.workspaces.len());
        win.workspaces.insert(at, result.workspace.clone());
        win.active_workspace_id = Some(result.workspace.id.clone());
        app_data.clone()
    };
    save_state(&data)?;
    log::info!(
        "share: imported workspace '{}' ({} agent tab(s), {} notice(s))",
        result.workspace.name,
        result.launches.len(),
        result.notices.len()
    );
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clone_command_quotes_every_argument() {
        assert_eq!(
            share_clone_command("git@h:o/r.git".into(), "/Users/me/it's here".into(), Some("main".into())),
            "git clone --branch 'main' -- 'git@h:o/r.git' '/Users/me/it'\\''s here'"
        );
        assert_eq!(share_clone_command("u".into(), "d".into(), None), "git clone -- 'u' 'd'");
    }
}
