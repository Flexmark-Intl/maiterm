use serde::Serialize;

use crate::comms::mattermost::MattermostClient;

#[derive(Serialize)]
pub struct CommsTestResult {
    pub ok: bool,
    pub bot_username: String,
}

/// Test a comms (Mattermost) server URL + bot token pair. Takes the values as
/// arguments — not from saved preferences — so the Preferences UI can test
/// before the user commits them.
#[tauri::command]
pub async fn comms_test_connection(
    server_url: String,
    bot_token: String,
) -> Result<CommsTestResult, String> {
    if server_url.trim().is_empty() || bot_token.trim().is_empty() {
        return Err("enter a server URL and bot token first".to_string());
    }
    let client = MattermostClient::new(&server_url, &bot_token, reqwest::Client::new());
    let me = client.me().await.map_err(|e| e.to_string())?;
    Ok(CommsTestResult {
        ok: true,
        bot_username: me.username,
    })
}
