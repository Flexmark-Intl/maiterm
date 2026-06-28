//! Frontend-facing commands for the maiLink mobile companion (docs/mailink-protocol.md).

use std::sync::Arc;

use tauri::State;

use crate::state::AppState;

/// Mint a one-time pairing code and return the QR payload the Preferences UI displays for a
/// phone to scan: `{ v, host, port, fp, code, name }`. Errors if the bridge isn't enabled/up.
#[tauri::command]
pub fn mailink_create_pairing(
    state: State<'_, Arc<AppState>>,
) -> Result<serde_json::Value, String> {
    crate::mailink::create_pairing(&state)
}
