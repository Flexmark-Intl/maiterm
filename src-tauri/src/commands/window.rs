use std::sync::Arc;
use tauri::{Manager, State};
use tauri::webview::WebviewWindowBuilder;

use crate::state::{save_state, AppState, Pane, Tab, WindowData, Workspace};
use crate::state::workspace::{SplitNode};

#[tauri::command]
pub fn get_window_data(window: tauri::Window, state: State<'_, Arc<AppState>>) -> Result<WindowData, String> {
    let label = window.label().to_string();
    let app_data = state.app_data.read();
    app_data.window(&label)
        .cloned()
        .ok_or_else(|| format!("No window data for label '{}'", label))
}

#[tauri::command]
pub fn create_window(app: tauri::AppHandle, state: State<'_, Arc<AppState>>) -> Result<String, String> {
    let label = format!("window-{}", uuid::Uuid::new_v4());

    // Create window data with a default workspace
    let mut win_data = WindowData::new(label.clone());
    let ws = Workspace::new("Default".to_string());
    win_data.active_workspace_id = Some(ws.id.clone());
    win_data.workspaces.push(ws);

    let data_clone = {
        let mut app_data = state.app_data.write();
        app_data.windows.push(win_data);
        app_data.clone()
    };
    save_state(&data_clone)?;

    // Spawn window creation in a background thread so the command returns
    // immediately and the calling window stays responsive. build_window_sync
    // internally dispatches to the main thread for the actual WebView2 init,
    // but the command handler thread (and thus the JS await) won't block.
    let app_clone = app.clone();
    let label_clone = label.clone();
    std::thread::spawn(move || {
        if let Err(e) = build_window_sync(&app_clone, &label_clone) {
            log::error!("Failed to create window '{}': {}", label_clone, e);
        }
    });

    Ok(label)
}

/// Context for each tab when duplicating a window.
#[derive(Debug, Clone, serde::Deserialize)]
pub struct TabContext {
    pub tab_id: String,
    pub scrollback: Option<String>,
    pub cwd: Option<String>,
    pub ssh_command: Option<String>,
    pub remote_cwd: Option<String>,
}

#[tauri::command]
pub fn duplicate_window(
    window: tauri::Window,
    app: tauri::AppHandle,
    state: State<'_, Arc<AppState>>,
    tab_contexts: Vec<TabContext>,
) -> Result<String, String> {
    let source_label = window.label().to_string();
    let new_label = format!("window-{}", uuid::Uuid::new_v4());

    let data_clone = {
        let mut app_data = state.app_data.write();
        let source = app_data.window(&source_label)
            .ok_or_else(|| format!("Source window '{}' not found", source_label))?
            .clone();

        let mut new_win = WindowData::new(new_label.clone());
        new_win.sidebar_width = source.sidebar_width;
        new_win.sidebar_collapsed = source.sidebar_collapsed;

        for ws in &source.workspaces {
            let cloned = clone_workspace_with_new_ids(ws, &tab_contexts);
            new_win.workspaces.push(cloned);
        }

        // Set active workspace to the cloned version of the source's active
        if let Some(ref active_id) = source.active_workspace_id {
            // Find the index of the active workspace in source
            if let Some(idx) = source.workspaces.iter().position(|w| w.id == *active_id) {
                if let Some(cloned_ws) = new_win.workspaces.get(idx) {
                    new_win.active_workspace_id = Some(cloned_ws.id.clone());
                }
            }
        }

        // Move scrollback from cloned tabs into SQLite
        for ws in &mut new_win.workspaces {
            for pane in &mut ws.panes {
                for tab in &mut pane.tabs {
                    if let Some(ref sb) = tab.scrollback {
                        let _ = state.scrollback_db.save(&tab.id, sb);
                        tab.scrollback = None;
                    }
                }
            }
        }

        app_data.windows.push(new_win);
        app_data.clone()
    };
    save_state(&data_clone)?;

    // Spawn window creation in background thread (see create_window comment)
    let app_clone = app.clone();
    let label_clone = new_label.clone();
    std::thread::spawn(move || {
        if let Err(e) = build_window_sync(&app_clone, &label_clone) {
            log::error!("Failed to create window '{}': {}", label_clone, e);
        }
    });

    Ok(new_label)
}

