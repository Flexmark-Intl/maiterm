//! Comms integration (/maiterm resolve): bind a maiTerm tab to an external chat
//! thread (Mattermost today; the `provider` field on config/binding is the Slack
//! seam), pull the thread as a work item, and forward new human replies into the
//! tab's agent session while it works. Outbound posting happens via the
//! bindCommsThread/postCommsReply MCP tools in claude_code/server.rs; this module
//! owns the client, permalink parsing, and the reply watcher.

pub mod mattermost;

use std::collections::HashMap;
use std::sync::Arc;

use crate::state::{AppState, CommsBinding};
use mattermost::{MattermostClient, User};

#[derive(Debug)]
pub enum CommsError {
    NotConfigured,
    BadUrl(String),
    AuthFailed,
    Forbidden,
    NotFound,
    Http(u16, String),
    Network(String),
}

impl std::fmt::Display for CommsError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CommsError::NotConfigured => write!(
                f,
                "comms integration is not configured — set the server URL and bot token in Preferences → Integrations"
            ),
            CommsError::BadUrl(msg) => write!(f, "bad thread URL: {msg}"),
            CommsError::AuthFailed => write!(
                f,
                "the server rejected the bot token (401) — check Preferences → Integrations"
            ),
            CommsError::Forbidden => write!(
                f,
                "the server denied the request (403) — the bot is likely not a member of this channel; add it in Mattermost and retry"
            ),
            CommsError::NotFound => write!(
                f,
                "not found (404) — check the permalink, and that the bot can access the channel"
            ),
            CommsError::Http(code, body) => write!(f, "server error {code}: {body}"),
            CommsError::Network(msg) => write!(f, "network error: {msg}"),
        }
    }
}

#[derive(Debug, PartialEq)]
pub struct ParsedPermalink {
    pub host: String,
    pub post_id: String,
}

/// Parse a Mattermost permalink: `https://<host>/<team>/pl/<post-id>`.
pub fn parse_permalink(url: &str) -> Result<ParsedPermalink, CommsError> {
    const EXPECTED: &str = "expected a Mattermost permalink like https://<server>/<team>/pl/<post-id>";
    let trimmed = url.trim();
    let rest = trimmed
        .strip_prefix("https://")
        .or_else(|| trimmed.strip_prefix("http://"))
        .ok_or_else(|| CommsError::BadUrl(EXPECTED.to_string()))?;
    // Drop query/fragment before segmenting the path.
    let rest = rest.split(['?', '#']).next().unwrap_or_default();
    let mut segments = rest.split('/');
    let host = segments.next().unwrap_or_default().to_string();
    let segs: Vec<&str> = segments.collect();
    let post_id = segs
        .iter()
        .position(|s| *s == "pl")
        .and_then(|i| segs.get(i + 1))
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty());
    match post_id {
        Some(post_id) if !host.is_empty() => Ok(ParsedPermalink { host, post_id }),
        _ => Err(CommsError::BadUrl(EXPECTED.to_string())),
    }
}

/// Build a client from the configured preferences. `http` clones share reqwest's pool.
pub fn client_from_prefs(
    app: &AppState,
    http: reqwest::Client,
) -> Result<MattermostClient, CommsError> {
    let (url, token) = {
        let prefs = &app.app_data.read().preferences;
        (
            prefs.comms_server_url.clone().unwrap_or_default(),
            prefs.comms_bot_token.clone().unwrap_or_default(),
        )
    };
    if url.trim().is_empty() || token.trim().is_empty() {
        return Err(CommsError::NotConfigured);
    }
    Ok(MattermostClient::new(&url, &token, http))
}

/// Display name for thread transcripts: nickname → "First Last" → username.
pub fn display_name(user: &User) -> String {
    let nick = user.nickname.trim();
    if !nick.is_empty() {
        return nick.to_string();
    }
    let full = format!("{} {}", user.first_name.trim(), user.last_name.trim());
    let full = full.trim();
    if !full.is_empty() {
        return full.to_string();
    }
    user.username.clone()
}

