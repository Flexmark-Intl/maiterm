//! The extension's WebSocket (`/browser-link`, docs/browser-link.md §3).

use axum::extract::ws::{Message as WsMessage, WebSocket, WebSocketUpgrade};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use futures_util::{SinkExt, StreamExt};
use serde_json::Value;
use std::sync::Arc;
use std::time::Duration;
use tauri::AppHandle;
use tokio::sync::mpsc;

use super::{
    drop_others_for_one_tab_extension, emit_changed, front_tab, holds_several, prune, remove_link_by_chrome, reserve_handles, set_link,
    tab_exists, tab_used, tell, token, Conn, Link, ALLOWED_EXTENSION_IDS, PROTOCOL, REG,
};
use crate::state::AppState;

/// How long a fresh connection has to say `hello` before it is dropped.
const HELLO_TIMEOUT: Duration = Duration::from_secs(5);
/// How soon a link to a closed maiTerm tab is dropped (and its Chrome tab detached).
const PRUNE_EVERY: Duration = Duration::from_secs(5);

/// Is this the upgrade's `Origin` one of our extensions? A web page can open a socket to
/// localhost too, but its Origin is its own site, and a browser won't let it forge one.
fn origin_allowed(headers: &HeaderMap) -> bool {
    let Some(origin) = headers.get("origin").and_then(|v| v.to_str().ok()) else {
        return false;
    };
    let Some(id) = origin.strip_prefix("chrome-extension://") else {
        return false;
    };
    ALLOWED_EXTENSION_IDS.contains(&id.trim_end_matches('/'))
}

pub fn upgrade(ws: WebSocketUpgrade, headers: HeaderMap, state: Arc<AppState>, app: AppHandle) -> Response {
    if !origin_allowed(&headers) {
        log::warn!("[browser-link] refused a connection from origin {:?}", headers.get("origin"));
        return StatusCode::FORBIDDEN.into_response();
    }
    ws.on_upgrade(move |socket| run(socket, state, app))
}

fn reply(id: &Value, result: Result<Value, String>) -> String {
    match result {
        Ok(r) => serde_json::json!({ "id": id, "result": r }),
        Err(e) => serde_json::json!({ "id": id, "error": e }),
    }
    .to_string()
}

