//! The agents' browser tools (docs/browser-link.md §4). Each acts on the CALLER's link only: the
//! tab comes from the connection's stated identity (`OWN_TAB_ONLY_TOOLS`), and no argument names
//! a Chrome tab.

use serde_json::Value;
use std::sync::Arc;
use tauri::AppHandle;

use super::{command, connected, link_for, prune, tab_name};
use crate::state::AppState;

/// The tool → extension action table. `browserStatus` never reaches the extension.
fn action_for(tool: &str) -> Option<&'static str> {
    Some(match tool {
        "browserNavigate" => "navigate",
        "browserSnapshot" => "snapshot",
        "browserClick" => "click",
        "browserType" => "type",
        "browserPressKey" => "press",
        "browserScreenshot" => "screenshot",
        "browserEval" => "eval",
        "browserConsole" => "console",
        "browserNetwork" => "network",
        _ => return None,
    })
}

pub fn is_browser_tool(tool: &str) -> bool {
    tool == "browserStatus" || action_for(tool).is_some()
}

/// What an agent without a usable link is told: the step only its human can take.
fn not_linked(state: &Arc<AppState>, tab_id: &str) -> String {
    let name = tab_name(state, tab_id).unwrap_or_else(|| "this tab".into());
    if !connected() {
        return "The maiTerm Chrome extension isn't connected. Ask your human to install it \
                (maiTerm → Preferences → Chrome extension) and make sure Chrome is open."
            .into();
    }
    format!(
        "Your tab isn't linked to a Chrome tab. Ask your human to open the page in Chrome, click \
         the maiTerm extension's icon, and choose \u{201C}{name}\u{201D}. Only they can link one."
    )
}

fn error(msg: impl Into<String>) -> Value {
    serde_json::json!({ "error": msg.into() })
}

/// Run a browser tool. Results in `__mcp_content` are passed through as the MCP content array
/// (an image, or plain text that would read badly JSON-escaped).
pub async fn handle_tool(tool: &str, arguments: &Value, state: &Arc<AppState>, app: &AppHandle) -> Value {
    let Some(tab_id) = arguments.get("tabId").and_then(|v| v.as_str()).filter(|s| !s.is_empty()) else {
        return error("tabId is required: run /maiterm init, then call this without one.");
    };
    // The tool list is cached by the agent, so a tool can outlive the switch being turned off.
    if !super::enabled(state) {
        return error("The browser link is turned off in maiTerm. Only your human can turn it on (Preferences → Chrome extension).");
    }
    prune(state, app);
    let link = link_for(tab_id);

    if tool == "browserStatus" {
        return match link {
            Some(l) => serde_json::json!({
                "linked": true,
                "browser": l.browser,
                "title": l.title,
                "url": l.url,
                "note": "browserSnapshot reads the page; its [ref=…] values are what browserClick and browserType take.",
            }),
            None => serde_json::json!({ "linked": false, "extension_connected": connected(), "how": not_linked(state, tab_id) }),
        };
    }

    let Some(action) = action_for(tool) else {
        return error(format!("unknown browser tool {tool}"));
    };
    let Some(link) = link else {
        return error(not_linked(state, tab_id));
    };
    let mut args = arguments.clone();
    if let Some(o) = args.as_object_mut() {
        o.remove("tabId");
    }
    match command(&link, action, args).await {
        Err(e) => error(e),
        Ok(r) => match action {
            "screenshot" => match r.get("data").and_then(|d| d.as_str()) {
                Some(data) => serde_json::json!({ "__mcp_content": [
                    { "type": "image", "data": data, "mimeType": r.get("mimeType").and_then(|m| m.as_str()).unwrap_or("image/png") }
                ]}),
                None => error("The browser returned no image."),
            },
            "snapshot" => {
                let head = format!(
                    "Page: {}\nURL: {}\n\n",
                    r.get("title").and_then(|v| v.as_str()).unwrap_or(""),
                    r.get("url").and_then(|v| v.as_str()).unwrap_or("")
                );
                let body = r.get("text").and_then(|v| v.as_str()).unwrap_or("");
                serde_json::json!({ "__mcp_content": [{ "type": "text", "text": format!("{head}{body}") }] })
            }
            _ => r,
        },
    }
}
