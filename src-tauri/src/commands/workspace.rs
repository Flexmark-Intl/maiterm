use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};
use tauri::{Emitter, State};

use crate::state::{save_state, AppState, Pane, Preferences, Tab, Workspace};
use crate::state::workspace::WorkspaceNote;
use crate::state::persistence::{app_data_slug, parse_state};
use crate::state::workspace::{EditorFileInfo, SplitDirection, TabType};
use crate::state::ScrollbackDb;
use crate::commands::window::{TabContext, clone_workspace_with_id_mapping};

/// Extract any scrollback from imported AppData tabs into SQLite and clear from structs.
fn migrate_imported_scrollback(data: &mut crate::state::AppData, db: &ScrollbackDb) {
    for win in &mut data.windows {
        for ws in &mut win.workspaces {
            for pane in &mut ws.panes {
                for tab in &mut pane.tabs {
                    if let Some(ref sb) = tab.scrollback {
                        let _ = db.save(&tab.id, sb, None);
                        tab.scrollback = None;
                    }
                }
            }
            for tab in &mut ws.archived_tabs {
                if let Some(ref sb) = tab.scrollback {
                    let _ = db.save(&tab.id, sb, None);
                    tab.scrollback = None;
                }
            }
        }
    }
}

#[tauri::command]
pub fn exit_app(app: tauri::AppHandle, state: State<'_, Arc<AppState>>) {
    log::info!("exit_app called — cleaning up and terminating process");
    // Mark this run as having exited cleanly. If the process dies before
    // reaching this line, the marker stays on disk and the next run flags
    // previous_run_crashed=true in diagnostics.
    crate::state::persistence::clear_running_marker();
    // Shut down the Claude Code MCP server gracefully (releases the port)
    if let Some(tx) = state.mcp_shutdown.lock().take() {
        let _ = tx.send(true);
    }
    let port = *state.mcp_port.read();
    let auth = state.mcp_auth.read().clone().unwrap_or_default();
    if let Some(port) = port {
        for r in crate::claude_code::registrar::all_registrars() {
            r.unregister(port, &auth);
        }
    }

    // Kill all SSH MCP tunnels
    crate::commands::ssh_tunnel::kill_all_tunnels(&state);

    // Kill all remaining PTYs so their threads can exit cleanly
    let pty_ids: Vec<String> = state.pty_registry.read().keys().cloned().collect();
    for id in &pty_ids {
        let _ = crate::pty::kill_pty(&state, id);
    }

    // Spawn a watchdog: if the main exit doesn't complete within 2s
    // (e.g. PTY threads stuck on Windows), force-terminate the process.
    let app_clone = app.clone();
    std::thread::spawn(move || {
        std::thread::sleep(std::time::Duration::from_secs(2));
        log::warn!("Force-exiting after 2s timeout");
        app_clone.exit(0);
    });

    app.exit(0);
}

#[tauri::command]
pub fn sync_state(state: State<'_, Arc<AppState>>) -> Result<(), String> {
    log::info!("Forcing state sync to disk");
    let data_clone = state.app_data.read().clone();
    save_state(&data_clone)?;
    log::info!("State saved successfully");
    Ok(())
}

#[tauri::command]
pub fn get_app_data(state: State<'_, Arc<AppState>>) -> crate::state::AppData {
    state.app_data.read().clone()
}

#[tauri::command]
pub fn create_workspace(window: tauri::Window, state: State<'_, Arc<AppState>>, name: String) -> Result<Workspace, String> {
    let label = window.label().to_string();
    let workspace = Workspace::new(name);
    let data_clone = {
        let mut app_data = state.app_data.write();
        let win = app_data.window_mut(&label).ok_or("Window not found")?;
        win.workspaces.push(workspace.clone());
        win.active_workspace_id = Some(workspace.id.clone());
        app_data.clone()
    };
    save_state(&data_clone)?;
    Ok(workspace)
}

#[tauri::command]
pub fn delete_workspace(window: tauri::Window, state: State<'_, Arc<AppState>>, workspace_id: String) -> Result<(), String> {
    let label = window.label().to_string();
    let (data_clone, tab_ids) = {
        let mut app_data = state.app_data.write();
        let win = app_data.window_mut(&label).ok_or("Window not found")?;
        // Collect all tab IDs (active + archived) before removal for SQLite cleanup
        let tab_ids: Vec<String> = win.workspaces.iter()
            .filter(|w| w.id == workspace_id)
            .flat_map(|w| {
                w.panes.iter().flat_map(|p| p.tabs.iter().map(|t| t.id.clone()))
                    .chain(w.archived_tabs.iter().map(|t| t.id.clone()))
            })
            .collect();
        let old_index = win.workspaces.iter().position(|w| w.id == workspace_id).unwrap_or(0);
        win.workspaces.retain(|w| w.id != workspace_id);
        if win.active_workspace_id.as_ref() == Some(&workspace_id) {
            // Activate adjacent: prefer previous, fall back to next
            let adjacent = old_index.min(win.workspaces.len().saturating_sub(1));
            win.active_workspace_id = win.workspaces.get(adjacent).map(|w| w.id.clone());
        }
        (app_data.clone(), tab_ids)
    };
    // Clean up scrollback from SQLite
    for id in &tab_ids {
        let _ = state.scrollback_db.delete(id);
    }
    save_state(&data_clone)
}

#[tauri::command]
pub fn rename_workspace(
    window: tauri::Window,
    state: State<'_, Arc<AppState>>,
    workspace_id: String,
    name: String,
) -> Result<(), String> {
    let label = window.label().to_string();
    let data_clone = {
        let mut app_data = state.app_data.write();
        let win = app_data.window_mut(&label).ok_or("Window not found")?;
        if let Some(workspace) = win.workspaces.iter_mut().find(|w| w.id == workspace_id) {
            workspace.name = name;
        }
        app_data.clone()
    };
    save_state(&data_clone)
}

#[tauri::command]
pub fn split_pane(
    window: tauri::Window,
    state: State<'_, Arc<AppState>>,
    workspace_id: String,
    target_pane_id: String,
    direction: SplitDirection,
    scrollback: Option<String>,
    editor_file: Option<EditorFileInfo>,
) -> Result<Pane, String> {
    let label = window.label().to_string();
    let new_pane = if let Some(file_info) = editor_file {
        let name = file_info
            .file_path
            .rsplit('/')
            .next()
            .unwrap_or(&file_info.file_path)
            .to_string();
        let tab = Tab::new_editor(name.clone(), file_info);
        let tab_id = tab.id.clone();
        Pane {
            id: uuid::Uuid::new_v4().to_string(),
            name,
            tabs: vec![tab],
            active_tab_id: Some(tab_id),
        }
    } else {
        Pane::new("Terminal".to_string())
    };
    if let Some(ref sb) = scrollback {
        if let Some(tab) = new_pane.tabs.first() {
            let _ = state.scrollback_db.save(&tab.id, sb, None);
        }
    }
    let data_clone = {
        let mut app_data = state.app_data.write();
        let win = app_data.window_mut(&label).ok_or("Window not found")?;
        if let Some(workspace) = win.workspaces.iter_mut().find(|w| w.id == workspace_id) {
            if let Some(ref root) = workspace.split_root {
                workspace.split_root =
                    Some(root.split_pane(&target_pane_id, &new_pane.id, direction, false));
            }
            workspace.panes.push(new_pane.clone());
            workspace.active_pane_id = Some(new_pane.id.clone());
            app_data.clone()
        } else {
            return Err("Workspace not found".to_string());
        }
    };
    save_state(&data_clone)?;
    Ok(new_pane)
}

#[tauri::command]
pub fn delete_pane(
    window: tauri::Window,
    state: State<'_, Arc<AppState>>,
    workspace_id: String,
    pane_id: String,
) -> Result<(), String> {
    let label = window.label().to_string();
    let (data_clone, tab_ids) = {
        let mut app_data = state.app_data.write();
        let win = app_data.window_mut(&label).ok_or("Window not found")?;
        // Collect tab IDs before removal for SQLite cleanup
        let tab_ids: Vec<String> = win.workspaces.iter()
            .filter(|w| w.id == workspace_id)
            .flat_map(|w| w.panes.iter())
            .filter(|p| p.id == pane_id)
            .flat_map(|p| p.tabs.iter().map(|t| t.id.clone()))
            .collect();
        if let Some(workspace) = win.workspaces.iter_mut().find(|w| w.id == workspace_id) {
            if let Some(ref root) = workspace.split_root {
                workspace.split_root = root.remove_pane(&pane_id);
            }
            workspace.panes.retain(|p| p.id != pane_id);
            if workspace.active_pane_id.as_ref() == Some(&pane_id) {
                workspace.active_pane_id = workspace.panes.first().map(|p| p.id.clone());
            }
        }
        (app_data.clone(), tab_ids)
    };
    // Clean up scrollback from SQLite
    for id in &tab_ids {
        let _ = state.scrollback_db.delete(id);
    }
    save_state(&data_clone)
}

#[tauri::command]
pub fn rename_pane(
    window: tauri::Window,
    state: State<'_, Arc<AppState>>,
    workspace_id: String,
    pane_id: String,
    name: String,
) -> Result<(), String> {
    let label = window.label().to_string();
    let data_clone = {
        let mut app_data = state.app_data.write();
        let win = app_data.window_mut(&label).ok_or("Window not found")?;
        if let Some(workspace) = win.workspaces.iter_mut().find(|w| w.id == workspace_id) {
            if let Some(pane) = workspace.panes.iter_mut().find(|p| p.id == pane_id) {
                pane.name = name;
            }
        }
        app_data.clone()
    };
    save_state(&data_clone)
}

#[tauri::command]
pub fn create_tab(
    window: tauri::Window,
    state: State<'_, Arc<AppState>>,
    workspace_id: String,
    pane_id: String,
    name: String,
    after_tab_id: Option<String>,
) -> Result<Tab, String> {
    let label = window.label().to_string();
    let tab = Tab::new(name);
    let data_clone = {
        let mut app_data = state.app_data.write();
        let win = app_data.window_mut(&label).ok_or("Window not found")?;
        if let Some(workspace) = win.workspaces.iter_mut().find(|w| w.id == workspace_id) {
            if let Some(pane) = workspace.panes.iter_mut().find(|p| p.id == pane_id) {
                let insert_idx = after_tab_id
                    .and_then(|id| pane.tabs.iter().position(|t| t.id == id))
                    .map(|idx| idx + 1)
                    .unwrap_or(pane.tabs.len());
                pane.tabs.insert(insert_idx, tab.clone());
                pane.active_tab_id = Some(tab.id.clone());
                app_data.clone()
            } else {
                return Err("Pane not found".to_string());
            }
        } else {
            return Err("Pane not found".to_string());
        }
    };
    save_state(&data_clone)?;
    Ok(tab)
}

#[tauri::command]
pub fn delete_tab(
    window: tauri::Window,
    state: State<'_, Arc<AppState>>,
    workspace_id: String,
    pane_id: String,
    tab_id: String,
) -> Result<(), String> {
    let label = window.label().to_string();
    let data_clone = {
        let mut app_data = state.app_data.write();
        let win = app_data.window_mut(&label).ok_or("Window not found")?;
        if let Some(workspace) = win.workspaces.iter_mut().find(|w| w.id == workspace_id) {
            if let Some(pane) = workspace.panes.iter_mut().find(|p| p.id == pane_id) {
                if pane.active_tab_id.as_ref() == Some(&tab_id) {
                    let old_index = pane.tabs.iter().position(|t| t.id == tab_id).unwrap_or(0);
                    pane.tabs.retain(|t| t.id != tab_id);
                    pane.active_tab_id = if pane.tabs.is_empty() {
                        None
                    } else {
                        // Prefer previous (left) tab; fall back to next if closing first tab
                        let new_index = if old_index > 0 {
                            old_index - 1
                        } else {
                            0
                        };
                        Some(pane.tabs[new_index].id.clone())
                    };
                } else {
                    pane.tabs.retain(|t| t.id != tab_id);
                }
            }
        }
        app_data.clone()
    };
    // Clean up scrollback from SQLite
    let _ = state.scrollback_db.delete(&tab_id);
    save_state(&data_clone)
}

