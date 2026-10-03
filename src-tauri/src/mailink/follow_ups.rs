//! Watch scripts waiting for the human's approval, as the phone sees them (protocol 0.15,
//! docs/follow-ups.md §5.1).
//!
//! An agent's watch script runs later, unattended, as the user, outside the agent's own permission
//! checks, so it runs only after a human has read it and said yes. That card was desktop-only: a
//! script asked for while the human was away waited until they came back to the desk.
//!
//! The rules the desktop card keeps hold here too:
//! - **Approval is decided in Rust,** from a human's act. The phone's approve is that act; it
//!   goes through the same `watch::remember_approval` as the desktop's.
//! - **The approval is for the text the human READ.** The phone sends back the `scriptHash` of the
//!   script it showed (`watch::script_hash`: folder, NUL, script), and a hash that doesn't match
//!   the stored script is refused. A follow-up's script never changes under its id today; the
//!   check keeps "what was shown is what runs" from resting on that.
//! - **Designation is a gate.** Only a designated tab's scripts are served or answerable.
//! - **A backend write needs a frontend event:** the window's follow-ups store mirrors its tabs'
//!   lists, so every write here is announced as `follow-ups-changed`, like the runner's.

use crate::state::persistence::save_state;
use crate::state::workspace::FollowUp;
use crate::state::AppState;
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};