async fn run(socket: WebSocket, state: Arc<AppState>, app: AppHandle) {
    let (mut write, mut read) = socket.split();

    // hello: the token proves the native host gave it to this extension (§2).
    let hello = match tokio::time::timeout(HELLO_TIMEOUT, read.next()).await {
        Ok(Some(Ok(WsMessage::Text(t)))) => serde_json::from_str::<Value>(&t).ok(),
        _ => None,
    };
    let Some(hello) = hello.filter(|h| h.get("method").and_then(|m| m.as_str()) == Some("hello")) else {
        return;
    };
    let hid = hello.get("id").cloned().unwrap_or(Value::Null);
    let p = hello.get("params").cloned().unwrap_or(Value::Null);
    if p.get("token").and_then(|t| t.as_str()) != Some(token()) {
        let _ = write.send(WsMessage::Text(reply(&hid, Err("bad token".into())).into())).await;
        return;
    }
    if !super::enabled(&state) {
        let msg = "The browser link is turned off in maiTerm (Preferences → Chrome extension).";
        let _ = write.send(WsMessage::Text(reply(&hid, Err(msg.into())).into())).await;
        return;
    }
    let their_protocol = p.get("protocol").and_then(|v| v.as_u64()).unwrap_or(0);
    if their_protocol != PROTOCOL {
        let msg = if their_protocol < PROTOCOL {
            "This maiTerm needs a newer maiTerm Chrome extension. Update the extension."
        } else {
            "This extension needs a newer maiTerm. Update maiTerm."
        };
        let _ = write.send(WsMessage::Text(reply(&hid, Err(msg.into())).into())).await;
        return;
    }

    let conn_id = uuid::Uuid::new_v4().to_string();
    let browser = p.get("browser").and_then(|v| v.as_str()).unwrap_or("Chrome").to_string();
    let version = p.get("version").and_then(|v| v.as_str()).unwrap_or("?").to_string();
    let (tx, mut rx) = mpsc::unbounded_channel::<String>();
    REG.lock().conns.insert(conn_id.clone(), Conn { tx, browser: browser.clone(), version: version.clone() });
    log::info!("[browser-link] {browser} extension {version} connected");

    // The links it already holds (it kept them across a service-worker restart or our relaunch).
    // Only for maiTerm tabs that still exist: a link to a closed tab must not come back.
    // A link whose tab was reloaded while we were apart comes back under the new id.
    // The highest handle each agent tab has had, kept by the extension: a restart must not hand a
    // closed tab's handle to a new page while the agent may still have the old one in mind.
    if let Some(floors) = p.get("handleFloors").and_then(|v| v.as_object()) {
        for (id, n) in floors {
            let n = n.as_u64().unwrap_or(0).min(u32::MAX as u64) as u32;
            // Under the id the tab has now; a tab maiTerm can't see now (archived) comes back under
            // the same id, so its floor is kept as it is.
            let now = {
                let reg = REG.lock();
                super::current_id(&state, &reg, id)
            };
            reserve_handles(now.as_deref().unwrap_or(id), n);
        }
    }
    let mut accepted = Vec::new();
    // Beside `accepted`, not in it: an extension from before handles reads `accepted` as ids.
    let mut handles = serde_json::Map::new();
    let mut renamed = Vec::new();
    for l in p.get("links").and_then(|v| v.as_array()).cloned().unwrap_or_default() {
        if let Some(mut link) = parse_link(&l, &conn_id, &browser) {
            if let Some(changed) = super::resolve_announced(&state, &mut link) {
                let Ok(stored) = set_link(link, false) else { continue };
                accepted.push(stored.chrome_tab_id);
                handles.insert(stored.chrome_tab_id.to_string(), Value::String(stored.handle.clone()));
                if changed {
                    renamed.push(stored);
                }
            }
        }
    }
    let welcome = serde_json::json!({
        "instance": super::instance_label(),
        "protocol": PROTOCOL,
        "accepted": accepted,
        "handles": handles,
    });
    if write.send(WsMessage::Text(reply(&hid, Ok(welcome)).into())).await.is_err() {
        drop_conn(&conn_id, &app);
        return;
    }
    {
        let reg = REG.lock();
        for l in &renamed {
            super::notify_relinked(&reg, l, super::tab_name(&state, &l.maiterm_tab_id).as_deref());
        }
    }
    emit_changed(&app);

    let mut prune_tick = tokio::time::interval(PRUNE_EVERY);
    prune_tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        tokio::select! {
            _ = prune_tick.tick() => prune(&state, &app),
            msg = read.next() => match msg {
                Some(Ok(WsMessage::Text(t))) => {
                    let Ok(v) = serde_json::from_str::<Value>(&t) else { continue };
                    if let Some(out) = handle(&v, &conn_id, &browser, &version, &state, &app) {
                        if write.send(WsMessage::Text(out.into())).await.is_err() {
                            break;
                        }
                    }
                }
                Some(Ok(WsMessage::Ping(d))) => {
                    let _ = write.send(WsMessage::Pong(d)).await;
                }
                Some(Ok(WsMessage::Close(_))) | None | Some(Err(_)) => break,
                _ => {}
            },
            out = rx.recv() => match out {
                Some(s) => {
                    if write.send(WsMessage::Text(s.into())).await.is_err() {
                        break;
                    }
                }
                None => break,
            },
        }
    }
    drop_conn(&conn_id, &app);
    log::info!("[browser-link] {browser} extension disconnected");
}