#[tauri::command]
pub fn move_tab_to_workspace(
    window: tauri::Window,
    state: State<'_, Arc<AppState>>,
    source_workspace_id: String,
    source_pane_id: String,
    tab_id: String,
    target_workspace_id: String,
) -> Result<(), String> {
    let label = window.label().to_string();
    let data_clone = {
        let mut app_data = state.app_data.write();
        let win = app_data.window_mut(&label).ok_or("Window not found")?;

        // Extract the tab from source pane
        let source_ws = win.workspaces.iter_mut().find(|w| w.id == source_workspace_id)
            .ok_or("Source workspace not found")?;
        let source_pane = source_ws.panes.iter_mut().find(|p| p.id == source_pane_id)
            .ok_or("Source pane not found")?;
        let tab_pos = source_pane.tabs.iter().position(|t| t.id == tab_id)
            .ok_or("Tab not found")?;
        let tab = source_pane.tabs.remove(tab_pos);

        // Fix source pane's active tab if we removed the active one
        if source_pane.active_tab_id.as_ref() == Some(&tab_id) {
            source_pane.active_tab_id = if source_pane.tabs.is_empty() {
                None
            } else {
                let new_index = if tab_pos > 0 { tab_pos - 1 } else { 0 };
                Some(source_pane.tabs[new_index].id.clone())
            };
        }

        // Insert into target workspace's first pane and make it the active tab
        let target_ws = win.workspaces.iter_mut().find(|w| w.id == target_workspace_id)
            .ok_or("Target workspace not found")?;
        let target_pane = target_ws.panes.first_mut()
            .ok_or("Target workspace has no panes")?;
        target_pane.active_tab_id = Some(tab.id.clone());
        target_pane.tabs.push(tab);

        app_data.clone()
    };
    save_state(&data_clone)
}

/// Move a tab between panes within the same workspace. The tab (and its PTY)
/// moves as-is — nothing is cloned or respawned. If the source pane is left
/// empty it is removed and the split tree collapses around it.
#[tauri::command]
pub fn move_tab_to_pane(
    window: tauri::Window,
    state: State<'_, Arc<AppState>>,
    workspace_id: String,
    source_pane_id: String,
    tab_id: String,
    target_pane_id: String,
    insert_before_tab_id: Option<String>,
) -> Result<(), String> {
    if source_pane_id == target_pane_id {
        return Err("Source and target pane are the same".to_string());
    }
    let label = window.label().to_string();
    let data_clone = {
        let mut app_data = state.app_data.write();
        let win = app_data.window_mut(&label).ok_or("Window not found")?;
        let workspace = win.workspaces.iter_mut().find(|w| w.id == workspace_id)
            .ok_or("Workspace not found")?;
        // Validate the target before mutating — extracting the tab first
        // would lose it if the target lookup failed.
        if !workspace.panes.iter().any(|p| p.id == target_pane_id) {
            return Err("Target pane not found".to_string());
        }

        // Extract the tab from the source pane
        let source_pane = workspace.panes.iter_mut().find(|p| p.id == source_pane_id)
            .ok_or("Source pane not found")?;
        let tab_pos = source_pane.tabs.iter().position(|t| t.id == tab_id)
            .ok_or("Tab not found")?;
        let tab = source_pane.tabs.remove(tab_pos);
        if source_pane.active_tab_id.as_ref() == Some(&tab_id) {
            source_pane.active_tab_id = if source_pane.tabs.is_empty() {
                None
            } else {
                let new_index = if tab_pos > 0 { tab_pos - 1 } else { 0 };
                Some(source_pane.tabs[new_index].id.clone())
            };
        }
        let source_now_empty = source_pane.tabs.is_empty();

        // Insert into the target pane and focus it
        let target_pane = workspace.panes.iter_mut().find(|p| p.id == target_pane_id)
            .ok_or("Target pane not found")?;
        let insert_pos = insert_before_tab_id
            .as_ref()
            .and_then(|id| target_pane.tabs.iter().position(|t| &t.id == id))
            .unwrap_or(target_pane.tabs.len());
        target_pane.tabs.insert(insert_pos, tab);
        target_pane.active_tab_id = Some(tab_id.clone());

        if source_now_empty {
            if let Some(ref root) = workspace.split_root {
                workspace.split_root = root.remove_pane(&source_pane_id);
            }
            workspace.panes.retain(|p| p.id != source_pane_id);
        }
        workspace.active_pane_id = Some(target_pane_id.clone());

        app_data.clone()
    };
    save_state(&data_clone)
}

/// Move a tab into a brand-new split pane (no clone — the tab itself moves,
/// PTY intact). The new pane is created by splitting `target_pane_id` in
/// `direction`; `before` places it on the left/top side. An emptied source
/// pane is removed and the split tree collapses around it.
#[tauri::command]
pub fn move_tab_to_split(
    window: tauri::Window,
    state: State<'_, Arc<AppState>>,
    workspace_id: String,
    source_pane_id: String,
    tab_id: String,
    target_pane_id: String,
    direction: SplitDirection,
    before: Option<bool>,
) -> Result<Pane, String> {
    let label = window.label().to_string();
    let (data_clone, new_pane) = {
        let mut app_data = state.app_data.write();
        let win = app_data.window_mut(&label).ok_or("Window not found")?;
        let workspace = win.workspaces.iter_mut().find(|w| w.id == workspace_id)
            .ok_or("Workspace not found")?;
        if !workspace.panes.iter().any(|p| p.id == target_pane_id) {
            return Err("Target pane not found".to_string());
        }

        let source_pane = workspace.panes.iter_mut().find(|p| p.id == source_pane_id)
            .ok_or("Source pane not found")?;
        // Splitting a pane off with its only tab would just churn pane IDs —
        // the layout ends up identical.
        if source_pane_id == target_pane_id && source_pane.tabs.len() == 1 {
            return Err("Cannot split a pane using its only tab".to_string());
        }
        let tab_pos = source_pane.tabs.iter().position(|t| t.id == tab_id)
            .ok_or("Tab not found")?;
        let tab = source_pane.tabs.remove(tab_pos);
        if source_pane.active_tab_id.as_ref() == Some(&tab_id) {
            source_pane.active_tab_id = if source_pane.tabs.is_empty() {
                None
            } else {
                let new_index = if tab_pos > 0 { tab_pos - 1 } else { 0 };
                Some(source_pane.tabs[new_index].id.clone())
            };
        }
        let source_now_empty = source_pane.tabs.is_empty();

        let pane_name = match tab.tab_type {
            TabType::Terminal => "Terminal".to_string(),
            _ => tab.name.clone(),
        };
        let new_pane = Pane {
            id: uuid::Uuid::new_v4().to_string(),
            name: pane_name,
            tabs: vec![tab],
            active_tab_id: Some(tab_id.clone()),
        };
        if let Some(ref root) = workspace.split_root {
            workspace.split_root = Some(root.split_pane(
                &target_pane_id,
                &new_pane.id,
                direction,
                before.unwrap_or(false),
            ));
        }
        workspace.panes.push(new_pane.clone());

        if source_now_empty {
            if let Some(ref root) = workspace.split_root {
                workspace.split_root = root.remove_pane(&source_pane_id);
            }
            workspace.panes.retain(|p| p.id != source_pane_id);
        }
        workspace.active_pane_id = Some(new_pane.id.clone());

        (app_data.clone(), new_pane)
    };
    save_state(&data_clone)?;
    Ok(new_pane)
}

#[tauri::command]
pub fn rename_tab(
    window: tauri::Window,
    state: State<'_, Arc<AppState>>,
    workspace_id: String,
    pane_id: String,
    tab_id: String,
    name: String,
    custom_name: Option<bool>,
) -> Result<(), String> {
    let label = window.label().to_string();
    let mut app_data = state.app_data.write();
    let win = app_data.window_mut(&label).ok_or("Window not found")?;
    if let Some(workspace) = win.workspaces.iter_mut().find(|w| w.id == workspace_id) {
        if let Some(pane) = workspace.panes.iter_mut().find(|p| p.id == pane_id) {
            if let Some(tab) = pane.tabs.iter_mut().find(|t| t.id == tab_id) {
                tab.name = name;
                if let Some(cn) = custom_name {
                    tab.custom_name = cn;
                }
            }
        }
    }
    // Persist eagerly: renames otherwise live only in memory until some other
    // command triggers save_state() or a clean shutdown runs sync_state. An
    // updater relaunch (or any uncatchable exit) would lose the rename.
    let data_clone = app_data.clone();
    drop(app_data);
    let _ = save_state(&data_clone);
    Ok(())
}

/// Update an editor tab's file info and name (for replacing the displayed file in-place).
/// Searches all workspaces/panes in the current window to find the tab by ID.
#[tauri::command]
pub fn update_editor_tab_file(
    window: tauri::Window,
    state: State<'_, Arc<AppState>>,
    tab_id: String,
    name: String,
    file_info: EditorFileInfo,
) -> Result<(), String> {
    let label = window.label().to_string();
    let mut app_data = state.app_data.write();
    let win = app_data.window_mut(&label).ok_or("Window not found")?;
    for workspace in &mut win.workspaces {
        for pane in &mut workspace.panes {
            if let Some(tab) = pane.tabs.iter_mut().find(|t| t.id == tab_id) {
                tab.editor_file = Some(file_info);
                tab.name = name;
                let data_clone = app_data.clone();
                drop(app_data);
                let _ = save_state(&data_clone);
                return Ok(());
            }
        }
    }
    Err("Tab not found".to_string())
}

#[tauri::command]
pub fn set_active_workspace(window: tauri::Window, state: State<'_, Arc<AppState>>, workspace_id: String) -> Result<(), String> {
    let label = window.label().to_string();
    let mut app_data = state.app_data.write();
    let win = app_data.window_mut(&label).ok_or("Window not found")?;
    win.active_workspace_id = Some(workspace_id.clone());
    // Clear import highlight on activation
    if let Some(ws) = win.workspaces.iter_mut().find(|w| w.id == workspace_id) {
        ws.import_highlight = false;
    }
    Ok(())
}

#[tauri::command]
pub fn suspend_workspace(window: tauri::Window, state: State<'_, Arc<AppState>>, workspace_id: String) -> Result<(), String> {
    let label = window.label().to_string();
    let mut app_data = state.app_data.write();
    let win = app_data.window_mut(&label).ok_or("Window not found")?;
    let ws = win.workspaces.iter_mut().find(|w| w.id == workspace_id).ok_or("Workspace not found")?;
    ws.suspended = true;
    // Give the workspace's live terminal tabs their proper suspended status — the
    // frontend already killed the PTYs, so leaving pty_id set would relapse the
    // high-watermark and make these tabs read as "live" on the next restart.
    for pane in ws.panes.iter_mut() {
        for tab in pane.tabs.iter_mut() {
            if matches!(tab.tab_type, TabType::Terminal) && tab.pty_id.is_some() {
                tab.pty_id = None;
                tab.suspended_at = Some(iso_now());
            }
        }
    }
    save_state(&app_data)?;
    Ok(())
}

#[tauri::command]
pub fn resume_workspace(window: tauri::Window, state: State<'_, Arc<AppState>>, workspace_id: String) -> Result<(), String> {
    let label = window.label().to_string();
    let mut app_data = state.app_data.write();
    let win = app_data.window_mut(&label).ok_or("Window not found")?;
    let ws = win.workspaces.iter_mut().find(|w| w.id == workspace_id).ok_or("Workspace not found")?;
    ws.suspended = false;
    save_state(&app_data)?;
    Ok(())
}

