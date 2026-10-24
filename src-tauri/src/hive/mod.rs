//! Hive: a coordinator that spawns agents on a goal (docs/hive.md).
//!
//! This step holds the record (`record`), where a Hive lives on disk (`paths`, `layout`), the
//! trust maiTerm writes for its own folders (`trust`), and the lifecycle refusals below, which
//! the workspace and window commands call before they change anything (§11.2).

// Dormant until the Create sheet (build step 3d) lays a Hive out; nothing calls the disk side yet.
#![allow(dead_code)]

pub mod layout;
pub mod paths;
pub mod record;
pub mod trust;

pub use record::Hive;

use crate::state::{Tab, Workspace};

pub const ROLE_COORDINATOR: &str = "coordinator";
pub const ROLE_WORKER: &str = "worker";

fn live_tab<'a>(ws: &'a Workspace, tab_id: &str) -> Option<&'a Tab> {
    ws.panes.iter().flat_map(|p| p.tabs.iter()).find(|t| t.id == tab_id)
}

fn is_coordinator(t: &Tab) -> bool {
    t.hive_role.as_deref() == Some(ROLE_COORDINATOR)
}

/// Moving a tab: a Hive agent belongs to its Hive's worktrees, task list and coordinator, and
/// a tab moved INTO a Hive would land behind the Hive view with nothing to show it. `to` is
/// the destination workspace when it's another one.
pub fn refuse_tab_move(from: &Workspace, tab_id: &str, to: Option<&Workspace>) -> Result<(), String> {
    if live_tab(from, tab_id).is_some_and(|t| t.hive_role.is_some()) {
        return Err("A Hive's agent can't be moved: it belongs to its Hive".to_string());
    }
    if to.is_some_and(|w| w.id != from.id && w.hive.is_some()) {
        return Err("A tab can't be moved into a Hive".to_string());
    }
    Ok(())
}

/// A worker leaves through Stop, land or retire, which keep its worktrees and chat (§11.2).
pub fn refuse_tab_archive(ws: &Workspace, tab_id: &str) -> Result<(), String> {
    match live_tab(ws, tab_id) {
        Some(t) if t.hive_role.is_some() => Err("A Hive's agent can't be archived".to_string()),
        _ => Ok(()),
    }
}

/// The coordinator goes only with its Hive (Done or Delete) or its workspace, or when a reload
/// has already saved its replacement (`replaced_by`).
pub fn refuse_tab_close(ws: &Workspace, tab_id: &str, replaced_by: Option<&str>) -> Result<(), String> {
    match live_tab(ws, tab_id) {
        Some(t) if is_coordinator(t) && replaced_by.is_none() => {
            Err("A Hive's coordinator can't be closed: a Hive ends through Done or Delete".to_string())
        }
        _ => Ok(()),
    }
}

/// Closing a pane closes its tabs, the hidden coordinator's included.
pub fn refuse_pane_close(ws: &Workspace, pane_id: &str) -> Result<(), String> {
    let holds = ws.panes.iter().filter(|p| p.id == pane_id).flat_map(|p| p.tabs.iter()).any(is_coordinator);
    if holds {
        return Err("This pane holds the Hive's coordinator and can't be closed".to_string());
    }
    Ok(())
}

/// A copy would be hidden tabs with no Hive, or a second Hive on the same worktrees and
/// branches; neither is safe (§11.2).
pub fn refuse_workspace_duplicate(ws: &Workspace) -> Result<(), String> {
    if ws.hive.is_some() {
        return Err("A Hive can't be duplicated".to_string());
    }
    Ok(())
}

/// Its window-scoped rpc, events and spawn reservations would need re-homing mid-flight.
pub fn refuse_workspace_move(ws: &Workspace) -> Result<(), String> {
    if ws.hive.is_some() {
        return Err("A Hive can't move to another window".to_string());
    }
    Ok(())
}

