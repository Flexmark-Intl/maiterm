//! Which account each tab actually spawned under — maiLink §14 (docs/mailink-protocol.md).
//!
//! **Recorded, never inferred.** The one tempting shortcut here, "a tab runs as the active
//! account", is exactly the value this feature makes wrong: a tab reads the active account when
//! its shell spawns and keeps it after the active one changes. So the pairing is written at the
//! single spawn path (`pty::spawn_pty`) and read back as-is, and a tab with no record answers
//! "unknown" rather than borrowing today's active account.
//!
//! **In memory only, keyed by tab id.** A record describes a live shell's environment, which dies
//! with the PTY; every respawn — reload, restore, resume, auto-resume — goes back through
//! `spawn_pty` and writes a fresh one. Persisting it would let a record outlive the process it
//! describes, and a reload mints a new tab id anyway.
//!
//! The remote half is written by the §6 handoff commands (`prepare_remote_account_token`,
//! `discard_remote_account_token`), which see every outcome. It can only ever report how far DELIVERY got: a token has no identity to read back (login.md §2.4)
//! and a bad one falls through to the host's own login instead of failing (§6.1), so there is
//! deliberately no "applied" state.

use std::collections::BTreeMap;

use serde::Serialize;

/// How far the §6 handoff got for an SSH session.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RemoteState {
    /// The token was placed for this session. NOT proof the host is running as the account.
    SentUnverified,
    /// The host is NOT running as the intended account: delivery failed, the token expired, or it
    /// was prepared and never used.
    NotApplied,
    /// The host is not covered by the active account; the agent runs as whatever the host is
    /// signed in to. Informational.
    HostLogin,
}

/// An account as it was when the record was written. The label is kept so a chat whose account
/// has since been REMOVED can still say who it was, flagged `removed` rather than blanked.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AccountRef {
    pub id: String,
    pub label: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RemoteRecord {
    /// The account the desktop MEANT the remote to run as. None for `HostLogin`.
    pub account: Option<AccountRef>,
    pub state: RemoteState,
    /// One human sentence for `NotApplied`. Never token material.
    pub reason: Option<String>,
    /// The ssh process this handoff belongs to — bound on the first observation after it (the
    /// push usually happens BEFORE the ssh starts, so it cannot be known at record time). A tab
    /// can run many ssh sessions in one shell; without this, a later session that never went
    /// through the handoff (`ssh -t host claude`, the bridge off) was served this one's account.
    pub ssh_pid: Option<u32>,
    pub recorded_at: std::time::Instant,
}

impl RemoteRecord {
    pub fn new(account: Option<AccountRef>, state: RemoteState, reason: Option<String>) -> Self {
        Self { account, state, reason, ssh_pid: None, recorded_at: std::time::Instant::now() }
    }
}

/// How long an unbound record may wait for its ssh to appear. The handoff runs just before the
/// ssh is typed; past this, whatever ssh is running is not the one the record was written for.
pub const BIND_WINDOW: std::time::Duration = std::time::Duration::from_secs(60);

/// Where a tab's agent is running right now, as observed by the caller.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Place {
    Local,
    /// On another host. `ssh_pid` is the ssh process holding the terminal, when it could be read.
    Remote { ssh_pid: Option<u32> },
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TabAccount {
    /// runtime slug → the account injected at spawn. Empty = the tab spawned unmanaged.
    pub local: BTreeMap<String, AccountRef>,
    /// Set once an SSH session in this tab has been through the handoff. A respawn clears it —
    /// the new shell is local until it connects again.
    pub remote: Option<RemoteRecord>,
}

/// What a spawn records: the account injected for each runtime, from the same resolution the
/// spawn env uses, so the record cannot disagree with what the shell was actually handed.
pub fn for_spawn(prefs: &crate::state::Preferences) -> TabAccount {
    TabAccount {
        local: super::active_accounts(prefs)
            .into_iter()
            .map(|(rt, id)| (rt.slug().to_string(), account_ref(prefs, &id)))
            .collect(),
        remote: None,
    }
}

/// `id` plus its label as of now, for a record that must outlive a later rename or removal.
pub fn account_ref(prefs: &crate::state::Preferences, id: &str) -> AccountRef {
    let label = prefs
        .managed_accounts
        .iter()
        .find(|a| a.id == id)
        .map(|a| a.label.clone())
        .unwrap_or_else(|| id.to_string());
    AccountRef { id: id.to_string(), label }
}

