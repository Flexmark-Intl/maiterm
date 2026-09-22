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
    /// The ssh process this handoff belongs to. A tab can run many ssh sessions in one shell, and
    /// only some go through the handoff, so a record describes ONE process, never the tab.
    ///
    /// Bound only on evidence and only on an edge the app always sees — never on timing, and
    /// never lazily when maiLink happens to look (four review rounds each found one of those
    /// letting a later ssh inherit an earlier handoff):
    /// - the ssh maiTerm TYPED (spawn, reconnect, replay) carries the handoff file's path in its
    ///   own argv, so it binds when an ssh whose command line names THIS handoff's file is
    ///   observed — per handoff, not per tab (`handle`), or a history re-run or an outer ssh
    ///   around a replay would name it too;
    /// - where the fragment is typed INTO an ssh already running (the bridge's typed-ssh path,
    ///   the manual inject), the handoff command binds to that process at the moment it runs.
    /// Unbound is `{known:false}`.
    pub ssh_pid: Option<u32>,
    /// This handoff's own name (`remote::handoff_handle` — tab id plus a per-prepare nonce). The
    /// evidence an ssh is recognised by. `None` when nothing was staged (a refusal, host_login).
    pub handle: Option<String>,
}

impl RemoteRecord {
    pub fn new(account: Option<AccountRef>, state: RemoteState, reason: Option<String>) -> Self {
        Self { account, state, reason, ssh_pid: None, handle: None }
    }
}

/// Where a tab's agent is running right now, as observed by the caller.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Place {
    Local,
    /// On another host. `ssh` is the process holding the terminal (pid, command line), when it
    /// could be read.
    Remote { ssh: Option<(u32, String)> },
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

    let Place::Remote { ssh } = place else {
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
    let Some((now_pid, cmd)) = ssh else { return unknown() };
    let _ = cmd;
    // Served only for the ONE ssh this handoff was bound to, and binding never happens here.
    // Binding on a maiLink tick made the record depend on whether a phone happened to be
    // watching: an ssh that came and went unobserved left the record unbound, and a later
    // Up+Enter re-run — same argv, file already consumed — then bound it. See `try_bind`.
    if remote.ssh_pid != Some(now_pid) {
        return unknown();
    }

    // §14.2's union has exactly one nameless known branch, host_login. Every prep that reaches
    // sent_unverified or not_applied carries its account, so a nameless one is not produced
    // today — and must not reach the wire as `{known:true}` with no label if that ever changes.
    let mut out = match (remote.account.as_ref(), remote.state) {
        (Some(a), _) => account_fields(a, runtime_slug, prefs),
        (None, RemoteState::HostLogin) => serde_json::json!({ "known": true }),
        (None, _) => return unknown(),
    };
    out["remote"] = serde_json::to_value(remote.state).unwrap_or_default();
    if let Some(reason) = remote.reason.as_deref() {
        out["reason"] = serde_json::Value::String(reason.to_string());
    }
    Some(out)
}

/// Bind a remote record to the ssh maiTerm just typed for it — called on the edge the app always
/// sees (the frontend's own "ssh came up" poll right after typing it), never on a maiLink tick.
/// Binds only an UNBOUND record, and only to an ssh whose argv names THIS handoff. Returns whether
/// it bound.
///
/// A record with NO handle staged nothing — a refused or failed push (`not_applied`), or a host
/// not covered (`host_login`) — so the ssh typed after it carries no name to recognise. Those bind
/// to the ssh this edge sees, because the edge is the path's own poll right after typing it, and
/// because what they claim is a warning or "the host's own login", never an identity: misplaced,
/// the cost is a check, not false reassurance. `sent_unverified` always requires the name.
pub fn try_bind(remote: &mut RemoteRecord, pid: u32, cmd: &str) -> bool {
    if remote.ssh_pid.is_some() {
        return false;
    }
    let ok = match remote.handle.as_deref() {
        Some(h) => names_handoff(cmd, h),
        None => remote.state != RemoteState::SentUnverified,
    };
    if ok {
        remote.ssh_pid = Some(pid);
    }
    ok
}