#[tauri::command]
pub fn set_active_pane(
    window: tauri::Window,
    state: State<'_, Arc<AppState>>,
    workspace_id: String,
    pane_id: String,
) -> Result<(), String> {
    let label = window.label().to_string();
    let mut app_data = state.app_data.write();
    let win = app_data.window_mut(&label).ok_or("Window not found")?;
    if let Some(workspace) = win.workspaces.iter_mut().find(|w| w.id == workspace_id) {
        workspace.active_pane_id = Some(pane_id);
    }
    Ok(())
}

#[tauri::command]
pub fn set_active_tab(
    window: tauri::Window,
    state: State<'_, Arc<AppState>>,
    workspace_id: String,
    pane_id: String,
    tab_id: String,
) -> Result<(), String> {
    let label = window.label().to_string();
    let mut app_data = state.app_data.write();
    let win = app_data.window_mut(&label).ok_or("Window not found")?;
    if let Some(workspace) = win.workspaces.iter_mut().find(|w| w.id == workspace_id) {
        if let Some(pane) = workspace.panes.iter_mut().find(|p| p.id == pane_id) {
            pane.active_tab_id = Some(tab_id.clone());
            // Clear import highlight on activation
            if let Some(tab) = pane.tabs.iter_mut().find(|t| t.id == tab_id) {
                tab.import_highlight = false;
            }
        }
    }
    Ok(())
}

#[tauri::command]
pub fn set_tab_pty_id(
    window: tauri::Window,
    state: State<'_, Arc<AppState>>,
    workspace_id: String,
    pane_id: String,
    tab_id: String,
    pty_id: String,
) -> Result<(), String> {
    let label = window.label().to_string();
    let mut app_data = state.app_data.write();
    let win = app_data.window_mut(&label).ok_or("Window not found")?;
    if let Some(workspace) = win.workspaces.iter_mut().find(|w| w.id == workspace_id) {
        if let Some(pane) = workspace.panes.iter_mut().find(|p| p.id == pane_id) {
            if let Some(tab) = pane.tabs.iter_mut().find(|t| t.id == tab_id) {
                tab.pty_id = Some(pty_id);
                // Tab is live again — drop its suspended-age timestamp.
                tab.suspended_at = None;
            }
        }
    }
    Ok(())
}

#[tauri::command]
pub fn suspend_tab(
    window: tauri::Window,
    state: State<'_, Arc<AppState>>,
    workspace_id: String,
    pane_id: String,
    tab_id: String,
    cwd: Option<String>,
    ssh_command: Option<String>,
    remote_cwd: Option<String>,
) -> Result<(), String> {
    let label = window.label().to_string();
    let mut app_data = state.app_data.write();
    let win = app_data.window_mut(&label).ok_or("Window not found")?;
    let workspace = win.workspaces.iter_mut()
        .find(|w| w.id == workspace_id)
        .ok_or("Workspace not found")?;
    let pane = workspace.panes.iter_mut()
        .find(|p| p.id == pane_id)
        .ok_or("Pane not found")?;
    let tab = pane.tabs.iter_mut()
        .find(|t| t.id == tab_id)
        .ok_or("Tab not found")?;

    tab.pty_id = None;
    tab.restore_cwd = cwd;
    tab.restore_ssh_command = ssh_command;
    tab.restore_remote_cwd = remote_cwd;
    tab.suspended_at = Some(iso_now());

    let data_clone = app_data.clone();
    drop(app_data);
    save_state(&data_clone)
}

/// One tab's new suspended state for `mark_tabs_suspended`.
#[derive(serde::Deserialize)]
pub struct TabSuspendMark {
    pub tab_id: String,
    /// Last-active time (ISO 8601) to record as the suspend age; None → now.
    pub suspended_at: Option<String>,
}

/// Bulk-mark terminal tabs as properly suspended: clear the stale `pty_id` and
/// stamp `suspended_at`. Session restore calls this for tabs that weren't
/// genuinely live at the last shutdown, so the on-disk active-vs-suspended state
/// stops lying (a tab keeps its `pty_id` forever unless explicitly suspended).
/// Window-scoped and idempotent; only the listed terminal tabs are touched.
#[tauri::command]
pub fn mark_tabs_suspended(
    window: tauri::Window,
    state: State<'_, Arc<AppState>>,
    updates: Vec<TabSuspendMark>,
) -> Result<(), String> {
    if updates.is_empty() {
        return Ok(());
    }
    let marks: HashMap<String, Option<String>> =
        updates.into_iter().map(|u| (u.tab_id, u.suspended_at)).collect();

    let label = window.label().to_string();
    let mut app_data = state.app_data.write();
    let win = app_data.window_mut(&label).ok_or("Window not found")?;

    let mut changed = false;
    for ws in win.workspaces.iter_mut() {
        for pane in ws.panes.iter_mut() {
            for tab in pane.tabs.iter_mut() {
                if !matches!(tab.tab_type, TabType::Terminal) {
                    continue;
                }
                if let Some(ts) = marks.get(&tab.id) {
                    tab.pty_id = None;
                    tab.suspended_at = Some(ts.clone().unwrap_or_else(iso_now));
                    changed = true;
                }
            }
        }
    }

    if changed {
        let data_clone = app_data.clone();
        drop(app_data);
        save_state(&data_clone)?;
    }
    Ok(())
}

#[tauri::command]
pub fn set_tab_pinned(
    window: tauri::Window,
    state: State<'_, Arc<AppState>>,
    workspace_id: String,
    pane_id: String,
    tab_id: String,
    pinned: bool,
) -> Result<(), String> {
    let label = window.label().to_string();
    let mut app_data = state.app_data.write();
    let win = app_data.window_mut(&label).ok_or("Window not found")?;
    let workspace = win.workspaces.iter_mut()
        .find(|w| w.id == workspace_id)
        .ok_or("Workspace not found")?;
    let pane = workspace.panes.iter_mut()
        .find(|p| p.id == pane_id)
        .ok_or("Pane not found")?;
    let tab = pane.tabs.iter_mut()
        .find(|t| t.id == tab_id)
        .ok_or("Tab not found")?;

    tab.pinned = pinned;

    let data_clone = app_data.clone();
    drop(app_data);
    save_state(&data_clone)
}

#[tauri::command]
pub fn set_sidebar_width(window: tauri::Window, state: State<'_, Arc<AppState>>, width: u32) -> Result<(), String> {
    let label = window.label().to_string();
    let mut app_data = state.app_data.write();
    let win = app_data.window_mut(&label).ok_or("Window not found")?;
    win.sidebar_width = width;
    Ok(())
}

#[tauri::command]
pub fn set_sidebar_collapsed(window: tauri::Window, state: State<'_, Arc<AppState>>, collapsed: bool) -> Result<(), String> {
    let label = window.label().to_string();
    let mut app_data = state.app_data.write();
    let win = app_data.window_mut(&label).ok_or("Window not found")?;
    win.sidebar_collapsed = collapsed;
    Ok(())
}

#[tauri::command]
pub fn set_split_ratio(
    window: tauri::Window,
    state: State<'_, Arc<AppState>>,
    workspace_id: String,
    split_id: String,
    ratio: f64,
) -> Result<(), String> {
    let label = window.label().to_string();
    let mut app_data = state.app_data.write();
    let win = app_data.window_mut(&label).ok_or("Window not found")?;
    if let Some(workspace) = win.workspaces.iter_mut().find(|w| w.id == workspace_id) {
        if let Some(ref root) = workspace.split_root {
            workspace.split_root = Some(root.set_ratio(&split_id, ratio.clamp(0.1, 0.9)));
        }
    }
    Ok(())
}

#[tauri::command]
pub fn set_tab_scrollback(
    state: State<'_, Arc<AppState>>,
    tab_id: String,
    scrollback: Option<String>,
) -> Result<(), String> {
    match scrollback {
        Some(ref data) => {
            // Record the live grid size when this tab has a running terminal,
            // so background spawns after restart use real dimensions.
            let size = state
                .tab_pty_map
                .read()
                .get(&tab_id)
                .cloned()
                .and_then(|pty_id| state.live_grid_size(&pty_id));
            state.scrollback_db.save(&tab_id, data, size)
        }
        None => state.scrollback_db.delete(&tab_id),
    }
}

#[tauri::command]
pub fn set_tab_notes(
    window: tauri::Window,
    state: State<'_, Arc<AppState>>,
    workspace_id: String,
    pane_id: String,
    tab_id: String,
    notes: Option<String>,
) -> Result<(), String> {
    let label = window.label().to_string();
    let mut app_data = state.app_data.write();
    let win = app_data.window_mut(&label).ok_or("Window not found")?;
    if let Some(workspace) = win.workspaces.iter_mut().find(|w| w.id == workspace_id) {
        if let Some(pane) = workspace.panes.iter_mut().find(|p| p.id == pane_id) {
            if let Some(tab) = pane.tabs.iter_mut().find(|t| t.id == tab_id) {
                tab.notes = notes;
            }
        }
    }
    save_state(&app_data)?;
    Ok(())
}

#[tauri::command]
pub fn set_tab_notes_open(
    window: tauri::Window,
    state: State<'_, Arc<AppState>>,
    workspace_id: String,
    pane_id: String,
    tab_id: String,
    open: bool,
) -> Result<(), String> {
    let label = window.label().to_string();
    let mut app_data = state.app_data.write();
    let win = app_data.window_mut(&label).ok_or("Window not found")?;
    if let Some(workspace) = win.workspaces.iter_mut().find(|w| w.id == workspace_id) {
        if let Some(pane) = workspace.panes.iter_mut().find(|p| p.id == pane_id) {
            if let Some(tab) = pane.tabs.iter_mut().find(|t| t.id == tab_id) {
                tab.notes_open = open;
            }
        }
    }
    Ok(())
}

#[tauri::command]
pub fn set_tab_notes_mode(
    window: tauri::Window,
    state: State<'_, Arc<AppState>>,
    workspace_id: String,
    pane_id: String,
    tab_id: String,
    notes_mode: Option<String>,
) -> Result<(), String> {
    let label = window.label().to_string();
    let mut app_data = state.app_data.write();
    let win = app_data.window_mut(&label).ok_or("Window not found")?;
    if let Some(workspace) = win.workspaces.iter_mut().find(|w| w.id == workspace_id) {
        if let Some(pane) = workspace.panes.iter_mut().find(|p| p.id == pane_id) {
            if let Some(tab) = pane.tabs.iter_mut().find(|t| t.id == tab_id) {
                tab.notes_mode = notes_mode;
            }
        }
    }
    Ok(())
}

#[tauri::command]
pub fn set_tab_composer_open(
    window: tauri::Window,
    state: State<'_, Arc<AppState>>,
    workspace_id: String,
    pane_id: String,
    tab_id: String,
    open: Option<bool>,
) -> Result<(), String> {
    let label = window.label().to_string();
    let mut app_data = state.app_data.write();
    let win = app_data.window_mut(&label).ok_or("Window not found")?;
    if let Some(workspace) = win.workspaces.iter_mut().find(|w| w.id == workspace_id) {
        if let Some(pane) = workspace.panes.iter_mut().find(|p| p.id == pane_id) {
            if let Some(tab) = pane.tabs.iter_mut().find(|t| t.id == tab_id) {
                tab.composer_open = open;
            }
        }
    }
    Ok(())
}