/// A tab's account as served to maiLink — §14.2 `ChatAccount`.
///
/// `None` from this function means the wire value `null`: the tab is KNOWN to run unmanaged.
/// Unknown is `Some({known: false})`, never `None`. Takes the record mutably only to bind an
/// unbound remote record to the ssh process now holding the terminal (see `RemoteRecord::ssh_pid`).
pub fn wire(
    record: Option<&mut TabAccount>,
    runtime_slug: &str,
    place: Place,
    prefs: &crate::state::Preferences,
) -> Option<serde_json::Value> {
    let unknown = || Some(serde_json::json!({ "known": false }));
    let Some(record) = record else { return unknown() };

    let Place::Remote { ssh_pid } = place else {
        let a = record.local.get(runtime_slug)?;
        return Some(account_fields(a, runtime_slug, prefs));
    };

    // An SSH tab's agent runs on the remote, so the LOCAL shell's account says nothing about who
    // is answering. And the §6 handoff only ever carries a CLAUDE token: a Codex or Gemini chat
    // over the same ssh never reads it, so it must not be described by it.
    if runtime_slug != "claude" {
        return unknown();
    }
    let Some(remote) = record.remote.as_mut() else { return unknown() };
    let Some(now_pid) = ssh_pid else { return unknown() };
    match remote.ssh_pid {
        Some(bound) if bound == now_pid => {}
        Some(_) => return unknown(),
        None if remote.recorded_at.elapsed() <= BIND_WINDOW => remote.ssh_pid = Some(now_pid),
        None => return unknown(),
    }

    let mut out = match remote.account.as_ref() {
        Some(a) => account_fields(a, runtime_slug, prefs),
        None => serde_json::json!({ "known": true }),
    };
    out["remote"] = serde_json::to_value(remote.state).unwrap_or_default();
    if let Some(reason) = remote.reason.as_deref() {
        out["reason"] = serde_json::Value::String(reason.to_string());
    }
    Some(out)
}

