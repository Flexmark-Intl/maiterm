use std::sync::Arc;
use tauri::{Manager, State};
use tauri::webview::WebviewWindowBuilder;

use crate::state::{save_state, AppState, Pane, Tab, WindowData, Workspace};
use crate::state::workspace::{SplitNode};

/// The colour a window shows wherever the page hasn't painted. Tokyo Night's `--bg-dark`,
/// the default theme: the frontend overrides it from `applyUiTheme` once it knows the
/// real one, so this only covers the moments before the first frame.
pub const DEFAULT_WINDOW_BG: tauri::window::Color = tauri::window::Color(26, 27, 38, 255);

/// Paint the native window — and on macOS the WKWebView's own base layer — in the theme's
/// background colour. WebKit discards the page's compositing layers while the window is
/// occluded (display sleep, lock screen) and rebuilds them on wake; until that first frame
/// the window shows these colours, which default to white. That is the "white malformed
/// window" seen for a second or two on unlock. Tauri's webview-layer colour is a no-op on
/// macOS, hence the direct `underPageBackgroundColor` call.
#[tauri::command]
pub fn set_window_background(window: tauri::WebviewWindow, hex: String) -> Result<(), String> {
    let (r, g, b) = parse_hex_rgb(&hex).ok_or_else(|| format!("not a #rrggbb colour: {hex}"))?;
    window
        .set_background_color(Some(tauri::window::Color(r, g, b, 255)))
        .map_err(|e| e.to_string())?;
    #[cfg(target_os = "macos")]
    window
        .with_webview(move |wv| unsafe {
            let view = &*(wv.inner() as *const objc2_web_kit::WKWebView);
            let color = objc2_app_kit::NSColor::colorWithSRGBRed_green_blue_alpha(
                r as f64 / 255.0,
                g as f64 / 255.0,
                b as f64 / 255.0,
                1.0,
            );
            view.setUnderPageBackgroundColor(Some(&color));
        })
        .map_err(|e| e.to_string())?;
    Ok(())
}

fn parse_hex_rgb(hex: &str) -> Option<(u8, u8, u8)> {
    let s = hex.trim().trim_start_matches('#');
    // Byte-sliced below, so a non-ASCII string (hand-edited state) must fail here, not panic.
    if s.len() != 6 || !s.is_ascii() {
        return None;
    }
    let ch = |i: usize| u8::from_str_radix(&s[i..i + 2], 16).ok();
    Some((ch(0)?, ch(2)?, ch(4)?))
}

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

#[derive(Debug, Clone, serde::Serialize)]
pub struct DuplicatedWindow {
    pub label: String,
    /// Names of the Hive workspaces left out of the copy (docs/hive.md §11.2).
    pub skipped_hives: Vec<String>,
}

#[tauri::command]
pub fn duplicate_window(
    window: tauri::Window,
    app: tauri::AppHandle,
    state: State<'_, Arc<AppState>>,
    tab_contexts: Vec<TabContext>,
) -> Result<DuplicatedWindow, String> {
    let source_label = window.label().to_string();
    let new_label = format!("window-{}", uuid::Uuid::new_v4());

    let (data_clone, skipped_hives) = {
        let mut app_data = state.app_data.write();
        let source = app_data.window(&source_label)
            .ok_or_else(|| format!("Source window '{}' not found", source_label))?
            .clone();

        // Start from the source and name the EXCEPTIONS. Building the copy up
        // field by field from WindowData::new() is an allowlist, and an allowlist
        // makes every new WindowData field a silent regression: that is exactly
        // how `name` came to be dropped, so a duplicate of a window you had named
        // arrived unnamed and fell back to its active workspace's name. Same
        // lesson as `carry_tab_state_on_reload` — see CLAUDE.md.
        let mut new_win = source.clone();
        new_win.id = uuid::Uuid::new_v4().to_string();
        new_win.label = new_label.clone();
        // Rebuilt below, with fresh ids throughout.
        new_win.workspaces = Vec::new();
        new_win.active_workspace_id = None;
        // Geometry describes where the SOURCE was put; inheriting it would open
        // the duplicate exactly on top of the window it came from.
        new_win.window_geometry = std::collections::HashMap::new();
        new_win.last_geometry_monitors = None;
        // The ledger is a verbatim record of what was typed into the source's
        // tabs. The copy's tabs are new ids with no such history.
        new_win.overlord_ledger = Vec::new();
        new_win.overlord_tasks = Vec::new();

        let (copied, skipped) = split_out_hives(&source.workspaces);
        if !skipped.is_empty() {
            log::info!("duplicate_window: skipped {} Hive workspace(s): {}", skipped.len(), skipped.join(", "));
        }
        for ws in &copied {
            let cloned = clone_workspace_with_new_ids(ws, &tab_contexts);
            new_win.workspaces.push(cloned);
        }
        remint_mesh_groups(&mut new_win.workspaces);

        // Set active workspace to the cloned version of the source's active
        if let Some(ref active_id) = source.active_workspace_id {
            // Find the index of the active workspace among the copied ones
            if let Some(idx) = copied.iter().position(|w| w.id == *active_id) {
                if let Some(cloned_ws) = new_win.workspaces.get(idx) {
                    new_win.active_workspace_id = Some(cloned_ws.id.clone());
                }
            }
        }
        // Skipping Hives can leave nothing (or only the Overlord's) to show: a new window's
        // "Default", as `move_workspace_to_window` leaves behind. A skipped active one hands
        // over to the first ordinary workspace.
        if !skipped.is_empty() {
            if !new_win.workspaces.iter().any(|w| !w.overlord) {
                let fresh = Workspace::new("Default".to_string());
                new_win.active_workspace_id = Some(fresh.id.clone());
                new_win.workspaces.push(fresh);
            } else if new_win.active_workspace_id.is_none() {
                new_win.active_workspace_id = new_win.workspaces.iter().find(|w| !w.overlord).map(|w| w.id.clone());
            }
        }

        // Move scrollback from cloned tabs into SQLite
        for ws in &mut new_win.workspaces {
            for pane in &mut ws.panes {
                for tab in &mut pane.tabs {
                    if let Some(ref sb) = tab.scrollback {
                        let _ = state.scrollback_db.save(&tab.id, sb, None);
                        tab.scrollback = None;
                    }
                }
            }
        }

        app_data.windows.push(new_win);
        (app_data.clone(), skipped)
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

    Ok(DuplicatedWindow { label: new_label, skipped_hives })
}

/// The workspaces a window duplicate copies, and the names of the Hives it leaves out: a
/// copy of a Hive is either hidden tabs with no Hive or a second Hive on the same worktrees
/// (docs/hive.md §11.2).
pub(crate) fn split_out_hives(workspaces: &[Workspace]) -> (Vec<&Workspace>, Vec<String>) {
    let (hives, copied): (Vec<&Workspace>, Vec<&Workspace>) = workspaces.iter().partition(|w| w.hive.is_some());
    (copied, hives.into_iter().map(|w| w.name.clone()).collect())
}

/// Follow-ups discarded with a window's workspaces (docs/follow-ups.md §3). Logged from these
/// Rust paths because no reload goes through them, so a drop here is never false; per-tab and
/// per-pane closes log from the frontend store instead, where reload can be told apart.
fn log_follow_ups_dropped(workspaces: &[Workspace], how: &str) {
    for ws in workspaces {
        for tab in ws.panes.iter().flat_map(|p| p.tabs.iter()).chain(ws.archived_tabs.iter()) {
            if !tab.follow_ups.is_empty() {
                log::warn!(
                    "follow-ups: tab {} {how}, {} pending dropped",
                    &tab.id[..tab.id.len().min(8)],
                    tab.follow_ups.len()
                );
            }
        }
    }
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
        if let Some(win) = app_data.window(&label) {
            log_follow_ups_dropped(&win.workspaces, "closed with its window");
        }
        app_data.windows.retain(|w| w.label != label);
        (app_data.clone(), orphan_ids)
    };
    let _ = state.scrollback_db.delete_many(&orphan_ids);
    save_state(&data_clone)?;
    Ok(())
}

