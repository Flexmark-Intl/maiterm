//! maiLink §14 — which account a chat is running as (docs/mailink-protocol.md).
//!
//! Three things live here: the per-chat `account` field (served from the spawn record in
//! `accounts::tab_record`, never inferred), the account list the phone can switch between, and
//! the remote-state notes the §6 handoff writes — including the one doorbell this feature rings.

use std::sync::Arc;

use serde_json::{json, Value};
use tauri::Emitter;

use crate::accounts::tab_record::{self, RemoteRecord, RemoteState};
use crate::state::AppState;

/// §14.5: observations this soon after start are session restore respawning every tab. A restart
/// would otherwise make each restored SSH tab on a host with a dead token look newly failed and
/// ring once per tab — the 08a1289 lesson, where in-memory transition state emptied by a restart
/// turned every resumed tab into an edge.
const RING_BASELINE_WINDOW: std::time::Duration = std::time::Duration::from_secs(180);

/// Where this tab's agent runs right now, for §14.
///
/// Remote-ness comes from the same foreground probe the rest of the app uses (which works on
/// Windows too, through its sysinfo tree walk), or the bridge's tunnel — not the tunnel alone,
/// which a typed ssh with the bridge off never gets and would then be served the LOCAL shell's
/// account. The (pid, argv) pair used for binding is a separate, Unix-only probe; where it is
/// missing the tab is still Remote, just with nothing to bind to, which reads as unknown.
fn place(app: &AppState, tab_id: &str) -> tab_record::Place {
    let pty = app.tab_pty_map.read().get(tab_id).cloned();
    let fg = pty
        .as_deref()
        .and_then(|p| crate::pty::get_pty_foreground(app, p, false).ok().flatten());
    let tunnel = app.ssh_tunnels.read().values().any(|t| t.tab_ids.contains(tab_id));
    if fg.is_none() && !tunnel {
        return tab_record::Place::Local;
    }
    let ssh = pty.as_deref().and_then(|p| crate::pty::get_pty_foreground_ssh(app, p, false));
    tab_record::Place::Remote { ssh }
}

/// Whether this tab's agent runs on another host right now.
pub(crate) fn tab_runs_remote(app: &AppState, tab_id: &str) -> bool {
    matches!(place(app, tab_id), tab_record::Place::Remote { .. })
}

/// The wire `account` for one chat. `Value::Null` = known unmanaged (§14.1 rule 1).
///
/// **The process probe is skipped only when nothing CAN be managed**: the record is empty and no
/// account could be handed to a tab right now. This runs per tab on every WS and doorbell tick, and the probe spawns `ps` once
/// its 800 ms cache lapses, so a machine that never turned the feature on pays nothing. Gating on
/// the record alone was wrong: an empty record over SSH is `{known:false}`, not `null`.
pub(crate) fn chat_account(app: &AppState, tab_id: &str, runtime_slug: &str) -> Value {
    let record_empty = match app.tab_accounts.read().get(tab_id) {
        None => return json!({ "known": false }),
        Some(r) => r.local.is_empty() && r.remote.is_none(),
    };
    // Nothing to describe AND nothing that could be handed out now (feature off, or no supported
    // runtime with an active account — every Windows machine today): `null`, no probe.
    if record_empty && crate::accounts::active_accounts(&app.app_data.read().preferences).is_empty() {
        return Value::Null;
    }
    let place = place(app, tab_id);
    let mut records = app.tab_accounts.write();
    let data = app.app_data.read();
    tab_record::wire(records.get_mut(tab_id), runtime_slug, place, &data.preferences)
        .unwrap_or(Value::Null)
}

/// Bind this tab's fresh remote record to the ssh process holding its terminal RIGHT NOW — for
/// the handoff paths that type the fragment into an ssh already running (the bridge's typed-ssh
/// path, the manual inject), where the argv carries no handoff path to recognise it by. Fresh
/// probe: this is an edge, and a cached snapshot could predate the ssh.
pub(crate) fn bind_remote_now(app: &AppState, tab_id: &str) {
    let pty = app.tab_pty_map.read().get(tab_id).cloned();
    let Some((pid, _)) = pty.and_then(|p| crate::pty::get_pty_foreground_ssh(app, &p, true)) else {
        return;
    };
    if let Some(remote) = app.tab_accounts.write().get_mut(tab_id).and_then(|r| r.remote.as_mut()) {
        remote.ssh_pid = Some(pid);
    }
}

/// `GET /accounts` and the WS `accounts` frame — §14.3 `AccountsSnapshot`.
pub(crate) fn snapshot(app: &AppState) -> Value {
    let data = app.app_data.read();
    let p = &data.preferences;
    let accounts: Vec<Value> = p
        .managed_accounts
        .iter()
        .map(|a| {
            let mut v = json!({
                "id": a.id,
                "runtime": a.runtime,
                "label": a.label,
                "active": p.active_account_ids.get(&a.runtime) == Some(&a.id),
            });
            if let Some(org) = &a.org_name {
                v["org"] = json!(org);
            }
            if let Some(plan) = &a.plan {
                v["plan"] = json!(plan);
            }
            v
        })
        .collect();
    json!({ "enabled": p.accounts_setup_complete && p.accounts_enabled, "accounts": accounts })
}

