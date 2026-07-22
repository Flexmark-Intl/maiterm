//! Frontend-facing commands for the maiLink mobile companion (docs/mailink-protocol.md).

use std::sync::Arc;

use serde_json::{json, Value};
use tauri::State;

use crate::state::{save_state, AppState};

/// Mint a one-time pairing code and return the QR payload the Preferences UI displays for a
/// phone to scan: `{ v, host, port, fp, code, name }`. Errors if the bridge isn't enabled/up.
#[tauri::command]
pub fn mailink_create_pairing(
    state: State<'_, Arc<AppState>>,
) -> Result<serde_json::Value, String> {
    crate::mailink::create_pairing(&state)
}

/// Persist the maiLink bridge enable flag AND start/stop the live LAN listener immediately, so
/// the toggle takes effect without an app restart. On enable, the listener starts and publishes
/// its (fingerprint, port) so pairing works right away; on disable, it is graceful-shutdown.
#[tauri::command]
pub fn mailink_set_enabled(
    app: tauri::AppHandle,
    state: State<'_, Arc<AppState>>,
    enabled: bool,
) -> Result<(), String> {
    let data_clone = {
        let mut app_data = state.app_data.write();
        app_data.preferences.mailink_enabled = enabled;
        app_data.clone()
    };
    save_state(&data_clone)?;
    if enabled {
        crate::mailink::start(&state, app)?;
    } else {
        crate::mailink::shutdown(&state);
    }
    Ok(())
}

/// List paired maiLink devices for the Preferences "Paired devices" list. A sanitized view:
/// the device's bearer-token hash and relay capability are backend-only and never returned.
/// Each entry: `{ id, name, push_platform, push_env, has_push, created_at, last_seen_at }`,
/// where `has_push` means the device registered both a push token and a relay capability (so
/// the doorbell can ring it).
#[tauri::command]
pub fn mailink_list_devices(state: State<'_, Arc<AppState>>) -> Vec<Value> {
    state
        .app_data
        .read()
        .preferences
        .mailink_devices
        .iter()
        .map(|d| {
            json!({
                "id": d.id,
                "name": d.name,
                "push_platform": d.push_platform,
                "push_env": d.push_env,
                "has_push": d.push_token.is_some() && d.push_cap.is_some(),
                "created_at": d.created_at,
                "last_seen_at": d.last_seen_at,
            })
        })
        .collect()
}

/// Unpair a device: drop its record so its bearer token is no longer accepted on the LAN
/// bridge and the doorbell stops ringing it. Idempotent — removing an unknown id is a no-op
/// success. An already-open WebSocket stays live until it next makes an authed request; new
/// connections are rejected immediately.
#[tauri::command]
pub fn mailink_remove_device(
    state: State<'_, Arc<AppState>>,
    device_id: String,
) -> Result<(), String> {
    let data_clone = {
        let mut app_data = state.app_data.write();
        let before = app_data.preferences.mailink_devices.len();
        app_data
            .preferences
            .mailink_devices
            .retain(|d| d.id != device_id);
        if app_data.preferences.mailink_devices.len() == before {
            return Ok(()); // unknown id — nothing to persist
        }
        app_data.clone()
    };
    save_state(&data_clone)?;
    log::info!("[maiLink] removed device {device_id}");
    Ok(())
}