/// Is this follow-up a watch script waiting for a human to approve it? The Rust twin of
/// `needsApproval` (src/lib/followUps/model.ts), plus "not expired": an expired one will never run
/// and the desktop card offers no approve for it either.
fn awaits_approval(f: &FollowUp, unattended: bool, now_ms: i64) -> bool {
    f.due.kind == "script"
        && f.due.met_at.is_none()
        && !f.due.approved
        && !unattended
        && f.due.script.is_some()
        && f.due.cwd.is_some()
        && !f.expires_at.as_deref()
            .map(super::transcript::rfc3339_to_ms)
            .is_some_and(|t| t > 0 && t < now_ms)
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

fn designated_set(app: &AppState) -> HashSet<String> {
    super::designated_tabs(app).into_iter().map(|m| m.tab_id).collect()
}

/// tab id → how many of its watch scripts wait for approval, designated tabs only.
/// `Chat.scriptsWaiting` on the roster; the WS ticker diffs it like `asks`.
pub(crate) fn waiting_by_tab(app: &AppState) -> HashMap<String, usize> {
    let designated = designated_set(app);
    let data = app.app_data.read();
    let unattended = data.preferences.follow_ups_scripts_unattended;
    let now = now_ms();
    let mut out = HashMap::new();
    for ws in data.windows.iter().flat_map(|w| w.workspaces.iter()) {
        for tab in ws.panes.iter().flat_map(|p| p.tabs.iter()) {
            if !designated.contains(&tab.id) {
                continue;
            }
            let n = tab.follow_ups.iter().filter(|f| awaits_approval(f, unattended, now)).count();
            if n > 0 {
                out.insert(tab.id.clone(), n);
            }
        }
    }
    out
}

/// One waiting script, as the phone's card draws it (`ScriptApproval`, protocol §4.3).
fn to_json(f: &FollowUp) -> Value {
    let script = f.due.script.as_deref().unwrap_or("");
    let cwd = f.due.cwd.as_deref().unwrap_or("");
    json!({
        "id": f.id,
        "label": f.due.label,
        "script": script,
        "folder": cwd,
        "everySecs": f.due.every_secs.unwrap_or(60),
        "timeoutSecs": f.due.timeout_secs.unwrap_or(10),
        "message": f.text,
        "author": f.author,
        "createdAt": f.created_at,
        "expiresAt": f.expires_at,
        "scriptHash": crate::watch::script_hash(cwd, script),
    })
}

/// A tab's waiting scripts, oldest first. The caller has checked designation (chat_detail).
pub(crate) fn waiting_for_tab(app: &AppState, tab_id: &str) -> Vec<Value> {
    let data = app.app_data.read();
    let unattended = data.preferences.follow_ups_scripts_unattended;
    let now = now_ms();
    data.windows
        .iter()
        .flat_map(|w| w.workspaces.iter())
        .flat_map(|ws| ws.panes.iter())
        .flat_map(|p| p.tabs.iter())
        .find(|t| t.id == tab_id)
        .map(|t| t.follow_ups.iter().filter(|f| awaits_approval(f, unattended, now)).map(to_json).collect())
        .unwrap_or_default()
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) enum Decision {
    Approve,
    Reject,
}

/// What a decision did: the window and workspace to announce it to, and the tab's list now.
pub(crate) struct Decided {
    pub window_label: String,
    pub workspace_id: String,
    pub follow_ups: Vec<FollowUp>,
}

#[derive(Debug, PartialEq)]
pub(crate) enum Refusal {
    /// No such designated tab: 404, like every other chat route.
    NotFound,
    /// The script isn't waiting any more, or isn't the one the phone showed. The sentence is
    /// shown to the human verbatim.
    Stale(&'static str),
}

/// The human approved or rejected a waiting watch script from the phone. Reject cancels it, as the
/// desktop's Reject does.
pub(crate) fn decide(
    app: &AppState,
    tab_id: &str,
    follow_up_id: &str,
    script_hash: &str,
    decision: Decision,
) -> Result<Decided, Refusal> {
    // Before the write lock: designated_tabs takes its own read, and parking_lot reads are not
    // reentrant when a writer is queued.
    if !designated_set(app).contains(tab_id) {
        return Err(Refusal::NotFound);
    }
    let mut data = app.app_data.write();
    let unattended = data.preferences.follow_ups_scripts_unattended;
    let now = now_ms();
    let mut found: Option<(String, String, String, String)> = None; // label, ws, script, cwd
    'find: for w in data.windows.iter_mut() {
        for ws in w.workspaces.iter_mut() {
            let Some(tab) = ws.panes.iter_mut().flat_map(|p| p.tabs.iter_mut()).find(|t| t.id == tab_id) else {
                continue;
            };
            let Some(pos) = tab.follow_ups.iter().position(|f| f.id == follow_up_id) else {
                return Err(Refusal::Stale("That watch script is no longer on this chat: it was approved, rejected or cancelled."));
            };
            let f = &tab.follow_ups[pos];
            if !awaits_approval(f, unattended, now) {
                return Err(Refusal::Stale("That watch script isn't waiting for approval any more."));
            }
            let (script, cwd) = (f.due.script.clone().unwrap_or_default(), f.due.cwd.clone().unwrap_or_default());
            if crate::watch::script_hash(&cwd, &script) != script_hash {
                return Err(Refusal::Stale("The script changed since you read it. Refresh and read it again."));
            }
            match decision {
                Decision::Approve => tab.follow_ups[pos].due.approved = true,
                Decision::Reject => {
                    tab.follow_ups.remove(pos);
                }
            }
            found = Some((w.label.clone(), ws.id.clone(), script, cwd));
            break 'find;
        }
    }
    let Some((window_label, workspace_id, script, cwd)) = found else {
        return Err(Refusal::NotFound);
    };
    if decision == Decision::Approve {
        crate::watch::remember_approval(&mut data, &cwd, &script);
    }
    let follow_ups = data
        .windows
        .iter()
        .flat_map(|w| w.workspaces.iter())
        .flat_map(|ws| ws.panes.iter())
        .flat_map(|p| p.tabs.iter())
        .find(|t| t.id == tab_id)
        .map(|t| t.follow_ups.clone())
        .unwrap_or_default();
    let data_clone = data.clone();
    drop(data);
    if let Err(e) = save_state(&data_clone) {
        log::error!("[maiLink] saving a watch-script decision failed: {e}");
    }
    log::info!(
        "[maiLink] watch script {} on tab {} {} from the phone",
        &follow_up_id[..8.min(follow_up_id.len())],
        &tab_id[..8.min(tab_id.len())],
        if decision == Decision::Approve { "approved" } else { "rejected" },
    );
    Ok(Decided { window_label, workspace_id, follow_ups })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::workspace::FollowUpDue;

    fn script(approved: bool) -> FollowUp {
        FollowUp {
            id: "f1".into(),
            text: "check CI".into(),
            due: FollowUpDue {
                kind: "script".into(),
                script: Some("gh pr checks 12".into()),
                cwd: Some("/repo".into()),
                every_secs: Some(90),
                approved,
                ..Default::default()
            },
            author: "agent".into(),
            created_at: "2026-10-02T10:00:00Z".into(),
            expires_at: None,
        }
    }

    #[test]
    fn only_unapproved_unmet_unexpired_scripts_wait() {
        let now = 1_900_000_000_000;
        assert!(awaits_approval(&script(false), false, now));
        assert!(!awaits_approval(&script(true), false, now));
        // The waiver releases it: nothing to ask.
        assert!(!awaits_approval(&script(false), true, now));
        let mut met = script(false);
        met.due.met_at = Some("2026-10-02T10:05:00Z".into());
        assert!(!awaits_approval(&met, false, now));
        let mut expired = script(false);
        expired.expires_at = Some("2026-01-01T00:00:00Z".into());
        assert!(!awaits_approval(&expired, false, now));
        let mut timed = script(false);
        timed.due.kind = "at".into();
        assert!(!awaits_approval(&timed, false, now));
    }

    #[test]
    fn the_card_carries_the_hash_of_what_it_shows() {
        let v = to_json(&script(false));
        assert_eq!(v["scriptHash"], crate::watch::script_hash("/repo", "gh pr checks 12"));
        assert_eq!(v["everySecs"], 90);
        assert_eq!(v["expiresAt"], Value::Null);
    }
}