/// Forget a connection: its links (re-announced if it comes back) and its unanswered commands,
/// whose waiters then answer "disconnected".
fn drop_conn(conn_id: &str, app: &AppHandle) {
    let mut reg = REG.lock();
    reg.conns.remove(conn_id);
    reg.links.retain(|l| l.conn_id != conn_id);
    reg.pending.retain(|_, (c, _)| c != conn_id);
    drop(reg);
    emit_changed(app);
}

fn parse_link(v: &Value, conn_id: &str, browser: &str) -> Option<Link> {
    Some(Link {
        maiterm_tab_id: v.get("maitermTabId")?.as_str()?.to_string(),
        chrome_tab_id: v.get("chromeTabId")?.as_i64()?,
        handle: v.get("handle").and_then(|t| t.as_str()).unwrap_or("").to_string(),
        opened_by: v.get("openedBy").and_then(|t| t.as_str()).map(str::to_string),
        conn_id: conn_id.to_string(),
        browser: browser.to_string(),
        title: v.get("title").and_then(|t| t.as_str()).unwrap_or("").to_string(),
        url: v.get("url").and_then(|t| t.as_str()).unwrap_or("").to_string(),
    })
}

/// One frame from the extension. Returns the frame to send back, if any.
fn handle(v: &Value, conn_id: &str, browser: &str, version: &str, state: &Arc<AppState>, app: &AppHandle) -> Option<String> {
    let id = v.get("id").cloned();
    let Some(method) = v.get("method").and_then(|m| m.as_str()) else {
        // An answer to one of our commands.
        let id = id?.as_u64()?;
        let waiter = REG.lock().pending.remove(&id);
        if let Some((_, tx)) = waiter {
            let r = match v.get("error") {
                Some(e) if !e.is_null() => Err(e.as_str().map(str::to_string).unwrap_or_else(|| e.to_string())),
                _ => Ok(v.get("result").cloned().unwrap_or(Value::Null)),
            };
            let _ = tx.send(r);
        }
        return None;
    };
    let params = v.get("params").cloned().unwrap_or(Value::Null);
    let result: Result<Value, String> = match method {
        "ping" => Ok(serde_json::json!({})),
        "listTabs" => Ok(list_tabs(state, app)),
        "link" => {
            let Some(mut link) = parse_link(&params, conn_id, browser) else {
                return id.map(|i| reply(&i, Err("link needs maitermTabId and chromeTabId".into())));
            };
            // A tab a linked tab opened (§1.3): it joins only the opener's own agent.
            let opener = params.get("openerChromeTabId").and_then(|v| v.as_i64());
            let opener_link = opener.and_then(|o| {
                REG.lock().links.iter().find(|l| l.conn_id == conn_id && l.chrome_tab_id == o).cloned()
            });
            link.handle.clear();
            link.opened_by = opener_link.as_ref().map(|l| l.handle.clone());
            if !tab_exists(state, &link.maiterm_tab_id) {
                Err("That maiTerm tab no longer exists.".into())
            } else if opener.is_some() && opener_link.as_ref().is_none_or(|l| l.maiterm_tab_id != link.maiterm_tab_id) {
                Err("The tab that opened this one isn't linked to that agent.".into())
            } else {
                match set_link(link, true) {
                    Err(e) => Err(e),
                    Ok(stored) => {
                        if !holds_several(version) {
                            drop_others_for_one_tab_extension(&stored);
                        }
                        log::info!("[browser-link] {browser} tab {} linked to maiTerm tab {} as {}", stored.chrome_tab_id, stored.maiterm_tab_id, stored.handle);
                        let msg = match &stored.opened_by {
                            Some(by) => format!(
                                "{by} opened a new tab (a popup, or a page it opened); it is linked to you as {}. browserStatus shows where it is.",
                                stored.handle
                            ),
                            None => format!("Your human linked a Chrome tab to you: {}.", stored.describe()),
                        };
                        tell(&stored.maiterm_tab_id, msg);
                        emit_changed(app);
                        Ok(serde_json::json!({ "ok": true, "handle": stored.handle }))
                    }
                }
            }
        }
        "unlink" => {
            let chrome = params.get("chromeTabId").and_then(|v| v.as_i64()).unwrap_or(-1);
            let why = match params.get("reason").and_then(|v| v.as_str()) {
                Some("closed") => "was closed",
                Some("cancelled") => "was unlinked: your human cancelled Chrome's debugging bar",
                Some("moved") => "was linked to an agent in another maiTerm",
                _ => "was unlinked by your human",
            };
            if remove_link_by_chrome(conn_id, chrome, why) {
                emit_changed(app);
            }
            Ok(serde_json::json!({ "ok": true }))
        }
        "tabUpdated" => {
            let chrome = params.get("chromeTabId").and_then(|v| v.as_i64()).unwrap_or(-1);
            let mut reg = REG.lock();
            let mut changed = false;
            for l in reg.links.iter_mut().filter(|l| l.conn_id == conn_id && l.chrome_tab_id == chrome) {
                if let Some(t) = params.get("title").and_then(|v| v.as_str()) {
                    l.title = t.to_string();
                }
                if let Some(u) = params.get("url").and_then(|v| v.as_str()) {
                    l.url = u.to_string();
                }
                changed = true;
            }
            drop(reg);
            if changed {
                emit_changed(app);
            }
            Ok(Value::Null)
        }
        other => Err(format!("unknown method {other}")),
    };
    id.map(|i| reply(&i, result))
}