#[tauri::command]
pub fn close_window(window: tauri::Window, state: State<'_, Arc<AppState>>) -> Result<(), String> {
    let label = window.label().to_string();
    let (data_clone, orphan_ids) = {
        let mut app_data = state.app_data.write();
        let orphan_ids: Vec<String> = app_data.windows.iter()
            .filter(|w| w.label == label)
            .flat_map(|w| {
                w.workspaces.iter().flat_map(|ws| {
                    ws.panes.iter().flat_map(|p| p.tabs.iter().map(|t| t.id.clone()))
                        .chain(ws.archived_tabs.iter().map(|t| t.id.clone()))
                })
            })
            .collect();
        app_data.windows.retain(|w| w.label != label);
        (app_data.clone(), orphan_ids)
    };
    let _ = state.scrollback_db.delete_many(&orphan_ids);
    save_state(&data_clone)?;
    Ok(())
}

#[tauri::command]
pub fn save_window_geometry(window: tauri::Window, state: State<'_, Arc<AppState>>, monitor_count: usize) -> Result<(), String> {
    let label = window.label().to_string();
    let scale = window.scale_factor().unwrap_or(1.0);

    let pos = window.outer_position().map_err(|e| e.to_string())?;
    let size = window.inner_size().map_err(|e| e.to_string())?;

    let geom = crate::state::WindowGeometry {
        x: pos.x as f64 / scale,
        y: pos.y as f64 / scale,
        width: size.width as f64 / scale,
        height: size.height as f64 / scale,
    };

    let data_clone = {
        let mut app_data = state.app_data.write();
        let win = app_data.window_mut(&label).ok_or("Window not found")?;
        // Migrate legacy flat fields if present
        win.migrate_legacy_geometry(monitor_count);
        win.window_geometry.insert(monitor_count.to_string(), geom);
        app_data.clone()
    };
    save_state(&data_clone)?;
    Ok(())
}

/// Get the number of connected monitors.
#[tauri::command]
pub fn get_monitor_count(window: tauri::Window) -> usize {
    window.available_monitors()
        .map(|m| m.len())
        .unwrap_or(1)
}

/// Restore window geometry for the given monitor count.
/// Returns true if geometry was found and applied, false otherwise.
#[tauri::command]
pub fn restore_window_geometry(window: tauri::Window, state: State<'_, Arc<AppState>>, monitor_count: usize) -> bool {
    let label = window.label().to_string();
    let geometry = {
        let data = state.app_data.read();
        data.window(&label).and_then(|w| w.geometry_for(monitor_count)).cloned()
    };

    if let Some(geom) = geometry {
        let scale = window.scale_factor().unwrap_or(1.0);
        let _ = window.set_size(tauri::Size::Logical(tauri::LogicalSize::new(geom.width, geom.height)));
        let phys_x = (geom.x * scale) as i32;
        let phys_y = (geom.y * scale) as i32;
        let _ = window.set_position(tauri::Position::Physical(tauri::PhysicalPosition::new(phys_x, phys_y)));
        log::info!("Restored geometry for '{}' (monitors={}) at ({}, {}) {}x{}",
            label, monitor_count, geom.x, geom.y, geom.width, geom.height);
        true
    } else {
        false
    }
}

