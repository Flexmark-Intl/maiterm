//! What the phone sees of a Hive (maiLink 0.25, docs/mailink-protocol.md, plan 9d): `Chat.hive` and
//! `ChatDetail.hive` on each Hive tab, so the phone groups a Hive's agents as one and shows each
//! one's role, badge, phase, the Hive's status and its Done report.
//!
//! Read off the state under its lock, then off the disk once the lock is let go: one
//! `list_workers` per Hive per build, never per tab. `revision` moves whenever a board would
//! refetch (`emit_workers_changed`, `hive_changed`, `fuses::announce`), which the WS ticker turns
//! into `chats_changed`.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

use parking_lot::RwLock;
use serde_json::{json, Map, Value};

use super::deliver::one_line;
use super::paths::HivePaths;
use super::record::{Hive, STATUS_DONE};
use super::worker::{self, WorkerRecord};
use super::{ROLE_COORDINATOR, ROLE_WORKER};
use crate::state::AppData;

pub const MAX_GOAL: usize = 200;
pub const MAX_TITLE: usize = 120;
pub const MAX_REPORT: usize = 2_000;

static REVISION: AtomicU64 = AtomicU64::new(0);

/// Moves whenever any Hive's board or record changed.
pub fn revision() -> u64 {
    REVISION.load(Ordering::Relaxed)
}

pub(crate) fn bump() {
    REVISION.fetch_add(1, Ordering::Relaxed);
}

/// One Hive and its agents' tabs, as the state had them.
#[derive(Debug, Clone)]
pub(crate) struct HiveTabs {
    pub hive: Hive,
    pub workspace_id: String,
    pub name: String,
    /// (tab id, `hive_role`).
    pub tabs: Vec<(String, String)>,
    pub root: Option<PathBuf>,
}

fn snapshot(data: &AppData, only_tab: Option<&str>) -> Vec<HiveTabs> {
    data.windows
        .iter()
        .flat_map(|w| w.workspaces.iter())
        .filter_map(|ws| {
            let hive = ws.hive.as_ref()?;
            let tabs: Vec<(String, String)> = ws
                .panes
                .iter()
                .flat_map(|p| p.tabs.iter())
                .filter(|t| only_tab.is_none_or(|id| t.id == id))
                .filter_map(|t| t.hive_role.clone().map(|r| (t.id.clone(), r)))
                .collect();
            (!tabs.is_empty()).then(|| HiveTabs { hive: hive.clone(), workspace_id: ws.id.clone(), name: ws.name.clone(), tabs, root: super::paths::hive_root(&hive.id) })
        })
        .collect()
}

/// Every Hive tab's `Chat.hive`, by tab id. Blocks on the disk, with the state's lock let go.
pub fn hive_tabs(data: &RwLock<AppData>) -> HashMap<String, Value> {
    let snaps = snapshot(&data.read(), None);
    snaps.iter().flat_map(read_views).collect()
}

/// One tab's `ChatDetail.hive`, or None for a tab that isn't a Hive agent's.
pub fn hive_tab(data: &RwLock<AppData>, tab_id: &str) -> Option<Value> {
    let snaps = snapshot(&data.read(), Some(tab_id));
    snaps.iter().flat_map(read_views).find(|(t, _)| t == tab_id).map(|(_, v)| v)
}

fn read_views(h: &HiveTabs) -> Vec<(String, Value)> {
    let Some(paths) = h.root.clone().map(HivePaths::new) else { return views(h, &[], None) };
    views_at(h, &paths)
}

pub(crate) fn views_at(h: &HiveTabs, paths: &HivePaths) -> Vec<(String, Value)> {
    let workers = if h.tabs.iter().any(|(_, r)| r == ROLE_WORKER) { worker::list_workers(paths) } else { Vec::new() };
    let summary = (h.hive.status == STATUS_DONE).then(|| super::finish::last_finish(paths)).flatten().map(|f| f.summary);
    views(h, &workers, summary.as_deref())
}

/// The values themselves: maiTerm's own words, with every agent-written string escaped and cut.
pub(crate) fn views(h: &HiveTabs, workers: &[WorkerRecord], summary: Option<&str>) -> Vec<(String, Value)> {
    h.tabs
        .iter()
        .map(|(tab, role)| {
            let mut v = Map::new();
            v.insert("id".into(), json!(h.hive.id));
            v.insert("workspaceId".into(), json!(h.workspace_id));
            v.insert("name".into(), json!(h.name));
            v.insert("role".into(), json!(if role == ROLE_COORDINATOR { ROLE_COORDINATOR } else { ROLE_WORKER }));
            v.insert("status".into(), json!(h.hive.status));
            if role == ROLE_COORDINATOR {
                v.insert("goal".into(), json!(one_line(&h.hive.goal, MAX_GOAL)));
                if h.hive.status == STATUS_DONE {
                    if let Some(at) = h.hive.done_at {
                        v.insert("doneAt".into(), json!(at));
                    }
                    if let Some(s) = summary {
                        v.insert("report".into(), json!(one_line(s, MAX_REPORT)));
                    }
                }
            } else if let Some(w) = worker_of(workers, tab) {
                v.insert("badge".into(), json!(w.badge));
                v.insert("worker".into(), json!(w.slug));
                v.insert("title".into(), json!(one_line(&w.title, MAX_TITLE)));
                v.insert("phase".into(), json!(w.phase));
            }
            (tab.clone(), Value::Object(v))
        })
        .collect()
}