/// `POST /accounts/active` — switch what NEW tabs launch as. §13.4 answer shape.
///
/// Done here rather than through the webview: it is a preference write, which Rust already
/// performs directly for MCP `setPreference`, broadcasting `preferences-changed` so every window's
/// store follows. So the answer is always `confirmed` — a sleeping desktop screen cannot leave it
/// "sent, not confirmed". It respawns nothing (§14.3): a phone restarting tabs it cannot see
/// kills agents mid-turn.
pub(crate) fn set_active(
    app: &Arc<AppState>,
    handle: Option<&tauri::AppHandle>,
    runtime: &str,
    account_id: &str,
) -> Value {
    let refuse = |reason: &str| json!({ "accepted": false, "confirmed": true, "reason": reason });
    let data_clone = {
        let mut data = app.app_data.write();
        let p = &mut data.preferences;
        if !p.accounts_setup_complete || !p.accounts_enabled {
            return refuse("disabled");
        }
        let Some(account) = p.managed_accounts.iter().find(|a| a.id == account_id) else {
            return refuse("unknown_account");
        };
        if account.runtime != runtime {
            return refuse("runtime_mismatch");
        }
        let previous = p.active_account_ids.insert(runtime.to_string(), account_id.to_string());
        (data.clone(), previous)
    };
    let (data_clone, previous) = data_clone;
    if let Err(e) = crate::state::save_state(&data_clone) {
        // **Rolled back, not left live.** `spawn_pty` reads this in-memory value, so a switch the
        // phone was told did not happen would still launch every new tab under the new account —
        // while the desktop pane (which never got the event) and the phone both said otherwise,
        // until the next frontend save silently reverted it. That is §6.1's failure made locally.
        log::warn!("[maiLink] account switch not saved, rolling back: {e}");
        let mut data = app.app_data.write();
        let ids = &mut data.preferences.active_account_ids;
        // Only undo OUR write. If something else set this runtime while the save ran (the desktop
        // pane's whole-object replace), putting `previous` back would clobber that instead.
        if ids.get(runtime).map(String::as_str) != Some(account_id) {
            return refuse("not_saved");
        }
        match previous {
            Some(prev) => {
                ids.insert(runtime.to_string(), prev);
            }
            None => {
                ids.remove(runtime);
            }
        }
        return refuse("not_saved");
    }
    if let Some(h) = handle {
        let _ = h.emit("preferences-changed", &data_clone.preferences);
    }
    json!({
        "accepted": true,
        "confirmed": true,
        "result": { "tabsStillOnPrevious": tabs_off_active(app, runtime) },
    })
}

/// The tabs the phone will now see as `stale` and running locally — counted with the SAME
/// predicate `wire` serves, over the tabs the phone can see, so "N other tabs still on X" matches
/// the badges it renders. An SSH tab's account follows its next connect, so it is not counted.
fn tabs_off_active(app: &AppState, runtime: &str) -> usize {
    super::designated_tabs(app)
        .into_iter()
        .filter(|t| t.runtime.as_key() == runtime)
        .filter(|t| app.tab_pty_map.read().contains_key(&t.tab_id))
        .map(|t| chat_account(app, &t.tab_id, runtime))
        .filter(|v| v["stale"] == json!(true) && v.get("remote").is_none())
        .count()
}

/// Record how far an SSH handoff got (written by the §6 commands). Queues the `account` doorbell
/// on a change INTO `not_applied`, outside the restart window.
pub(crate) fn note_remote(app: &AppState, tab_id: &str, remote: RemoteRecord) {
    let entering_failure = {
        let mut records = app.tab_accounts.write();
        let entry = records.entry(tab_id.to_string()).or_default();
        let was = entry.remote.as_ref().map(|r| r.state);
        let now = remote.state;
        entry.remote = Some(remote);
        now == RemoteState::NotApplied && was != Some(RemoteState::NotApplied)
    };
    if entering_failure && app.started_at.elapsed() >= RING_BASELINE_WINDOW {
        queue_ring(app, tab_id);
    }
}

/// The handoff errored before it could say how far it got: whatever an earlier session recorded
/// no longer describes this one.
pub(crate) fn clear_remote(app: &AppState, tab_id: &str) {
    if let Some(r) = app.tab_accounts.write().get_mut(tab_id) {
        r.remote = None;
    }
}