#[tauri::command]
pub fn set_tab_composer_draft(
    window: tauri::Window,
    state: State<'_, Arc<AppState>>,
    workspace_id: String,
    pane_id: String,
    tab_id: String,
    draft: Option<String>,
) -> Result<(), String> {
    let label = window.label().to_string();
    let mut app_data = state.app_data.write();
    let win = app_data.window_mut(&label).ok_or("Window not found")?;
    if let Some(workspace) = win.workspaces.iter_mut().find(|w| w.id == workspace_id) {
        if let Some(pane) = workspace.panes.iter_mut().find(|p| p.id == pane_id) {
            if let Some(tab) = pane.tabs.iter_mut().find(|t| t.id == tab_id) {
                tab.composer_draft = draft;
            }
        }
    }
    save_state(&app_data)?;
    Ok(())
}

/// Set a tab's Mesh Workspace purpose (persisted, survives restart).
#[tauri::command]
pub fn set_tab_mesh_purpose(
    window: tauri::Window,
    state: State<'_, Arc<AppState>>,
    workspace_id: String,
    pane_id: String,
    tab_id: String,
    purpose: Option<String>,
) -> Result<(), String> {
    let label = window.label().to_string();
    let mut app_data = state.app_data.write();
    let win = app_data.window_mut(&label).ok_or("Window not found")?;
    if let Some(workspace) = win.workspaces.iter_mut().find(|w| w.id == workspace_id) {
        if let Some(pane) = workspace.panes.iter_mut().find(|p| p.id == pane_id) {
            if let Some(tab) = pane.tabs.iter_mut().find(|t| t.id == tab_id) {
                tab.mesh_purpose = purpose;
            }
        }
    }
    save_state(&app_data)?;
    Ok(())
}

#[tauri::command]
pub fn reorder_tabs(
    window: tauri::Window,
    state: State<'_, Arc<AppState>>,
    workspace_id: String,
    pane_id: String,
    tab_ids: Vec<String>,
) -> Result<(), String> {
    let label = window.label().to_string();
    let data_clone = {
        let mut app_data = state.app_data.write();
        let win = app_data.window_mut(&label).ok_or("Window not found")?;
        if let Some(workspace) = win.workspaces.iter_mut().find(|w| w.id == workspace_id) {
            if let Some(pane) = workspace.panes.iter_mut().find(|p| p.id == pane_id) {
                let mut reordered = Vec::with_capacity(tab_ids.len());
                for id in &tab_ids {
                    if let Some(tab) = pane.tabs.iter().find(|t| &t.id == id) {
                        reordered.push(tab.clone());
                    }
                }
                pane.tabs = reordered;
            }
        }
        app_data.clone()
    };
    save_state(&data_clone)
}

#[tauri::command]
pub fn get_preferences(state: State<'_, Arc<AppState>>) -> Preferences {
    state.app_data.read().preferences.clone()
}

#[tauri::command]
pub fn set_preferences(app: tauri::AppHandle, state: State<'_, Arc<AppState>>, mut preferences: Preferences) -> Result<(), String> {
    let data_clone = {
        let mut app_data = state.app_data.write();
        // The marker is backend-owned — the client payload omits it, which would
        // reset it to false (serde default) and re-fire the one-time migration on
        // next launch, undoing a deliberate opt-out. Preserve the stored value.
        preferences.shell_integration_default_migrated =
            app_data.preferences.shell_integration_default_migrated;
        preferences.restore_session_default_migrated =
            app_data.preferences.restore_session_default_migrated;
        // maiLink paired devices are backend-owned: the listener's /pair and
        // /push-register handlers mutate them at runtime. The client payload omits the
        // list, so a wholesale replace here would wipe every paired phone on any
        // unrelated preference change. Preserve the stored value.
        preferences.mailink_devices = app_data.preferences.mailink_devices.clone();
        app_data.preferences = preferences.clone();
        app_data.clone()
    };
    save_state(&data_clone)?;
    // Broadcast to all windows so other windows pick up the change
    let _ = app.emit("preferences-changed", &preferences);
    Ok(())
}

#[tauri::command]
pub fn set_tab_restore_context(
    window: tauri::Window,
    state: State<'_, Arc<AppState>>,
    workspace_id: String,
    pane_id: String,
    tab_id: String,
    cwd: Option<String>,
    ssh_command: Option<String>,
    remote_cwd: Option<String>,
) -> Result<(), String> {
    let label = window.label().to_string();
    let mut app_data = state.app_data.write();
    let win = app_data.window_mut(&label).ok_or("Window not found")?;
    if let Some(workspace) = win.workspaces.iter_mut().find(|w| w.id == workspace_id) {
        if let Some(pane) = workspace.panes.iter_mut().find(|p| p.id == pane_id) {
            if let Some(tab) = pane.tabs.iter_mut().find(|t| t.id == tab_id) {
                tab.restore_cwd = cwd;
                tab.restore_ssh_command = ssh_command;
                tab.restore_remote_cwd = remote_cwd;
            }
        }
    }
    Ok(())
}

#[tauri::command]
pub fn set_tab_last_cwd(
    window: tauri::Window,
    state: State<'_, Arc<AppState>>,
    workspace_id: String,
    pane_id: String,
    tab_id: String,
    cwd: Option<String>,
) -> Result<(), String> {
    let label = window.label().to_string();
    let mut app_data = state.app_data.write();
    let win = app_data.window_mut(&label).ok_or("Window not found")?;
    if let Some(workspace) = win.workspaces.iter_mut().find(|w| w.id == workspace_id) {
        if let Some(pane) = workspace.panes.iter_mut().find(|p| p.id == pane_id) {
            if let Some(tab) = pane.tabs.iter_mut().find(|t| t.id == tab_id) {
                tab.last_cwd = cwd;
            }
        }
    }
    Ok(())
}

#[tauri::command]
pub fn set_tab_auto_resume_context(
    window: tauri::Window,
    state: State<'_, Arc<AppState>>,
    workspace_id: String,
    pane_id: String,
    tab_id: String,
    cwd: Option<String>,
    ssh_command: Option<String>,
    remote_cwd: Option<String>,
    command: Option<String>,
    pinned: Option<bool>,
) -> Result<(), String> {
    let label = window.label().to_string();
    let mut app_data = state.app_data.write();
    let win = app_data.window_mut(&label).ok_or("Window not found")?;
    if let Some(workspace) = win.workspaces.iter_mut().find(|w| w.id == workspace_id) {
        if let Some(pane) = workspace.panes.iter_mut().find(|p| p.id == pane_id) {
            if let Some(tab) = pane.tabs.iter_mut().find(|t| t.id == tab_id) {
                tab.auto_resume_cwd = cwd;
                tab.auto_resume_ssh_command = ssh_command;
                tab.auto_resume_remote_cwd = remote_cwd;
                if command.is_some() {
                    tab.auto_resume_remembered_command = command.clone();
                }
                tab.auto_resume_command = command;
                tab.auto_resume_enabled = true;
                if let Some(p) = pinned {
                    tab.auto_resume_pinned = p;
                }
            }
        }
    }
    save_state(&app_data)?;
    Ok(())
}

#[tauri::command]
pub fn set_tab_auto_resume_enabled(
    window: tauri::Window,
    state: State<'_, Arc<AppState>>,
    workspace_id: String,
    pane_id: String,
    tab_id: String,
    enabled: bool,
) -> Result<(), String> {
    let label = window.label().to_string();
    let mut app_data = state.app_data.write();
    let win = app_data.window_mut(&label).ok_or("Window not found")?;
    if let Some(workspace) = win.workspaces.iter_mut().find(|w| w.id == workspace_id) {
        if let Some(pane) = workspace.panes.iter_mut().find(|p| p.id == pane_id) {
            if let Some(tab) = pane.tabs.iter_mut().find(|t| t.id == tab_id) {
                tab.auto_resume_enabled = enabled;
            }
        }
    }
    save_state(&app_data)?;
    Ok(())
}

/// Persist (or clear) the Agent Bridge pairing on a tab. The frontend keeps the live
/// routing in memory and writes the durable pairing here so it survives restart.
#[tauri::command]
pub fn set_tab_agent_bridge(
    window: tauri::Window,
    state: State<'_, Arc<AppState>>,
    workspace_id: String,
    pane_id: String,
    tab_id: String,
    bridge: Option<crate::state::AgentBridge>,
) -> Result<(), String> {
    let label = window.label().to_string();
    let mut app_data = state.app_data.write();
    let win = app_data.window_mut(&label).ok_or("Window not found")?;
    if let Some(workspace) = win.workspaces.iter_mut().find(|w| w.id == workspace_id) {
        if let Some(pane) = workspace.panes.iter_mut().find(|p| p.id == pane_id) {
            if let Some(tab) = pane.tabs.iter_mut().find(|t| t.id == tab_id) {
                tab.agent_bridge = bridge;
            }
        }
    }
    save_state(&app_data)?;
    Ok(())
}