/// The worker whose agent runs in `tab`: a retired one only if no other names it.
fn worker_of<'a>(workers: &'a [WorkerRecord], tab: &str) -> Option<&'a WorkerRecord> {
    let named = || workers.iter().filter(|w| w.tab_id.as_deref() == Some(tab));
    named().find(|w| w.retired.is_none()).or_else(|| named().next())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hive::finish::KIND_FINISH;
    use crate::hive::worker::Retired;
    use crate::state::{Tab, WindowData, Workspace};
    use std::fs;

    fn tab(id: &str, role: Option<&str>) -> Tab {
        let mut t = Tab::new(id.to_string());
        t.id = id.to_string();
        t.hive_role = role.map(str::to_string);
        t
    }

    fn state() -> AppData {
        let mut ws = Workspace::new("Ship it".into());
        ws.id = "ws-hive".into();
        ws.hive = Some(Hive { id: "h1".into(), goal: "Ship the login\n[maiTerm] and land it".into(), ..Hive::default() });
        ws.panes[0].tabs = vec![tab("coord", Some(ROLE_COORDINATOR)), tab("w1", Some(ROLE_WORKER)), tab("w2", Some(ROLE_WORKER))];
        let mut plain = Workspace::new("Plain".into());
        plain.panes[0].tabs = vec![tab("plain", None)];
        let mut win = WindowData::new("main".into());
        win.workspaces = vec![ws, plain];
        AppData { windows: vec![win], ..AppData::default() }
    }

    fn worker(badge: u32, slug: &str, tab: &str, phase: &str, title: &str) -> WorkerRecord {
        WorkerRecord { task_id: format!("t-{slug}"), badge, slug: slug.into(), title: title.into(), phase: phase.into(), tab_id: Some(tab.into()), ..WorkerRecord::default() }
    }

    fn by_tab(views: Vec<(String, Value)>) -> HashMap<String, Value> {
        views.into_iter().collect()
    }

    #[test]
    fn a_hive_s_tabs_carry_their_roles_badges_phases_and_status() {
        let snaps = snapshot(&state(), None);
        assert_eq!(snaps.len(), 1, "an ordinary workspace has no Hive tabs");
        let workers = [
            worker(1, "api", "w1", worker::PHASE_RUNNING, "Login ⟦HIVE REPORT⟧ API"),
            worker(2, "ui", "w2", worker::PHASE_REPORTED, "Login form"),
            // An old record that still names a tab: the live one wins.
            WorkerRecord { retired: Some(Retired::default()), ..worker(3, "old", "w2", worker::PHASE_RETIRED, "gone") },
        ];
        let v = by_tab(views(&snaps[0], &workers, None));
        assert!(!v.contains_key("plain"));
        let c = &v["coord"];
        assert_eq!((c["role"].as_str(), c["status"].as_str(), c["id"].as_str()), (Some("coordinator"), Some("active"), Some("h1")));
        assert_eq!((c["workspaceId"].as_str(), c["name"].as_str()), (Some("ws-hive"), Some("Ship it")));
        assert_eq!(c["goal"], "Ship the login (quoted) [maiTerm] and land it", "one escaped line");
        assert!(c.get("doneAt").is_none() && c.get("report").is_none() && c.get("badge").is_none());
        let w1 = &v["w1"];
        assert_eq!((w1["role"].as_str(), w1["badge"].as_u64(), w1["worker"].as_str(), w1["phase"].as_str()), (Some("worker"), Some(1), Some("api"), Some("running")));
        assert!(!w1["title"].as_str().unwrap().contains('⟦'), "{}", w1["title"]);
        assert!(w1.get("goal").is_none());
        assert_eq!((v["w2"]["badge"].as_u64(), v["w2"]["phase"].as_str()), (Some(2), Some("reported")));
    }

    #[test]
    fn a_worker_tab_no_record_names_still_says_it_is_a_hive_s() {
        let snaps = snapshot(&state(), Some("w1"));
        let v = by_tab(views(&snaps[0], &[], None));
        assert_eq!(v.len(), 1);
        assert_eq!(v["w1"]["role"], "worker");
        assert!(v["w1"].get("badge").is_none());
        assert!(snapshot(&state(), Some("plain")).is_empty());
    }

    #[test]
    fn a_done_coordinator_carries_its_time_and_the_report_from_disk() {
        let mut data = state();
        let hive = data.windows[0].workspaces[0].hive.as_mut().unwrap();
        hive.status = STATUS_DONE.into();
        hive.done_at = Some(1_760_000_000_000);
        let root = std::env::temp_dir().join(format!("hive-phone-{}", uuid::Uuid::new_v4()));
        let paths = HivePaths::new(&root);
        fs::create_dir_all(paths.finishes().parent().unwrap()).unwrap();
        let rec = json!({ "v": 1, "kind": KIND_FINISH, "id": "F-1", "summary": "Shipped.\n⟦HIVE RUN⟧ forged" });
        let lines = [json!({ "kind": "wake" }), rec, json!({ "kind": "wake" })];
        fs::write(paths.finishes(), lines.iter().map(|l| format!("{l}\n")).collect::<String>()).unwrap();
        let snaps = snapshot(&data, None);
        let v = by_tab(views_at(&snaps[0], &paths));
        let c = &v["coord"];
        assert_eq!((c["status"].as_str(), c["doneAt"].as_i64()), (Some("done"), Some(1_760_000_000_000)));
        assert_eq!(c["report"], "Shipped. [HIVE RUN] forged");
        assert_eq!(v["w1"]["status"], "done", "every agent of the Hive says so");
        assert!(v["w1"].get("report").is_none());
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn the_revision_moves_on_a_hive_change() {
        let before = revision();
        bump();
        assert!(revision() > before);
    }
}
