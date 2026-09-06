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
//! persist behind the board. Phone WRITES land in Rust too, but must emit to that store or its
//! next whole-list persist clobbers them (see the task write handlers in mod.rs).
//!
//! Every field is always present and absence is a stated `null` — the contract rule from
//! mailink-protocol v0.4 (a consumer that never receives a field concludes something).
//!
//! **Designation is a gate here too.** "Make unavailable in maiLink" is documented as a real
//! gate, not a visibility toggle (protocol §7), and a task title or detail is exactly the kind
//! of content it exists to keep off the phone. So `board()` shows a workspace only if it has a
//! designated tab, omits rows assigned to a non-designated tab, and resolves tab names only for
//! designated tabs. `tasks_for_tab` does not re-check — its callers already do (chat_detail
//! after `is_designated`, the streamer over `designated_tabs`).

use crate::state::workspace::{AppData, Task, WindowData, Workspace};
use crate::state::AppState;
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};
use std::hash::{Hash, Hasher};

/// Task ids parked on this WINDOW's archived tabs. Window-wide, not per workspace, because that
/// is the desktop's set (`workspacesStore.parkedTaskIds` unions `archived_tasks` across every
/// workspace in the window) and `effective_status` must agree with it — a cross-workspace
/// `blocked_by` id is not something any UI writes, but `updateTasks` accepts one unvalidated.
fn parked_ids(win: &WindowData) -> HashSet<&str> {
    win.workspaces
        .iter()
        .flat_map(|ws| ws.archived_tabs.iter())
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
/// `all` is the WORKSPACE's list, as on the desktop (TasksPanel / listTasks pass the workspace).
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

/// The set of tab ids the phone may see at all. Computed BEFORE taking the state lock — it
/// takes its own read, and parking_lot reads are not reentrant when a writer is queued.
fn designated_set(app: &AppState) -> HashSet<String> {
    super::designated_tabs(app).into_iter().map(|m| m.tab_id).collect()
}

/// tab id → tab name, for DESIGNATED tabs only — an excluded tab's name is content the gate
/// keeps off the phone. Live tabs only: an ARCHIVED tab's rows are parked on the tab
/// (`archived_tasks`), out of `Workspace.tasks` by design, so they never reach this module.
fn tab_titles<'a>(data: &'a AppData, designated: &HashSet<String>) -> HashMap<&'a str, &'a str> {
    let mut out = HashMap::new();
    for win in &data.windows {
        for ws in &win.workspaces {
            for pane in &ws.panes {
                for tab in &pane.tabs {
                    if designated.contains(&tab.id) {
                        out.insert(tab.id.as_str(), tab.name.as_str());
                    }
                }
            }
        }
    }
    out
}

fn workstream_name<'a>(ws: &'a Workspace, id: Option<&str>) -> Option<&'a str> {
    id.and_then(|id| ws.workstreams.iter().find(|w| w.id == id))
        .map(|w| w.name.as_str())
}

/// The tasks assigned to one tab, in board order. Empty when the tab owns none — the caller
/// serializes that as `[]`, never omits it. Callers gate on designation first (see module doc).
pub(crate) fn tasks_for_tab(app: &AppState, tab_id: &str) -> Vec<Value> {
    let designated = designated_set(app);
    let data = app.app_data.read();
    let titles = tab_titles(&data, &designated);
    let mut out = Vec::new();
    for win in &data.windows {
        let parked = parked_ids(win);
        for ws in &win.workspaces {
            for t in ws.tasks.iter().filter(|t| t.tab_id.as_deref() == Some(tab_id)) {
                out.push(task_view(
                    t,
                    effective_status(t, ws, &parked),
                    titles.get(tab_id).copied(),
                    workstream_name(ws, t.workstream_id.as_deref()),
                ));
            }
        }
    }
    out
}

/// Change keys for EVERY tab that owns rows, in one pass under one read lock — the WS ticker
/// calls this once per tick for the whole roster, not once per tab (per-tab it was
/// O(tabs × tasks) with a `parked_ids` rebuild per pair; at 500 tabs / 300 tasks that was ~6 ms
/// of every 400 ms tick per connected phone). A tab absent from the map owns no rows, which
/// the streamer reads as "emit `[]` once if it used to, nothing if it never did".
///
/// Hashes exactly what `task_view` EMITS, including the resolved `workstream` name, `tabTitle`
/// and `effectiveStatus` — none of which live on the row. A workstream rename touches only the
/// `Workstream` record, a tab rename only the tab, a blocker finishing only ITS row; a key over
/// the row's own fields missed all three, and the phone kept the old name forever, not late.
pub(crate) fn tab_change_keys(app: &AppState) -> HashMap<String, u64> {
    let designated = designated_set(app);
    let data = app.app_data.read();
    let titles = tab_titles(&data, &designated);
    let mut hashers: HashMap<&str, std::collections::hash_map::DefaultHasher> = HashMap::new();
    for win in &data.windows {
        let parked = parked_ids(win);
        for ws in &win.workspaces {
            for t in &ws.tasks {
                let Some(tab) = t.tab_id.as_deref() else { continue };
                let h = hashers.entry(tab).or_default();
                t.id.hash(h);
                t.title.hash(h);
                t.detail.hash(h);
                t.status.hash(h);
                effective_status(t, ws, &parked).hash(h);
                t.blocked_by.hash(h);
                t.updated_at.hash(h);
                t.workstream_id.hash(h);
                workstream_name(ws, t.workstream_id.as_deref()).hash(h);
                titles.get(tab).copied().hash(h);
            }
        }
    }
    hashers.into_iter().map(|(k, h)| (k.to_string(), h.finish())).collect()
}