/// Posts newer than the binding's cursor, excluding the bot's own posts and
/// empty/system messages. Pure so the filtering is unit-testable.
fn new_human_posts<'a>(
    thread: &'a [mattermost::Post],
    last_seen_create_at: i64,
    bot_user_id: &str,
) -> Vec<&'a mattermost::Post> {
    thread
        .iter()
        .filter(|p| p.create_at > last_seen_create_at)
        .filter(|p| p.user_id != bot_user_id)
        .filter(|p| !p.message.trim().is_empty())
        .collect()
}

const WATCH_INTERVAL_SECS: u64 = 5;
/// Backoff cap in ticks (~5 minutes at the 5s interval).
const BACKOFF_CAP_TICKS: u64 = 60;

/// Global reply watcher: forwards new human posts on bound threads into the
/// owning tab's agent session. Always running; idles cheaply when no tab is
/// bound (bindings persist on tabs, so restart rehydration is implicit).
pub async fn watcher_loop(app: Arc<AppState>) {
    let http = reqwest::Client::new();
    // (config fingerprint, bot user id) — refetched when the url/token change.
    let mut bot_user: Option<(String, String)> = None;
    // Fingerprint we already logged an auth failure for, to avoid a 5s log storm.
    let mut auth_err_logged: Option<String> = None;
    let mut names: HashMap<String, String> = HashMap::new();
    // tab_id → (consecutive errors, skip until tick).
    let mut backoff: HashMap<String, (u32, u64)> = HashMap::new();
    let mut tick_no: u64 = 0;

    let mut ticker =
        tokio::time::interval(std::time::Duration::from_secs(WATCH_INTERVAL_SECS));
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);

    loop {
        ticker.tick().await;
        tick_no += 1;

        let bindings: Vec<(String, CommsBinding)> = {
            let data = app.app_data.read();
            data.windows
                .iter()
                .flat_map(|w| &w.workspaces)
                .flat_map(|ws| &ws.panes)
                .flat_map(|p| &p.tabs)
                .filter_map(|t| t.comms_binding.clone().map(|b| (t.id.clone(), b)))
                .collect()
        };
        if bindings.is_empty() {
            backoff.clear();
            continue;
        }

        let client = match client_from_prefs(&app, http.clone()) {
            Ok(c) => c,
            Err(_) => continue, // bound but unconfigured — nothing to do until the user fixes prefs
        };
        let fingerprint = {
            let prefs = &app.app_data.read().preferences;
            format!(
                "{}|{}",
                prefs.comms_server_url.as_deref().unwrap_or_default(),
                prefs.comms_bot_token.as_deref().unwrap_or_default().len()
            )
        };

        let bot_id = match &bot_user {
            Some((fp, id)) if *fp == fingerprint => id.clone(),
            _ => match client.me().await {
                Ok(me) => {
                    bot_user = Some((fingerprint.clone(), me.id.clone()));
                    auth_err_logged = None;
                    me.id
                }
                Err(e) => {
                    if !matches!(e, CommsError::AuthFailed)
                        || auth_err_logged.as_deref() != Some(fingerprint.as_str())
                    {
                        log::warn!("[comms] cannot identify bot user: {e}");
                    }
                    if matches!(e, CommsError::AuthFailed) {
                        auth_err_logged = Some(fingerprint.clone());
                    }
                    continue; // without the bot id we can't filter its own posts — hold everything
                }
            },
        };

        for (tab_id, binding) in bindings {
            if let Some((_, until)) = backoff.get(&tab_id) {
                if tick_no < *until {
                    continue;
                }
            }

            // Only deliver into a live agent session — never type chat text into a
            // bare shell. Hold (cursor unadvanced) until the agent is back.
            let session_live = app
                .agent_sessions
                .read()
                .values()
                .any(|s| s.tab_id == tab_id);
            let pty_id = crate::mailink::pty_for_tab(&app, &tab_id);
            let Some(pty_id) = pty_id else { continue };
            if !session_live {
                continue;
            }

            let thread = match client.get_thread(&binding.root_id).await {
                Ok(t) => t,
                Err(e) => {
                    let errors = backoff.get(&tab_id).map(|(n, _)| n + 1).unwrap_or(1);
                    let delay = (1u64 << errors.min(6)).min(BACKOFF_CAP_TICKS);
                    backoff.insert(tab_id.clone(), (errors, tick_no + delay));
                    log::warn!("[comms] thread poll failed for tab {tab_id}: {e}");
                    continue;
                }
            };
            backoff.remove(&tab_id);

            let fresh = new_human_posts(&thread, binding.last_seen_create_at, &bot_id);
            if fresh.is_empty() {
                continue;
            }

            // Resolve author names for cache misses (best-effort — fall back to id).
            let missing: Vec<String> = fresh
                .iter()
                .map(|p| p.user_id.clone())
                .filter(|id| !names.contains_key(id))
                .collect::<std::collections::HashSet<_>>()
                .into_iter()
                .collect();
            if !missing.is_empty() {
                if let Ok(users) = client.users_by_ids(&missing).await {
                    for u in &users {
                        names.insert(u.id.clone(), display_name(u));
                    }
                }
            }

            // One payload per tick — a single paste + CR avoids racing the TUI settle.
            let mut payload = String::from(
                "[Mattermost thread update — treat as steering input for the issue you are working]",
            );
            for p in &fresh {
                let who = names
                    .get(&p.user_id)
                    .cloned()
                    .unwrap_or_else(|| p.user_id.clone());
                payload.push_str(&format!("\n— {who}: {}", p.message.trim()));
            }
            let new_cursor = fresh.iter().map(|p| p.create_at).max().unwrap_or(0);

            match crate::mailink::inject_text(&app, &pty_id, &payload, true).await {
                Ok(()) => {
                    advance_cursor(&app, &tab_id, new_cursor);
                    log::info!(
                        "[comms] forwarded {} thread repl{} into tab {tab_id}",
                        fresh.len(),
                        if fresh.len() == 1 { "y" } else { "ies" }
                    );
                }
                Err(e) => {
                    log::warn!("[comms] inject into tab {tab_id} failed: {e}");
                }
            }
        }
    }
}