/// The share allowlist already leaves the Hive fields out; a Hive without them would arrive as
/// a workspace of hidden-agent leftovers, so it isn't offered at all.
pub fn refuse_share(ws: &Workspace) -> Result<(), String> {
    if ws.hive.is_some() {
        return Err("A Hive can't be shared".to_string());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::{AppData, Pane, WindowData};

    fn tab(id: &str, role: Option<&str>) -> Tab {
        let mut t = Tab::new(id.to_string());
        t.id = id.to_string();
        t.hive_role = role.map(str::to_string);
        t
    }

    /// One window: a Hive (coordinator + worker hidden, one ordinary tab in a second pane) and
    /// a plain workspace.
    fn app() -> AppData {
        let mut hive_ws = Workspace::new("Hive".into());
        hive_ws.id = "hive-ws".into();
        hive_ws.hive = Some(Hive { id: "h1".into(), slug: "s".into(), ..Hive::default() });
        hive_ws.panes[0].id = "p1".into();
        hive_ws.panes[0].tabs = vec![tab("coord", Some(ROLE_COORDINATOR)), tab("w1", Some(ROLE_WORKER))];
        hive_ws.panes[0].active_tab_id = None;
        let mut p2 = Pane::new("Terminal".into());
        p2.id = "p2".into();
        p2.tabs = vec![tab("plain-in-hive", None)];
        hive_ws.panes.push(p2);
        let mut plain = Workspace::new("Plain".into());
        plain.id = "plain-ws".into();
        plain.panes[0].tabs = vec![tab("t1", None)];
        let mut win = WindowData::new("main".into());
        win.workspaces = vec![hive_ws, plain];
        AppData { windows: vec![win], ..AppData::default() }
    }

    fn ws<'a>(d: &'a AppData, id: &str) -> &'a Workspace {
        d.window("main").unwrap().workspaces.iter().find(|w| w.id == id).unwrap()
    }

    #[test]
    fn hive_tabs_cannot_move_and_nothing_moves_into_a_hive() {
        let d = app();
        let (h, p) = (ws(&d, "hive-ws"), ws(&d, "plain-ws"));
        assert!(refuse_tab_move(h, "coord", Some(p)).is_err());
        assert!(refuse_tab_move(h, "w1", None).is_err(), "not even to another pane");
        assert!(refuse_tab_move(p, "t1", Some(h)).is_err(), "into a Hive");
        assert!(refuse_tab_move(h, "plain-in-hive", None).is_ok(), "an ordinary tab within its workspace");
        assert!(refuse_tab_move(h, "plain-in-hive", Some(p)).is_ok(), "an ordinary tab out of a Hive");
        assert!(refuse_tab_move(p, "t1", None).is_ok());
    }

    #[test]
    fn hive_tabs_cannot_be_archived() {
        let d = app();
        let h = ws(&d, "hive-ws");
        assert!(refuse_tab_archive(h, "coord").is_err());
        assert!(refuse_tab_archive(h, "w1").is_err());
        assert!(refuse_tab_archive(h, "plain-in-hive").is_ok());
        assert!(refuse_tab_archive(ws(&d, "plain-ws"), "t1").is_ok());
    }

    #[test]
    fn the_coordinator_closes_only_for_a_reload() {
        let d = app();
        let h = ws(&d, "hive-ws");
        assert!(refuse_tab_close(h, "coord", None).is_err());
        assert!(refuse_tab_close(h, "coord", Some("coord-2")).is_ok(), "reload: its replacement is saved");
        assert!(refuse_tab_close(h, "w1", None).is_ok(), "a worker's close is Stop, decided by the caller (step 4)");
        assert!(refuse_tab_close(h, "gone", None).is_ok());
        assert!(refuse_pane_close(h, "p1").is_err(), "a pane takes its hidden coordinator with it");
        assert!(refuse_pane_close(h, "p2").is_ok());
    }

    #[test]
    fn a_hive_workspace_cannot_be_duplicated_moved_or_shared() {
        let d = app();
        let (h, p) = (ws(&d, "hive-ws"), ws(&d, "plain-ws"));
        for refuse in [refuse_workspace_duplicate, refuse_workspace_move, refuse_share] {
            assert!(refuse(h).is_err());
            assert!(refuse(p).is_ok());
        }
    }

    #[test]
    fn a_cloned_workspace_carries_no_hive_and_no_hive_role() {
        let d = app();
        let (copy, _) = crate::commands::window::clone_workspace_with_id_mapping(ws(&d, "hive-ws"), &[]);
        assert!(copy.hive.is_none());
        assert!(copy.panes.iter().flat_map(|p| p.tabs.iter()).all(|t| t.hive_role.is_none()));
    }

    #[test]
    fn a_window_duplicate_skips_hives_and_names_them() {
        let d = app();
        let (kept, skipped) = crate::commands::window::split_out_hives(&d.window("main").unwrap().workspaces);
        assert_eq!(kept.iter().map(|w| w.id.as_str()).collect::<Vec<_>>(), vec!["plain-ws"]);
        assert_eq!(skipped, vec!["Hive".to_string()]);
    }

    #[test]
    fn the_hive_survives_a_state_round_trip() {
        let d = app();
        let back: AppData = serde_json::from_str(&serde_json::to_string(&d).unwrap()).unwrap();
        let h = ws(&back, "hive-ws");
        assert_eq!(h.hive.as_ref().map(|h| h.id.as_str()), Some("h1"));
        assert_eq!(h.panes[0].tabs[0].hive_role.as_deref(), Some(ROLE_COORDINATOR));
    }
}