#[tauri::command]
pub fn reorder_workspaces(
    window: tauri::Window,
    state: State<'_, Arc<AppState>>,
    workspace_ids: Vec<String>,
) -> Result<(), String> {
    let label = window.label().to_string();
    let data_clone = {
        let mut app_data = state.app_data.write();
        let win = app_data.window_mut(&label).ok_or("Window not found")?;
        let mut reordered = Vec::with_capacity(workspace_ids.len());
        for id in &workspace_ids {
            if let Some(ws) = win.workspaces.iter().find(|w| &w.id == id) {
                reordered.push(ws.clone());
            }
        }
        win.workspaces = reordered;
        app_data.clone()
    };
    save_state(&data_clone)
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct DuplicateWorkspaceResult {
    pub workspace: Workspace,
    pub tab_id_map: HashMap<String, String>,
}

#[tauri::command]
pub fn duplicate_workspace(
    window: tauri::Window,
    state: State<'_, Arc<AppState>>,
    workspace_id: String,
    position: usize,
    tab_contexts: Vec<TabContext>,
) -> Result<DuplicateWorkspaceResult, String> {
    let label = window.label().to_string();
    let (data_clone, result) = {
        let mut app_data = state.app_data.write();
        let win = app_data.window_mut(&label).ok_or("Window not found")?;
        let source = win.workspaces.iter()
            .find(|w| w.id == workspace_id)
            .ok_or("Workspace not found")?
            .clone();

        let (mut cloned, tab_id_map) = clone_workspace_with_id_mapping(&source, &tab_contexts);

        // Move scrollback from cloned tabs into SQLite
        for pane in &mut cloned.panes {
            for tab in &mut pane.tabs {
                if let Some(ref sb) = tab.scrollback {
                    let _ = state.scrollback_db.save(&tab.id, sb, None);
                    tab.scrollback = None;
                }
            }
        }

        let result = DuplicateWorkspaceResult {
            workspace: cloned.clone(),
            tab_id_map,
        };

        let insert_pos = position.min(win.workspaces.len());
        win.workspaces.insert(insert_pos, cloned);

        (app_data.clone(), result)
    };
    save_state(&data_clone)?;
    Ok(result)
}

#[tauri::command]
pub fn set_tab_trigger_variables(
    window: tauri::Window,
    state: State<'_, Arc<AppState>>,
    workspace_id: String,
    pane_id: String,
    tab_id: String,
    vars: HashMap<String, String>,
) -> Result<(), String> {
    let label = window.label().to_string();
    let mut app_data = state.app_data.write();
    let win = app_data.window_mut(&label).ok_or("Window not found")?;
    if let Some(workspace) = win.workspaces.iter_mut().find(|w| w.id == workspace_id) {
        if let Some(pane) = workspace.panes.iter_mut().find(|p| p.id == pane_id) {
            if let Some(tab) = pane.tabs.iter_mut().find(|t| t.id == tab_id) {
                tab.trigger_variables = vars;
            }
        }
    }
    Ok(())
}

#[tauri::command]
pub fn copy_tab_history(source_tab_id: String, dest_tab_id: String) -> Result<(), String> {
    let data_dir = dirs::data_dir().ok_or("No data directory")?;
    let history_dir = data_dir.join(app_data_slug()).join("history");

    let safe_source = source_tab_id.replace(['/', '\\', '.'], "");
    let safe_dest = dest_tab_id.replace(['/', '\\', '.'], "");

    let source_path = history_dir.join(format!("{}.history", safe_source));
    let dest_path = history_dir.join(format!("{}.history", safe_dest));

    if source_path.exists() {
        std::fs::copy(&source_path, &dest_path).map_err(|e| e.to_string())?;
    }

    Ok(())
}

/// Return all tab info across all windows (for preferences UI trigger scoping).
#[tauri::command]
pub fn get_all_tabs(state: State<'_, Arc<AppState>>) -> Vec<(String, String, String, String, bool)> {
    let app_data = state.app_data.read();
    let mut result = Vec::new();
    for win in &app_data.windows {
        // Determine active tab across all workspaces in this window
        let active_ws = win.active_workspace_id.as_deref()
            .and_then(|id| win.workspaces.iter().find(|w| w.id == id));
        let active_tab_id = active_ws.and_then(|ws| {
            let pane = ws.active_pane_id.as_deref()
                .and_then(|pid| ws.panes.iter().find(|p| p.id == pid));
            pane.and_then(|p| p.active_tab_id.clone())
        });

        for ws in &win.workspaces {
            for pane in &ws.panes {
                for tab in &pane.tabs {
                    let is_active = active_tab_id.as_deref() == Some(&tab.id);
                    result.push((tab.id.clone(), tab.name.clone(), ws.id.clone(), ws.name.clone(), is_active));
                }
            }
        }
    }
    result
}

/// Return all workspace id/name pairs across all windows (for preferences UI).
#[tauri::command]
pub fn get_all_workspaces(state: State<'_, Arc<AppState>>) -> Vec<(String, String)> {
    let app_data = state.app_data.read();
    let mut result = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for win in &app_data.windows {
        for ws in &win.workspaces {
            if seen.insert(ws.id.clone()) {
                result.push((ws.id.clone(), ws.name.clone()));
            }
        }
    }
    result
}

/// Return the list of directories where system sounds live on this platform.
fn system_sound_dirs() -> Vec<PathBuf> {
    #[cfg(target_os = "macos")]
    {
        vec![
            PathBuf::from("/System/Library/Sounds"),
            PathBuf::from("/Library/Sounds"),
        ]
    }
    #[cfg(target_os = "linux")]
    {
        vec![
            PathBuf::from("/usr/share/sounds/freedesktop/stereo"),
            PathBuf::from("/usr/share/sounds"),
        ]
    }
    #[cfg(target_os = "windows")]
    {
        if let Some(windir) = std::env::var_os("SystemRoot") {
            vec![PathBuf::from(windir).join("Media")]
        } else {
            vec![PathBuf::from("C:\\Windows\\Media")]
        }
    }
}

#[tauri::command]
pub fn list_system_sounds() -> Vec<String> {
    let mut names = Vec::new();
    for dir in system_sound_dirs() {
        if let Ok(entries) = std::fs::read_dir(&dir) {
            for entry in entries.flatten() {
                let path = entry.path();
                if let Some(ext) = path.extension() {
                    let ext_lower = ext.to_string_lossy().to_lowercase();
                    if matches!(ext_lower.as_str(), "aiff" | "aif" | "wav" | "ogg" | "mp3") {
                        if let Some(stem) = path.file_stem() {
                            names.push(stem.to_string_lossy().to_string());
                        }
                    }
                }
            }
        }
    }
    names.sort();
    names.dedup();
    names
}

#[tauri::command]
pub fn play_system_sound(name: String, volume: u32) -> Result<(), String> {
    // Find the sound file
    for dir in system_sound_dirs() {
        if let Ok(entries) = std::fs::read_dir(&dir) {
            for entry in entries.flatten() {
                let path = entry.path();
                if let Some(stem) = path.file_stem() {
                    if stem.to_string_lossy() == name {
                        // Spawn playback in background (non-blocking)
                        let vol = (volume as f64 / 100.0).min(1.0);
                        #[cfg(target_os = "macos")]
                        {
                            let vol_str = format!("{:.2}", vol);
                            std::thread::spawn(move || {
                                let _ = std::process::Command::new("afplay")
                                    .arg("-v")
                                    .arg(&vol_str)
                                    .arg(&path)
                                    .output();
                            });
                            return Ok(());
                        }
                        #[cfg(target_os = "linux")]
                        {
                            std::thread::spawn(move || {
                                // Try paplay first (PulseAudio), fall back to aplay
                                let vol_pa = format!("{}", (vol * 65536.0) as u32);
                                let result = std::process::Command::new("paplay")
                                    .arg("--volume")
                                    .arg(&vol_pa)
                                    .arg(&path)
                                    .output();
                                if result.is_err() {
                                    let _ = std::process::Command::new("aplay")
                                        .arg(&path)
                                        .output();
                                }
                            });
                            return Ok(());
                        }
                        #[cfg(target_os = "windows")]
                        {
                            std::thread::spawn(move || {
                                let _ = std::process::Command::new("powershell")
                                    .arg("-c")
                                    .arg(format!(
                                        "(New-Object Media.SoundPlayer '{}').PlaySync()",
                                        path.display()
                                    ))
                                    .output();
                            });
                            return Ok(());
                        }
                        #[allow(unreachable_code)]
                        {
                            return Err("Unsupported platform".to_string());
                        }
                    }
                }
            }
        }
    }
    Err(format!("Sound '{}' not found", name))
}

#[tauri::command]
pub fn play_bell_sound() {
    std::thread::spawn(|| {
        #[cfg(target_os = "macos")]
        {
            // Read the user's configured alert sound from system preferences,
            // then play it with afplay (blocks until complete — no cutoff).
            let sound_path = std::process::Command::new("defaults")
                .arg("read")
                .arg(".GlobalPreferences")
                .arg("com.apple.sound.beep.sound")
                .output()
                .ok()
                .and_then(|o| if o.status.success() {
                    Some(String::from_utf8_lossy(&o.stdout).trim().to_string())
                } else {
                    None
                })
                .unwrap_or_else(|| "/System/Library/Sounds/Tink.aiff".to_string());
            let _ = std::process::Command::new("afplay")
                .arg(&sound_path)
                .output();
        }
        #[cfg(target_os = "linux")]
        {
            // XDG sound theme bell, fall back to freedesktop bell sound
            let result = std::process::Command::new("canberra-gtk-play")
                .arg("--id=bell")
                .output();
            if result.is_err() {
                let _ = std::process::Command::new("paplay")
                    .arg("/usr/share/sounds/freedesktop/stereo/bell.oga")
                    .output();
            }
        }
        #[cfg(target_os = "windows")]
        {
            let _ = std::process::Command::new("powershell")
                .arg("-c")
                .arg("[System.Media.SystemSounds]::Beep.Play()")
                .output();
        }
    });
}

/// Returns (year, month, day, hour, minute, second) in UTC from the current system time.
fn now_utc_parts() -> (i64, u32, u32, u64, u64, u64) {
    let secs = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_secs();
    let s = secs % 60;
    let m = (secs / 60) % 60;
    let h = (secs / 3600) % 24;
    let (y, mo, da) = civil_from_days((secs / 86400) as i64);
    (y, mo, da, h, m, s)
}

fn iso_now() -> String {
    let (y, mo, da, h, m, s) = now_utc_parts();
    format!("{:04}-{:02}-{:02}T{:02}:{:02}:{:02}Z", y, mo, da, h, m, s)
}

/// Convert days since Unix epoch to (year, month, day). Civil algorithm from Howard Hinnant.
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719468;
    let era = if z >= 0 { z } else { z - 146096 } / 146097;
    let doe = (z - era * 146097) as u32;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    (y, m, d)
}

#[tauri::command]
pub fn add_workspace_note(
    window: tauri::Window,
    state: State<'_, Arc<AppState>>,
    workspace_id: String,
    content: String,
    mode: Option<String>,
) -> Result<WorkspaceNote, String> {
    let label = window.label().to_string();
    let now = iso_now();
    let note = WorkspaceNote {
        id: uuid::Uuid::new_v4().to_string(),
        content,
        mode,
        created_at: now.clone(),
        updated_at: now,
    };
    let data_clone = {
        let mut app_data = state.app_data.write();
        let win = app_data.window_mut(&label).ok_or("Window not found")?;
        if let Some(workspace) = win.workspaces.iter_mut().find(|w| w.id == workspace_id) {
            workspace.workspace_notes.push(note.clone());
            app_data.clone()
        } else {
            return Err("Workspace not found".to_string());
        }
    };
    save_state(&data_clone)?;
    Ok(note)
}

#[tauri::command]
pub fn update_workspace_note(
    window: tauri::Window,
    state: State<'_, Arc<AppState>>,
    workspace_id: String,
    note_id: String,
    content: String,
    mode: Option<String>,
) -> Result<(), String> {
    let label = window.label().to_string();
    let now = iso_now();
    let data_clone = {
        let mut app_data = state.app_data.write();
        let win = app_data.window_mut(&label).ok_or("Window not found")?;
        if let Some(workspace) = win.workspaces.iter_mut().find(|w| w.id == workspace_id) {
            if let Some(note) = workspace.workspace_notes.iter_mut().find(|n| n.id == note_id) {
                note.content = content;
                note.mode = mode;
                note.updated_at = now;
            }
            app_data.clone()
        } else {
            return Err("Workspace not found".to_string());
        }
    };
    save_state(&data_clone)
}

#[tauri::command]
pub fn delete_workspace_note(
    window: tauri::Window,
    state: State<'_, Arc<AppState>>,
    workspace_id: String,
    note_id: String,
) -> Result<(), String> {
    let label = window.label().to_string();
    let data_clone = {
        let mut app_data = state.app_data.write();
        let win = app_data.window_mut(&label).ok_or("Window not found")?;
        if let Some(workspace) = win.workspaces.iter_mut().find(|w| w.id == workspace_id) {
            workspace.workspace_notes.retain(|n| n.id != note_id);
            app_data.clone()
        } else {
            return Err("Workspace not found".to_string());
        }
    };
    save_state(&data_clone)
}

/// Toggle a workspace into (or out of) Mesh mode. A mesh workspace bridges every agent
/// tab in it N:M (see docs/mesh-workspace.md). Turning mesh OFF leaves any topic registry
/// in place (harmless for a normal workspace; restored if mesh is re-enabled).
#[tauri::command]
pub fn set_workspace_bridge_all(
    window: tauri::Window,
    state: State<'_, Arc<AppState>>,
    workspace_id: String,
    enabled: bool,
) -> Result<(), String> {
    let label = window.label().to_string();
    let data_clone = {
        let mut app_data = state.app_data.write();
        let win = app_data.window_mut(&label).ok_or("Window not found")?;
        let workspace = win
            .workspaces
            .iter_mut()
            .find(|w| w.id == workspace_id)
            .ok_or("Workspace not found")?;
        workspace.bridge_all = enabled;
        app_data.clone()
    };
    save_state(&data_clone)?;
    Ok(())
}

/// Toggle a single tab's maiLink-native designation (whether it appears as a chat in the
/// maiLink mobile companion). Per-tab opt-in; orthogonal to the workspace-wide flag. See
/// docs/mailink-protocol.md.
#[tauri::command]
pub fn set_tab_mailink_native(
    window: tauri::Window,
    state: State<'_, Arc<AppState>>,
    workspace_id: String,
    pane_id: String,
    tab_id: String,
    mailink_native: bool,
) -> Result<(), String> {
    let label = window.label().to_string();
    let mut app_data = state.app_data.write();
    let win = app_data.window_mut(&label).ok_or("Window not found")?;
    let workspace = win.workspaces.iter_mut()
        .find(|w| w.id == workspace_id)
        .ok_or("Workspace not found")?;
    let pane = workspace.panes.iter_mut()
        .find(|p| p.id == pane_id)
        .ok_or("Pane not found")?;
    let tab = pane.tabs.iter_mut()
        .find(|t| t.id == tab_id)
        .ok_or("Tab not found")?;

    tab.mailink_native = mailink_native;

    let data_clone = app_data.clone();
    drop(app_data);
    save_state(&data_clone)
}