#[tauri::command]
pub fn reset_window(window: tauri::Window, state: State<'_, Arc<AppState>>) -> Result<(), String> {
    let label = window.label().to_string();
    let (data_clone, orphan_ids) = {
        let mut app_data = state.app_data.write();
        let win = app_data.window_mut(&label).ok_or("Window not found")?;
        let orphan_ids: Vec<String> = win.workspaces.iter()
            .flat_map(|ws| {
                ws.panes.iter().flat_map(|p| p.tabs.iter().map(|t| t.id.clone()))
                    .chain(ws.archived_tabs.iter().map(|t| t.id.clone()))
            })
            .collect();
        win.workspaces.clear();
        win.active_workspace_id = None;
        (app_data.clone(), orphan_ids)
    };
    let _ = state.scrollback_db.delete_many(&orphan_ids);
    save_state(&data_clone)?;
    Ok(())
}

#[tauri::command]
pub fn get_window_count(app: tauri::AppHandle) -> usize {
    app.webview_windows().iter()
        .filter(|(label, _)| label.as_str() != "preferences" && label.as_str() != "help")
        .count()
}

#[tauri::command]
pub fn open_preferences_window(window: tauri::WebviewWindow, app: tauri::AppHandle) -> Result<(), String> {
    // If already open, focus it
    if let Some(win) = app.get_webview_window("preferences") {
        let _ = win.set_focus();
        return Ok(());
    }

    let pref_w: f64 = 900.0;
    let pref_h: f64 = 650.0;

    // Compute position from the calling window before spawning (WebviewWindow is not Send)
    let position = if let (Ok(pos), Ok(size)) = (window.outer_position(), window.outer_size()) {
        let scale = window.scale_factor().unwrap_or(1.0);
        let win_x = pos.x as f64 / scale;
        let win_y = pos.y as f64 / scale;
        let win_w = size.width as f64 / scale;
        let win_h = size.height as f64 / scale;
        Some((win_x + (win_w - pref_w) / 2.0, win_y + (win_h - pref_h) / 2.0))
    } else {
        None
    };

    // Spawn in background thread — calling build() on the command handler thread
    // deadlocks on Windows because WebView2 init dispatches to the main thread
    // while the main thread waits for the sync command to return.
    std::thread::spawn(move || {
        let url = if cfg!(debug_assertions) {
            tauri::WebviewUrl::External("http://localhost:1420/preferences".parse().unwrap())
        } else {
            tauri::WebviewUrl::App("preferences".into())
        };

        let title = if cfg!(debug_assertions) { "Preferences (Dev)" } else { "Preferences" };

        let mut builder = WebviewWindowBuilder::new(&app, "preferences", url)
            .title(title)
            .inner_size(pref_w, pref_h)
            .min_inner_size(500.0, 400.0)
            .resizable(true)
            .fullscreen(false);

        #[cfg(target_os = "macos")]
        {
            builder = builder.hidden_title(true);
        }

        if let Some((x, y)) = position {
            builder = builder.position(x, y);
        }

        if let Err(e) = builder.build() {
            log::error!("Failed to create preferences window: {}", e);
        }
    });

    Ok(())
}

#[tauri::command]
pub fn open_help_window(window: tauri::WebviewWindow, app: tauri::AppHandle, section: Option<String>) -> Result<(), String> {
    // If already open, navigate to section via JS and focus
    if let Some(win) = app.get_webview_window("help") {
        if let Some(ref s) = section {
            let _ = win.eval(&format!(
                "localStorage.setItem('help-section','{}');window.dispatchEvent(new CustomEvent('help-section',{{detail:'{}'}}));",
                s, s
            ));
        }
        let _ = win.set_focus();
        return Ok(());
    }

    let help_w: f64 = 680.0;
    let help_h: f64 = 600.0;

    // Compute position from the calling window before spawning (WebviewWindow is not Send)
    let position = if let (Ok(pos), Ok(size)) = (window.outer_position(), window.outer_size()) {
        let scale = window.scale_factor().unwrap_or(1.0);
        let win_x = pos.x as f64 / scale;
        let win_y = pos.y as f64 / scale;
        let win_w = size.width as f64 / scale;
        let win_h = size.height as f64 / scale;
        Some((win_x + (win_w - help_w) / 2.0, win_y + (win_h - help_h) / 2.0))
    } else {
        None
    };

    // Spawn in background thread — see open_preferences_window for rationale
    std::thread::spawn(move || {
        let path = match &section {
            Some(s) => format!("help?section={}", s),
            None => "help".to_string(),
        };

        let url = if cfg!(debug_assertions) {
            tauri::WebviewUrl::External(format!("http://localhost:1420/{}", path).parse().unwrap())
        } else {
            tauri::WebviewUrl::App(path.into())
        };

        let title = if cfg!(debug_assertions) { "Help (Dev)" } else { "Help" };

        let mut builder = WebviewWindowBuilder::new(&app, "help", url)
            .title(title)
            .inner_size(help_w, help_h)
            .min_inner_size(500.0, 400.0)
            .resizable(true)
            .fullscreen(false);

        #[cfg(target_os = "macos")]
        {
            builder = builder.hidden_title(true);
        }

        if let Some((x, y)) = position {
            builder = builder.position(x, y);
        }

        if let Err(e) = builder.build() {
            log::error!("Failed to create help window: {}", e);
        }
    });

    Ok(())
}

