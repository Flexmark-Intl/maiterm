//! The Overlord engine as the phone sees it (docs/mailink-protocol.md §13).
//!
//! The engine (docs/overlord.md) is a per-WINDOW store in the desktop webview: escalations,
//! proposals, agent reports, outstanding directives, ritual progress — none of it is in Rust,
//! so this server cannot read it. And a webview whose screen is asleep throttles its timers,
//! which is exactly when a phone is in use. So the phone is never served by asking the webview:
//! the engine PUBLISHES a snapshot here on change (`publish_overlord_snapshot`, precedent
//! `append_overlord_ledger`), and REST/WS serve the last one.
//!
//! That makes `asOf` load-bearing. The snapshot may be an hour old because the desktop slept;
//! the field is the only thing that says so, and the phone renders its age (staleness is
//! `(ts − asOf) + elapsed since receipt` — both stamps are this desktop's clock, so phone-clock
//! skew never masquerades as age). A mirror with no stamp would be absence read as a claim.
//!
//! Designation gates this too: rows about a tab marked "unavailable in maiLink" are dropped
//! here, whatever the frontend sent — an escalation's `detail` is agent text.

use crate::state::AppState;
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};

/// Snapshot arrays whose rows name a tab. Every row here is gated on that tab's designation.
const TAB_SCOPED: [&str; 6] = [
    "escalations",
    "proposals",
    "agentReports",
    "outstandingDirectives",
    "ritualProgress",
    "spentTabs",
];

fn now_ms() -> u64 {
    super::now_ms()
}

