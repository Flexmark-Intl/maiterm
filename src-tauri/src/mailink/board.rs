//! maiTerm tasks (docs/tasks.md) as the phone sees them.
//!
//! This REPLACED the Claude session task board on the wire (mailink-protocol v0.5). That board
//! (`~/.claude/tasks/<sid>/`) is demoted to an importer input in docs/tasks.md — maiTerm's own
//! `Workspace.tasks` is the source of truth, and for a Claude tab with the importer running the
//! two carried the same work through two pipes, so serving both would have shown it twice and
//! left the phone to guess which to trust.
//!
//! Read straight from Rust state: the frontend `tasks` store owns the editing surface and
//! persists whole lists per workspace (`set_workspace_tasks`), so what is here is at most one
//! persist behind the board. WRITES do not come through here — they go back to the frontend
//! store via the maiLink frontend request (see `frontend_rpc`), because a row written into Rust
//! state directly would be clobbered by that store's next whole-list persist.
//!
//! Every field is always present and absence is a stated `null` — the contract rule from
//! mailink-protocol v0.4 (a consumer that never receives a field concludes something).

use crate::state::workspace::{Task, Workspace};
use crate::state::AppState;
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};
use std::hash::{Hash, Hasher};

/// Task ids parked on this workspace's ARCHIVED tabs. They are out of `Workspace.tasks` but
/// not gone, and `effective_status` needs the distinction (see there).
fn parked_ids(ws: &Workspace) -> HashSet<&str> {
    ws.archived_tabs
        .iter()
        .flat_map(|tab| tab.archived_tasks.iter().map(|t| t.id.as_str()))
        .collect()
}

/// The status the desktop DISPLAYS, which is not always the one it stores. Mirror of
/// `effectiveStatus` in src/lib/tasks/model.ts — change the two together:
///
/// An unfinished task with an unmet dependency renders as `blocked` whatever its stored lane
/// says, so a dependency chain is visible without anyone restating it. A `blocked_by` id that
/// resolves to nothing is treated as MET (a deleted prerequisite must not wedge its dependents
/// forever) — unless it is parked on an archived tab, which is not deleted, only off the list,
/// and treating that as met silently unblocked every dependent the moment a tab was archived.
///
/// This crosses the wire as its own field because the phone cannot derive it: it would need
/// the workspace's whole list AND the parked set, and it receives neither with a tab's rows.
/// Serving only the stored status made a task the desktop shows as blocked arrive as `todo`.
fn effective_status<'a>(t: &'a Task, ws: &Workspace, parked: &HashSet<&str>) -> &'a str {
    if t.status == "done" {
        return "done";
    }
    let unmet = t.blocked_by.iter().any(|id| match ws.tasks.iter().find(|d| &d.id == id) {
        Some(dep) => dep.status != "done",
        None => parked.contains(id.as_str()),
    });
    if unmet {
        "blocked"
    } else {
        &t.status
    }
}

/// One task row for the phone. `tab_title`/`workstream` are the display names the ids resolve
/// to on THIS desktop right now — a tab id is not durable across a reload
/// (docs/tasks.md), so the phone must never key anything on `tabId` alone. `status` is the
/// STORED lane (what a phone edit writes back); `effectiveStatus` is what to RENDER.
fn task_view(t: &Task, effective: &str, tab_title: Option<&str>, workstream: Option<&str>) -> Value {
    json!({
        "id": t.id,
        "title": t.title,
        "detail": t.detail,
        "status": t.status,
        "effectiveStatus": effective,
        "tabId": t.tab_id,
        "tabTitle": tab_title,
        "blockedBy": t.blocked_by,
        "origin": t.origin,
        "createdAt": t.created_at,
        "updatedAt": t.updated_at,
        "workstreamId": t.workstream_id,
        "workstream": workstream,
        "topicId": t.topic_id,
    })
}

/// tab id → tab name across every window, so an assignee renders as a title rather than a uuid.
/// Live tabs only: an ARCHIVED tab's rows are parked on the tab (`archived_tasks`), out of
/// `Workspace.tasks` by design, so they never reach this module in the first place.
fn tab_titles(data: &crate::state::workspace::AppData) -> HashMap<&str, &str> {
    let mut out = HashMap::new();
    for win in &data.windows {
        for ws in &win.workspaces {
            for pane in &ws.panes {
                for tab in &pane.tabs {
                    out.insert(tab.id.as_str(), tab.name.as_str());
                }
            }
        }
    }
    out
}

