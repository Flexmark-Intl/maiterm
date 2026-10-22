//! The agents' browser tools (docs/browser-link.md §4). Each acts on the CALLER's links only: the
//! agent tab comes from the connection's stated identity (`OWN_TAB_ONLY_TOOLS`), and the optional
//! `tab` argument is a handle among that agent's own links, never a Chrome tab id.

use serde_json::Value;
use std::sync::Arc;
use tauri::AppHandle;

use super::{command, connected, current_handle, links_of, pick, prune, tab_name, take_news, Link};
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

/// Hand the agent what changed about its linked tabs since its last call, on whatever it gets back.
fn with_news(mut v: Value, news: Vec<String>) -> Value {
    if news.is_empty() {
        return v;
    }
    if let Some(content) = v.get_mut("__mcp_content").and_then(|c| c.as_array_mut()) {
        content.insert(0, serde_json::json!({ "type": "text", "text": format!("Your linked tabs changed:\n- {}", news.join("\n- ")) }));
    } else if let Some(o) = v.as_object_mut() {
        o.insert("linked_tabs_changed".into(), serde_json::json!(news));
    }
    v
}

/// A ref names its tab (`b1:e42`, as browserSnapshot writes them), so a ref from one page can't
/// land on another when the agent's current tab has moved. A bare `e42` is taken as before.
fn split_ref(r: &str) -> (Option<&str>, &str) {
    match r.split_once(':') {
        Some((tab, rest)) if tab.starts_with('b') && tab[1..].chars().all(|c| c.is_ascii_digit()) => (Some(tab), rest),
        _ => (None, r),
    }
}

fn tab_line(l: &Link) -> String {
    format!("Tab {}: {} — {}", l.handle, l.title, l.url)
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
    let news = take_news(tab_id);
    let handle = arguments.get("tab").and_then(|v| v.as_str()).map(str::trim).filter(|s| !s.is_empty());

    if tool == "browserStatus" {
        let links = links_of(tab_id);
        if links.is_empty() {
            let v = serde_json::json!({ "linked": false, "extension_connected": connected(), "how": not_linked(state, tab_id) });
            return with_news(v, news);
        }
        let current = current_handle(tab_id);
        let tabs: Vec<Value> = links
            .iter()
            .map(|l| {
                serde_json::json!({
                    "tab": l.handle,
                    "title": l.title,
                    "url": l.url,
                    "browser": l.browser,
                    "opened_by": l.opened_by,
                    "current": current.as_deref() == Some(l.handle.as_str()),
                })
            })
            .collect();
        let v = serde_json::json!({
            "linked": true,
            "tabs": tabs,
            "note": "A browser tool acts on the tab you name with tab, which then becomes current, or else on the current one. A tab your human links later, or one a page opens, doesn't become current until you name it. Refs from browserSnapshot carry their tab (b1:e42), so a click or type with one goes to that tab.",
        });
        return with_news(v, news);
    }

    let Some(action) = action_for(tool) else {
        return error(format!("unknown browser tool {tool}"));
    };
    let raw_ref = arguments.get("ref").and_then(|v| v.as_str());
    let (ref_tab, bare_ref) = raw_ref.map(split_ref).unwrap_or((None, ""));
    if let (Some(t), Some(r)) = (handle, ref_tab) {
        if t != r {
            return with_news(error(format!("ref {} is from tab {r}, not {t}.", raw_ref.unwrap_or(""))), news);
        }
    }
    let link = match pick(tab_id, handle.or(ref_tab)) {
        None => return with_news(error(not_linked(state, tab_id)), news),
        Some(Err(e)) => return with_news(error(e), news),
        Some(Ok(l)) => l,
    };
    let mut args = arguments.clone();
    if let Some(o) = args.as_object_mut() {
        o.remove("tabId");
        o.remove("tab");
        if raw_ref.is_some() {
            o.insert("ref".into(), Value::String(bare_ref.to_string()));
        }
    }
    let out = match command(&link, action, args).await {
        Err(e) => serde_json::json!({ "tab": link.handle, "error": e }),
        Ok(r) => match action {
            "screenshot" => match r.get("data").and_then(|d| d.as_str()) {
                Some(data) => serde_json::json!({ "__mcp_content": [
                    { "type": "text", "text": tab_line(&link) },
                    { "type": "image", "data": data, "mimeType": r.get("mimeType").and_then(|m| m.as_str()).unwrap_or("image/png") }
                ]}),
                None => error("The browser returned no image."),
            },
            "snapshot" => {
                let head = format!(
                    "Tab: {}\nPage: {}\nURL: {}\n\n",
                    link.handle,
                    r.get("title").and_then(|v| v.as_str()).unwrap_or(""),
                    r.get("url").and_then(|v| v.as_str()).unwrap_or("")
                );
                let body = r.get("text").and_then(|v| v.as_str()).unwrap_or("").replace("[ref=e", &format!("[ref={}:e", link.handle));
                serde_json::json!({ "__mcp_content": [{ "type": "text", "text": format!("{head}{body}") }] })
            }
            _ => match r {
                Value::Object(mut o) => {
                    o.insert("tab".into(), Value::String(link.handle.clone()));
                    Value::Object(o)
                }
                other => serde_json::json!({ "tab": link.handle, "result": other }),
            },
        },
    };
    with_news(out, news)
}