/// Whether an ssh command line is the one maiTerm typed for this handoff.
///
/// Matched as a whole path: `tok-a1` must not be found inside `tok-a10`.
fn names_handoff(cmd: &str, handle: &str) -> bool {
    let Some(path) = crate::accounts::remote::token_path(handle) else { return false };
    cmd.match_indices(path.as_str()).any(|(i, _)| {
        cmd[i + path.len()..]
            .chars()
            .next()
            .is_none_or(|c| !(c.is_ascii_alphanumeric() || c == '-'))
    })
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

    /// This handoff's handle — what `rec` stamps and `typed` names.
    const H: &str = "t1-aaaa";
    fn typed_for(pid: u32, handle: &str) -> Place {
        Place::Remote {
            ssh: Some((pid, format!("ssh -t nova '[ -r ~/.maiterm/tokens/tok-{handle} ] && . x; exec $SHELL -l'"))),
        }
    }
    /// The ssh maiTerm typed for handoff `H`.
    fn typed(pid: u32) -> Place {
        typed_for(pid, H)
    }
    fn rec(account: Option<AccountRef>, state: RemoteState, reason: Option<String>) -> RemoteRecord {
        let mut r = RemoteRecord::new(account, state, reason);
        r.handle = Some(H.into());
        r
    }
    fn other(pid: u32) -> Place {
        Place::Remote { ssh: Some((pid, "ssh -t nova claude".into())) }
    }
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

    /// A record bound to `pid` the way the spawn poll binds it.
    fn bound(account: Option<AccountRef>, state: RemoteState, reason: Option<String>, pid: u32) -> RemoteRecord {
        let mut r = rec(account, state, reason);
        let Place::Remote { ssh: Some((_, cmd)) } = typed(pid) else { unreachable!() };
        assert!(try_bind(&mut r, pid, &cmd));
        r
    }

    #[test]
    fn an_ssh_tab_ignores_the_local_shell_and_reports_the_handoff() {
        let p = prefs_with("a");
        let mut r = local("a");
        // Local shell account is irrelevant to a remote agent: no handoff ⇒ unknown.
        assert_eq!(wire(Some(&mut r), "claude", typed(100), &p), unknown());

        r.remote = Some(bound(Some(aref("a")), RemoteState::NotApplied, Some("token expired".into()), 100));
        let v = wire(Some(&mut r), "claude", typed(100), &p).unwrap();
        assert_eq!(v["remote"], "not_applied");
        assert_eq!(v["reason"], "token expired");
        assert_eq!(v["label"], "a@example.com");

        r.remote = Some(bound(None, RemoteState::HostLogin, None, 200));
        let v = wire(Some(&mut r), "claude", typed(200), &p).unwrap();
        assert_eq!(v, serde_json::json!({ "known": true, "remote": "host_login" }));
    }

    #[test]
    fn serving_never_binds_so_an_unobserved_ssh_cannot_be_inherited_by_its_rerun() {
        let p = prefs_with("a");
        let mut r = local("a");
        // The typed ssh came and went before anything bound it; the user re-runs it from history.
        // Same argv, file already consumed. Serving must not bind it.
        r.remote = Some(rec(Some(aref("a")), RemoteState::SentUnverified, None));
        assert_eq!(wire(Some(&mut r), "claude", typed(200), &p), unknown());
        assert!(r.remote.as_ref().unwrap().ssh_pid.is_none());
    }

    #[test]
    fn try_bind_takes_only_this_handoffs_ssh_and_only_once() {
        let mut r = rec(Some(aref("a")), RemoteState::SentUnverified, None);
        assert!(!try_bind(&mut r, 1, "ssh -t nova claude"), "a hand-typed ssh names no handoff");
        assert!(!try_bind(&mut r, 2, "ssh nova '. ~/.maiterm/tokens/tok-t1-aaaa0'"), "a longer handle is not ours");
        assert!(!try_bind(&mut r, 3, "ssh nova '. ~/.maiterm/tokens/tok-t1-bbbb'"), "an earlier handoff is not ours");
        assert!(!try_bind(&mut r, 4, "ssh nova '. ~/.maiterm/tokens/tok-t2'"), "another tab's is not ours");
        let Place::Remote { ssh: Some((_, cmd)) } = typed(100) else { unreachable!() };
        assert!(try_bind(&mut r, 100, &cmd));
        assert!(!try_bind(&mut r, 101, &cmd), "a bound record is never re-bound — a re-run is a new pid");
        assert_eq!(r.ssh_pid, Some(100));
    }

    #[test]
    fn a_failed_push_binds_on_the_edge_so_its_warning_reaches_the_phone() {
        // A failed push stages no file, so the typed ssh names nothing — bind on the edge anyway.
        let mut failed = RemoteRecord::new(Some(aref("a")), RemoteState::NotApplied, Some("no key".into()));
        assert!(try_bind(&mut failed, 7, "ssh -t nova 'cd /x && exec $SHELL -l'"));
        // …but an unnamed SENT record never binds: that would be an identity claim on no evidence.
        let mut sent = RemoteRecord::new(Some(aref("a")), RemoteState::SentUnverified, None);
        assert!(!try_bind(&mut sent, 7, "ssh -t nova claude"));
    }

    #[test]
    fn a_bound_record_is_served_only_while_its_own_ssh_runs() {
        let p = prefs_with("a");
        let mut r = local("a");
        r.remote = Some(bound(Some(aref("a")), RemoteState::SentUnverified, None, 100));
        assert_eq!(wire(Some(&mut r), "claude", typed(100), &p).unwrap()["remote"], "sent_unverified");
        assert_eq!(wire(Some(&mut r), "claude", other(200), &p), unknown());
        assert_eq!(wire(Some(&mut r), "claude", Place::Remote { ssh: None }, &p), unknown());
        // Bound at handoff time (the bridge path): served for that pid whatever its argv.
        let mut r2 = local("a");
        let mut rec2 = RemoteRecord::new(Some(aref("a")), RemoteState::SentUnverified, None);
        rec2.ssh_pid = Some(200);
        r2.remote = Some(rec2);
        assert_eq!(wire(Some(&mut r2), "claude", other(200), &p).unwrap()["remote"], "sent_unverified");
    }

    #[test]
    fn a_nameless_non_host_login_record_never_reaches_the_wire_as_known() {
        let p = prefs_with("a");
        let mut r = local("a");
        let mut rec = RemoteRecord::new(None, RemoteState::NotApplied, Some("x".into()));
        rec.ssh_pid = Some(100);
        r.remote = Some(rec);
        assert_eq!(wire(Some(&mut r), "claude", typed(100), &p), unknown());
    }

    #[test]
    fn a_codex_chat_over_ssh_is_never_described_by_the_claude_token() {
        let p = prefs_with("a");
        let mut r = local("a");
        r.remote = Some(bound(Some(aref("a")), RemoteState::SentUnverified, None, 100));
        assert_eq!(wire(Some(&mut r), "codex", typed(100), &p), unknown());
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
