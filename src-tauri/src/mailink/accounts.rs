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

/// Whether this tab's agent runs on another host right now.
///
/// Not `tab_is_ssh` alone: that reads the BRIDGE's tunnel table, and a typed ssh with the bridge
/// off has no tunnel — it would then be served the LOCAL shell's account, which is exactly the
/// wrong-identity claim §14 exists to avoid. The foreground probe is the cached polling variant,
/// the right one for a 2 s summary tick.
pub(crate) fn tab_runs_remote(app: &AppState, tab_id: &str) -> bool {
    if app.ssh_tunnels.read().values().any(|t| t.tab_ids.contains(tab_id)) {
        return true;
    }
    let pty = app.tab_pty_map.read().get(tab_id).cloned();
    pty.and_then(|p| crate::pty::get_pty_foreground(app, &p, false).ok().flatten())
        .is_some()
}

/// The wire `account` for one chat. `Value::Null` = known unmanaged (§14.1 rule 1).
pub(crate) fn chat_account(app: &AppState, tab_id: &str, runtime_slug: &str) -> Value {
    let remote = tab_runs_remote(app, tab_id);
    let records = app.tab_accounts.read();
    let data = app.app_data.read();
    tab_record::wire(records.get(tab_id), runtime_slug, remote, &data.preferences)
        .unwrap_or(Value::Null)
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
        p.active_account_ids.insert(runtime.to_string(), account_id.to_string());
        data.clone()
    };
    if let Err(e) = crate::state::save_state(&data_clone) {
        // The in-memory switch already happened; say the write failed rather than pretend it
        // took. A restart would revert it.
        log::warn!("[maiLink] account switch not saved: {e}");
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

/// Live tabs whose spawn account for `runtime` is no longer the active one — the set a switch
/// just made `stale`. Local tabs only: an SSH tab's account follows its next connect, and counting
/// it here would promise a remote switch that has not happened either.
fn tabs_off_active(app: &Arc<AppState>, runtime: &str) -> usize {
    let live: Vec<String> = app.tab_pty_map.read().keys().cloned().collect();
    let active = app.app_data.read().preferences.active_account_ids.get(runtime).cloned();
    let records = app.tab_accounts.read();
    live.iter()
        .filter_map(|t| records.get(t))
        .filter(|r| r.remote.is_none())
        .filter_map(|r| r.local.get(runtime))
        .filter(|a| Some(&a.id) != active.as_ref())
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

/// A prepared handoff that the session never used: whatever was recorded, this session is not
/// running as the account.
pub(crate) fn downgrade_remote(app: &AppState, tab_id: &str, reason: &str) {
    let current = app.tab_accounts.read().get(tab_id).and_then(|r| r.remote.clone());
    let account = current.and_then(|r| r.account);
    note_remote(
        app,
        tab_id,
        RemoteRecord { account, state: RemoteState::NotApplied, reason: Some(reason.to_string()) },
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
    let title = {
        let data = app.app_data.read();
        data.windows
            .iter()
            .flat_map(|w| &w.workspaces)
            .flat_map(|ws| &ws.panes)
            .flat_map(|p| &p.tabs)
            .find(|t| t.id == tab_id)
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
        RemoteRecord { account: None, state: RemoteState::NotApplied, reason: Some(reason.into()) }
    }

    fn sent() -> RemoteRecord {
        RemoteRecord { account: None, state: RemoteState::SentUnverified, reason: None }
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
        note_remote(&app, &tab, RemoteRecord { account: account.clone(), state: RemoteState::SentUnverified, reason: None });
        downgrade_remote(&app, &tab, "prepared but not used");
        let r = app.tab_accounts.read()[&tab].remote.clone().unwrap();
        assert_eq!(r.state, RemoteState::NotApplied);
        assert_eq!(r.account, account);
    }
}