/// The agent tabs the popup offers to link to, with what each holds now. Most recently used first,
/// and each window's front tab above everything else: the agent the human is looking at is the one
/// they most likely mean.
fn list_tabs(state: &Arc<AppState>, app: &AppHandle) -> Value {
    prune(state, app);
    let links = REG.lock().links.clone();
    let data = state.app_data.read();
    let mut tabs = Vec::new();
    for w in data.windows.iter().filter(|w| w.label != "preferences" && w.label != "help") {
        let front = front_tab(w);
        let window = w.name.clone().unwrap_or_else(|| w.label.clone());
        for ws in &w.workspaces {
            for t in ws.panes.iter().flat_map(|p| p.tabs.iter()) {
                if t.runtime.is_none() || t.service_id.is_some() {
                    continue;
                }
                let linked: Vec<Value> = links
                    .iter()
                    .filter(|l| l.maiterm_tab_id == t.id)
                    .map(|l| serde_json::json!({ "handle": l.handle, "title": l.title, "browser": l.browser }))
                    .collect();
                let is_front = front == Some(t.id.as_str());
                tabs.push((
                    is_front,
                    tab_used(&t.id),
                    serde_json::json!({
                        "id": t.id,
                        "name": t.name,
                        "workspace": ws.name,
                        "window": window,
                        "runtime": t.runtime,
                        "front": is_front,
                        "linked": linked,
                    }),
                ));
            }
        }
    }
    // Stable: tabs never used since launch keep their tree order below the rest.
    tabs.sort_by(|a, b| b.0.cmp(&a.0).then(b.1.cmp(&a.1)));
    let windows = data.windows.iter().filter(|w| w.label != "preferences" && w.label != "help").count();
    serde_json::json!({ "tabs": tabs.into_iter().map(|t| t.2).collect::<Vec<_>>(), "windows": windows })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn origin(v: &str) -> HeaderMap {
        let mut h = HeaderMap::new();
        h.insert("origin", v.parse().unwrap());
        h
    }

    #[test]
    fn only_our_extension_may_connect() {
        assert!(origin_allowed(&origin("chrome-extension://mdaccealpnngffgibmnijfgakddfgjpa")));
        assert!(origin_allowed(&origin("chrome-extension://mdaccealpnngffgibmnijfgakddfgjpa/")));
        assert!(!origin_allowed(&origin("chrome-extension://aaaabbbbccccddddeeeeffffgggghhhh")));
        assert!(!origin_allowed(&origin("https://mdaccealpnngffgibmnijfgakddfgjpa.example")));
        assert!(!origin_allowed(&HeaderMap::new()));
    }
}