fn build_window_sync(app: &tauri::AppHandle, label: &str) -> Result<(), String> {
    let url = if cfg!(debug_assertions) {
        tauri::WebviewUrl::External("http://localhost:1420".parse().unwrap())
    } else {
        tauri::WebviewUrl::App("index.html".into())
    };

    let title = if cfg!(debug_assertions) { "maiTerm (Dev)" } else { "maiTerm" };

    // Read saved geometry for current monitor count
    let monitor_count = app.primary_monitor()
        .ok()
        .flatten()
        .and_then(|_| app.available_monitors().ok())
        .map(|m| m.len())
        .unwrap_or(1);

    let geometry = app.try_state::<Arc<AppState>>().and_then(|state| {
        let data = state.app_data.read();
        let win = data.window(label)?;
        win.geometry_for(monitor_count).cloned()
    });

    let (w, h) = geometry.as_ref()
        .map(|g| (g.width, g.height))
        .unwrap_or((1200.0, 800.0));

    let mut builder = WebviewWindowBuilder::new(app, label, url)
        .title(title)
        .inner_size(w, h)
        .min_inner_size(800.0, 600.0)
        .resizable(true)
        .fullscreen(false);

    #[cfg(target_os = "macos")]
    {
        builder = builder
            .hidden_title(true)
            .title_bar_style(tauri::TitleBarStyle::Overlay);
    }

    let win = builder.build()
        .map_err(|e| format!("Failed to create window: {}", e))?;

    if let Some(ref geom) = geometry {
        let scale = win.scale_factor().unwrap_or(1.0);
        let phys_x = (geom.x * scale) as i32;
        let phys_y = (geom.y * scale) as i32;
        let _ = win.set_position(tauri::Position::Physical(tauri::PhysicalPosition::new(phys_x, phys_y)));
    }

    Ok(())
}

fn clone_workspace_with_new_ids(ws: &Workspace, tab_contexts: &[TabContext]) -> Workspace {
    let (cloned, _) = clone_workspace_with_id_mapping(ws, tab_contexts);
    cloned
}