/// A window a workspace or tab can be moved to, as the "Move to Window" menus list it.
#[derive(Debug, Clone, serde::Serialize)]
pub struct MoveTargetWindow {
    /// The window's persistent id — what `listWindows` gives agents (`moveWorkspace`).
    pub id: String,
    pub label: String,
    pub name: Option<String>,
    /// What an unnamed window is called in its own titlebar: its active workspace's name.
    pub active_workspace_name: Option<String>,
    pub is_current: bool,
    /// Never the Overlord workspace: it lives behind its own accessor row, one per window.
    pub workspaces: Vec<MoveTargetWorkspace>,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct MoveTargetWorkspace {
    pub id: String,
    pub name: String,
    /// Its mesh, for deciding whether moving a tab in changes who can message it
    /// (agent_requests.rs): a window cannot see another window's workspaces otherwise.
    pub bridge_all: bool,
    pub mesh_group: Option<String>,
    /// It is a Hive's workspace: the media setup sheet scopes an MCP server to "Hive workspaces
    /// only" from Preferences (docs/hive-media.md §6), across every window.
    pub hive: bool,
}

/// Every app window (not Preferences/Help) with its workspaces, in window order.
#[tauri::command]
pub fn list_move_targets(window: tauri::Window, state: State<'_, Arc<AppState>>) -> Vec<MoveTargetWindow> {
    let current = window.label();
    let app_data = state.app_data.read();
    app_data.windows.iter()
        .filter(|w| w.label != "preferences" && w.label != "help")
        .map(|w| MoveTargetWindow {
            id: w.id.clone(),
            label: w.label.clone(),
            name: w.name.clone(),
            active_workspace_name: w.active_workspace_id.as_ref()
                .and_then(|id| w.workspaces.iter().find(|ws| &ws.id == id))
                .map(|ws| ws.name.clone()),
            is_current: w.label == current,
            workspaces: w.workspaces.iter()
                .filter(|ws| !ws.overlord)
                .map(|ws| MoveTargetWorkspace {
                    id: ws.id.clone(),
                    name: ws.name.clone(),
                    bridge_all: ws.bridge_all,
                    mesh_group: ws.mesh_group.clone(),
                    hive: ws.hive.is_some(),
                })
                .collect(),
        })
        .collect()
}

/// Move a whole workspace to the bottom of another window's list and make it that window's
/// active workspace. The record moves as-is — its tabs keep their ids, `pty_id`s, scrollback
/// and stack — so the live PTYs are reattached by the target webview, not respawned. This is
/// NOT delete + create: `delete_workspace` wipes every tab's scrollback from SQLite.
///
/// The webview handoff (PTYs preserved, per-tab stores passed across) is the frontend's —
/// `workspacesStore.moveWorkspaceToWindow`. A source left with no workspace besides the
/// Overlord's gets a fresh "Default", as a new window does.
#[tauri::command]
pub fn move_workspace_to_window(
    window: tauri::Window,
    state: State<'_, Arc<AppState>>,
    workspace_id: String,
    target_label: String,
) -> Result<(), String> {
    let source_label = window.label().to_string();
    if source_label == target_label {
        return Err("Workspace is already in that window".to_string());
    }
    let data_clone = {
        let mut app_data = state.app_data.write();
        // Validate both ends before mutating: extracting first would lose the workspace
        // if the target lookup failed.
        if app_data.window(&target_label).is_none() {
            return Err("Target window not found".to_string());
        }
        let source = app_data.window_mut(&source_label).ok_or("Window not found")?;
        let index = source.workspaces.iter().position(|w| w.id == workspace_id)
            .ok_or("Workspace not found")?;
        if source.workspaces[index].overlord {
            return Err("The Overlord workspace belongs to its window and cannot move".to_string());
        }
        crate::hive::refuse_workspace_move(&source.workspaces[index])?;
        let ws = source.workspaces.remove(index);

        if !source.workspaces.iter().any(|w| !w.overlord) {
            let fresh = Workspace::new("Default".to_string());
            source.active_workspace_id = Some(fresh.id.clone());
            source.workspaces.push(fresh);
        } else if source.active_workspace_id.as_deref() == Some(workspace_id.as_str()) {
            // The one that slid into its place, else the one before — as delete_workspace
            // picks — but never the Overlord's, which lives behind its own accessor row.
            let (before, after) = source.workspaces.split_at(index);
            let pick = after.iter().chain(before.iter().rev())
                .find(|w| !w.overlord)
                .map(|w| w.id.clone());
            source.active_workspace_id = pick;
        }

        let target = app_data.window_mut(&target_label).ok_or("Target window not found")?;
        target.active_workspace_id = Some(ws.id.clone());
        target.workspaces.push(ws);
        app_data.clone()
    };
    save_state(&data_clone)
}

/// Move a tab into a workspace of another window: that workspace's first pane, as its
/// active tab, and the workspace becomes the target window's active one. The tab (and its
/// PTY) moves as-is, like `move_tab_to_workspace`, whose rules it follows — including
/// dropping `service_id`, since the stack binding belongs to the workspace it left.
/// The source pane is left as it falls; the caller cleans up an emptied one, as
/// `moveTabToWorkspace` does.
#[tauri::command]
pub fn move_tab_to_window(
    window: tauri::Window,
    state: State<'_, Arc<AppState>>,
    source_workspace_id: String,
    source_pane_id: String,
    tab_id: String,
    target_label: String,
    target_workspace_id: String,
) -> Result<(), String> {
    let source_label = window.label().to_string();
    if source_label == target_label {
        return Err("Same window: use move_tab_to_workspace".to_string());
    }
    let data_clone = {
        let mut app_data = state.app_data.write();
        let target_ws = app_data.window(&target_label)
            .and_then(|w| w.workspaces.iter().find(|ws| ws.id == target_workspace_id))
            .filter(|ws| !ws.panes.is_empty())
            .ok_or("Target workspace not found")?;
        let source_ws = app_data.window(&source_label)
            .and_then(|w| w.workspaces.iter().find(|ws| ws.id == source_workspace_id))
            .ok_or("Source workspace not found")?;
        crate::hive::refuse_tab_move(source_ws, &tab_id, Some(target_ws))?;

        let source = app_data.window_mut(&source_label).ok_or("Window not found")?;
        let source_ws = source.workspaces.iter_mut().find(|w| w.id == source_workspace_id)
            .ok_or("Source workspace not found")?;
        let source_pane = source_ws.panes.iter_mut().find(|p| p.id == source_pane_id)
            .ok_or("Source pane not found")?;
        let tab_pos = source_pane.tabs.iter().position(|t| t.id == tab_id)
            .ok_or("Tab not found")?;
        let mut tab: Tab = source_pane.tabs.remove(tab_pos);
        tab.unbind_service();
        if source_pane.active_tab_id.as_ref() == Some(&tab_id) {
            source_pane.active_tab_id = super::workspace::pick_active_after_close(&source_pane.tabs, tab_pos);
        }

        let target = app_data.window_mut(&target_label).ok_or("Target window not found")?;
        target.active_workspace_id = Some(target_workspace_id.clone());
        let target_ws = target.workspaces.iter_mut().find(|w| w.id == target_workspace_id)
            .ok_or("Target workspace not found")?;
        let target_pane: &mut Pane = target_ws.panes.first_mut().ok_or("Target workspace has no panes")?;
        target_pane.active_tab_id = Some(tab.id.clone());
        target_pane.tabs.push(tab);
        app_data.clone()
    };
    save_state(&data_clone)
}

/// Name this window (titlebar centre, maiLink, `listWindows`), or clear the name with `None`
/// or a blank string so those surfaces fall back to their derived text again.
#[tauri::command]
pub fn set_window_name(
    window: tauri::Window,
    state: State<'_, Arc<AppState>>,
    name: Option<String>,
) -> Result<(), String> {
    let label = window.label().to_string();
    let name = name.map(|n| n.trim().to_string()).filter(|n| !n.is_empty());
    let data_clone = {
        let mut app_data = state.app_data.write();
        let win = app_data.window_mut(&label).ok_or("Window not found")?;
        win.name = name;
        app_data.clone()
    };
    save_state(&data_clone)?;
    Ok(())
}

/// Reject raw physical window rects that represent a minimized or collapsed window.
///
/// On Windows, minimizing a window moves it to `(-32000, -32000)` in physical pixels and
/// shrinks its client size to `0x0`. Checking the raw physical coordinates catches that
/// sentinel even at extreme DPI scales (400%+ where `-32000 / scale` would exceed `-10000`).
fn is_valid_physical_rect(pos_x: i32, pos_y: i32, width: u32, height: u32) -> bool {
    if width == 0 || height == 0 {
        return false;
    }
    if pos_x <= -30_000 && pos_y <= -30_000 {
        return false;
    }
    true
}

/// Pure overlap check between a window rect and a slice of monitor rects `(x, y, width, height)`
/// in the same coordinate space (logical on macOS/Linux, physical on Windows). Returns true when
/// the monitors list is empty (unreadable) or when at least 50x30 units intersect some monitor.
fn rect_visible_on_monitors(
    x: i32,
    y: i32,
    w: u32,
    h: u32,
    monitors: &[(i32, i32, u32, u32)],
) -> bool {
    if monitors.is_empty() {
        return true;
    }
    let x1 = x as i64;
    let y1 = y as i64;
    let x2 = x1 + w as i64;
    let y2 = y1 + h as i64;
    monitors.iter().any(|&(mx, my, mw, mh)| {
        let mx1 = mx as i64;
        let my1 = my as i64;
        let mx2 = mx1 + mw as i64;
        let my2 = my1 + mh as i64;
        let overlap_w = x2.min(mx2) - x1.max(mx1);
        let overlap_h = y2.min(my2) - y1.max(my1);
        overlap_w >= 50 && overlap_h >= 30
    })
}

/// Visibility check in global logical coordinates (macOS / Linux).
/// In tao on macOS and Linux, each `Monitor`'s `position()` and `size()` are multiplied by that
/// monitor's own `scale_factor()`, whereas `WindowGeometry` stores global logical coordinates
/// (divided by the window's scale factor). Normalizing each monitor by its own scale factor
/// restores a single shared logical desktop space across mixed-DPI displays.
#[cfg_attr(target_os = "windows", allow(dead_code))]
fn geometry_visible_logical(
    geom: &crate::state::WindowGeometry,
    monitors: &[(i32, i32, u32, u32, f64)],
) -> bool {
    // Windows entries store unified physical virtual-desktop origins (pixel sums across
    // mixed-DPI monitors) which cannot be converted into macOS/Linux logical space.
    // Skip positioning (logical width/height remain usable when creating the window).
    if geom.physical_position || !geom.is_valid() {
        return false;
    }
    let x = geom.x.round() as i32;
    let y = geom.y.round() as i32;
    let w = geom.width.round().max(0.0) as u32;
    let h = geom.height.round().max(0.0) as u32;
    let rects: Vec<(i32, i32, u32, u32)> = monitors
        .iter()
        .map(|&(px, py, pw, ph, m_scale)| {
            let s = if m_scale.is_finite() && m_scale > 0.0 { m_scale } else { 1.0 };
            (
                (px as f64 / s).round() as i32,
                (py as f64 / s).round() as i32,
                (pw as f64 / s).round().max(0.0) as u32,
                (ph as f64 / s).round().max(0.0) as u32,
            )
        })
        .collect();
    rect_visible_on_monitors(x, y, w, h, &rects)
}

/// Logical minimum inner dimensions for main/workspace app windows (matches `tauri.conf.json`).
#[cfg_attr(not(target_os = "windows"), allow(dead_code))]
const APP_WINDOW_MIN_WIDTH: f64 = 800.0;
#[cfg_attr(not(target_os = "windows"), allow(dead_code))]
const APP_WINDOW_MIN_HEIGHT: f64 = 600.0;

/// Build a `WindowGeometry` from raw physical `outer_position` and `inner_size` plus the
/// window's current DPI `scale`.
///
/// - On Windows (`physical_origin == true`), `x`/`y` are stored as unified physical
///   virtual-desktop pixels (`physical_position: true`, `scale_factor: Some(scale)`) because
///   `GetWindowRect` and `SetWindowPos` do not scale with per-monitor DPI.
/// - On macOS/Linux (`physical_origin == false`), `x`/`y` are divided by `scale` into
///   global logical coordinates (`physical_position: false`, `scale_factor: None`), matching
///   tao's `outer_position`/`set_outer_position`.
/// - `width`/`height` are always stored as logical inner size (DIPs).
fn build_window_geometry(
    pos_x: i32,
    pos_y: i32,
    width: u32,
    height: u32,
    scale: f64,
    physical_origin: bool,
) -> crate::state::WindowGeometry {
    let (x, y) = if physical_origin {
        (pos_x as f64, pos_y as f64)
    } else {
        (pos_x as f64 / scale, pos_y as f64 / scale)
    };
    crate::state::WindowGeometry {
        x,
        y,
        width: width as f64 / scale,
        height: height as f64 / scale,
        physical_position: physical_origin,
        scale_factor: physical_origin.then_some(scale),
    }
}

/// Convert a saved `WindowGeometry` origin to the `PhysicalPosition` passed to `set_position`.
///
/// - On Windows (`is_windows == true`), entries saved with `physical_position == true` already
///   store unified physical virtual-desktop coordinates and are used directly regardless of the
///   restoring window's current DPI. Legacy entries (`physical_position == false`) fall back to
///   `geom * window_scale` so single-DPI upgrades keep restoring accurately.
/// - On macOS/Linux (`is_windows == false`), `tao`'s `set_outer_position` divides by the
///   window's `scale_factor()`, so multiplying logical `geom` by `window_scale` cancels out.
fn resolve_physical_position(
    geom: &crate::state::WindowGeometry,
    window_scale: f64,
    is_windows: bool,
) -> (i32, i32) {
    if is_windows && geom.physical_position {
        (geom.x.round() as i32, geom.y.round() as i32)
    } else {
        (
            (geom.x * window_scale).round() as i32,
            (geom.y * window_scale).round() as i32,
        )
    }
}

pub fn target_physical_position(geom: &crate::state::WindowGeometry, window_scale: f64) -> (i32, i32) {
    resolve_physical_position(geom, window_scale, cfg!(target_os = "windows"))
}

/// Visibility check in unified physical virtual-desktop coordinates (Windows), where Win32
/// `rcMonitor` and `SetWindowPos` share one physical pixel space.
///
/// `monitors` holds `(pos_x, pos_y, width, height, monitor_scale)` in physical pixels.
/// - When `geom.physical_position` is true, `(geom.x, geom.y)` is already in physical pixels
///   and each monitor's own `monitor_scale` converts logical `(geom.width, geom.height)` to
///   physical size on that monitor — independent of the restoring window's DPI.
/// - When `geom.physical_position` is false (legacy pre-fix JSON), falls back to scaling by
///   `window_scale`.
#[cfg_attr(not(target_os = "windows"), allow(dead_code))]
fn geometry_visible_physical(
    geom: &crate::state::WindowGeometry,
    window_scale: f64,
    monitors: &[(i32, i32, u32, u32, f64)],
) -> bool {
    if !geom.is_valid() || !window_scale.is_finite() || window_scale <= 0.0 {
        return false;
    }
    if monitors.is_empty() {
        return true;
    }
    if geom.physical_position {
        let phys_x = geom.x.round() as i32;
        let phys_y = geom.y.round() as i32;
        // Always evaluate visibility using each connected monitor's current `m_scale` (not a
        // potentially stale `geom.scale_factor`) so a window that would shrink off-screen once
        // `set_size(Logical)` runs at the connected monitor's DPI is rejected.
        monitors.iter().any(|&(mx, my, mw, mh, m_scale)| {
            let s = if m_scale.is_finite() && m_scale > 0.0 { m_scale } else { 1.0 };
            let phys_w = (geom.width * s).round().clamp(0.0, u32::MAX as f64) as u32;
            let phys_h = (geom.height * s).round().clamp(0.0, u32::MAX as f64) as u32;
            rect_visible_on_monitors(phys_x, phys_y, phys_w, phys_h, &[(mx, my, mw, mh)])
        })
    } else {
        let phys_x = (geom.x * window_scale).round() as i32;
        let phys_y = (geom.y * window_scale).round() as i32;
        let phys_w = (geom.width * window_scale).round().clamp(0.0, u32::MAX as f64) as u32;
        let phys_h = (geom.height * window_scale).round().clamp(0.0, u32::MAX as f64) as u32;
        let rects: Vec<(i32, i32, u32, u32)> = monitors
            .iter()
            .map(|&(mx, my, mw, mh, _)| (mx, my, mw, mh))
            .collect();
        rect_visible_on_monitors(phys_x, phys_y, phys_w, phys_h, &rects)
    }
}

/// Whether `geom` lands visibly on at least one currently connected monitor. Prevents restoring
/// a window off-screen (e.g. a stale minimized coordinate or a monitor arrangement that changed).
pub fn geometry_visible_on_monitors(
    geom: &crate::state::WindowGeometry,
    scale: f64,
    monitors: &[tauri::Monitor],
) -> bool {
    let rects: Vec<(i32, i32, u32, u32, f64)> = monitors
        .iter()
        .map(|m| {
            let p = m.position();
            let s = m.size();
            (p.x, p.y, s.width, s.height, m.scale_factor())
        })
        .collect();
    #[cfg(target_os = "windows")]
    {
        geometry_visible_physical(geom, scale, &rects)
    }
    #[cfg(not(target_os = "windows"))]
    {
        let _ = scale;
        geometry_visible_logical(geom, &rects)
    }
}

/// Compute the saved physical inner dimensions `(width, height)` for a Windows restore before
/// moving the window to `(phys_x, phys_y)`.
///
/// Pre-sizing to this physical footprint (with `min_size` temporarily lifted) ensures that
/// moving directly to `(phys_x, phys_y)` lands with the exact saved footprint on the target
/// monitor — triggering `WM_DPICHANGED` cleanly for left, above, sandwiched, and straddling
/// mixed-DPI monitors without heuristic staging hops.
#[cfg_attr(not(target_os = "windows"), allow(dead_code))]
fn windows_target_physical_size(
    geom: &crate::state::WindowGeometry,
    phys_x: i32,
    phys_y: i32,
    window_scale: f64,
    monitors: &[(i32, i32, u32, u32, f64)],
) -> (u32, u32) {
    let s = if geom.physical_position {
        geom.scale_factor
            .filter(|v| v.is_finite() && *v > 0.0)
            .or_else(|| {
                // Sample an interior point past Windows' invisible DWM frame (~7–8 logical px,
                // up to ~32 physical px at 400% DPI) so snapped/maximized origins like
                // `(mx - 7, 0)` or `(mx - 8, -8)` still resolve to their own monitor.
                let px = (phys_x as i64).saturating_add(64);
                let py = (phys_y as i64).saturating_add(64);
                monitors
                    .iter()
                    .filter(|&&(_, _, _, _, m_scale)| m_scale.is_finite() && m_scale > 0.0)
                    .min_by_key(|&&(mx, my, mw, mh, _)| {
                        let mx1 = mx as i64;
                        let my1 = my as i64;
                        let mx2 = mx1.saturating_add(mw as i64).saturating_sub(1);
                        let my2 = my1.saturating_add(mh as i64).saturating_sub(1);
                        let dx = if px < mx1 { mx1 - px } else if px > mx2 { px - mx2 } else { 0 };
                        let dy = if py < my1 { my1 - py } else if py > my2 { py - my2 } else { 0 };
                        dx.saturating_mul(dx).saturating_add(dy.saturating_mul(dy))
                    })
                    .map(|&(_, _, _, _, m_scale)| m_scale)
            })
            .unwrap_or(window_scale)
    } else {
        window_scale
    };
    let s = if s.is_finite() && s > 0.0 { s } else { 1.0 };
    (
        (geom.width * s).round().clamp(1.0, u32::MAX as f64) as u32,
        (geom.height * s).round().clamp(1.0, u32::MAX as f64) as u32,
    )
}

/// Apply a saved `WindowGeometry` position to a newly built `WebviewWindow` if it lands
/// visibly on a connected monitor. On Windows, pre-sizes to the saved physical footprint
/// before moving so `WM_DPICHANGED` fires cleanly across mixed-DPI monitors, then re-applies
/// the logical inner size and exact physical origin at the settled DPI. Returns `true` if applied.
pub fn apply_restored_position(
    win: &tauri::WebviewWindow,
    geom: &crate::state::WindowGeometry,
    label: &str,
) -> bool {
    let scale = win.scale_factor().ok().filter(|s| s.is_finite() && *s > 0.0).unwrap_or(1.0);
    let monitors = win.available_monitors().unwrap_or_default();
    if !geometry_visible_on_monitors(geom, scale, &monitors) {
        log::warn!(
            "Skipping off-screen saved position for '{}' at ({}, {}) {}x{}",
            label, geom.x, geom.y, geom.width, geom.height
        );
        return false;
    }
    let (phys_x, phys_y) = target_physical_position(geom, scale);
    #[cfg(target_os = "windows")]
    {
        let monitor_rects: Vec<(i32, i32, u32, u32, f64)> = monitors
            .iter()
            .map(|m| {
                let p = m.position();
                let s = m.size();
                (p.x, p.y, s.width, s.height, m.scale_factor())
            })
            .collect();
        let (target_w, target_h) = windows_target_physical_size(geom, phys_x, phys_y, scale, &monitor_rects);
        let _ = win.set_min_size(None::<tauri::Size>);
        let _ = win.set_size(tauri::Size::Physical(tauri::PhysicalSize::new(target_w, target_h)));
        let pos_result = win.set_position(tauri::Position::Physical(tauri::PhysicalPosition::new(phys_x, phys_y)));
        if pos_result.is_ok() {
            let _ = win.set_size(tauri::Size::Logical(tauri::LogicalSize::new(geom.width, geom.height)));
            let _ = win.set_position(tauri::Position::Physical(tauri::PhysicalPosition::new(phys_x, phys_y)));
        }
        let _ = win.set_min_size(Some(tauri::Size::Logical(tauri::LogicalSize::new(
            APP_WINDOW_MIN_WIDTH,
            APP_WINDOW_MIN_HEIGHT,
        ))));
        if let Err(e) = pos_result {
            log::warn!("Failed to set position for '{}': {}", label, e);
            return false;
        }
    }
    #[cfg(not(target_os = "windows"))]
    {
        if let Err(e) = win.set_position(tauri::Position::Physical(tauri::PhysicalPosition::new(phys_x, phys_y))) {
            log::warn!("Failed to set position for '{}': {}", label, e);
            return false;
        }
    }
    true
}

#[tauri::command]
pub fn save_window_geometry(window: tauri::Window, state: State<'_, Arc<AppState>>, monitor_count: usize) -> Result<(), String> {
    // Never key a layout to an absence (see `monitor_count`). The frontend already holds
    // while the displays are dark; this is the backstop that keeps a "0" entry — which
    // launch would then restore — out of the state file for good.
    if monitor_count == 0 {
        return Ok(());
    }
    let label = window.label().to_string();
    // Never overwrite saved geometry while minimized: on Windows a minimized window sits at
    // (-32000, -32000) with 0x0 inner size, and Win32 fires WM_MOVE/WM_SIZE on minimize.
    if window.is_minimized().unwrap_or(false) {
        log::debug!("Skipping geometry save for '{}' while minimized", label);
        return Ok(());
    }
    let scale = window.scale_factor().ok().filter(|s| s.is_finite() && *s > 0.0).unwrap_or(1.0);

    let pos = window.outer_position().map_err(|e| e.to_string())?;
    let size = window.inner_size().map_err(|e| e.to_string())?;
    if !is_valid_physical_rect(pos.x, pos.y, size.width, size.height) {
        log::debug!(
            "Skipping invalid physical geometry for '{}': ({}, {}) {}x{}",
            label, pos.x, pos.y, size.width, size.height
        );
        return Ok(());
    }

    let geom = build_window_geometry(
        pos.x,
        pos.y,
        size.width,
        size.height,
        scale,
        cfg!(target_os = "windows"),
    );
    if !geom.is_valid() {
        log::debug!(
            "Skipping invalid geometry for '{}': ({}, {}) {}x{}",
            label, geom.x, geom.y, geom.width, geom.height
        );
        return Ok(());
    }

    let data_clone = {
        let mut app_data = state.app_data.write();
        let win = app_data.window_mut(&label).ok_or("Window not found")?;
        // Migrate legacy flat fields if present
        win.migrate_legacy_geometry(monitor_count);
        win.window_geometry.insert(monitor_count.to_string(), geom);
        win.last_geometry_monitors = Some(monitor_count);
        app_data.clone()
    };
    save_state(&data_clone)?;
    Ok(())
}

/// The number of connected monitors, or None while that is unknowable.
///
/// Zero monitors is an absence, not a display configuration: macOS reports every screen
/// gone while the displays sleep or the lock screen is up. Anything keyed on a monitor
/// count must hold rather than invent one — a "0" key is a phantom layout, and restoring
/// it at launch is what moved every window after a deploy done in the dark.
pub fn monitor_count(app: &tauri::AppHandle) -> Option<usize> {
    match app.available_monitors() {
        Ok(m) if !m.is_empty() => Some(m.len()),
        _ => None,
    }
}

/// Get the number of connected monitors. 0 means "no answer" — displays asleep, locked,
/// or the list unreadable — and the caller is expected to hold, not to act on it.
#[tauri::command]
pub fn get_monitor_count(window: tauri::Window) -> usize {
    window.available_monitors()
        .map(|m| m.len())
        .unwrap_or(0)
}

/// Restore window geometry for the given monitor count.
/// Returns true if geometry was found and applied, false otherwise.
#[tauri::command]
pub fn restore_window_geometry(window: tauri::Window, state: State<'_, Arc<AppState>>, monitor_count: usize) -> bool {
    if monitor_count == 0 || window.is_minimized().unwrap_or(false) {
        return false;
    }
    let label = window.label().to_string();
    let geometry = {
        let data = state.app_data.read();
        data.window(&label).and_then(|w| w.geometry_for(monitor_count)).cloned()
    };

    if let Some(geom) = geometry {
        let scale = window.scale_factor().ok().filter(|s| s.is_finite() && *s > 0.0).unwrap_or(1.0);
        let monitors = window.available_monitors().unwrap_or_default();
        if !geometry_visible_on_monitors(&geom, scale, &monitors) {
            log::warn!(
                "Skipping off-screen saved geometry for '{}' (monitors={}) at ({}, {}) {}x{}",
                label, monitor_count, geom.x, geom.y, geom.width, geom.height
            );
            return false;
        }
        let (phys_x, phys_y) = target_physical_position(&geom, scale);
        #[cfg(target_os = "windows")]
        {
            if window.is_maximized().unwrap_or(false) {
                let _ = window.unmaximize();
            }
            let monitor_rects: Vec<(i32, i32, u32, u32, f64)> = monitors
                .iter()
                .map(|m| {
                    let p = m.position();
                    let s = m.size();
                    (p.x, p.y, s.width, s.height, m.scale_factor())
                })
                .collect();
            let (target_w, target_h) = windows_target_physical_size(&geom, phys_x, phys_y, scale, &monitor_rects);
            // Temporarily lift min_inner_size so WM_GETMINMAXINFO does not clamp to the source
            // monitor's DPI-scaled minimum before moving onto a lower-DPI monitor. Pre-size to
            // the saved physical footprint before moving so MonitorFromWindow selects the target
            // monitor on the first move, then re-apply logical size + exact origin and restore min.
            let _ = window.set_min_size(None::<tauri::Size>);
            let _ = window.set_size(tauri::Size::Physical(tauri::PhysicalSize::new(target_w, target_h)));
            let _ = window.set_position(tauri::Position::Physical(tauri::PhysicalPosition::new(phys_x, phys_y)));
            let _ = window.set_size(tauri::Size::Logical(tauri::LogicalSize::new(geom.width, geom.height)));
            let _ = window.set_position(tauri::Position::Physical(tauri::PhysicalPosition::new(phys_x, phys_y)));
            let _ = window.set_min_size(Some(tauri::Size::Logical(tauri::LogicalSize::new(
                APP_WINDOW_MIN_WIDTH,
                APP_WINDOW_MIN_HEIGHT,
            ))));
        }
        #[cfg(not(target_os = "windows"))]
        {
            // Size before position on macOS/Linux: AppKit converts top-left position using
            // frame height, so resizing after positioning shifts the top-left Y.
            let _ = window.set_size(tauri::Size::Logical(tauri::LogicalSize::new(geom.width, geom.height)));
            let _ = window.set_position(tauri::Position::Physical(tauri::PhysicalPosition::new(phys_x, phys_y)));
        }
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
        // Reached by deleting a window's LAST workspace from the sidebar, and by closing the
        // last window on macOS — neither goes through deleteWorkspace or close_window.
        log_follow_ups_dropped(&win.workspaces, "removed when its window was reset");
        win.workspaces.clear();
        win.active_workspace_id = None;
        (app_data.clone(), orphan_ids)
    };
    let _ = state.scrollback_db.delete_many(&orphan_ids);
    save_state(&data_clone)?;
    Ok(())
}

/// Reload one app window's webview, as the menu's Reload Current Window does: its PTYs live on
/// and its tabs reattach. For an agent's approved `reloadWindow` (agent_requests.rs) aimed at a
/// window other than the one carrying it out.
#[tauri::command]
pub fn reload_window(app: tauri::AppHandle, label: String) -> Result<(), String> {
    if label == "preferences" || label == "help" {
        return Err("not an app window".into());
    }
    let win = app.get_webview_window(&label).ok_or("the window is gone")?;
    win.eval("window.location.reload()").map_err(|e| e.to_string())
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
    let position = if !window.is_minimized().unwrap_or(false) {
        if let (Ok(pos), Ok(size)) = (window.outer_position(), window.outer_size()) {
            if is_valid_physical_rect(pos.x, pos.y, size.width, size.height) {
                let scale = window.scale_factor().ok().filter(|s| s.is_finite() && *s > 0.0).unwrap_or(1.0);
                let win_x = pos.x as f64 / scale;
                let win_y = pos.y as f64 / scale;
                let win_w = size.width as f64 / scale;
                let win_h = size.height as f64 / scale;
                Some((win_x + (win_w - pref_w) / 2.0, win_y + (win_h - pref_h) / 2.0))
            } else {
                None
            }
        } else {
            None
        }
    } else {
        None
    };

    // Spawn in background thread — calling build() on the command handler thread
    // deadlocks on Windows because WebView2 init dispatches to the main thread
    // while the main thread waits for the sync command to return.
    std::thread::spawn(move || {
        let url = if cfg!(debug_assertions) {
            dev_url(&app, "preferences")
        } else {
            tauri::WebviewUrl::App("preferences".into())
        };

        let title = if cfg!(debug_assertions) { "Preferences (Dev)" } else { "Preferences" };

        let mut builder = WebviewWindowBuilder::new(&app, "preferences", url)
            .title(title)
            .inner_size(pref_w, pref_h)
            .min_inner_size(500.0, 400.0)
            .resizable(true)
            .fullscreen(false)
            .background_color(DEFAULT_WINDOW_BG);

        // The page draws the title bar (as the main window does): a native one shows no title
        #[cfg(target_os = "macos")]
        {
            builder = builder
                .hidden_title(true)
                .title_bar_style(tauri::TitleBarStyle::Overlay);
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
    let position = if !window.is_minimized().unwrap_or(false) {
        if let (Ok(pos), Ok(size)) = (window.outer_position(), window.outer_size()) {
            if is_valid_physical_rect(pos.x, pos.y, size.width, size.height) {
                let scale = window.scale_factor().ok().filter(|s| s.is_finite() && *s > 0.0).unwrap_or(1.0);
                let win_x = pos.x as f64 / scale;
                let win_y = pos.y as f64 / scale;
                let win_w = size.width as f64 / scale;
                let win_h = size.height as f64 / scale;
                Some((win_x + (win_w - help_w) / 2.0, win_y + (win_h - help_h) / 2.0))
            } else {
                None
            }
        } else {
            None
        }
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
            dev_url(&app, &path)
        } else {
            tauri::WebviewUrl::App(path.into())
        };

        let title = if cfg!(debug_assertions) { "Help (Dev)" } else { "Help" };

        let mut builder = WebviewWindowBuilder::new(&app, "help", url)
            .title(title)
            .inner_size(help_w, help_h)
            .min_inner_size(500.0, 400.0)
            .resizable(true)
            .fullscreen(false)
            .background_color(DEFAULT_WINDOW_BG);

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

/// A dev build's page on the frontend dev server: the config's `devUrl`, which an isolated
/// instance moves off 1420 (scripts/dev-isolated.mjs). A copy on 1420 may be another checkout.
pub(crate) fn dev_url(app: &tauri::AppHandle, path: &str) -> tauri::WebviewUrl {
    let base = app.config().build.dev_url.as_ref().map(|u| u.to_string()).unwrap_or_else(|| "http://localhost:1420".to_string());
    tauri::WebviewUrl::External(format!("{}/{}", base.trim_end_matches('/'), path).parse().unwrap())
}

fn build_window_sync(app: &tauri::AppHandle, label: &str) -> Result<(), String> {
    let url = if cfg!(debug_assertions) {
        dev_url(app, "")
    } else {
        tauri::WebviewUrl::App("index.html".into())
    };

    let title = if cfg!(debug_assertions) { "maiTerm (Dev)" } else { "maiTerm" };

    // Read saved geometry for the current monitor count. With no count — displays asleep
    // or locked — there is no layout to restore: come up at the default and let the
    // frontend place the window when the displays return.
    let count = monitor_count(app);
    let geometry = count.and_then(|count| {
        app.try_state::<Arc<AppState>>().and_then(|state| {
            let data = state.app_data.read();
            let win = data.window(label)?;
            win.geometry_for(count).cloned()
        })
    });

    // With no count the size is still known even though the position isn't — see
    // `WindowData::last_geometry`.
    let last_size = count.is_none().then(|| {
        app.try_state::<Arc<AppState>>().and_then(|state| {
            let data = state.app_data.read();
            let win = data.window(label)?;
            win.last_geometry().map(|g| (g.width, g.height))
        })
    }).flatten();

    let (w, h) = geometry.as_ref()
        .map(|g| (g.width, g.height))
        .or(last_size)
        .unwrap_or((1200.0, 800.0));

    let mut builder = WebviewWindowBuilder::new(app, label, url)
        .title(title)
        .inner_size(w, h)
        .min_inner_size(800.0, 600.0)
        .resizable(true)
        .fullscreen(false)
        .background_color(DEFAULT_WINDOW_BG);

    #[cfg(target_os = "macos")]
    {
        builder = builder
            .hidden_title(true)
            .title_bar_style(tauri::TitleBarStyle::Overlay);
    }

    let win = builder.build()
        .map_err(|e| format!("Failed to create window: {}", e))?;

    if let Some(ref geom) = geometry {
        apply_restored_position(&win, geom, label);
    }

    Ok(())
}

/// A duplicated window's linked meshes get FRESH group ids, one per source group, so the
/// copies link to each other but never to their sources — moving a copy back into the source
/// window would otherwise rejoin it and put every role on that mesh twice.
fn remint_mesh_groups(workspaces: &mut [Workspace]) {
    let mut fresh: std::collections::HashMap<String, String> = std::collections::HashMap::new();
    for ws in workspaces {
        ws.mesh_group = ws.mesh_group.take().map(|g| {
            fresh.entry(g).or_insert_with(|| uuid::Uuid::new_v4().to_string()).clone()
        });
    }
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
                // A duplicated window's tabs are live, not archived — and the source tab's
                // rows stay with the source tab.
                archived_tasks: Vec::new(),
                scrollback: ctx.and_then(|c| c.scrollback.clone()),
                custom_name: tab.custom_name,
                pinned: tab.pinned,
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
                tasks_open: tab.tasks_open,
                overlord_exempt: tab.overlord_exempt,
                muted: tab.muted,
                composer_open: tab.composer_open,
                composer_draft: tab.composer_draft.clone(),
                mesh_purpose: tab.mesh_purpose.clone(),
                trigger_variables: tab.trigger_variables.clone(),
                archived_name: None,
                archived_at: None,
                suspended_at: None,
                wake_on_resume: false,
                // The stack definition travels with the workspace (below), but none of
                // its services are running in the copy, so no tab in it may claim one.
                service_id: None,
                service_target: None,
                // A copy of a Hive agent is not a Hive agent: the Hive's launch record and
                // worktrees belong to the original (docs/hive.md §5).
                hive_role: None,
                // A follow-up is one agent's note about its own session. The copy is a new
                // tab with a new agent; carrying them would deliver each one twice.
                follow_ups: Vec::new(),
                // The copy's agent is a new one; its first review asks it what it committed.
                last_review: None,
                tab_type: tab.tab_type.clone(),
                editor_file: tab.editor_file.clone(),
                last_cwd: tab.last_cwd.clone(),
                diff_context: tab.diff_context.clone(),
                import_highlight: false,
                // Tab ids are remapped for the new window, so an Agent Bridge (which
                // references the partner by tab id) can't carry over — drop it.
                agent_bridge: None,
                // Runtime is just a per-tab marker (no tab-id refs) — carry it over.
                runtime: tab.runtime,
                // maiLink designation is a per-tab marker (no tab-id refs) — carry it over.
                mailink_native: tab.mailink_native,
                mailink_excluded: tab.mailink_excluded,
                // A comms thread binding must stay unique to one tab — cloning it would
                // make the watcher double-inject thread replies. Never carry it over.
                // Same for monitoring: two monitors on one channel would double-pickup.
                comms_binding: None,
                comms_bindings: Vec::new(),
                comms_monitor: None,
                // A receipt records what THIS tab's agent has already been shown; a new
                // window's tabs run fresh sessions that have seen none of it.
                comms_thread_receipts: Vec::new(),
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

    // Tasks travel with the workspace — a duplicated workspace is a duplicated project,
    // and its work list is the point. Ids can't travel, though: task ids are minted fresh
    // (a `blocked_by` edge in the copy must point at the copy, not the original), tab
    // assignees are remapped through the same id_map the panes used.
    let task_id_map: std::collections::HashMap<String, String> = ws
        .tasks
        .iter()
        .map(|t| (t.id.clone(), uuid::Uuid::new_v4().to_string()))
        .collect();
    // Workstreams are copied with fresh ids for the same reason task ids are: the copy's
    // grouping must point at the copy.
    let workstream_id_map: std::collections::HashMap<String, String> = ws
        .workstreams
        .iter()
        .map(|w| (w.id.clone(), uuid::Uuid::new_v4().to_string()))
        .collect();
    let new_workstreams: Vec<crate::state::Workstream> = ws
        .workstreams
        .iter()
        .map(|w| crate::state::Workstream {
            id: workstream_id_map[&w.id].clone(),
            ..w.clone()
        })
        .collect();
    let new_tasks: Vec<crate::state::Task> = ws
        .tasks
        .iter()
        .map(|t| crate::state::Task {
            id: task_id_map[&t.id].clone(),
            tab_id: t
                .tab_id
                .as_ref()
                .and_then(|id| tab_id_map.get(id))
                .cloned(),
            // Drop edges to tasks that aren't in this workspace rather than leaving a
            // dangling id, which would render as permanently blocked.
            blocked_by: t
                .blocked_by
                .iter()
                .filter_map(|id| task_id_map.get(id))
                .cloned()
                .collect(),
            workstream_id: t
                .workstream_id
                .as_ref()
                .and_then(|id| workstream_id_map.get(id))
                .cloned(),
            // A card's command for another tab (docs/tasks.md §3.1.3) follows that tab to its
            // copy. A peer outside the workspace keeps its id: Run judges it when used, against
            // the copy's carrier (`runInTarget`), and refuses it unless it is that tab's peer.
            blocker: t.blocker.clone().map(|mut b| {
                if let Some(copy) = b.run_in.as_ref().and_then(|id| tab_id_map.get(id)) {
                    b.run_in = Some(copy.clone());
                }
                b
            }),
            ..t.clone()
        })
        .collect();

    let cloned = Workspace {
        id: new_ws_id,
        name: ws.name.clone(),
        panes: new_panes,
        active_pane_id: new_active_pane,
        split_root: new_split_root,
        // Fresh ids: note tools address a note by id, and two workspaces holding one id
        // would make that address ambiguous.
        workspace_notes: ws
            .workspace_notes
            .iter()
            .map(|n| crate::state::workspace::WorkspaceNote {
                id: uuid::Uuid::new_v4().to_string(),
                ..n.clone()
            })
            .collect(),
        // Preserve the mesh nature, but drop topics — they reference source tab ids that
        // were remapped for the new window (same reason agent_bridge is dropped per-tab).
        bridge_all: ws.bridge_all,
        mailink_native: ws.mailink_native,
        mesh_topics: Vec::new(),
        // Kept here; each caller decides. `duplicate_window` re-mints it per source group (its
        // copies stay linked to each other, never to their sources); `duplicate_workspace`
        // clears it, since a copy beside its source would put every role on the mesh twice.
        mesh_group: ws.mesh_group.clone(),
        tasks: new_tasks,
        workstreams: new_workstreams,
        // A copy of the project runs the same services; the definitions carry no tab ids.
        // Their ids must be fresh, though: the frontend stack runtime is keyed by service
        // id alone, so a shared id would share status, starts and restart timers with the
        // source workspace's service.
        stack: ws
            .stack
            .iter()
            // A job row is one run's, in this workspace only (docs/hive.md §6.8).
            .filter(|s| !s.is_job())
            .map(|s| crate::state::Service {
                id: uuid::Uuid::new_v4().to_string(),
                // Nothing runs in the copy; starting it there would fight the source for its port.
                was_running: false,
                running_targets: Vec::new(),
                // `shared_from` stays: it names the other workspace, which the copy shares too.
                ..s.clone()
            })
            .collect(),
        // Never duplicate an Overlord workspace — at most one per window.
        overlord: false,
        // An exemption is a property of the work, not the window: it travels.
        overlord_exempt: ws.overlord_exempt,
        muted: ws.muted,
        // A pin names a folder, not a tab: the copy is the same project and keeps its root.
        file_tree_pin: ws.file_tree_pin.clone(),
        // Callers refuse or skip a Hive (docs/hive.md §11.2); this is the backstop, with
        // `hive_role: None` per tab above.
        hive: None,
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

#[cfg(test)]
mod clone_ids_tests {
    use super::clone_workspace_with_id_mapping;
    use crate::state::workspace::{FollowUp, FollowUpDue, WorkspaceNote};
    use crate::state::{Service, Tab, Workspace};

    #[test]
    fn services_and_notes_get_fresh_ids() {
        let mut ws = Workspace::new("proj".to_string());
        ws.stack.push(Service {
            id: "svc-1".to_string(),
            name: "web".to_string(),
            normalized_name: "web".to_string(),
            command: "npm run dev".to_string(),
            cwd: "/src".to_string(),
            env: Vec::new(),
            ssh_command: None,
            auto_start: false,
            was_running: false,
            restart: "on_crash".to_string(),
            ready_pattern: None,
            port: None,
            url: None,
            origin: "human".to_string(),
            agent_edited: true,
            running_targets: vec!["branch:session-store".to_string()],
            shared_from: Some(crate::state::workspace::ServiceRef { workspace_id: "ws-db".into(), service_id: "svc-db".into() }),
            kind: crate::state::workspace::SERVICE_KIND.to_string(),
            created_at: String::new(),
            updated_at: String::new(),
        });
        let job = Service { id: "j-1".into(), kind: crate::state::workspace::JOB_KIND.into(), ..ws.stack[0].clone() };
        ws.stack.push(job);
        ws.panes[0].tabs[0].service_id = Some("svc-1".to_string());
        ws.panes[0].tabs[0].service_target = Some("branch:session-store".to_string());
        ws.workspace_notes.push(WorkspaceNote {
            id: "note-1".to_string(),
            content: "hi".to_string(),
            mode: None,
            created_at: String::new(),
            updated_at: String::new(),
        });

        let (cloned, _) = clone_workspace_with_id_mapping(&ws, &[]);

        assert_eq!(cloned.stack.len(), 1, "a job row stays with its run");
        assert_ne!(cloned.stack[0].id, "svc-1");
        assert_eq!(cloned.stack[0].name, "web");
        assert!(cloned.stack[0].agent_edited, "a copy keeps who wrote the definition");
        assert!(cloned.stack[0].running_targets.is_empty(), "no copy runs in the duplicate");
        assert_eq!(cloned.stack[0].shared_from, ws.stack[0].shared_from, "it still uses the other workspace's");
        let tab = &cloned.panes[0].tabs[0];
        assert!(tab.service_id.is_none() && tab.service_target.is_none());
        assert_ne!(cloned.workspace_notes[0].id, "note-1");
        assert_eq!(cloned.workspace_notes[0].content, "hi");
    }

    #[test]
    fn a_duplicated_window_remints_mesh_groups_per_source_group() {
        let mut ws: Vec<Workspace> = (0..4).map(|i| Workspace::new(format!("w{i}"))).collect();
        ws[0].mesh_group = Some("g1".to_string());
        ws[1].mesh_group = Some("g1".to_string());
        ws[2].mesh_group = Some("g2".to_string());
        super::remint_mesh_groups(&mut ws);
        let g = |i: usize| ws[i].mesh_group.clone();
        assert!(g(0).is_some() && g(0) != Some("g1".to_string()), "never the source's group");
        assert_eq!(g(0), g(1), "copies of one group stay linked to each other");
        assert!(g(2).is_some() && g(2) != Some("g2".to_string()) && g(2) != g(0));
        assert_eq!(g(3), None, "an unlinked workspace stays unlinked");
    }

    #[test]
    fn a_duplicated_workspace_carries_no_follow_ups() {
        // The copy's tabs are new tabs with new agents. A follow-up carried into one would be
        // delivered twice — once to the original agent, once to the copy (docs/follow-ups.md §3).
        let mut ws = Workspace::new("proj".to_string());
        ws.panes[0].tabs[0].follow_ups.push(FollowUp {
            id: "fu-1".to_string(),
            text: "check the deploy".to_string(),
            due: FollowUpDue {
                kind: "at".to_string(),
                at: Some("2026-10-01T09:00:00Z".to_string()),
                ..Default::default()
            },
            author: "agent".to_string(),
            created_at: "2026-09-30T09:00:00Z".to_string(),
            expires_at: None,
        });

        let (cloned, _) = clone_workspace_with_id_mapping(&ws, &[]);

        assert_eq!(ws.panes[0].tabs[0].follow_ups.len(), 1, "the original keeps its own");
        assert!(cloned.panes[0].tabs[0].follow_ups.is_empty());
    }

    #[test]
    fn a_duplicated_workspace_has_no_hive_tabs() {
        // A copy of a Hive agent is not a Hive agent (docs/hive.md §5): carrying the role
        // would hide the copy from the strip with no Hive to show it.
        let mut ws = Workspace::new("proj".to_string());
        ws.panes[0].tabs[0].hive_role = Some("worker".to_string());

        let (cloned, _) = clone_workspace_with_id_mapping(&ws, &[]);

        assert_eq!(ws.panes[0].tabs[0].hive_role.as_deref(), Some("worker"));
        assert_eq!(cloned.panes[0].tabs[0].hive_role, None);
        assert!(Tab::new("t".to_string()).hive_role.is_none(), "nor does a fresh tab");
    }

    #[test]
    fn a_fresh_tab_has_no_follow_ups() {
        // Every frontend duplicate path (duplicateTab, split, copy-to-workspace, new
        // conversation) builds its tab from these constructors and copies named fields, so this
        // is what keeps a duplicate from inheriting them.
        assert!(Tab::new("t".to_string()).follow_ups.is_empty());
    }

    #[test]
    fn window_geometry_rejects_windows_minimized_sentinels_and_collapsed_sizes() {
        use crate::state::WindowGeometry;
        let g = |x, y, width, height| WindowGeometry { x, y, width, height, physical_position: false, scale_factor: None };
        let gp = |x, y, width, height| WindowGeometry { x, y, width, height, physical_position: true, scale_factor: Some(1.0) };

        // Normal primary and negative multi-monitor placements.
        assert!(g(100.0, 100.0, 1200.0, 800.0).is_valid());
        assert!(g(-1920.0, 0.0, 1200.0, 800.0).is_valid());
        assert!(g(-2560.0, -1080.0, 1400.0, 900.0).is_valid());
        assert!(gp(4000.0, 100.0, 1200.0, 800.0).is_valid());
        assert!(gp(-11520.0, 0.0, 1200.0, 800.0).is_valid());

        // Win32 minimized sentinel (-32000, -32000) across legacy scaled & raw physical saves.
        assert!(!gp(-32000.0, -32000.0, 0.0, 0.0).is_valid());
        assert!(!gp(-32000.0, -32000.0, 1200.0, 800.0).is_valid());
        assert!(!g(-32000.0, -32000.0, 0.0, 0.0).is_valid());
        assert!(!g(-25600.0, -25600.0, 160.0, 28.0).is_valid());
        assert!(!g(-21333.33, -21333.33, 1200.0, 800.0).is_valid());
        assert!(!g(-16000.0, -16000.0, 1200.0, 800.0).is_valid());

        // Collapsed / iconic / extreme / non-finite rects and invalid scale factors.
        assert!(!g(100.0, 100.0, 0.0, 0.0).is_valid());
        assert!(!g(100.0, 100.0, 160.0, 28.0).is_valid());
        assert!(!g(100.0, 100.0, 1e20, 800.0).is_valid());
        assert!(!g(f64::NAN, 100.0, 1200.0, 800.0).is_valid());
        assert!(!g(100.0, f64::INFINITY, 1200.0, 800.0).is_valid());
        assert!(!WindowGeometry { x: 100.0, y: 100.0, width: 1200.0, height: 800.0, physical_position: true, scale_factor: Some(0.0) }.is_valid());
        assert!(!WindowGeometry { x: 100.0, y: 100.0, width: 1200.0, height: 800.0, physical_position: true, scale_factor: Some(f64::NAN) }.is_valid());
    }

    #[test]
    fn physical_rect_guard_catches_win32_sentinel_at_any_dpi_and_zero_size() {
        assert!(!super::is_valid_physical_rect(-32000, -32000, 1200, 800));
        assert!(!super::is_valid_physical_rect(100, 100, 0, 800));
        assert!(!super::is_valid_physical_rect(100, 100, 1200, 0));
        assert!(super::is_valid_physical_rect(-1920, 0, 1200, 800));
        assert!(super::is_valid_physical_rect(0, 0, 1920, 1080));
    }

    #[test]
    fn rect_visibility_check_accepts_on_screen_and_rejects_off_screen_rects() {
        let monitors = [(0, 0, 1920, 1080), (-1920, 0, 1920, 1080)];
        assert!(super::rect_visible_on_monitors(100, 100, 1200, 800, &monitors));
        assert!(super::rect_visible_on_monitors(-1800, 50, 1200, 800, &monitors));
        // Partially off-screen right edge, still 100x800 visible.
        assert!(super::rect_visible_on_monitors(1820, 100, 1200, 800, &monitors));
        // Minimized / completely off-screen rects (including 400% scaled -8000).
        assert!(!super::rect_visible_on_monitors(-32000, -32000, 1200, 800, &monitors));
        assert!(!super::rect_visible_on_monitors(-8000, -8000, 1200, 800, &monitors));
        assert!(!super::rect_visible_on_monitors(5000, 0, 1200, 800, &monitors));
        // Extreme i32 bounds must not overflow.
        assert!(!super::rect_visible_on_monitors(i32::MIN, i32::MIN, 1200, 800, &monitors));
        assert!(!super::rect_visible_on_monitors(i32::MAX, i32::MAX, 1200, 800, &monitors));
        // Unreadable/empty monitor list does not spuriously reject.
        assert!(super::rect_visible_on_monitors(100, 100, 1200, 800, &[]));
    }

    #[test]
    fn mixed_dpi_logical_visibility_handles_retina_and_standard_monitors() {
        use crate::state::WindowGeometry;
        let g = |x, y, width, height| WindowGeometry { x, y, width, height, physical_position: false, scale_factor: None };

        // macOS/Linux: 2x Retina primary (logical 0..1512, tao reports 0..3024) +
        // 1x external right (logical 1512..4072, tao reports 1512..4072).
        let retina_left = [(0, 0, 3024, 1964, 2.0), (1512, 0, 2560, 1440, 1.0)];
        assert!(super::geometry_visible_logical(&g(2200.0, 100.0, 1200.0, 800.0), &retina_left));
        assert!(super::geometry_visible_logical(&g(100.0, 100.0, 1200.0, 800.0), &retina_left));
        assert!(!super::geometry_visible_logical(&g(4500.0, 100.0, 1200.0, 800.0), &retina_left));

        // Reverse order: 1x primary (logical 0..1920) + 2x Retina right (logical 1920..3432,
        // tao reports 3840..6864). Must not reject windows in the logical [1920, 3432) range.
        let retina_right = [(0, 0, 1920, 1080, 1.0), (3840, 0, 3024, 1964, 2.0)];
        assert!(super::geometry_visible_logical(&g(2000.0, 100.0, 1200.0, 800.0), &retina_right));

        // 1x external on the left (logical -1920..0) + 2x Retina primary (logical 0..1512,
        // tao reports 0..3024). A stale rect at logical x=1800 is off-screen past 1512 and
        // must be rejected even though 1800 < 3024 in raw tao pixels.
        let external_left = [(-1920, 0, 1920, 1080, 1.0), (0, 0, 3024, 1964, 2.0)];
        assert!(!super::geometry_visible_logical(&g(1800.0, 100.0, 1200.0, 800.0), &external_left));
        assert!(super::geometry_visible_logical(&g(-1800.0, 100.0, 1200.0, 800.0), &external_left));

        // Cross-platform Windows backup (`physical_position: true`) must never be placed as
        // macOS/Linux logical coordinates, even when the monitors list is empty.
        let win_backup = WindowGeometry {
            x: 1400.0,
            y: 400.0,
            width: 1200.0,
            height: 800.0,
            physical_position: true,
            scale_factor: Some(2.0),
        };
        assert!(!super::geometry_visible_logical(&win_backup, &retina_left));
        assert!(!super::geometry_visible_logical(&win_backup, &[]));
    }

    #[test]
    fn windows_mixed_dpi_save_and_restore_preserves_exact_physical_origin_and_footprint() {
        use crate::state::WindowGeometry;

        // Case 1: Primary 4K@200% [0..3840) + secondary 1080p@100% [3840..5760).
        // Window saved on 1080p@100% at physical (4000, 100), inner size 1200x800.
        let monitors_4k_primary = [(0, 0, 3840, 2160, 2.0), (3840, 0, 1920, 1080, 1.0)];
        let saved = super::build_window_geometry(4000, 100, 1200, 800, 1.0, true);
        assert!(saved.physical_position);
        assert_eq!(saved.scale_factor, Some(1.0));
        assert_eq!((saved.x, saved.y, saved.width, saved.height), (4000.0, 100.0, 1200.0, 800.0));
        // Restoring from Primary 4K@200% (window_scale = 2.0) keeps (4000, 100) and 1200x800 physical!
        assert!(super::geometry_visible_physical(&saved, 2.0, &monitors_4k_primary));
        assert_eq!(super::resolve_physical_position(&saved, 2.0, true), (4000, 100));
        assert_eq!(super::windows_target_physical_size(&saved, 4000, 100, 2.0, &monitors_4k_primary), (1200, 800));
        // Unplugging secondary monitor rejects the stale (4000, 100) rect.
        assert!(!super::geometry_visible_physical(&saved, 2.0, &monitors_4k_primary[..1]));

        // Case 2: Primary 1080p@100% [0..1920) + secondary 4K@200% [1920..5760).
        // Window saved on 4K@200% at physical (2000, 100), physical inner size 2400x1600.
        let monitors_1080p_primary = [(0, 0, 1920, 1080, 1.0), (1920, 0, 3840, 2160, 2.0)];
        let saved_4k = super::build_window_geometry(2000, 100, 2400, 1600, 2.0, true);
        assert!(saved_4k.physical_position);
        assert_eq!(saved_4k.scale_factor, Some(2.0));
        assert_eq!((saved_4k.x, saved_4k.y, saved_4k.width, saved_4k.height), (2000.0, 100.0, 1200.0, 800.0));
        // Restoring from Primary 1080p@100% (window_scale = 1.0) lands at (2000, 100) with 2400x1600 physical!
        assert!(super::geometry_visible_physical(&saved_4k, 1.0, &monitors_1080p_primary));
        assert_eq!(super::resolve_physical_position(&saved_4k, 1.0, true), (2000, 100));
        assert_eq!(super::windows_target_physical_size(&saved_4k, 2000, 100, 1.0, &monitors_1080p_primary), (2400, 1600));

        // Case 3: Lower-DPI window near/straddling higher-DPI neighbor keeps its exact 1x footprint
        // (never misclassified as 2x) while a 2x-saved straddling window keeps its 2x footprint.
        let near_edge_1x = super::build_window_geometry(720, 750, 1200, 800, 1.0, true);
        assert_eq!(super::windows_target_physical_size(&near_edge_1x, 720, 750, 1.0, &monitors_1080p_primary), (1200, 800));
        let straddle_1x = super::build_window_geometry(1200, 100, 1200, 800, 1.0, true);
        assert_eq!(super::windows_target_physical_size(&straddle_1x, 1200, 100, 2.0, &monitors_1080p_primary), (1200, 800));
        let straddle_2x = super::build_window_geometry(1500, 100, 2400, 1600, 2.0, true);
        assert_eq!(super::windows_target_physical_size(&straddle_2x, 1500, 100, 1.0, &monitors_1080p_primary), (2400, 1600));

        // Case 4: Secondary 1080p@100% on LEFT [-1920..0) or sandwiched between 4K@300% monitors:
        // pre-sizing uses the saved 1x physical size (1200x800) so moving to (-1400, 100) lands
        // 100% inside [-1920..0) with zero overlap on neighboring 4K monitors.
        let sandwiched = [(-5760, 0, 3840, 2160, 3.0), (-1920, 0, 1920, 1080, 1.0), (0, 0, 3840, 2160, 3.0)];
        let saved_mid = super::build_window_geometry(-1400, 100, 1200, 800, 1.0, true);
        assert_eq!(super::windows_target_physical_size(&saved_mid, -1400, 100, 3.0, &sandwiched), (1200, 800));
        // Fallback when `physical_position: true` lacks `scale_factor` picks the monitor containing
        // the interior of the window — including DWM-bordered snapped/maximized origins like (-1927, 0).
        let no_scale = WindowGeometry { x: -1400.0, y: 100.0, width: 1200.0, height: 800.0, physical_position: true, scale_factor: None };
        assert_eq!(super::windows_target_physical_size(&no_scale, -1400, 100, 3.0, &sandwiched), (1200, 800));
        let dwm_snapped = WindowGeometry { x: -1927.0, y: 0.0, width: 1200.0, height: 800.0, physical_position: true, scale_factor: None };
        assert_eq!(super::windows_target_physical_size(&dwm_snapped, -1927, 0, 3.0, &sandwiched), (1200, 800));
        let dwm_maximized = WindowGeometry { x: -24.0, y: -24.0, width: 1200.0, height: 800.0, physical_position: true, scale_factor: None };
        assert_eq!(super::windows_target_physical_size(&dwm_maximized, -24, -24, 1.0, &sandwiched), (3600, 2400));

        // Stale higher-DPI `scale_factor` must not fool `geometry_visible_physical` into accepting
        // a rect that shrinks completely off-screen at the connected monitor's lower current DPI.
        let stale_2x_left = WindowGeometry {
            x: -1500.0,
            y: 100.0,
            width: 1200.0,
            height: 800.0,
            physical_position: true,
            scale_factor: Some(2.0),
        };
        assert!(!super::geometry_visible_physical(&stale_2x_left, 1.0, &[(0, 0, 1920, 1080, 1.0)]));

        // Case 5: Legacy pre-fix JSON (no `physical_position` / `scale_factor`) deserializes cleanly
        // and preserves single-DPI restore via window_scale.
        let legacy: WindowGeometry = serde_json::from_str(
            r#"{"x":100.0,"y":50.0,"width":1200.0,"height":800.0}"#
        ).expect("legacy WindowGeometry deserializes");
        assert!(!legacy.physical_position);
        assert_eq!(legacy.scale_factor, None);
        assert_eq!(super::resolve_physical_position(&legacy, 1.5, true), (150, 75));
        assert!(super::geometry_visible_physical(&legacy, 1.5, &[(0, 0, 2880, 1620, 1.5)]));
        // Serializing logical geometry omits both Windows-only fields (no JSON bloat on macOS/Linux).
        let logical_json = serde_json::to_string(&legacy).unwrap();
        assert!(!logical_json.contains("physical_position"));
        assert!(!logical_json.contains("scale_factor"));
        // Serializing Windows physical geometry preserves both fields.
        let physical_json = serde_json::to_string(&saved).unwrap();
        assert!(physical_json.contains("\"physical_position\":true"));
        assert!(physical_json.contains("\"scale_factor\":1.0"));
    }

    #[test]
    fn window_data_geometry_readers_filter_out_invalid_entries() {
        use crate::state::{WindowData, WindowGeometry};
        let mut win = WindowData::new("main".into());
        win.window_geometry.insert("1".into(), WindowGeometry { x: -32000.0, y: -32000.0, width: 0.0, height: 0.0, physical_position: true, scale_factor: Some(1.0) });
        win.last_geometry_monitors = Some(1);
        assert!(win.geometry_for(1).is_none());
        assert!(win.last_geometry().is_none());

        win.window_geometry.insert("2".into(), WindowGeometry { x: 100.0, y: 100.0, width: 1200.0, height: 800.0, physical_position: false, scale_factor: None });
        assert!(win.geometry_for(2).is_some());
        // Sole valid entry serves as fallback when last_geometry_monitors points to an invalid one.
        assert_eq!(win.last_geometry().map(|g| (g.width, g.height)), Some((1200.0, 800.0)));
    }
}