/// A prepared handoff that the session never used: whatever was recorded, this session is not
/// running as the account.
pub(crate) fn downgrade_remote(app: &AppState, tab_id: &str, reason: &str) {
    let current = app.tab_accounts.read().get(tab_id).and_then(|r| r.remote.clone());
    let account = current.and_then(|r| r.account);
    note_remote(
        app,
        tab_id,
        RemoteRecord::new(account, RemoteState::NotApplied, Some(reason.to_string())),
    );
}

/// Content-light by construction (§14.5): the queue carries the tab id and its NAME, and the
/// doorbell sends only those plus the kind. Never the reason or the account label — the push
/// crosses a public relay onto a lock screen.
fn queue_ring(app: &AppState, tab_id: &str) {
    // Only for tabs the phone can see: an excluded tab must not ring, which would also leak that
    // it exists.
    if app.mailink_info.read().is_none() || !super::is_designated(app, tab_id) {
        return;
    }
    // The handoff carries a CLAUDE token. A Codex or Gemini chat over the same ssh is not
    // running as that account either way, so its failure is not this chat's news.
    let title = {
        let data = app.app_data.read();
        data.windows
            .iter()
            .flat_map(|w| &w.workspaces)
            .flat_map(|ws| &ws.panes)
            .flat_map(|p| &p.tabs)
            .find(|t| t.id == tab_id)
            .filter(|t| t.runtime.map_or(true, |r| r == crate::state::AgentRuntime::Claude))
            .map(|t| t.name.clone())
    };
    let Some(title) = title else { return };
    let mut q = app.mailink_pending_rings.lock();
    q.push((tab_id.to_string(), title, "account"));
    if q.len() > super::overlord::MAX_PENDING_RINGS {
        let drop = q.len() - super::overlord::MAX_PENDING_RINGS;
        q.drain(..drop);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::workspace::{WindowData, Workspace};

    fn failed(reason: &str) -> RemoteRecord {
        RemoteRecord::new(None, RemoteState::NotApplied, Some(reason.into()))
    }

    fn sent() -> RemoteRecord {
        RemoteRecord::new(None, RemoteState::SentUnverified, None)
    }

    /// maiLink running, one window with one tab named "worker". `aged` puts the app past the
    /// restart window.
    fn fixture(aged: bool) -> (AppState, String) {
        let mut app = AppState::new();
        if aged {
            app.started_at = std::time::Instant::now() - RING_BASELINE_WINDOW * 2;
        }
        *app.mailink_info.write() = Some(("test".into(), 1));
        let tab = {
            let mut data = app.app_data.write();
            data.preferences.mailink_expose_all = true;
            let mut win = WindowData::new("main".into());
            let mut ws = Workspace::new("Proj".into());
            ws.panes[0].tabs[0].name = "worker".into();
            // An agent tab, so maiLink exposes it — the ring is gated on that.
            ws.panes[0].tabs[0].runtime = Some(crate::state::AgentRuntime::Claude);
            let id = ws.panes[0].tabs[0].id.clone();
            win.workspaces.push(ws);
            data.windows.push(win);
            id
        };
        (app, tab)
    }

    #[test]
    fn nothing_rings_inside_the_restart_window_but_the_state_still_says_so() {
        let (app, tab) = fixture(false);
        note_remote(&app, &tab, failed("token expired"));
        assert!(app.mailink_pending_rings.lock().is_empty());
        assert_eq!(app.tab_accounts.read()[&tab].remote.as_ref().unwrap().state, RemoteState::NotApplied);
    }

    #[test]
    fn entering_not_applied_rings_once_content_light() {
        let (app, tab) = fixture(true);
        note_remote(&app, &tab, sent());
        assert!(app.mailink_pending_rings.lock().is_empty(), "sent_unverified is not a failure");
        note_remote(&app, &tab, failed("ssh refused the key"));
        note_remote(&app, &tab, failed("again"));
        let q = app.mailink_pending_rings.lock();
        assert_eq!(q.len(), 1, "a repeat failure is not a transition");
        // Only the tab name and the kind — never the reason, never an account label.
        assert_eq!(q[0], (tab.clone(), "worker".to_string(), "account"));
    }

    #[test]
    fn a_tab_the_phone_cannot_see_never_rings() {
        let (app, tab) = fixture(true);
        app.app_data.write().windows[0].workspaces[0].panes[0].tabs[0].mailink_excluded = true;
        note_remote(&app, &tab, failed("x"));
        assert!(app.mailink_pending_rings.lock().is_empty());
    }

    #[test]
    fn an_unused_prep_is_downgraded_and_keeps_its_intended_account() {
        let (app, tab) = fixture(false);
        let account = Some(tab_record::AccountRef { id: "a".into(), label: "a@example.com".into() });
        note_remote(&app, &tab, RemoteRecord::new(account.clone(), RemoteState::SentUnverified, None));
        downgrade_remote(&app, &tab, "prepared but not used");
        let r = app.tab_accounts.read()[&tab].remote.clone().unwrap();
        assert_eq!(r.state, RemoteState::NotApplied);
        assert_eq!(r.account, account);
    }
}
