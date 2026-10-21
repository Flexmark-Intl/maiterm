//! Preferences → Chrome extension (docs/browser-link.md §5).

use std::sync::Arc;
use tauri::{AppHandle, State};

use crate::browser_link::{self, install, StatusView};
use crate::state::AppState;

#[tauri::command]
pub fn browser_link_status(state: State<'_, Arc<AppState>>, app: AppHandle) -> StatusView {
    browser_link::status(&state, &app)
}

/// Bring the host manifests in line with the (already saved) preference: written when on,
/// removed when off. The pane calls this after saving the toggle.
#[tauri::command]
pub async fn browser_link_sync(state: State<'_, Arc<AppState>>, app: AppHandle) -> Result<(), String> {
    let enabled = state.app_data.read().preferences.browser_link_enabled;
    if !enabled {
        browser_link::shut_down(&app);
    }
    tauri::async_runtime::spawn_blocking(move || {
        install::sync(enabled);
        if enabled {
            install::refresh_unpacked_copy(&app).map(|_| ())
        } else {
            Ok(())
        }
    })
    .await
    .map_err(|e| e.to_string())?
}

#[tauri::command]
pub fn browser_link_unlink(app: AppHandle, maiterm_tab_id: String) -> bool {
    browser_link::unlink_maiterm_tab(&app, &maiterm_tab_id)
}

/// Open Chrome on its extensions page. A `chrome://` URL can't be handed to the OS's URL opener,
/// so Chrome is launched with it. False when that didn't work; the pane then says where to go.
#[tauri::command]
pub async fn browser_link_open_extensions_page() -> bool {
    tauri::async_runtime::spawn_blocking(|| {
        #[cfg(target_os = "macos")]
        let opened = std::process::Command::new("open")
            .args(["-a", "Google Chrome", "chrome://extensions"])
            .status()
            .is_ok_and(|s| s.success());
        // Launched, not waited on: on Linux the browser process IS the command.
        #[cfg(target_os = "linux")]
        let opened = std::process::Command::new("google-chrome").arg("chrome://extensions").spawn().is_ok();
        #[cfg(windows)]
        let opened = std::process::Command::new("cmd")
            .args(["/C", "start", "chrome", "chrome://extensions"])
            .status()
            .is_ok_and(|s| s.success());
        opened
    })
    .await
    .unwrap_or(false)
}

/// "Load unpacked": refresh the extension folder and return it; the pane reveals it and opens
/// the browser's extensions page.
#[tauri::command]
pub async fn browser_link_prepare_unpacked(app: AppHandle) -> Result<String, String> {
    tauri::async_runtime::spawn_blocking(move || install::refresh_unpacked_copy(&app))
        .await
        .map_err(|e| e.to_string())?
        .map(|p| p.to_string_lossy().into_owned())
}