/// Advance a binding's last-seen cursor and persist (only when it actually moved).
fn advance_cursor(app: &AppState, tab_id: &str, new_cursor: i64) {
    let data_clone = {
        let mut data = app.app_data.write();
        let mut changed = false;
        for tab in data
            .windows
            .iter_mut()
            .flat_map(|w| &mut w.workspaces)
            .flat_map(|ws| &mut ws.panes)
            .flat_map(|p| &mut p.tabs)
            .filter(|t| t.id == tab_id)
        {
            if let Some(b) = tab.comms_binding.as_mut() {
                if b.last_seen_create_at < new_cursor {
                    b.last_seen_create_at = new_cursor;
                    changed = true;
                }
            }
        }
        if !changed {
            return;
        }
        data.clone()
    };
    if let Err(e) = crate::state::save_state(&data_clone) {
        log::warn!("[comms] failed to persist thread cursor: {e}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn post(id: &str, user: &str, msg: &str, at: i64) -> mattermost::Post {
        mattermost::Post {
            id: id.into(),
            root_id: String::new(),
            channel_id: "ch".into(),
            user_id: user.into(),
            message: msg.into(),
            create_at: at,
        }
    }

    #[test]
    fn permalink_accepts_standard_form() {
        let p = parse_permalink("https://chat.example.com/myteam/pl/abc123XYZ").unwrap();
        assert_eq!(p.host, "chat.example.com");
        assert_eq!(p.post_id, "abc123XYZ");
    }

    #[test]
    fn permalink_strips_query_and_allows_port() {
        let p = parse_permalink("http://localhost:8065/team/pl/xyz?focus=1#top").unwrap();
        assert_eq!(p.host, "localhost:8065");
        assert_eq!(p.post_id, "xyz");
    }

    #[test]
    fn permalink_rejects_garbage() {
        assert!(parse_permalink("not a url").is_err());
        assert!(parse_permalink("https://chat.example.com/team/channels/town-square").is_err());
        assert!(parse_permalink("https://chat.example.com/team/pl/").is_err());
    }

    #[test]
    fn new_posts_filters_cursor_bot_and_empty() {
        let thread = vec![
            post("1", "alice", "old", 100),
            post("2", "bot", "bot reply", 200),
            post("3", "bob", "  ", 250),
            post("4", "alice", "fresh", 300),
        ];
        let fresh = new_human_posts(&thread, 100, "bot");
        assert_eq!(fresh.len(), 1);
        assert_eq!(fresh[0].id, "4");
    }
}