/// The tasks assigned to one tab, in board order. Empty when the tab owns none — the caller
/// serializes that as `[]`, never omits it.
pub(crate) fn tasks_for_tab(app: &AppState, tab_id: &str) -> Vec<Value> {
    let data = app.app_data.read();
    let titles = tab_titles(&data);
    let mut out = Vec::new();
    for win in &data.windows {
        for ws in &win.workspaces {
            let parked = parked_ids(ws);
            for t in ws.tasks.iter().filter(|t| t.tab_id.as_deref() == Some(tab_id)) {
                let stream = t
                    .workstream_id
                    .as_deref()
                    .and_then(|id| ws.workstreams.iter().find(|w| w.id == id))
                    .map(|w| w.name.as_str());
                out.push(task_view(
                    t,
                    effective_status(t, ws, &parked),
                    titles.get(tab_id).copied(),
                    stream,
                ));
            }
        }
    }
    out
}

/// Cheap change key for one tab's rows, for the WS `tasks` event. `None` when the tab owns no
/// rows, so the streamer can tell "emptied" from "never had any" the same way the old
/// session-board key did. Hashes only what the phone renders — a persist that rewrites
/// `normalized_title` without changing anything visible must not wake every socket.
pub(crate) fn tab_change_key(app: &AppState, tab_id: &str) -> Option<u64> {
    let data = app.app_data.read();
    let mut h = std::collections::hash_map::DefaultHasher::new();
    let mut any = false;
    for win in &data.windows {
        for ws in &win.workspaces {
            let parked = parked_ids(ws);
            for t in ws.tasks.iter().filter(|t| t.tab_id.as_deref() == Some(tab_id)) {
                any = true;
                t.id.hash(&mut h);
                t.title.hash(&mut h);
                t.detail.hash(&mut h);
                t.status.hash(&mut h);
                // The rendered status depends on OTHER rows: a blocker finishing on another
                // tab changes this tab's lock, and nothing on this tab's own rows moved.
                effective_status(t, ws, &parked).hash(&mut h);
                t.blocked_by.hash(&mut h);
                t.updated_at.hash(&mut h);
                t.workstream_id.hash(&mut h);
            }
        }
    }
    any.then(|| h.finish())
}