/// Toggle a workspace-wide maiLink-native designation: every agent tab in the workspace is
/// exposed to maiLink as a chat. See docs/mailink-protocol.md.
#[tauri::command]
pub fn set_workspace_mailink_native(
    window: tauri::Window,
    state: State<'_, Arc<AppState>>,
    workspace_id: String,
    enabled: bool,
) -> Result<(), String> {
    let label = window.label().to_string();
    let data_clone = {
        let mut app_data = state.app_data.write();
        let win = app_data.window_mut(&label).ok_or("Window not found")?;
        let workspace = win
            .workspaces
            .iter_mut()
            .find(|w| w.id == workspace_id)
            .ok_or("Workspace not found")?;
        workspace.mailink_native = enabled;
        app_data.clone()
    };
    save_state(&data_clone)?;
    Ok(())
}

/// Replace a mesh workspace's topic registry wholesale. The frontend `agentMesh` store is
/// authoritative for topics (it mints ids + timestamps in JS and dedups by normalized
/// label), so persistence is a coarse replace rather than granular CRUD — right-sized for
/// the handful of topics a mesh carries, and it captures the current turn counters at flush
/// time. Called on structural changes (create / complete / participant join).
#[tauri::command]
pub fn set_workspace_mesh_topics(
    window: tauri::Window,
    state: State<'_, Arc<AppState>>,
    workspace_id: String,
    mut topics: Vec<crate::state::MeshTopic>,
) -> Result<(), String> {
    let label = window.label().to_string();
    // Integrity guard: canonicalize the dedup key server-side so it can never drift from
    // the label, regardless of what the caller sent (defense-in-depth — the TS and Rust
    // normalizers must agree, and this makes Rust the source of truth on persist).
    for t in topics.iter_mut() {
        t.normalized_label = crate::state::MeshTopic::normalize_label(&t.label);
    }
    let data_clone = {
        let mut app_data = state.app_data.write();
        let win = app_data.window_mut(&label).ok_or("Window not found")?;
        let workspace = win
            .workspaces
            .iter_mut()
            .find(|w| w.id == workspace_id)
            .ok_or("Workspace not found")?;
        workspace.mesh_topics = topics;
        app_data.clone()
    };
    save_state(&data_clone)?;
    Ok(())
}

#[tauri::command]
pub fn create_diff_tab(
    window: tauri::Window,
    state: State<'_, Arc<AppState>>,
    workspace_id: String,
    pane_id: String,
    name: String,
    diff_context: crate::state::DiffContext,
    after_tab_id: Option<String>,
) -> Result<Tab, String> {
    let label = window.label().to_string();
    let mut app_data = state.app_data.write();
    let win = app_data.window_mut(&label).ok_or("Window not found")?;
    let ws = win
        .workspaces
        .iter_mut()
        .find(|w| w.id == workspace_id)
        .ok_or("Workspace not found")?;
    let pane = ws
        .panes
        .iter_mut()
        .find(|p| p.id == pane_id)
        .ok_or("Pane not found")?;

    let tab = Tab::new_diff(name, diff_context);
    let tab_id = tab.id.clone();

    let insert_idx = after_tab_id
        .and_then(|id| pane.tabs.iter().position(|t| t.id == id))
        .map(|idx| idx + 1)
        .unwrap_or(pane.tabs.len());
    pane.tabs.insert(insert_idx, tab.clone());
    pane.active_tab_id = Some(tab_id);

    Ok(tab)
}

#[tauri::command]
pub fn archive_tab(
    window: tauri::Window,
    state: State<'_, Arc<AppState>>,
    workspace_id: String,
    pane_id: String,
    tab_id: String,
    display_name: String,
    scrollback: Option<String>,
    cwd: Option<String>,
    ssh_command: Option<String>,
    remote_cwd: Option<String>,
) -> Result<(), String> {
    let label = window.label().to_string();
    let data_clone = {
        let mut app_data = state.app_data.write();
        let win = app_data.window_mut(&label).ok_or("Window not found")?;
        let workspace = win.workspaces.iter_mut()
            .find(|w| w.id == workspace_id)
            .ok_or("Workspace not found")?;
        let pane = workspace.panes.iter_mut()
            .find(|p| p.id == pane_id)
            .ok_or("Pane not found")?;

        let tab_index = pane.tabs.iter().position(|t| t.id == tab_id)
            .ok_or("Tab not found")?;
        let mut tab = pane.tabs.remove(tab_index);

        // Store resolved display name for the archive list; preserve original name/custom_name
        tab.archived_name = Some(display_name);
        tab.pty_id = None;
        // Write scrollback to SQLite, not the JSON state
        if let Some(ref sb) = scrollback {
            let _ = state.scrollback_db.save(&tab.id, sb, None);
        }
        tab.scrollback = None;
        tab.restore_cwd = cwd;
        tab.restore_ssh_command = ssh_command;
        tab.restore_remote_cwd = remote_cwd;
        tab.archived_at = Some(iso_now());

        // Adjust active_tab_id (same logic as delete_tab)
        if pane.active_tab_id.as_ref() == Some(&tab_id) {
            pane.active_tab_id = if pane.tabs.is_empty() {
                None
            } else {
                let new_index = if tab_index > 0 { tab_index - 1 } else { 0 };
                Some(pane.tabs[new_index].id.clone())
            };
        }

        workspace.archived_tabs.push(tab);
        app_data.clone()
    };
    save_state(&data_clone)
}

#[tauri::command]
pub fn restore_archived_tab(
    window: tauri::Window,
    state: State<'_, Arc<AppState>>,
    workspace_id: String,
    pane_id: String,
    tab_id: String,
) -> Result<Tab, String> {
    let label = window.label().to_string();
    let (data_clone, tab) = {
        let mut app_data = state.app_data.write();
        let win = app_data.window_mut(&label).ok_or("Window not found")?;
        let workspace = win.workspaces.iter_mut()
            .find(|w| w.id == workspace_id)
            .ok_or("Workspace not found")?;

        let arch_index = workspace.archived_tabs.iter().position(|t| t.id == tab_id)
            .ok_or("Archived tab not found")?;
        let mut tab = workspace.archived_tabs.remove(arch_index);
        tab.archived_name = None;
        tab.archived_at = None;

        let pane = workspace.panes.iter_mut()
            .find(|p| p.id == pane_id)
            .ok_or("Pane not found")?;
        let insert_index = pane.active_tab_id.as_ref()
            .and_then(|active_id| pane.tabs.iter().position(|t| t.id == *active_id))
            .map(|i| i + 1)
            .unwrap_or(0);
        pane.tabs.insert(insert_index, tab.clone());
        pane.active_tab_id = Some(tab.id.clone());

        (app_data.clone(), tab)
    };
    save_state(&data_clone)?;
    Ok(tab)
}

#[tauri::command]
pub fn delete_archived_tab(
    window: tauri::Window,
    state: State<'_, Arc<AppState>>,
    workspace_id: String,
    tab_id: String,
) -> Result<(), String> {
    let label = window.label().to_string();
    let data_clone = {
        let mut app_data = state.app_data.write();
        let win = app_data.window_mut(&label).ok_or("Window not found")?;
        let workspace = win.workspaces.iter_mut()
            .find(|w| w.id == workspace_id)
            .ok_or("Workspace not found")?;
        workspace.archived_tabs.retain(|t| t.id != tab_id);
        app_data.clone()
    };
    // Clean up scrollback from SQLite
    let _ = state.scrollback_db.delete(&tab_id);
    save_state(&data_clone)
}

/// Clone app_data and filter out ephemeral diff tabs + optionally strip scrollback.
/// When `!exclude_scrollback`, populates `tab.scrollback` from SQLite so exports include it.
pub fn prepare_export(data: &crate::state::AppData, exclude_scrollback: bool, db: &crate::state::ScrollbackDb) -> crate::state::AppData {
    let mut filtered = data.clone();
    for win in &mut filtered.windows {
        for ws in &mut win.workspaces {
            for pane in &mut ws.panes {
                pane.tabs.retain(|t| t.tab_type != crate::state::workspace::TabType::Diff);
                if let Some(ref active_id) = pane.active_tab_id {
                    if !pane.tabs.iter().any(|t| t.id == *active_id) {
                        pane.active_tab_id = pane.tabs.last().map(|t| t.id.clone());
                    }
                }
                for tab in &mut pane.tabs {
                    if exclude_scrollback {
                        tab.scrollback = None;
                    } else {
                        // Populate from SQLite for export
                        tab.scrollback = db.load(&tab.id).unwrap_or(None);
                    }
                }
            }
            for tab in &mut ws.archived_tabs {
                if exclude_scrollback {
                    tab.scrollback = None;
                } else {
                    tab.scrollback = db.load(&tab.id).unwrap_or(None);
                }
            }
        }
    }
    filtered
}

#[tauri::command]
pub fn export_state(state: State<'_, Arc<AppState>>, path: String, exclude_scrollback: bool) -> Result<(), String> {
    let app_data = state.app_data.read();
    let filtered = prepare_export(&app_data, exclude_scrollback, &state.scrollback_db);

    let json = serde_json::to_string_pretty(&filtered).map_err(|e| e.to_string())?;

    use flate2::write::GzEncoder;
    use flate2::Compression;
    use std::io::Write;
    let file = std::fs::File::create(&path)
        .map_err(|e| format!("Failed to create export file: {}", e))?;
    let mut encoder = GzEncoder::new(file, Compression::default());
    encoder.write_all(json.as_bytes())
        .map_err(|e| format!("Failed to write compressed export: {}", e))?;
    encoder.finish()
        .map_err(|e| format!("Failed to finish compression: {}", e))?;

    log::info!("State exported to {}", path);
    Ok(())
}

/// Body of `run_scheduled_backup`, callable directly from background tasks.
/// Logs and returns the written backup path.
pub(crate) fn do_scheduled_backup(state: &AppState) -> Result<String, String> {
    let app_data = state.app_data.read();
    let prefs = &app_data.preferences;

    let dir = prefs.backup_directory.as_deref()
        .ok_or("No backup directory configured")?;

    let dir_path = PathBuf::from(dir);
    if !dir_path.exists() {
        std::fs::create_dir_all(&dir_path)
            .map_err(|e| format!("Failed to create backup directory: {}", e))?;
    }

    let filtered = prepare_export(&app_data, prefs.backup_exclude_scrollback, &state.scrollback_db);
    let json = serde_json::to_string_pretty(&filtered).map_err(|e| e.to_string())?;

    let (y, mo, da, h, m, _) = now_utc_parts();
    let timestamp = format!("{:04}{:02}{:02}_{:02}{:02}", y, mo, da, h, m);

    let filename = format!("aiterm_backup_{}.json.gz", timestamp);
    let file_path = dir_path.join(&filename);

    use flate2::write::GzEncoder;
    use flate2::Compression;
    use std::io::Write;
    let file = std::fs::File::create(&file_path)
        .map_err(|e| format!("Failed to create backup file: {}", e))?;
    let mut encoder = GzEncoder::new(file, Compression::default());
    encoder.write_all(json.as_bytes())
        .map_err(|e| format!("Failed to write compressed backup: {}", e))?;
    encoder.finish()
        .map_err(|e| format!("Failed to finish compression: {}", e))?;

    let path_str = file_path.to_string_lossy().to_string();
    log::info!("Scheduled backup written to {}", path_str);
    Ok(path_str)
}

