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

/// Fresh foreground-SSH `(pid, cmd)` probe for `tab_id` (used on one-shot edges like
/// `prepare_remote_account_token(bind_now=true)`, never on periodic ticks).
pub(crate) fn foreground_ssh_fresh(app: &AppState, tab_id: &str) -> Option<(u32, String)> {
    let pty = app.tab_pty_map.read().get(tab_id).cloned()?;
    crate::pty::get_pty_foreground_ssh(app, &pty, true)
}

/// Fresh foreground-SSH PID probe for `tab_id` (used on one-shot edges like
/// `discard_remote_account_token`, never on periodic ticks).
pub(crate) fn foreground_ssh_pid_fresh(app: &AppState, tab_id: &str) -> Option<u32> {
    foreground_ssh_fresh(app, tab_id).map(|(pid, _)| pid)
}

/// True iff `tab_id`'s remote record still belongs to `handle` and is in `NotApplied`.
/// Checked after an async `ps` probe so a concurrent `prepare_remote_account_token` cannot have
/// its newer healthy state overwritten by an older discard's event.
pub(crate) fn remote_handle_is_not_applied(app: &AppState, tab_id: &str, handle: &str) -> bool {
    app.tab_accounts
        .read()
        .get(tab_id)
        .and_then(|r| r.remote.as_ref())
        .is_some_and(|r| r.handle.as_deref() == Some(handle) && r.state == RemoteState::NotApplied)
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

fn not_applied_status_event(
    tab_id: &str,
    remote: &RemoteRecord,
    source: &'static str,
) -> Option<RemoteAccountStatusEvent> {
    if remote.state != RemoteState::NotApplied {
        return None;
    }
    Some(RemoteAccountStatusEvent {
        tab_id: tab_id.to_string(),
        status: "not_applied",
        account_label: remote.account.as_ref().map(|a| a.label.clone()),
        reason: remote
            .reason
            .clone()
            .or_else(|| Some("the token could not be placed".into())),
        source,
    })
}

/// Bind this tab's remote record to the ssh maiTerm just typed for it — the spawn/reconnect/replay
/// paths call this from their own "ssh came up" poll, which runs whether or not maiLink is. Fresh
/// probe; `try_bind` checks the handoff handle in argv for `SentUnverified` and checks `target_host`
/// for `NotApplied`/`HostLogin` (where no handle was staged) so an unrelated ssh host never binds.
///
/// When binding succeeds for a `NotApplied` prep (e.g. missing/expired keychain token or staging
/// failure), also returns the desktop `"remote-account-status"` event so the error badge only
/// ever appears on a LIVE, BOUND SSH session — never before `ssh` starts, on a fast-failing `ssh`,
/// or after a gated reconnect is retracted before typing `ssh`.
pub(crate) fn bind_after_ssh_up(
    app: &AppState,
    tab_id: &str,
) -> (bool, Option<RemoteAccountStatusEvent>) {
    let pty = app.tab_pty_map.read().get(tab_id).cloned();
    let Some((pid, cmd)) = pty.and_then(|p| crate::pty::get_pty_foreground_ssh(app, &p, true)) else {
        return (false, None);
    };
    bind_after_ssh_up_with_ssh(app, tab_id, pid, &cmd)
}

fn bind_after_ssh_up_with_ssh(
    app: &AppState,
    tab_id: &str,
    pid: u32,
    cmd: &str,
) -> (bool, Option<RemoteAccountStatusEvent>) {
    let mut records = app.tab_accounts.write();
    let Some(remote) = records.get_mut(tab_id).and_then(|r| r.remote.as_mut()) else {
        return (false, None);
    };
    if !tab_record::try_bind(remote, pid, cmd) {
        return (false, None);
    }
    let ev = not_applied_status_event(tab_id, remote, "prepare");
    (true, ev)
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
    note_remote_at_uptime(app, tab_id, remote, app.started_at.elapsed());
}

/// `note_remote` with how long the app has been up passed in, so a test can say "long enough"
/// without fabricating an old `Instant` (which panics on Windows when the machine has been up
/// for less than the age asked for).
fn note_remote_at_uptime(app: &AppState, tab_id: &str, remote: RemoteRecord, uptime: std::time::Duration) {
    let entering_failure = {
        let mut records = app.tab_accounts.write();
        let entry = records.entry(tab_id.to_string()).or_default();
        let was = entry.remote.as_ref().map(|r| r.state);
        let now = remote.state;
        entry.remote = Some(remote);
        now == RemoteState::NotApplied && was != Some(RemoteState::NotApplied)
    };
    if entering_failure && uptime >= RING_BASELINE_WINDOW {
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

/// Payload for the desktop `"remote-account-status"` event so a tab can show (or clear) a
/// persistent error state whenever a managed remote account fails delivery or env precedence
/// (§6.1). Never carries token material.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize)]
pub(crate) struct RemoteAccountStatusEvent {
    pub tab_id: String,
    /// `"not_applied"` or `"ok"` (clears any previous tab error).
    pub status: &'static str,
    pub account_label: Option<String>,
    pub reason: Option<String>,
    /// `"hook"` (remote `SessionStart` precedence check), `"prepare"`, `"discard"`, or `"clear"`.
    pub source: &'static str,
}

pub(crate) fn emit_remote_account_status(
    handle: &tauri::AppHandle,
    event: &RemoteAccountStatusEvent,
) {
    let _ = handle.emit("remote-account-status", event);
}

/// Fast, lock-only precheck so `hooks_handler` pays zero `ps` overhead on local tabs and
/// unmanaged SSH tabs: true only when this tab has a managed remote account record that a
/// `SessionStart` precedence observation can downgrade (or clear after a previous env mismatch).
pub(crate) fn needs_remote_auth_check(app: &AppState, tab_id: &str) -> bool {
    let records = app.tab_accounts.read();
    let Some(remote) = records.get(tab_id).and_then(|r| r.remote.as_ref()) else {
        return false;
    };
    remote.account.is_some()
        && matches!(remote.state, RemoteState::SentUnverified | RemoteState::NotApplied)
}

/// §6.1: verify the presence-only env report from a remote Claude `SessionStart` against this
/// tab's bound remote handoff. Run off the async executor (it probes the foreground ssh via `ps`).
pub(crate) fn verify_remote_auth_env(
    app: &AppState,
    handle: &tauri::AppHandle,
    tab_id: &str,
    env: &tab_record::RemoteAuthEnv,
) {
    if !needs_remote_auth_check(app, tab_id) {
        return;
    }
    // Fresh probe: this runs once per managed remote Claude launch (never on a periodic tick), so
    // an 800 ms cache predating the ssh must not hide the live ssh process.
    let pty = app.tab_pty_map.read().get(tab_id).cloned();
    let fg = pty
        .as_deref()
        .and_then(|p| crate::pty::get_pty_foreground(app, p, true).ok().flatten());
    let tunnel = app.ssh_tunnels.read().values().any(|t| t.tab_ids.contains(tab_id));
    let current_place = if fg.is_none() && !tunnel {
        tab_record::Place::Local
    } else {
        let ssh = pty.as_deref().and_then(|p| crate::pty::get_pty_foreground_ssh(app, p, true));
        tab_record::Place::Remote { ssh }
    };
    if let Some(ev) = apply_remote_auth_env(app, tab_id, current_place, env, app.started_at.elapsed()) {
        emit_remote_account_status(handle, &ev);
    }
}

/// Pure state transition for `verify_remote_auth_env`, separated from `ps` and `AppHandle` so unit
/// tests can exercise every local/remote/binding/recovery edge directly.
fn apply_remote_auth_env(
    app: &AppState,
    tab_id: &str,
    current_place: tab_record::Place,
    env: &tab_record::RemoteAuthEnv,
    uptime: std::time::Duration,
) -> Option<RemoteAccountStatusEvent> {
    let tab_record::Place::Remote { ssh } = current_place else {
        // Local shell (e.g. user exited SSH and launched local `claude` in the same tab): local
        // tabs use CLAUDE_CONFIG_DIR and intentionally scrub CLAUDE_CODE_OAUTH_TOKEN. Never
        // downgrade the old remote record, and clear any lingering remote error badge on the tab.
        let had_remote_err = app
            .tab_accounts
            .read()
            .get(tab_id)
            .and_then(|r| r.remote.as_ref())
            .is_some_and(|r| r.state == RemoteState::NotApplied);
        return had_remote_err.then(|| RemoteAccountStatusEvent {
            tab_id: tab_id.to_string(),
            status: "ok",
            account_label: None,
            reason: None,
            source: "hook",
        });
    };

    let mismatch = env.mismatch_reason();
    let (entering_failure, event) = {
        let mut records = app.tab_accounts.write();
        let Some(remote) = records.get_mut(tab_id).and_then(|r| r.remote.as_mut()) else {
            return None;
        };
        let Some(account_label) = remote.account.as_ref().map(|a| a.label.clone()) else {
            return None;
        };
        if remote.state != RemoteState::SentUnverified && !remote.env_mismatch {
            return None;
        }
        // A clean SessionStart on a healthy (`!env_mismatch`) record has nothing to change —
        // return BEFORE `try_bind` so we never lazily bind an unobserved history re-run to
        // `SentUnverified` (preserving `tab_record`'s Up+Enter re-run guard).
        if mismatch.is_none() && !remote.env_mismatch {
            return None;
        }
        // Only act when this SessionStart belongs to the exact SSH process this handoff owns.
        // A different ssh (or an unbound handoff whose argv doesn't match) must not be mutated.
        match (remote.ssh_pid, ssh) {
            (Some(bound_pid), Some((now_pid, _))) if bound_pid == now_pid => {}
            (None, Some((now_pid, ref cmd))) if tab_record::try_bind(remote, now_pid, cmd) => {}
            _ => return None,
        }

        match mismatch {
            Some(reason) => {
                let was = remote.state;
                let changed = was != RemoteState::NotApplied
                    || remote.reason.as_deref() != Some(reason.as_str());
                // Keep `ssh_pid` and `handle` intact so maiLink immediately serves `not_applied`
                // for this live SSH session rather than dropping back to `{known: false}`.
                remote.state = RemoteState::NotApplied;
                remote.reason = Some(reason.clone());
                remote.env_mismatch = true;
                let entering_failure = was != RemoteState::NotApplied;
                let ev = changed.then(|| RemoteAccountStatusEvent {
                    tab_id: tab_id.to_string(),
                    status: "not_applied",
                    account_label: Some(account_label),
                    reason: Some(reason),
                    source: "hook",
                });
                (entering_failure, ev)
            }
            None if remote.env_mismatch => {
                // The user fixed the remote environment (e.g. `unset ANTHROPIC_API_KEY`) and
                // started Claude again in the same SSH session. Return to `SentUnverified` and
                // clear the desktop tab error badge.
                remote.state = RemoteState::SentUnverified;
                remote.reason = None;
                remote.env_mismatch = false;
                (
                    false,
                    Some(RemoteAccountStatusEvent {
                        tab_id: tab_id.to_string(),
                        status: "ok",
                        account_label: Some(account_label),
                        reason: None,
                        source: "hook",
                    }),
                )
            }
            None => (false, None),
        }
    };

    if entering_failure && uptime >= RING_BASELINE_WINDOW {
        queue_ring(app, tab_id);
    }
    event
}

/// Outcome of `downgrade_remote`: returned so the caller (`discard_remote_account_token`) can
/// confirm the bound SSH process is still alive before emitting the desktop badge or ringing
/// maiLink's phone doorbell (avoiding spurious alerts on `retract()` or after SSH has exited).
pub(crate) struct DowngradeOutcome {
    pub(crate) entering_failure: bool,
    pub(crate) bound_pid: Option<u32>,
    pub(crate) event: RemoteAccountStatusEvent,
}

/// A prepared handoff that the session never used: that session is not running as the account.
///
/// **Only if the record is still THAT handoff's.** Checked and updated under a single write lock
/// (no read/write TOCTOU with a concurrent `prepare_remote_account_token`), and updated IN PLACE
/// so a bound `ssh_pid` and `handle` stay intact (allowing maiLink to serve `not_applied` for a
/// `bindNow` session and allowing the caller to verify the exact SSH PID before emitting a tab
/// badge or doorbell). Does NOT ring the doorbell itself; callers call
/// `ring_remote_account_failure` once liveness is confirmed.
pub(crate) fn downgrade_remote(
    app: &AppState,
    tab_id: &str,
    handle: &str,
    reason: &str,
) -> Option<DowngradeOutcome> {
    let mut records = app.tab_accounts.write();
    let remote = records.get_mut(tab_id)?.remote.as_mut()?;
    if remote.handle.as_deref() != Some(handle) {
        return None;
    }
    let was = remote.state;
    remote.state = RemoteState::NotApplied;
    remote.reason = Some(reason.to_string());
    remote.env_mismatch = false;
    let bound_pid = remote.ssh_pid;
    let account_label = remote.account.as_ref().map(|a| a.label.clone());
    let event = RemoteAccountStatusEvent {
        tab_id: tab_id.to_string(),
        status: "not_applied",
        account_label,
        reason: Some(reason.to_string()),
        source: "discard",
    };
    Some(DowngradeOutcome {
        entering_failure: was != RemoteState::NotApplied,
        bound_pid,
        event,
    })
}

/// Queue the maiLink phone doorbell for a confirmed live remote-account failure, respecting the
/// startup baseline window.
pub(crate) fn ring_remote_account_failure(app: &AppState, tab_id: &str) {
    ring_remote_account_failure_at_uptime(app, tab_id, app.started_at.elapsed());
}

fn ring_remote_account_failure_at_uptime(
    app: &AppState,
    tab_id: &str,
    uptime: std::time::Duration,
) {
    if uptime >= RING_BASELINE_WINDOW {
        queue_ring(app, tab_id);
    }
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

    /// How long the app has been up, past the restart window and inside it.
    const UP_LONG: std::time::Duration = std::time::Duration::from_secs(RING_BASELINE_WINDOW.as_secs() * 2);
    const UP_JUST_NOW: std::time::Duration = std::time::Duration::ZERO;

    /// maiLink running, one window with one tab named "worker".
    fn fixture() -> (AppState, String) {
        let app = AppState::new();
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
        let (app, tab) = fixture();
        note_remote_at_uptime(&app, &tab, failed("token expired"), UP_JUST_NOW);
        assert!(app.mailink_pending_rings.lock().is_empty());
        assert_eq!(app.tab_accounts.read()[&tab].remote.as_ref().unwrap().state, RemoteState::NotApplied);
    }

    #[test]
    fn entering_not_applied_rings_once_content_light() {
        let (app, tab) = fixture();
        note_remote_at_uptime(&app, &tab, sent(), UP_LONG);
        assert!(app.mailink_pending_rings.lock().is_empty(), "sent_unverified is not a failure");
        note_remote_at_uptime(&app, &tab, failed("ssh refused the key"), UP_LONG);
        note_remote_at_uptime(&app, &tab, failed("again"), UP_LONG);
        let q = app.mailink_pending_rings.lock();
        assert_eq!(q.len(), 1, "a repeat failure is not a transition");
        // Only the tab name and the kind — never the reason, never an account label.
        assert_eq!(q[0], (tab.clone(), "worker".to_string(), "account"));
    }

    #[test]
    fn a_tab_the_phone_cannot_see_never_rings() {
        let (app, tab) = fixture();
        app.app_data.write().windows[0].workspaces[0].panes[0].tabs[0].mailink_excluded = true;
        note_remote_at_uptime(&app, &tab, failed("x"), UP_LONG);
        assert!(app.mailink_pending_rings.lock().is_empty());
    }

    #[test]
    fn an_unused_prep_is_downgraded_and_keeps_its_intended_account() {
        let (app, tab) = fixture();
        let account = Some(tab_record::AccountRef { id: "a".into(), label: "a@example.com".into() });
        let mut r = RemoteRecord::new(account.clone(), RemoteState::SentUnverified, None);
        r.handle = Some("h1".into());
        r.ssh_pid = Some(555);
        note_remote(&app, &tab, r);
        assert!(downgrade_remote(&app, &tab, "h-other", "not ours").is_none());
        assert_eq!(
            app.tab_accounts.read()[&tab].remote.as_ref().unwrap().state,
            RemoteState::SentUnverified,
            "a discard for a different handoff must not touch this record"
        );
        let outcome = downgrade_remote(&app, &tab, "h1", "prepared but not used").unwrap();
        assert!(outcome.entering_failure);
        assert_eq!(outcome.bound_pid, Some(555));
        assert_eq!(outcome.event.status, "not_applied");
        assert_eq!(outcome.event.source, "discard");
        let r = app.tab_accounts.read()[&tab].remote.clone().unwrap();
        assert_eq!(r.state, RemoteState::NotApplied);
        assert_eq!(r.account, account);
        assert_eq!(r.ssh_pid, Some(555), "ssh_pid must survive downgrade so maiLink serves not_applied");
        assert_eq!(r.handle.as_deref(), Some("h1"));
        assert!(remote_handle_is_not_applied(&app, &tab, "h1"));
        assert!(!remote_handle_is_not_applied(&app, &tab, "h2"));
        // `downgrade_remote` alone must not ring (retract / already-exited SSH must stay silent).
        assert!(app.mailink_pending_rings.lock().is_empty());
        // Once the caller verifies the bound SSH is still live, ringing queues the doorbell.
        ring_remote_account_failure_at_uptime(&app, &tab, UP_LONG);
        assert_eq!(app.mailink_pending_rings.lock().len(), 1);
    }

    #[test]
    fn remote_session_start_env_mismatch_downgrades_bound_ssh_keeps_pid_rings_and_recovers() {
        let (app, tab) = fixture();
        let account = Some(tab_record::AccountRef { id: "a".into(), label: "work@example.com".into() });
        let mut r = RemoteRecord::new(account.clone(), RemoteState::SentUnverified, None);
        r.handle = Some("h1".into());
        r.ssh_pid = Some(100);
        note_remote_at_uptime(&app, &tab, r, UP_LONG);

        let bound_place = tab_record::Place::Remote { ssh: Some((100, "ssh nova".into())) };
        let other_place = tab_record::Place::Remote { ssh: Some((200, "ssh other".into())) };
        let shadowed = tab_record::RemoteAuthEnv { oauth_token: true, shadowed_by: vec!["ANTHROPIC_API_KEY"] };
        let missing = tab_record::RemoteAuthEnv { oauth_token: false, shadowed_by: vec![] };
        let clean = tab_record::RemoteAuthEnv { oauth_token: true, shadowed_by: vec![] };

        // A local Claude session in the same tab (after exiting ssh) must NEVER downgrade the remote record.
        assert!(apply_remote_auth_env(&app, &tab, tab_record::Place::Local, &missing, UP_LONG).is_none());
        assert_eq!(app.tab_accounts.read()[&tab].remote.as_ref().unwrap().state, RemoteState::SentUnverified);

        // An unrelated ssh session (different pid) must NOT mutate this handoff either.
        assert!(apply_remote_auth_env(&app, &tab, other_place, &shadowed, UP_LONG).is_none());
        assert_eq!(app.tab_accounts.read()[&tab].remote.as_ref().unwrap().state, RemoteState::SentUnverified);

        // A mismatch on the bound ssh downgrades to NotApplied, keeps ssh_pid bound so maiLink
        // serves not_applied immediately, rings once, and emits the desktop tab error event.
        let ev = apply_remote_auth_env(&app, &tab, bound_place.clone(), &shadowed, UP_LONG).unwrap();
        assert_eq!(ev.status, "not_applied");
        assert_eq!(ev.account_label.as_deref(), Some("work@example.com"));
        assert!(ev.reason.as_deref().unwrap().contains("ANTHROPIC_API_KEY"));
        let stored = app.tab_accounts.read()[&tab].remote.clone().unwrap();
        assert_eq!(stored.state, RemoteState::NotApplied);
        assert_eq!(stored.ssh_pid, Some(100), "ssh_pid must stay bound so maiLink serves not_applied");
        assert!(stored.env_mismatch);
        assert_eq!(app.mailink_pending_rings.lock().len(), 1);

        // Repeat with the identical reason is a no-op (no duplicate event or doorbell).
        assert!(apply_remote_auth_env(&app, &tab, bound_place.clone(), &shadowed, UP_LONG).is_none());
        assert_eq!(app.mailink_pending_rings.lock().len(), 1);

        // Fixing the remote env and starting Claude again in the SAME ssh recovers back to SentUnverified.
        let ok = apply_remote_auth_env(&app, &tab, bound_place, &clean, UP_LONG).unwrap();
        assert_eq!(ok.status, "ok");
        let stored = app.tab_accounts.read()[&tab].remote.clone().unwrap();
        assert_eq!(stored.state, RemoteState::SentUnverified);
        assert!(stored.reason.is_none());
        assert!(!stored.env_mismatch);
    }

    #[test]
    fn remote_session_start_binds_matching_unbound_handoff_and_never_clears_delivery_failure() {
        let (app, tab) = fixture();
        let account = Some(tab_record::AccountRef { id: "a".into(), label: "work@example.com".into() });
        let mut r = RemoteRecord::new(account.clone(), RemoteState::SentUnverified, None);
        r.handle = Some("t1-abcd".into());
        note_remote_at_uptime(&app, &tab, r, UP_LONG);

        let matching_ssh = tab_record::Place::Remote {
            ssh: Some((300, "ssh -t nova '[ -r ~/.maiterm/tokens/tok-t1-abcd ] && . x'".into())),
        };
        let clean = tab_record::RemoteAuthEnv { oauth_token: true, shadowed_by: vec![] };
        // A clean SessionStart must NEVER lazily bind an unbound handoff — otherwise an unobserved
        // history Up+Enter re-run on a host exporting its own token gets served as `sent_unverified`.
        assert!(apply_remote_auth_env(&app, &tab, matching_ssh.clone(), &clean, UP_LONG).is_none());
        assert_eq!(app.tab_accounts.read()[&tab].remote.as_ref().unwrap().ssh_pid, None);

        let missing = tab_record::RemoteAuthEnv { oauth_token: false, shadowed_by: vec![] };
        let ev = apply_remote_auth_env(&app, &tab, matching_ssh, &missing, UP_LONG).unwrap();
        assert_eq!(ev.status, "not_applied");
        assert_eq!(app.tab_accounts.read()[&tab].remote.as_ref().unwrap().ssh_pid, Some(300));

        // A delivery/expiry refusal (env_mismatch == false) must NEVER be cleared by a clean hook
        // on the remote SSH session, though leaving SSH to run local Claude clears the tab badge.
        let mut refused = RemoteRecord::new(account, RemoteState::NotApplied, Some("token expired".into()));
        refused.ssh_pid = Some(400);
        note_remote_at_uptime(&app, &tab, refused, UP_LONG);
        let place400 = tab_record::Place::Remote { ssh: Some((400, "ssh nova".into())) };
        assert!(apply_remote_auth_env(&app, &tab, place400, &clean, UP_LONG).is_none());
        assert_eq!(app.tab_accounts.read()[&tab].remote.as_ref().unwrap().state, RemoteState::NotApplied);
        let local_clear = apply_remote_auth_env(&app, &tab, tab_record::Place::Local, &missing, UP_LONG).unwrap();
        assert_eq!(local_clear.status, "ok");
    }

    #[test]
    fn bind_after_ssh_up_emits_prepare_error_only_when_not_applied_binds_live_ssh() {
        let (app, tab) = fixture();
        let account = Some(tab_record::AccountRef { id: "a".into(), label: "work@example.com".into() });

        // Failed prep (handle = None) with target_host: must NOT bind to a different SSH host
        // (e.g. user manually running `ssh other` after a fast-failing baked ssh to `nova`), and
        // MUST bind + emit once for the matching host (even with full binary path / flags).
        let mut r = RemoteRecord::new(account.clone(), RemoteState::NotApplied, Some("token expired".into()));
        r.target_host = Some("nova".into());
        note_remote(&app, &tab, r);
        assert_eq!(bind_after_ssh_up_with_ssh(&app, &tab, 699, "ssh other"), (false, None));
        assert_eq!(app.tab_accounts.read()[&tab].remote.as_ref().unwrap().ssh_pid, None);

        let (bound, ev) = bind_after_ssh_up_with_ssh(&app, &tab, 700, "/usr/bin/ssh -p 2222 nova");
        assert!(bound);
        let ev = ev.expect("binding a NotApplied prep must emit the desktop error status");
        assert_eq!(ev.status, "not_applied");
        assert_eq!(ev.source, "prepare");
        assert_eq!(ev.account_label.as_deref(), Some("work@example.com"));
        assert_eq!(ev.reason.as_deref(), Some("token expired"));
        assert_eq!(app.tab_accounts.read()[&tab].remote.as_ref().unwrap().ssh_pid, Some(700));
        assert_eq!(bind_after_ssh_up_with_ssh(&app, &tab, 700, "ssh nova"), (false, None));

        // `autossh -M <port>` must not parse `<port>` as the target host (and plain `ssh -M`
        // still treats `-M` as a no-arg OpenSSH flag).
        let mut auto_r = RemoteRecord::new(account.clone(), RemoteState::NotApplied, Some("token expired".into()));
        auto_r.target_host = Some("ews@nova".into());
        note_remote(&app, &tab, auto_r);
        let (bound, ev) = bind_after_ssh_up_with_ssh(
            &app,
            &tab,
            702,
            "autossh -t -o ControlMaster=no -M 0 ews@nova 'exec $SHELL -l'",
        );
        assert!(bound);
        assert_eq!(ev.unwrap().status, "not_applied");

        // `HostLogin` with `target_host` must not bind to an unrelated host either.
        let mut hl = RemoteRecord::new(None, RemoteState::HostLogin, None);
        hl.target_host = Some("hostA".into());
        note_remote(&app, &tab, hl);
        assert_eq!(bind_after_ssh_up_with_ssh(&app, &tab, 703, "ssh hostB"), (false, None));
        assert_eq!(bind_after_ssh_up_with_ssh(&app, &tab, 704, "ssh -M hostA"), (true, None));
        assert_eq!(app.tab_accounts.read()[&tab].remote.as_ref().unwrap().ssh_pid, Some(704));

        // Healthy SentUnverified prep: binds when argv matches the handoff, emits no error event.
        let mut ok = RemoteRecord::new(account, RemoteState::SentUnverified, None);
        ok.handle = Some("t1-abcd".into());
        note_remote(&app, &tab, ok);
        let (bound, ev) = bind_after_ssh_up_with_ssh(
            &app,
            &tab,
            701,
            "ssh -t nova '[ -r ~/.maiterm/tokens/tok-t1-abcd ] && . x'",
        );
        assert!(bound);
        assert!(ev.is_none());
    }
}