/// The whole board: every workspace in every window, with its workstreams and tasks — the
/// Overlord board's workstream index (docs/overlord.md §11) as data. Unassigned rows
/// (`tabId: null`) are the workspace backlog. Workspaces with no tasks are included so the
/// phone can offer "add a task here"; the Overlord workspace itself is flagged rather than
/// filtered, since the phone decides what to show.
pub(crate) fn board(app: &AppState) -> Value {
    let data = app.app_data.read();
    let titles = tab_titles(&data);
    let mut workspaces = Vec::new();
    for win in &data.windows {
        for ws in &win.workspaces {
            let name_of = |id: Option<&str>| {
                id.and_then(|id| ws.workstreams.iter().find(|w| w.id == id))
                    .map(|w| w.name.as_str())
            };
            let parked = parked_ids(ws);
            workspaces.push(json!({
                "workspaceId": ws.id,
                "workspace": ws.name,
                "windowLabel": win.label,
                "overlord": ws.overlord,
                "suspended": ws.suspended,
                "workstreams": ws.workstreams.iter().map(|w| json!({ "id": w.id, "name": w.name })).collect::<Vec<_>>(),
                "tasks": ws.tasks.iter().map(|t| task_view(
                    t,
                    effective_status(t, ws, &parked),
                    t.tab_id.as_deref().and_then(|id| titles.get(id).copied()),
                    name_of(t.workstream_id.as_deref()),
                )).collect::<Vec<_>>(),
            }));
        }
    }
    json!({ "workspaces": workspaces })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::workspace::{WindowData, Workspace, Workstream};

    fn task(id: &str, title: &str, tab: Option<&str>, stream: Option<&str>) -> Task {
        Task {
            id: id.into(),
            title: title.into(),
            normalized_title: Task::normalize_title(title),
            detail: None,
            status: "todo".into(),
            tab_id: tab.map(Into::into),
            blocked_by: Vec::new(),
            origin: "human".into(),
            created_at: "2026-09-06T00:00:00Z".into(),
            updated_at: "2026-09-06T00:00:00Z".into(),
            workstream_id: stream.map(Into::into),
            topic_id: None,
        }
    }

    /// One window, one workspace whose auto-created tab is the assignee of two rows, plus one
    /// unassigned backlog row and one row on a tab that doesn't exist any more.
    fn fixture() -> (AppState, String) {
        let app = AppState::new();
        let tab_id = {
            let mut data = app.app_data.write();
            let mut win = WindowData::new("main".into());
            let mut ws = Workspace::new("Proj".into());
            let tab_id = ws.panes[0].tabs[0].id.clone();
            ws.panes[0].tabs[0].name = "worker".into();
            ws.workstreams.push(Workstream {
                id: "ws-1".into(),
                name: "Auth refactor".into(),
                normalized_name: "auth refactor".into(),
                created_at: String::new(),
                updated_at: String::new(),
            });
            ws.tasks.push(task("t1", "Add the guard", Some(&tab_id), Some("ws-1")));
            ws.tasks.push(task("t2", "Write the test", Some(&tab_id), None));
            ws.tasks.push(task("t3", "Someday", None, None));
            ws.tasks.push(task("t4", "Orphan", Some("gone-tab"), None));
            win.workspaces.push(ws);
            data.windows.push(win);
            tab_id
        };
        (app, tab_id)
    }

    #[test]
    fn a_tabs_rows_come_back_with_every_field_stated() {
        let (app, tab) = fixture();
        let rows = tasks_for_tab(&app, &tab);
        assert_eq!(rows.len(), 2, "only this tab's rows, not the backlog or another tab's");
        let t1 = rows.iter().find(|r| r["id"] == "t1").unwrap();
        assert_eq!(t1["tabTitle"], "worker");
        assert_eq!(t1["workstream"], "Auth refactor");
        // Absence is a stated null, never a missing key — the phone must not infer.
        let t2 = rows.iter().find(|r| r["id"] == "t2").unwrap();
        assert!(t2["workstream"].is_null() && t2.get("workstream").is_some());
        assert!(t2["detail"].is_null() && t2.get("detail").is_some());
        assert!(t2["topicId"].is_null() && t2.get("topicId").is_some());
        assert!(tasks_for_tab(&app, "nobody").is_empty());
    }

    #[test]
    fn the_change_key_moves_for_this_tab_only_and_says_none_when_emptied() {
        let (app, tab) = fixture();
        let k0 = tab_change_key(&app, &tab).expect("tab owns rows");
        assert_eq!(tab_change_key(&app, "nobody"), None);
        // A status change on a row of another tab must not wake this tab's socket.
        {
            let mut data = app.app_data.write();
            let ws = &mut data.windows[0].workspaces[0];
            ws.tasks.iter_mut().find(|t| t.id == "t4").unwrap().status = "done".into();
        }
        assert_eq!(tab_change_key(&app, &tab), Some(k0));
        {
            let mut data = app.app_data.write();
            let ws = &mut data.windows[0].workspaces[0];
            ws.tasks.iter_mut().find(|t| t.id == "t1").unwrap().status = "done".into();
        }
        assert_ne!(tab_change_key(&app, &tab), Some(k0));
        {
            let mut data = app.app_data.write();
            data.windows[0].workspaces[0].tasks.retain(|t| t.tab_id.as_deref() != Some(tab.as_str()));
        }
        assert_eq!(tab_change_key(&app, &tab), None, "emptied reads as None so the streamer emits [] once");
    }

    #[test]
    fn the_rendered_status_is_derived_like_the_desktop_does_and_moves_the_key() {
        use crate::state::workspace::Tab;
        let app = AppState::new();
        let (tab_a, tab_b) = {
            let mut data = app.app_data.write();
            let mut win = WindowData::new("main".into());
            let mut ws = Workspace::new("Proj".into());
            let tab_a = ws.panes[0].tabs[0].id.clone();
            let b = Tab::new("b".into());
            let tab_b = b.id.clone();
            ws.panes[0].tabs.push(b);
            // A prerequisite parked on an archived tab: off the list, not gone.
            let mut archived = Tab::new("old".into());
            archived.archived_tasks.push(task("parked", "Migrate schema", None, None));
            ws.archived_tabs.push(archived);

            ws.tasks.push(task("blocker", "Land the API", Some(&tab_b), None)); // todo, on B
            let mut done_dep = task("finished", "Old dep", Some(&tab_b), None);
            done_dep.status = "done".into();
            ws.tasks.push(done_dep);
            let mut waits = task("waits", "Wire the UI", Some(&tab_a), None);
            waits.blocked_by = vec!["blocker".into()];
            ws.tasks.push(waits);
            let mut freed = task("freed", "Ship it", Some(&tab_a), None);
            freed.blocked_by = vec!["finished".into()];
            ws.tasks.push(freed);
            let mut orphaned = task("orphaned", "Depends on nothing now", Some(&tab_a), None);
            orphaned.blocked_by = vec!["deleted-long-ago".into()];
            ws.tasks.push(orphaned);
            let mut parked_dep = task("parked-dep", "Needs the migration", Some(&tab_a), None);
            parked_dep.blocked_by = vec!["parked".into()];
            ws.tasks.push(parked_dep);
            let mut done_anyway = task("done-anyway", "Already shipped", Some(&tab_a), None);
            done_anyway.status = "done".into();
            done_anyway.blocked_by = vec!["blocker".into()];
            ws.tasks.push(done_anyway);
            win.workspaces.push(ws);
            data.windows.push(win);
            (tab_a, tab_b)
        };
        let rows = tasks_for_tab(&app, &tab_a);
        let eff = |id: &str| rows.iter().find(|r| r["id"] == id).unwrap()["effectiveStatus"].clone();
        // Stored `todo`, shown `blocked` — the desktop's dependency override, cross-tab.
        assert_eq!(eff("waits"), "blocked");
        assert_eq!(rows.iter().find(|r| r["id"] == "waits").unwrap()["status"], "todo", "the stored lane is untouched");
        assert_eq!(eff("freed"), "todo", "a finished prerequisite gates nothing");
        assert_eq!(eff("orphaned"), "todo", "an id that resolves to nothing is MET, not a permanent wedge");
        assert_eq!(eff("parked-dep"), "blocked", "an id parked on an archived tab is WAITING, not gone");
        assert_eq!(eff("done-anyway"), "done", "done is done whatever it was blocked by");

        // The peer's scenario: the blocker finishes on ITS tab, nothing on tab A's own rows
        // changes, and tab A's lock must still lift — so tab A's change key must move.
        let k0 = tab_change_key(&app, &tab_a).unwrap();
        {
            let mut data = app.app_data.write();
            let ws = &mut data.windows[0].workspaces[0];
            ws.tasks.iter_mut().find(|t| t.id == "blocker").unwrap().status = "done".into();
        }
        assert_ne!(tab_change_key(&app, &tab_a), Some(k0), "a lock lifting on tab A must wake tab A's socket");
        assert_eq!(
            tasks_for_tab(&app, &tab_a).iter().find(|r| r["id"] == "waits").unwrap()["effectiveStatus"],
            "todo"
        );
    }

    #[test]
    fn the_board_groups_by_workspace_and_names_what_it_can() {
        let (app, _tab) = fixture();
        let b = board(&app);
        let ws = &b["workspaces"][0];
        assert_eq!(ws["workspace"], "Proj");
        assert_eq!(ws["windowLabel"], "main");
        assert_eq!(ws["overlord"], false);
        assert_eq!(ws["workstreams"][0]["name"], "Auth refactor");
        let tasks = ws["tasks"].as_array().unwrap();
        assert_eq!(tasks.len(), 4, "the board carries the backlog and orphans too");
        let orphan = tasks.iter().find(|t| t["id"] == "t4").unwrap();
        assert_eq!(orphan["tabId"], "gone-tab");
        assert!(orphan["tabTitle"].is_null(), "an assignee that no longer exists is a stated null, not a guess");
        let backlog = tasks.iter().find(|t| t["id"] == "t3").unwrap();
        assert!(backlog["tabId"].is_null());
    }
}