#[tauri::command]
pub fn run_scheduled_backup(state: State<'_, Arc<AppState>>) -> Result<String, String> {
    do_scheduled_backup(state.inner().as_ref())
}

/// Body of `trim_old_backups`, callable directly from background tasks.
pub(crate) fn do_trim_old_backups(state: &AppState) -> Result<u32, String> {
    let app_data = state.app_data.read();
    let prefs = &app_data.preferences;

    let dir = prefs.backup_directory.as_deref()
        .ok_or("No backup directory configured")?;

    if !prefs.backup_trim_enabled {
        return Ok(0);
    }

    let max_age_secs: u64 = match prefs.backup_trim_age.as_str() {
        "1h" => 3600,
        "1d" => 86400,
        "1w" => 7 * 86400,
        "1m" => 30 * 86400,
        "1y" => 365 * 86400,
        _ => 30 * 86400, // default to 1 month
    };

    let now = SystemTime::now();
    let dir_path = PathBuf::from(dir);
    let mut deleted = 0u32;

    if let Ok(entries) = std::fs::read_dir(&dir_path) {
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().to_string();
            if !name.starts_with("aiterm_backup_") {
                continue;
            }
            if !name.ends_with(".json") && !name.ends_with(".json.gz") {
                continue;
            }
            // Check file age via filesystem metadata
            if let Ok(meta) = entry.metadata() {
                if let Ok(modified) = meta.modified() {
                    if let Ok(age) = now.duration_since(modified) {
                        if age.as_secs() > max_age_secs {
                            if std::fs::remove_file(entry.path()).is_ok() {
                                deleted += 1;
                                log::info!("Trimmed old backup: {}", name);
                            }
                        }
                    }
                }
            }
        }
    }

    if deleted > 0 {
        log::info!("Trimmed {} old backup(s)", deleted);
    }
    Ok(deleted)
}

#[tauri::command]
pub fn trim_old_backups(state: State<'_, Arc<AppState>>) -> Result<u32, String> {
    do_trim_old_backups(state.inner().as_ref())
}

#[tauri::command]
pub async fn pick_backup_directory(app: tauri::AppHandle) -> Result<Option<String>, String> {
    use tauri_plugin_dialog::DialogExt;

    let dir = app.dialog().file()
        .blocking_pick_folder();

    Ok(dir.map(|p| p.to_string()))
}

/// Read and parse a backup file (supports .gz).
fn read_backup_file(path: &str) -> Result<crate::state::AppData, String> {
    let contents = if path.ends_with(".gz") {
        use std::io::Read;
        let file = std::fs::File::open(path)
            .map_err(|e| format!("Failed to open import file: {}", e))?;
        let mut decoder = flate2::read::GzDecoder::new(file);
        let mut s = String::new();
        decoder.read_to_string(&mut s)
            .map_err(|e| format!("Failed to decompress import file: {}", e))?;
        s
    } else {
        std::fs::read_to_string(path)
            .map_err(|e| format!("Failed to read import file: {}", e))?
    };

    let mut imported = parse_state(&contents)
        .map_err(|e| format!("Invalid state file: {}", e))?;
    crate::state::persistence::migrate_app_data(&mut imported);
    Ok(imported)
}

#[tauri::command]
pub fn preview_import(path: String) -> Result<serde_json::Value, String> {
    let data = read_backup_file(&path)?;

    let file_meta = std::fs::metadata(&path).ok();
    let file_size = file_meta.map(|m| m.len()).unwrap_or(0);

    let windows: Vec<serde_json::Value> = data.windows.iter().map(|win| {
        let workspaces: Vec<serde_json::Value> = win.workspaces.iter().map(|ws| {
            let tabs: Vec<serde_json::Value> = ws.panes.iter()
                .flat_map(|p| p.tabs.iter())
                .filter(|t| t.tab_type != crate::state::workspace::TabType::Diff)
                .map(|t| {
                    serde_json::json!({
                        "id": t.id,
                        "name": t.name,
                        "tab_type": t.tab_type,
                        "has_scrollback": t.scrollback.is_some(),
                        "has_notes": t.notes.is_some(),
                        "has_auto_resume": t.auto_resume_command.is_some(),
                        "editor_file_path": t.editor_file.as_ref().map(|f| f.file_path.clone()),
                    })
                })
                .collect();
            serde_json::json!({
                "id": ws.id,
                "name": ws.name,
                "tab_count": tabs.len(),
                "tabs": tabs,
                "note_count": ws.workspace_notes.len(),
                "archived_count": ws.archived_tabs.len(),
            })
        }).collect();
        serde_json::json!({
            "label": win.label,
            "workspaces": workspaces,
        })
    }).collect();

    Ok(serde_json::json!({
        "windows": windows,
        "file_size": file_size,
        "has_preferences": true,
    }))
}

/// Reorder workspaces to match a reference order. IDs not in the reference are appended at the end.
fn reorder_workspaces_by(workspaces: &mut Vec<Workspace>, order: &[String]) {
    workspaces.sort_by(|a, b| {
        let pos_a = order.iter().position(|id| id == &a.id).unwrap_or(usize::MAX);
        let pos_b = order.iter().position(|id| id == &b.id).unwrap_or(usize::MAX);
        pos_a.cmp(&pos_b)
    });
}

/// Reorder tabs to match a reference order. IDs not in the reference are appended at the end.
fn reorder_tabs_by(tabs: &mut Vec<Tab>, order: &[String]) {
    tabs.sort_by(|a, b| {
        let pos_a = order.iter().position(|id| id == &a.id).unwrap_or(usize::MAX);
        let pos_b = order.iter().position(|id| id == &b.id).unwrap_or(usize::MAX);
        pos_a.cmp(&pos_b)
    });
}

#[derive(serde::Deserialize)]
pub struct ImportConfig {
    pub mode: String,                       // "overwrite" or "merge"
    pub selected_workspace_ids: Vec<String>, // workspace IDs to import
    pub import_preferences: bool,
}

#[tauri::command]
pub fn import_state_selective(
    app: tauri::AppHandle,
    state: State<'_, Arc<AppState>>,
    path: String,
    config: ImportConfig,
) -> Result<(), String> {
    let mut imported = read_backup_file(&path)?;

    // Extract any scrollback from imported JSON into SQLite
    migrate_imported_scrollback(&mut imported, &state.scrollback_db);

    // Filter to selected workspaces only
    for win in &mut imported.windows {
        win.workspaces.retain(|ws| config.selected_workspace_ids.contains(&ws.id));
        // Also filter diff tabs
        for ws in &mut win.workspaces {
            for pane in &mut ws.panes {
                pane.tabs.retain(|t| t.tab_type != crate::state::workspace::TabType::Diff);
                if let Some(ref active_id) = pane.active_tab_id {
                    if !pane.tabs.iter().any(|t| t.id == *active_id) {
                        pane.active_tab_id = pane.tabs.last().map(|t| t.id.clone());
                    }
                }
            }
        }
    }

    // Collect the backup's workspace ID order for reordering after import
    let backup_order: Vec<String> = imported.windows.iter()
        .flat_map(|w| w.workspaces.iter().map(|ws| ws.id.clone()))
        .collect();

    let is_merge = config.mode == "merge";

    match config.mode.as_str() {
        "merge" => {
            let mut app_data = state.app_data.write();
            if let Some(target_win) = app_data.windows.first_mut() {
                for src_win in &imported.windows {
                    for src_ws in &src_win.workspaces {
                        if let Some(existing_ws) = target_win.workspaces.iter_mut().find(|w| w.id == src_ws.id) {
                            // Deep merge: merge tabs and notes into existing workspace
                            existing_ws.import_highlight = true;
                            for src_pane in &src_ws.panes {
                                if let Some(existing_pane) = existing_ws.panes.iter_mut().find(|p| p.id == src_pane.id) {
                                    // Merge tabs within matching pane
                                    let tab_order: Vec<String> = src_pane.tabs.iter().map(|t| t.id.clone()).collect();
                                    for src_tab in &src_pane.tabs {
                                        if let Some(existing_tab) = existing_pane.tabs.iter_mut().find(|t| t.id == src_tab.id) {
                                            // Existing tab: only restore notes if currently empty
                                            if existing_tab.notes.as_ref().map_or(true, |n| n.is_empty()) {
                                                if src_tab.notes.as_ref().map_or(false, |n| !n.is_empty()) {
                                                    existing_tab.notes = src_tab.notes.clone();
                                                    existing_tab.notes_mode = src_tab.notes_mode.clone();
                                                    existing_tab.import_highlight = true;
                                                }
                                            }
                                        } else {
                                            // Missing tab: add it
                                            let mut new_tab = src_tab.clone();
                                            new_tab.import_highlight = true;
                                            existing_pane.tabs.push(new_tab);
                                        }
                                    }
                                    reorder_tabs_by(&mut existing_pane.tabs, &tab_order);
                                } else {
                                    // Missing pane: add it entirely — mark all its tabs
                                    let mut new_pane = src_pane.clone();
                                    for t in &mut new_pane.tabs {
                                        t.import_highlight = true;
                                    }
                                    existing_ws.panes.push(new_pane);
                                }
                            }
                            // Merge workspace notes: add missing ones by ID
                            for src_note in &src_ws.workspace_notes {
                                if !existing_ws.workspace_notes.iter().any(|n| n.id == src_note.id) {
                                    existing_ws.workspace_notes.push(src_note.clone());
                                }
                            }
                            // Merge archived tabs: add missing ones by ID
                            for src_tab in &src_ws.archived_tabs {
                                if !existing_ws.archived_tabs.iter().any(|t| t.id == src_tab.id) {
                                    existing_ws.archived_tabs.push(src_tab.clone());
                                }
                            }
                        } else {
                            // Workspace doesn't exist locally — add it with highlight
                            let mut new_ws = src_ws.clone();
                            new_ws.import_highlight = true;
                            for pane in &mut new_ws.panes {
                                for t in &mut pane.tabs {
                                    t.import_highlight = true;
                                }
                            }
                            target_win.workspaces.push(new_ws);
                        }
                    }
                }
                // Restore backup's workspace order, local-only workspaces appended at end
                reorder_workspaces_by(&mut target_win.workspaces, &backup_order);
            }
            if config.import_preferences {
                app_data.preferences = imported.preferences;
            }
            let data_clone = app_data.clone();
            drop(app_data);
            save_state(&data_clone)?;
        }
        _ => {
            // "overwrite" — replace matching workspaces, keep unselected existing ones
            let mut app_data = state.app_data.write();
            if let Some(target_win) = app_data.windows.first_mut() {
                for src_win in &imported.windows {
                    for ws in &src_win.workspaces {
                        // Remove existing workspace with same ID if present
                        target_win.workspaces.retain(|w| w.id != ws.id);
                        target_win.workspaces.push(ws.clone());
                    }
                }
                // Restore backup's workspace order, local-only workspaces appended at end
                reorder_workspaces_by(&mut target_win.workspaces, &backup_order);
            }
            if config.import_preferences {
                app_data.preferences = imported.preferences;
            }
            let data_clone = app_data.clone();
            drop(app_data);
            save_state(&data_clone)?;
        }
    }

    log::info!("State imported from {} (mode: {}, merge: {})", path, config.mode, is_merge);
    let _ = app.emit("state-imported", ());
    Ok(())
}

/// Legacy full-replace import (kept for File menu backward compat)
#[tauri::command]
pub fn import_state(app: tauri::AppHandle, state: State<'_, Arc<AppState>>, path: String) -> Result<(), String> {
    let mut imported = read_backup_file(&path)?;

    // Extract any scrollback from imported JSON into SQLite
    migrate_imported_scrollback(&mut imported, &state.scrollback_db);

    let live_ids = imported.all_tab_ids();

    {
        let mut app_data = state.app_data.write();
        *app_data = imported;
    }
    let data_clone = state.app_data.read().clone();
    save_state(&data_clone)?;

    // Drop any scrollback row that the import didn't claim.
    match state.scrollback_db.prune_orphans(&live_ids) {
        Ok(n) if n > 0 => log::info!("Pruned {} orphan scrollback rows after import", n),
        Ok(_) => {}
        Err(e) => log::warn!("Failed to prune scrollback after import: {}", e),
    }

    log::info!("State imported from {}", path);
    let _ = app.emit("state-imported", ());
    Ok(())
}