/// Clone a workspace with new UUIDs for all entities.
/// Returns the cloned workspace and a mapping of old_tab_id -> new_tab_id.
pub(crate) fn clone_workspace_with_id_mapping(
    ws: &Workspace,
    tab_contexts: &[TabContext],
) -> (Workspace, std::collections::HashMap<String, String>) {
    let mut id_map: std::collections::HashMap<String, String> = std::collections::HashMap::new();
    let mut tab_id_map: std::collections::HashMap<String, String> = std::collections::HashMap::new();

    let new_ws_id = uuid::Uuid::new_v4().to_string();
    id_map.insert(ws.id.clone(), new_ws_id.clone());

    let new_panes: Vec<Pane> = ws.panes.iter().map(|pane| {
        let new_pane_id = uuid::Uuid::new_v4().to_string();
        id_map.insert(pane.id.clone(), new_pane_id.clone());

        let new_tabs: Vec<Tab> = pane.tabs.iter().map(|tab| {
            let new_tab_id = uuid::Uuid::new_v4().to_string();
            id_map.insert(tab.id.clone(), new_tab_id.clone());
            tab_id_map.insert(tab.id.clone(), new_tab_id.clone());

            // Find matching context from the source window
            let ctx = tab_contexts.iter().find(|c| c.tab_id == tab.id);

            Tab {
                id: new_tab_id,
                name: tab.name.clone(),
                pty_id: None, // New window will spawn fresh PTYs
                scrollback: ctx.and_then(|c| c.scrollback.clone()),
                custom_name: tab.custom_name,
                restore_cwd: ctx.and_then(|c| c.cwd.clone()),
                restore_ssh_command: ctx.and_then(|c| c.ssh_command.clone()),
                restore_remote_cwd: ctx.and_then(|c| c.remote_cwd.clone()),
                auto_resume_cwd: tab.auto_resume_cwd.clone(),
                auto_resume_ssh_command: tab.auto_resume_ssh_command.clone(),
                auto_resume_remote_cwd: tab.auto_resume_remote_cwd.clone(),
                auto_resume_command: tab.auto_resume_command.clone(),
                auto_resume_remembered_command: tab.auto_resume_remembered_command.clone(),
                auto_resume_pinned: tab.auto_resume_pinned,
                auto_resume_enabled: tab.auto_resume_enabled,
                notes: tab.notes.clone(),
                notes_mode: tab.notes_mode.clone(),
                notes_open: tab.notes_open,
                composer_open: tab.composer_open,
                composer_draft: tab.composer_draft.clone(),
                trigger_variables: tab.trigger_variables.clone(),
                archived_name: None,
                archived_at: None,
                suspended_at: None,
                tab_type: tab.tab_type.clone(),
                editor_file: tab.editor_file.clone(),
                last_cwd: tab.last_cwd.clone(),
                diff_context: tab.diff_context.clone(),
                import_highlight: false,
                // Tab ids are remapped for the new window, so an Agent Bridge (which
                // references the partner by tab id) can't carry over — drop it.
                agent_bridge: None,
            }
        }).collect();

        let new_active_tab = pane.active_tab_id.as_ref()
            .and_then(|id| id_map.get(id))
            .cloned();

        Pane {
            id: new_pane_id,
            name: pane.name.clone(),
            tabs: new_tabs,
            active_tab_id: new_active_tab,
        }
    }).collect();

    let new_active_pane = ws.active_pane_id.as_ref()
        .and_then(|id| id_map.get(id))
        .cloned();

    let new_split_root = ws.split_root.as_ref().map(|root| clone_split_node(root, &id_map));

    let cloned = Workspace {
        id: new_ws_id,
        name: ws.name.clone(),
        panes: new_panes,
        active_pane_id: new_active_pane,
        split_root: new_split_root,
        workspace_notes: ws.workspace_notes.clone(),
        archived_tabs: Vec::new(),
        import_highlight: false,
        suspended: false,
        pane_sizes: None,
    };

    (cloned, tab_id_map)
}

fn clone_split_node(node: &SplitNode, id_map: &std::collections::HashMap<String, String>) -> SplitNode {
    match node {
        SplitNode::Leaf { pane_id } => SplitNode::Leaf {
            pane_id: id_map.get(pane_id).cloned().unwrap_or_else(|| pane_id.clone()),
        },
        SplitNode::Split { direction, ratio, children, .. } => SplitNode::Split {
            id: uuid::Uuid::new_v4().to_string(),
            direction: direction.clone(),
            ratio: *ratio,
            children: Box::new((
                clone_split_node(&children.0, id_map),
                clone_split_node(&children.1, id_map),
            )),
        },
    }
}