fn escalation_ids(snapshot: &Value) -> HashSet<String> {
    snapshot["escalations"]
        .as_array()
        .map(|a| {
            a.iter()
                .filter_map(|e| e["id"].as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default()
}

/// Store a window's snapshot: gate it, stamp it, and queue a doorbell for every escalation that
/// was not in the previous one. Frontend-owned shape otherwise — passed through verbatim so a
/// field the engine grows reaches the phone without a Rust change.
///
/// Stamps: `windowLabel` (Overlord is per window; several can exist), `version` (monotonic per
/// window — the WS ticker diffs on it), `receivedAt` (this clock, for the doc's staleness rule).
/// `asOf` is the frontend's, set when it BUILT the snapshot, and is not touched.
pub(crate) fn publish(app: &AppState, label: &str, mut snapshot: Value) {
    // Designation set and titles come from `designated_tabs`, which takes its own read of
    // state — so before any lock of ours, and before `app_data` below.
    let metas = super::designated_tabs(app);
    let designated: HashSet<&str> = metas.iter().map(|m| m.tab_id.as_str()).collect();
    let titles: HashMap<&str, &str> = metas.iter().map(|m| (m.tab_id.as_str(), m.title.as_str())).collect();

    if let Some(obj) = snapshot.as_object_mut() {
        for key in TAB_SCOPED {
            if let Some(rows) = obj.get_mut(key).and_then(Value::as_array_mut) {
                rows.retain(|row| match row["tabId"].as_str() {
                    // "" is how the engine spells "no tab" (an escalation raised by the board
                    // itself); a row that names no tab is about the window, and stays.
                    Some(t) if !t.is_empty() => designated.contains(t),
                    _ => true,
                });
            }
        }
        // Two non-array carriers of tab-scoped content, missed by the array sweep in review:
        // a pending rule-change batch is one OBJECT whose `rationale` is up to 1000 chars of
        // agent prose about a tab, and a scan's `silent` list is bare tab ids.
        // (`Map` indexing panics on a missing key; the frontend may omit the field.)
        let hide_batch = obj
            .get("pendingRuleChanges")
            .and_then(|b| b.get("tabId"))
            .and_then(Value::as_str)
            .is_some_and(|t| !t.is_empty() && !designated.contains(t));
        if hide_batch {
            obj.insert("pendingRuleChanges".to_string(), Value::Null);
        }
        if let Some(silent) = obj.get_mut("lastScan").and_then(|s| s.get_mut("silent")).and_then(Value::as_array_mut) {
            silent.retain(|id| id.as_str().is_some_and(|t| designated.contains(t)));
        }
    }

    // A closed window's snapshot would otherwise linger with an ever-older `asOf` — honest,
    // but noise. Prune to the windows that exist. And a window with NOTHING designated is not
    // the phone's to see at all: its engine still runs and publishes (every window's does),
    // but nothing in it can be opened from the phone, so it is dropped rather than served as
    // an empty board. (One read of app_data, dropped before our write below.)
    let (live, this_window_exposed) = {
        let data = app.app_data.read();
        let live: HashSet<String> = data.windows.iter().map(|w| w.label.clone()).collect();
        let exposed = data.windows.iter().find(|w| w.label == label).is_some_and(|w| {
            w.workspaces
                .iter()
                .flat_map(|ws| ws.panes.iter())
                .flat_map(|p| p.tabs.iter())
                .any(|t| designated.contains(t.id.as_str()))
        });
        (live, exposed)
    };
    // Rings only reach a phone while maiLink runs; queued while it is off they would burst
    // out, hours stale, on the next enable. The loop also discards leftovers when it starts.
    let mailink_on = app.mailink_info.read().is_some();

    let new_rings: Vec<(String, String)>;
    {
        let mut snaps = app.overlord_snapshots.write();
        snaps.retain(|k, _| live.contains(k) && (k != label || this_window_exposed));
        if !this_window_exposed {
            return;
        }
        let prev = snaps.get(label);
        let version = prev.and_then(|p| p["version"].as_u64()).unwrap_or(0) + 1;
        let prev_ids = prev.map(escalation_ids).unwrap_or_default();

        // Every human-addressed escalation the phone has not been told about yet. Rung from the
        // doorbell loop (which owns coverage and the relay client), not here: if a phone holds
        // the WS it gets the snapshot frame and no push, same rule as every other ring.
        new_rings = snapshot["escalations"]
            .as_array()
            .map(|a| {
                a.iter()
                    .filter(|e| e["id"].as_str().is_some_and(|id| !prev_ids.contains(id)))
                    .map(|e| {
                        let tab = e["tabId"].as_str().unwrap_or_default();
                        // The push carries a title and a collapse key; a window-level
                        // escalation has no tab, so it collapses under the board itself.
                        match titles.get(tab) {
                            Some(title) => (tab.to_string(), (*title).to_string()),
                            None => ("overlord".to_string(), "Overlord".to_string()),
                        }
                    })
                    .collect()
            })
            .unwrap_or_default();

        snapshot["windowLabel"] = json!(label);
        snapshot["version"] = json!(version);
        snapshot["receivedAt"] = json!(now_ms());
        snaps.insert(label.to_string(), snapshot);
    }
    if mailink_on && !new_rings.is_empty() {
        let mut q = app.mailink_pending_rings.lock();
        q.extend(new_rings);
        // Bounded: the doorbell drains every 2 s, so anything past this is a loop that isn't
        // running, and a burst of stale pushes is worse than a dropped one.
        if q.len() > MAX_PENDING_RINGS {
            let drop = q.len() - MAX_PENDING_RINGS;
            q.drain(..drop);
        }
    }
}

const MAX_PENDING_RINGS: usize = 32;

/// `GET /overlord`: every window's last snapshot, stable order. `[]` until the first publish —
/// a phone that sees an empty list on a desktop it knows has Overlord is looking at a webview
/// that has not published yet (asleep, or the engine is off), not at an empty board.
pub(crate) fn snapshots(app: &AppState) -> Value {
    let snaps = app.overlord_snapshots.read();
    let mut labels: Vec<&String> = snaps.keys().collect();
    labels.sort();
    json!({ "windows": labels.iter().map(|l| snaps[*l].clone()).collect::<Vec<_>>() })
}

/// WS `overlord` frames for every window whose version moved since this socket last looked —
/// the full snapshot inline (a signal-then-GET would double the window in which the phone acts
/// on something stale). `seen` is per socket: empty on connect, so the first tick is the
/// baseline. A window that vanished gets one frame with `window: null` — a stated absence.
pub(crate) fn changed_frames(app: &AppState, seen: &mut HashMap<String, u64>) -> Vec<Value> {
    let snaps = app.overlord_snapshots.read();
    let mut out = Vec::new();
    let mut labels: Vec<&String> = snaps.keys().collect();
    labels.sort();
    for label in labels {
        let snap = &snaps[label];
        let version = snap["version"].as_u64().unwrap_or(0);
        if seen.get(label) == Some(&version) {
            continue;
        }
        seen.insert(label.clone(), version);
        out.push(json!({ "type": "overlord", "windowLabel": label, "window": snap, "ts": now_ms() }));
    }
    let gone: Vec<String> = seen.keys().filter(|k| !snaps.contains_key(*k)).cloned().collect();
    for label in gone {
        seen.remove(&label);
        out.push(json!({ "type": "overlord", "windowLabel": label, "window": Value::Null, "ts": now_ms() }));
    }
    out
}

/// Drain the escalation rings queued by `publish`, for the doorbell loop.
pub(crate) fn take_pending_rings(app: &AppState) -> Vec<(String, String)> {
    std::mem::take(&mut *app.mailink_pending_rings.lock())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::workspace::{Tab, WindowData, Workspace};
    use crate::state::AgentRuntime;

    /// One window "main" with a designated tab and an excluded one; maiLink "running".
    fn fixture() -> (AppState, String, String) {
        let app = AppState::new();
        *app.mailink_info.write() = Some(("test".into(), 1));
        let (shown, hidden) = {
            let mut data = app.app_data.write();
            data.preferences.mailink_expose_all = true;
            let mut win = WindowData::new("main".into());
            let mut ws = Workspace::new("Proj".into());
            ws.panes[0].tabs[0].runtime = Some(AgentRuntime::Claude);
            ws.panes[0].tabs[0].name = "worker".into();
            let shown = ws.panes[0].tabs[0].id.clone();
            let mut secret = Tab::new("client-secrets".into());
            secret.runtime = Some(AgentRuntime::Claude);
            secret.mailink_excluded = true;
            let hidden = secret.id.clone();
            ws.panes[0].tabs.push(secret);
            win.workspaces.push(ws);
            data.windows.push(win);
            (shown, hidden)
        };
        (app, shown, hidden)
    }

    fn esc(id: &str, tab: &str) -> Value {
        json!({ "id": id, "ts": 1, "tabId": tab, "workspaceId": "w", "ruleId": null,
                "kind": "blocked", "detail": "agent said something", "read": false })
    }

    #[test]
    fn rows_about_an_excluded_tab_never_reach_the_mirror_and_window_rows_stay() {
        let (app, shown, hidden) = fixture();
        publish(&app, "main", json!({
            "asOf": 5, "running": true,
            "escalations": [esc("e1", &shown), esc("e2", &hidden), esc("e3", "")],
            "proposals": [{ "id": "p1", "tabId": hidden, "preview": "rm -rf" }],
            "agentReports": [{ "tabId": shown, "summary": "ok" }],
        }));
        let s = snapshots(&app);
        let w = &s["windows"][0];
        let text = w.to_string();
        assert!(!text.contains(&hidden), "an excluded tab's id leaked");
        assert!(!text.contains("rm -rf"), "an excluded tab's proposal leaked");
        let ids: Vec<&str> = w["escalations"].as_array().unwrap().iter().map(|e| e["id"].as_str().unwrap()).collect();
        assert_eq!(ids, vec!["e1", "e3"], "the designated tab's row and the window-level row survive");
        assert_eq!(w["agentReports"].as_array().unwrap().len(), 1);
        assert_eq!(w["windowLabel"], "main");
        assert_eq!(w["version"], 1);
        assert_eq!(w["asOf"], 5, "the frontend's build stamp is passed through untouched");
        assert!(w["receivedAt"].as_u64().unwrap() > 0);
    }

    #[test]
    fn the_non_array_carriers_are_gated_too_and_an_unexposed_window_is_not_served() {
        let (app, shown, hidden) = fixture();
        publish(&app, "main", json!({
            "escalations": [],
            "pendingRuleChanges": { "id": "b1", "tabId": hidden, "rationale": "client-payroll sat at the Stripe-key prompt", "changes": [] },
            "lastScan": { "at": 1, "tabsSeen": 2, "silent": [shown, hidden] },
        }));
        let w = snapshots(&app)["windows"][0].clone();
        assert!(w["pendingRuleChanges"].is_null(), "a batch about an excluded tab is a stated null");
        assert!(!w.to_string().contains("Stripe"), "the rationale leaked");
        assert_eq!(w["lastScan"]["silent"], json!([shown]), "excluded tab ids are dropped from the scan summary");
        // A batch about a VISIBLE tab passes through untouched.
        publish(&app, "main", json!({ "escalations": [], "pendingRuleChanges": { "id": "b2", "tabId": shown, "rationale": "ok", "changes": [] } }));
        assert_eq!(snapshots(&app)["windows"][0]["pendingRuleChanges"]["id"], "b2");

        // A second window with nothing designated publishes too (every window's engine runs)
        // — and must not appear, even as an empty board.
        {
            let mut data = app.app_data.write();
            let mut win = WindowData::new("personal".into());
            win.workspaces.push(Workspace::new("Home".into())); // plain shell tab, no runtime
            data.windows.push(win);
        }
        publish(&app, "personal", json!({ "running": true, "escalations": [esc("e9", "")], "pendingRuleChanges": { "id": "b3", "tabId": "", "rationale": "secret" } }));
        let labels: Vec<String> = snapshots(&app)["windows"].as_array().unwrap().iter().map(|w| w["windowLabel"].as_str().unwrap().to_string()).collect();
        assert_eq!(labels, vec!["main"], "a window with no designated tab is absent, not empty");
        assert!(take_pending_rings(&app).is_empty(), "and it rings nothing");
    }

    #[test]
    fn rings_are_not_queued_while_mailink_is_off() {
        let (app, shown, _) = fixture();
        *app.mailink_info.write() = None;
        publish(&app, "main", json!({ "escalations": [esc("e1", &shown)] }));
        assert!(take_pending_rings(&app).is_empty(), "no loop will drain them; queuing would burst on re-enable");
    }

    #[test]
    fn a_new_escalation_queues_one_ring_and_a_repeat_queues_none() {
        let (app, shown, hidden) = fixture();
        publish(&app, "main", json!({ "escalations": [esc("e1", &shown), esc("e2", &hidden), esc("e3", "")] }));
        let rings: HashSet<(String, String)> = take_pending_rings(&app).into_iter().collect();
        let want: HashSet<(String, String)> =
            [("overlord".to_string(), "Overlord".to_string()), (shown.clone(), "worker".to_string())].into();
        assert_eq!(
            rings, want,
            "one ring per NEW visible escalation: the tab's title for a tab, the board for none; the excluded tab's is dropped"
        );
        assert!(take_pending_rings(&app).is_empty(), "drained");
        // Same escalations again — nothing is new, nothing rings, the version still moves.
        publish(&app, "main", json!({ "escalations": [esc("e1", &shown), esc("e3", "")] }));
        assert!(take_pending_rings(&app).is_empty());
        assert_eq!(snapshots(&app)["windows"][0]["version"], 2);
        // One more arrives → exactly one ring.
        publish(&app, "main", json!({ "escalations": [esc("e1", &shown), esc("e3", ""), esc("e4", &shown)] }));
        assert_eq!(take_pending_rings(&app).len(), 1);
    }

    #[test]
    fn the_socket_sees_a_baseline_then_only_changes_then_a_stated_removal() {
        let (app, shown, _) = fixture();
        let mut seen = HashMap::new();
        assert!(changed_frames(&app, &mut seen).is_empty(), "nothing published yet, nothing to say");
        publish(&app, "main", json!({ "escalations": [esc("e1", &shown)] }));
        let frames = changed_frames(&app, &mut seen);
        assert_eq!(frames.len(), 1);
        assert_eq!(frames[0]["type"], "overlord");
        assert_eq!(frames[0]["window"]["version"], 1, "the snapshot rides INLINE, full replace");
        assert!(changed_frames(&app, &mut seen).is_empty(), "unchanged → silent");
        publish(&app, "main", json!({ "escalations": [] }));
        assert_eq!(changed_frames(&app, &mut seen)[0]["window"]["version"], 2);
        // The window closes: the next publish from any window prunes it; the socket is told.
        app.app_data.write().windows[0].label = "other".into();
        {
            let mut data = app.app_data.write();
            let mut win = WindowData::new("second".into());
            win.workspaces.push(Workspace::new("X".into()));
            data.windows.push(win);
        }
        publish(&app, "second", json!({ "escalations": [] }));
        let frames = changed_frames(&app, &mut seen);
        let removed = frames.iter().find(|f| f["windowLabel"] == "main").expect("the closed window is announced");
        assert!(removed["window"].is_null(), "a stated null, not a missing window");
        assert!(!seen.contains_key("main"));
    }
}