#[tauri::command]
pub fn get_app_diagnostics(state: State<'_, Arc<AppState>>) -> serde_json::Value {
    let app_data = state.app_data.read();

    let mut total_tabs = 0usize;
    let mut terminal_tabs = 0usize;
    let mut editor_tabs = 0usize;
    let mut diff_tabs = 0usize;
    let mut total_panes = 0usize;
    let mut total_workspaces = 0usize;
    // All terminal tab PTY IDs (for orphaned PTY detection)
    let mut all_tab_pty_ids: Vec<Option<String>> = Vec::new();

    for window in &app_data.windows {
        for ws in &window.workspaces {
            total_workspaces += 1;
            for pane in &ws.panes {
                total_panes += 1;
                for tab in &pane.tabs {
                    total_tabs += 1;
                    match tab.tab_type {
                        crate::state::workspace::TabType::Editor => editor_tabs += 1,
                        crate::state::workspace::TabType::Diff => diff_tabs += 1,
                        crate::state::workspace::TabType::Terminal => {
                            terminal_tabs += 1;
                            all_tab_pty_ids.push(tab.pty_id.clone());
                        }
                    }
                }
            }
        }
    }

    let pty_count = state.pty_registry.read().len();
    let file_watcher_count = state.file_watchers.read().len();

    // Classify terminal tabs by checking pty_id against the live PTY registry.
    // - active: pty_id exists in registry (live PTY running)
    // - suspended: pty_id set but not in registry (stale from previous session, awaiting re-init)
    // - uninitialized: no pty_id at all (never had a PTY spawned)
    let (mut active_terminal_tabs, mut suspended_tabs, mut uninitialized_tabs) = (0usize, 0usize, 0usize);
    // Orphaned refs: tabs that are the active tab in their pane of the active workspace,
    // have a pty_id, but no matching PTY — these should definitely be running.
    let mut orphaned_pty_refs: Vec<String> = Vec::new();
    {
        let registry = state.pty_registry.read();
        for window in &app_data.windows {
            let active_ws_id = window.active_workspace_id.as_deref();
            for ws in &window.workspaces {
                let is_active_ws = active_ws_id == Some(ws.id.as_str());
                for pane in &ws.panes {
                    for tab in &pane.tabs {
                        if tab.tab_type != crate::state::workspace::TabType::Terminal {
                            continue;
                        }
                        match &tab.pty_id {
                            Some(pty_id) if registry.contains_key(pty_id.as_str()) => {
                                active_terminal_tabs += 1;
                            }
                            Some(pty_id) => {
                                // Stale pty_id — but if this is the visible tab, it's an orphan
                                let is_visible = is_active_ws
                                    && pane.active_tab_id.as_deref() == Some(tab.id.as_str());
                                if is_visible {
                                    orphaned_pty_refs.push(pty_id.clone());
                                } else {
                                    suspended_tabs += 1;
                                }
                            }
                            None => {
                                uninitialized_tabs += 1;
                            }
                        }
                    }
                }
            }
        }
    }

    // PTYs in registry with no matching tab in any workspace (leaked processes)
    let orphaned_ptys: Vec<String> = {
        let registry = state.pty_registry.read();
        let all_ids: std::collections::HashSet<&str> = all_tab_pty_ids.iter()
            .filter_map(|id| id.as_deref())
            .collect();
        registry.keys()
            .filter(|k| !all_ids.contains(k.as_str()))
            .cloned()
            .collect()
    };

    // State file info
    let state_path = crate::state::persistence::get_state_path();
    let state_file_size = state_path.as_ref()
        .and_then(|p| std::fs::metadata(p).ok())
        .map(|m| m.len());

    // Process-level resource stats via sysinfo
    use sysinfo::{System, Pid, ProcessesToUpdate, ProcessRefreshKind, UpdateKind};
    let pid = Pid::from_u32(std::process::id());
    let mut sys = System::new();
    let refresh = ProcessRefreshKind::nothing()
        .with_memory()
        .with_cpu()
        .with_disk_usage();
    sys.refresh_processes_specifics(ProcessesToUpdate::Some(&[pid]), true, refresh);

    let process_info = sys.process(pid).map(|p| {
        serde_json::json!({
            "pid": std::process::id(),
            "memory_bytes": p.memory(),
            "virtual_memory_bytes": p.virtual_memory(),
            "cpu_usage_percent": p.cpu_usage(),
            "disk_read_bytes": p.disk_usage().read_bytes,
            "disk_written_bytes": p.disk_usage().written_bytes,
            "run_time_secs": p.run_time(),
        })
    });

    // Child PTY process stats
    let child_pids: Vec<u32> = {
        let registry = state.pty_registry.read();
        registry.values().filter_map(|h| h.child_pid).collect()
    };
    let mut child_info: Vec<serde_json::Value> = Vec::new();
    if !child_pids.is_empty() {
        let child_sysinfo_pids: Vec<Pid> = child_pids.iter().map(|&p| Pid::from_u32(p)).collect();
        let mut child_sys = System::new();
        let child_refresh = ProcessRefreshKind::nothing()
            .with_memory()
            .with_cpu()
            .with_cmd(UpdateKind::Always);
        child_sys.refresh_processes_specifics(ProcessesToUpdate::Some(&child_sysinfo_pids), true, child_refresh);
        for &cpid in &child_pids {
            if let Some(p) = child_sys.process(Pid::from_u32(cpid)) {
                let cmd_str: Vec<String> = p.cmd().iter().map(|s| s.to_string_lossy().into_owned()).collect();
                child_info.push(serde_json::json!({
                    "pid": cpid,
                    "name": p.name().to_string_lossy(),
                    "cmd": cmd_str.join(" "),
                    "memory_bytes": p.memory(),
                    "cpu_usage_percent": p.cpu_usage(),
                }));
            }
        }
    }

    // PTY throughput stats
    let pty_throughput: Vec<serde_json::Value> = {
        use std::sync::atomic::Ordering;
        let stats = state.pty_stats.read();
        stats.iter().map(|(pty_id, s)| {
            serde_json::json!({
                "pty_id": pty_id,
                "bytes_read": s.bytes_read.load(Ordering::Relaxed),
                "bytes_written": s.bytes_written.load(Ordering::Relaxed),
            })
        }).collect()
    };

    // State save timing
    let (save_count, save_last_us, save_total_us, save_last_bytes) =
        crate::state::persistence::get_save_stats();

    // System-level stats
    let mut total_sys = System::new();
    total_sys.refresh_memory();
    total_sys.refresh_cpu_all();

    // memory_trend is populated by the periodic memory_sampler task — read only here.
    let memory_trend: Vec<crate::state::app_state::MemorySample> =
        state.memory_samples.read().clone();

    // Crash forensics: did we exit cleanly last time? And what does macOS's
    // DiagnosticReports directory have on us?
    let prev_run = crate::state::persistence::previous_run_info();
    let crash_reports = crate::commands::system::scan_crash_reports(20, 30);

    let ssh_mcp_tunnel_info: Vec<serde_json::Value> = {
        let tunnels = state.ssh_tunnels.read();
        tunnels.values().map(|t| {
            let tab_ids: Vec<&str> = t.tab_ids.iter().map(|s| s.as_str()).collect();
            serde_json::json!({
                "host_key": t.host_key,
                "remote_port": t.remote_port,
                "pid": t.pid,
                "alive": crate::commands::ssh_tunnel::is_tunnel_alive(t.pid),
                "tab_ids": tab_ids,
            })
        }).collect()
    };

    serde_json::json!({
        "version": crate::APP_VERSION,
        "mode": if cfg!(debug_assertions) { "dev" } else { "production" },
        "windows": app_data.windows.len(),
        "workspaces": total_workspaces,
        "panes": total_panes,
        "tabs": {
            "total": total_tabs,
            "terminal": terminal_tabs,
            "editor": editor_tabs,
            "diff": diff_tabs,
        },
        "pty_registry_count": pty_count,
        "active_terminal_tabs": active_terminal_tabs,
        "suspended_terminal_tabs": suspended_tabs,
        "uninitialized_terminal_tabs": uninitialized_tabs,
        "file_watcher_count": file_watcher_count,
        "orphaned_pty_refs": orphaned_pty_refs,
        "orphaned_ptys": orphaned_ptys,
        "pty_throughput": pty_throughput,
        "state_save": {
            "count": save_count,
            "last_duration_us": save_last_us,
            "avg_duration_us": if save_count > 0 { save_total_us / save_count } else { 0 },
            "total_duration_us": save_total_us,
            "last_bytes": save_last_bytes,
        },
        "state_file_bytes": state_file_size,
        "process": process_info,
        "pty_processes": child_info,
        "ssh_mcp_tunnels": ssh_mcp_tunnel_info,
        "system": {
            "total_memory_bytes": total_sys.total_memory(),
            "used_memory_bytes": total_sys.used_memory(),
            "cpu_count": total_sys.cpus().len(),
        },
        "memory_trend": memory_trend,
        "previous_run": {
            "crashed": prev_run.crashed,
            "marker_mtime_secs": prev_run.marker_mtime_secs,
        },
        "crash_reports": crash_reports,
    })
}

#[tauri::command]
pub fn read_app_logs(lines: Option<usize>, level: Option<String>, search: Option<String>) -> Result<serde_json::Value, String> {
    let max_lines = lines.unwrap_or(100).min(1000);
    let is_dev = cfg!(debug_assertions);
    let file_name = if is_dev { "aiterm-dev" } else { "aiterm" };

    // Resolve log directory (matches tauri-plugin-log default)
    let log_dir = dirs::data_dir()
        .or_else(dirs::config_dir)
        .map(|d| {
            if cfg!(target_os = "macos") {
                // macOS: ~/Library/Logs/com.aiterm.app/
                dirs::home_dir().unwrap_or(d.clone())
                    .join("Library/Logs/com.aiterm.app")
            } else {
                // Linux/Windows: config_dir/aiterm/logs/
                d.join("aiterm/logs")
            }
        })
        .ok_or("Could not determine log directory")?;

    let log_path = log_dir.join(format!("{}.log", file_name));

    if !log_path.exists() {
        return Ok(serde_json::json!({
            "path": log_path.to_string_lossy(),
            "lines": [],
            "truncated": false,
        }));
    }

    let content = std::fs::read_to_string(&log_path)
        .map_err(|e| format!("Failed to read log file: {}", e))?;

    let all_lines: Vec<&str> = content.lines().collect();

    // Filter by level if specified
    let level_upper = level.map(|l| l.to_uppercase());
    let filtered: Vec<&str> = all_lines.into_iter()
        .filter(|line| {
            if let Some(ref lvl) = level_upper {
                // Log lines look like: [2024-01-01][INFO][aiterm] message
                line.contains(&format!("[{}]", lvl))
            } else {
                true
            }
        })
        .filter(|line| {
            if let Some(ref q) = search {
                line.contains(q.as_str())
            } else {
                true
            }
        })
        .collect();

    let total = filtered.len();
    let truncated = total > max_lines;
    let result: Vec<&str> = filtered.into_iter().rev().take(max_lines).collect::<Vec<_>>().into_iter().rev().collect();

    Ok(serde_json::json!({
        "path": log_path.to_string_lossy(),
        "total_matching": total,
        "lines": result,
        "truncated": truncated,
    }))
}