/// The descriptive fields for one account id, plus `stale` pre-resolved against the CURRENT
/// active account so the phone never re-derives it.
fn account_fields(
    account: &AccountRef,
    runtime_slug: &str,
    prefs: &crate::state::Preferences,
) -> serde_json::Value {
    let id = account.id.as_str();
    // `label` is REQUIRED in this branch of §14.2's union. An account removed since the tab spawned
    // still names what the shell holds: it keeps its last-known label and says `removed` as a
    // flag, so the phone renders a fact about the account rather than parsing a placeholder name.
    let mut out = serde_json::json!({ "known": true, "id": id, "label": account.label });
    match prefs.managed_accounts.iter().find(|a| a.id == id) {
        None => {
            out["removed"] = true.into();
        }
        Some(a) => {
            out["label"] = a.label.clone().into();
            if let Some(org) = &a.org_name {
                out["org"] = org.clone().into();
            }
            if let Some(plan) = &a.plan {
                out["plan"] = plan.clone().into();
            }
        }
    }
    let active = prefs.active_account_ids.get(runtime_slug).map(String::as_str);
    if active != Some(id) || !prefs.accounts_enabled {
        out["stale"] = true.into();
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::{ManagedAccount, Preferences};

    fn prefs_with(active: &str) -> Preferences {
        let mut p = Preferences::default();
        p.accounts_setup_complete = true;
        p.accounts_enabled = true;
        for id in ["a", "b"] {
            let a: ManagedAccount = serde_json::from_value(serde_json::json!({
                "id": id, "runtime": "claude", "label": format!("{id}@example.com"),
            }))
            .unwrap();
            p.managed_accounts.push(a);
        }
        p.active_account_ids.insert("claude".into(), active.into());
        p
    }

    fn aref(id: &str) -> AccountRef {
        AccountRef { id: id.into(), label: format!("{id}@example.com") }
    }

    fn local(id: &str) -> TabAccount {
        TabAccount { local: [("claude".to_string(), aref(id))].into(), remote: None }
    }

    const SSH: Place = Place::Remote { ssh_pid: Some(100) };
    fn unknown() -> Option<serde_json::Value> {
        Some(serde_json::json!({ "known": false }))
    }

    #[test]
    fn no_record_is_unknown_never_the_active_account() {
        let p = prefs_with("a");
        assert_eq!(wire(None, "claude", Place::Local, &p), unknown());
    }

    #[test]
    fn an_empty_record_is_known_unmanaged() {
        let p = prefs_with("a");
        assert_eq!(wire(Some(&mut TabAccount::default()), "claude", Place::Local, &p), None);
    }

    #[test]
    fn a_tab_keeps_its_spawn_account_after_a_switch_and_says_it_is_stale() {
        let p = prefs_with("b");
        let v = wire(Some(&mut local("a")), "claude", Place::Local, &p).unwrap();
        assert_eq!(v["id"], "a");
        assert_eq!(v["label"], "a@example.com");
        assert_eq!(v["stale"], true);
        let v = wire(Some(&mut local("b")), "claude", Place::Local, &p).unwrap();
        assert!(v.get("stale").is_none());
    }

    #[test]
    fn an_ssh_tab_ignores_the_local_shell_and_reports_the_handoff() {
        let p = prefs_with("a");
        let mut r = local("a");
        // Local shell account is irrelevant to a remote agent: no handoff ⇒ unknown.
        assert_eq!(wire(Some(&mut r), "claude", SSH, &p), unknown());

        r.remote = Some(RemoteRecord::new(Some(aref("a")), RemoteState::NotApplied, Some("token expired".into())));
        let v = wire(Some(&mut r), "claude", SSH, &p).unwrap();
        assert_eq!(v["remote"], "not_applied");
        assert_eq!(v["reason"], "token expired");
        assert_eq!(v["label"], "a@example.com");

        r.remote = Some(RemoteRecord::new(None, RemoteState::HostLogin, None));
        let v = wire(Some(&mut r), "claude", SSH, &p).unwrap();
        assert_eq!(v, serde_json::json!({ "known": true, "remote": "host_login" }));
    }

    #[test]
    fn a_later_ssh_session_is_not_served_an_earlier_handoff() {
        let p = prefs_with("a");
        let mut r = local("a");
        r.remote = Some(RemoteRecord::new(Some(aref("a")), RemoteState::SentUnverified, None));
        // First observation binds the record to ssh pid 100.
        assert_eq!(wire(Some(&mut r), "claude", SSH, &p).unwrap()["remote"], "sent_unverified");
        // The user exits and runs a different ssh (pid 200) with no handoff: unknown.
        assert_eq!(wire(Some(&mut r), "claude", Place::Remote { ssh_pid: Some(200) }, &p), unknown());
        // Unreadable foreground: never guessed.
        assert_eq!(wire(Some(&mut r), "claude", Place::Remote { ssh_pid: None }, &p), unknown());
    }

    #[test]
    fn an_unbound_record_past_the_window_is_not_claimed_by_whatever_ssh_is_running() {
        let p = prefs_with("a");
        let mut r = local("a");
        let mut rec = RemoteRecord::new(Some(aref("a")), RemoteState::SentUnverified, None);
        rec.recorded_at = std::time::Instant::now() - BIND_WINDOW * 2;
        r.remote = Some(rec);
        assert_eq!(wire(Some(&mut r), "claude", SSH, &p), unknown());
    }

    #[test]
    fn a_codex_chat_over_ssh_is_never_described_by_the_claude_token() {
        let p = prefs_with("a");
        let mut r = local("a");
        r.remote = Some(RemoteRecord::new(Some(aref("a")), RemoteState::SentUnverified, None));
        assert_eq!(wire(Some(&mut r), "codex", SSH, &p), unknown());
    }

    #[test]
    fn a_removed_account_keeps_its_last_label_and_says_removed() {
        let mut p = prefs_with("a");
        let r = local("a");
        p.managed_accounts.retain(|a| a.id != "a");
        let mut r = r;
        let v = wire(Some(&mut r), "claude", Place::Local, &p).unwrap();
        assert_eq!(v["label"], "a@example.com");
        assert_eq!(v["removed"], true);
    }

    #[test]
    fn a_disabled_feature_marks_every_managed_tab_stale() {
        let mut p = prefs_with("a");
        p.accounts_enabled = false;
        assert_eq!(wire(Some(&mut local("a")), "claude", Place::Local, &p).unwrap()["stale"], true);
    }
}