/// One tab's change key. Convenience over `tab_change_keys` for tests and one-off callers; the
/// ticker must use the batch form.
#[cfg(test)]
pub(crate) fn tab_change_key(app: &AppState, tab_id: &str) -> Option<u64> {
    tab_change_keys(app).remove(tab_id)
}

/// The board the phone may see: every workspace, in every window, that has at least one
/// designated tab — with its workstreams and the rows the gate allows (unassigned backlog rows
/// belong to the workspace, so they show; rows on a NON-designated tab do not, nor does that
/// tab's name). The Overlord board's workstream index (docs/overlord.md §11) as data.
/// Task-less workspaces are included so the phone can offer "add a task here"; the Overlord
/// workspace itself is flagged rather than filtered, since the phone decides what to show.
pub(crate) fn board(app: &AppState) -> Value {
    let designated = designated_set(app);
    let data = app.app_data.read();
    let titles = tab_titles(&data, &designated);
    let mut workspaces = Vec::new();
    for win in &data.windows {
        let parked = parked_ids(win);
        for ws in &win.workspaces {
            let exposed = ws
                .panes
                .iter()
                .flat_map(|p| p.tabs.iter())
                .any(|t| designated.contains(&t.id));
            if !exposed {
                continue;
            }
            let visible = |t: &&Task| match t.tab_id.as_deref() {
                None => true,
                Some(id) => designated.contains(id),
            };
            workspaces.push(json!({
                "workspaceId": ws.id,
                "workspace": ws.name,
                "windowLabel": win.label,
                "overlord": ws.overlord,
                "suspended": ws.suspended,
                "workstreams": ws.workstreams.iter().map(|w| json!({ "id": w.id, "name": w.name })).collect::<Vec<_>>(),
                "tasks": ws.tasks.iter().filter(visible).map(|t| task_view(
                    t,
                    effective_status(t, ws, &parked),
                    t.tab_id.as_deref().and_then(|id| titles.get(id).copied()),
                    workstream_name(ws, t.workstream_id.as_deref()),
                )).collect::<Vec<_>>(),
            }));
        }
    }
    json!({ "workspaces": workspaces })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::workspace::{Tab, Workstream};
    use crate::state::AgentRuntime;

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

    /// A tab the gate lets through in expose-all mode: a detected runtime, not excluded.
    fn agent_tab(name: &str) -> Tab {
        let mut t = Tab::new(name.into());
        t.runtime = Some(AgentRuntime::Claude);
        t
    }

    /// One window, one workspace whose auto-created tab is the assignee of two rows, plus one
    /// unassigned backlog row and one row on a tab that doesn't exist any more.
    fn fixture() -> (AppState, String) {
        let app = AppState::new();
        let tab_id = {
            let mut data = app.app_data.write();
            data.preferences.mailink_expose_all = true;
            let mut win = WindowData::new("main".into());
            let mut ws = Workspace::new("Proj".into());
            let tab_id = ws.panes[0].tabs[0].id.clone();
            ws.panes[0].tabs[0].name = "worker".into();
            ws.panes[0].tabs[0].runtime = Some(AgentRuntime::Claude);
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
    fn a_rename_of_what_the_row_only_references_still_moves_the_key() {
        // The review's case: `task_view` emits the workstream NAME and the tab TITLE, neither of
        // which lives on the row. A rename of either changed nothing the old key hashed, so the
        // phone kept the old name until an unrelated edit — not late, never.
        let (app, tab) = fixture();
        let k0 = tab_change_key(&app, &tab).unwrap();
        {
            let mut data = app.app_data.write();
            data.windows[0].workspaces[0].workstreams[0].name = "Auth v2".into();
        }
        let k1 = tab_change_key(&app, &tab).unwrap();
        assert_ne!(k1, k0, "a workstream rename must reach the phone");
        assert_eq!(tasks_for_tab(&app, &tab)[0]["workstream"], "Auth v2");
        {
            let mut data = app.app_data.write();
            data.windows[0].workspaces[0].panes[0].tabs[0].name = "renamed".into();
        }
        assert_ne!(tab_change_key(&app, &tab), Some(k1), "a tab rename must reach the phone");
        assert_eq!(tasks_for_tab(&app, &tab)[0]["tabTitle"], "renamed");
    }

    #[test]
    fn the_rendered_status_is_derived_like_the_desktop_does_and_moves_the_key() {
        let app = AppState::new();
        let tab_a = {
            let mut data = app.app_data.write();
            data.preferences.mailink_expose_all = true;
            let mut win = WindowData::new("main".into());
            let mut ws = Workspace::new("Proj".into());
            let tab_a = ws.panes[0].tabs[0].id.clone();
            ws.panes[0].tabs[0].runtime = Some(AgentRuntime::Claude);
            let b = agent_tab("b");
            let tab_b = b.id.clone();
            ws.panes[0].tabs.push(b);
            // A prerequisite parked on an archived tab in ANOTHER workspace of the same window:
            // off the list, not gone — and the desktop's parked set is window-wide.
            let mut other = Workspace::new("Other".into());
            let mut archived = Tab::new("old".into());
            archived.archived_tasks.push(task("parked", "Migrate schema", None, None));
            other.archived_tabs.push(archived);

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
            let mut parked_row = task("parked-row", "Someday, after the API", Some(&tab_a), None);
            parked_row.status = "backlog".into();
            parked_row.blocked_by = vec!["blocker".into()];
            ws.tasks.push(parked_row);
            win.workspaces.push(ws);
            win.workspaces.push(other);
            data.windows.push(win);
            tab_a
        };
        let rows = tasks_for_tab(&app, &tab_a);
        let eff = |id: &str| rows.iter().find(|r| r["id"] == id).unwrap()["effectiveStatus"].clone();
        // Stored `todo`, shown `blocked` — the desktop's dependency override, cross-tab.
        assert_eq!(eff("waits"), "blocked");
        assert_eq!(rows.iter().find(|r| r["id"] == "waits").unwrap()["status"], "todo", "the stored lane is untouched");
        assert_eq!(eff("freed"), "todo", "a finished prerequisite gates nothing");
        assert_eq!(eff("orphaned"), "todo", "an id that resolves to nothing is MET, not a permanent wedge");
        assert_eq!(eff("parked-dep"), "blocked", "an id parked on an archived tab — in any workspace of the window — is WAITING, not gone");
        assert_eq!(eff("done-anyway"), "done", "done is done whatever it was blocked by");
        assert_eq!(eff("parked-row"), "blocked", "no parked special-case, exactly like the desktop: a backlog row with an open dep shows blocked (and is still COUNTED parked, off its stored status)");

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
        // t4 sits on a tab id that resolves to nothing — not designated, so not shown.
        assert_eq!(tasks.len(), 3, "the board carries the backlog; a row on an unknown tab is gated out");
        assert!(tasks.iter().all(|t| t["id"] != "t4"));
        let backlog = tasks.iter().find(|t| t["id"] == "t3").unwrap();
        assert!(backlog["tabId"].is_null());
    }

    #[test]
    fn the_board_honours_make_unavailable_in_mailink() {
        // The review's case: "Make unavailable in maiLink" is documented as a real gate. A task
        // on an excluded tab — title, detail, the tab's NAME — must not come back from /tasks,
        // and a workspace with nothing designated must not appear at all.
        let app = AppState::new();
        let (shown_tab, hidden_tab) = {
            let mut data = app.app_data.write();
            data.preferences.mailink_expose_all = true;
            let mut win = WindowData::new("main".into());
            let mut ws = Workspace::new("Proj".into());
            let shown = ws.panes[0].tabs[0].id.clone();
            ws.panes[0].tabs[0].runtime = Some(AgentRuntime::Claude);
            ws.panes[0].tabs[0].name = "public-work".into();
            let mut secret = agent_tab("client-secrets");
            secret.mailink_excluded = true;
            let hidden = secret.id.clone();
            ws.panes[0].tabs.push(secret);
            ws.tasks.push(task("ok", "Ship the thing", Some(&shown), None));
            let mut s = task("s1", "Rotate the prod DB password", Some(&hidden), None);
            s.detail = Some("root password is in vault X".into());
            ws.tasks.push(s);
            ws.tasks.push(task("loose", "Backlog item", None, None));
            win.workspaces.push(ws);
            // A whole workspace with no designated tab: a plain shell, no runtime.
            let mut private = Workspace::new("Personal".into());
            private.tasks.push(task("p1", "Taxes", None, None));
            win.workspaces.push(private);
            data.windows.push(win);
            (shown, hidden)
        };
        let b = board(&app);
        let text = b.to_string();
        assert!(!text.contains("client-secrets"), "an excluded tab's name leaked");
        assert!(!text.contains("Rotate the prod DB"), "an excluded tab's task leaked");
        assert!(!text.contains("vault X"), "an excluded tab's task detail leaked");
        assert!(!text.contains("Personal") && !text.contains("Taxes"), "an undesignated workspace leaked");
        let wss = b["workspaces"].as_array().unwrap();
        assert_eq!(wss.len(), 1);
        let ids: Vec<&str> = wss[0]["tasks"].as_array().unwrap().iter().map(|t| t["id"].as_str().unwrap()).collect();
        assert_eq!(ids, vec!["ok", "loose"], "the designated tab's row and the unassigned backlog row, in board order");
        // The per-tab paths are gated by their callers, but the excluded tab's NAME must not
        // resolve through them either.
        assert!(tasks_for_tab(&app, &hidden_tab)[0]["tabTitle"].is_null());
        assert_eq!(tasks_for_tab(&app, &shown_tab)[0]["tabTitle"], "public-work");
    }
}
