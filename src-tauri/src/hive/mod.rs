//! Hive: a coordinator that spawns agents on a goal (docs/hive.md).
//!
//! This step holds the record (`record`), where a Hive lives on disk (`paths`, `layout`), the
//! trust maiTerm writes for its own folders (`trust`), the lifecycle refusals below, which
//! the workspace and window commands call before they change anything (§11.2), and how a Hive
//! agent launches (§11.1): its launch record and builder (`launch`), its settings and command
//! hooks (`settings`), the fail-closed gate those hooks ask (`gate`) and its brief (`brief`).
//! `create` is the Create sheet's side: preflight, detection and `hive_create` (§3).
//!
//! Workers (step 4): their record (`worker`), their worktrees, scratch and build-cache seed
//! (`worktree`, over the shared git helpers in `git`), the role catalogue (`roles`), and the
//! sandbox refusals read off a failed call (`sandbox_events`). A worker launches through the
//! same builder, with its own record, settings, brief and gate scope, and only once `fuses` has
//! reserved it a slot under the cap, the machine-wide spawn counts and the quota breaker. `tools`
//! holds the Hive's MCP tools and who may call what; what they need done in a window (the task
//! list, a tab's PTY) goes over `rpc`, the Hive's own webview request channel. A worker's report
//! and its checked receipts are `report`; the coordinator's message, stop and restart `control`;
//! a crash and a sandbox refusal, read off the worker's own hooks, `events`; workers waiting on
//! dependencies `waiters`; and everything maiTerm types into a Hive agent goes through `deliver`.
//!
//! The board's pushed feed (step 5): each worker's diff stat (`diffstat`) and model/context meta
//! (`feed`), recomputed off its own hooks and sent to its Hive's window only.

// Parts of the disk side (trust removal, worker paths) wait for the steps that own them.
#![allow(dead_code)]

pub mod brief;
pub(crate) mod control;
pub mod create;
pub(crate) mod deliver;
pub mod diffstat;
pub mod events;
pub mod feed;
pub mod fuses;
pub mod gate;
pub(crate) mod git;
pub mod launch;
pub mod layout;
pub mod paths;
// Ignored probes that run a sandboxed `claude -p` through `sh`: Unix only, as the sandbox is.
#[cfg(all(test, unix))]
mod probe;
pub mod record;
pub(crate) mod report;
pub mod roles;
pub(crate) mod rpc;
pub mod sandbox_events;
pub mod settings;
pub mod tools;
pub mod trust;
pub mod waiters;
pub mod worker;
pub mod worktree;

pub use record::Hive;
pub use tools::refuse_agent_tool;

use crate::state::{AppData, Tab, Workspace};

/// The workspace and tab for a live `hive_role` tab in a Hive workspace.
pub fn find_hive_tab<'a>(data: &'a AppData, tab_id: &str) -> Option<(&'a Workspace, &'a Tab)> {
    data.windows
        .iter()
        .flat_map(|w| w.workspaces.iter())
        .filter(|ws| ws.hive.is_some())
        .find_map(|ws| live_tab(ws, tab_id).filter(|t| t.hive_role.is_some()).map(|t| (ws, t)))
}

/// The window label and workspace id of the Hive a live `hive_role` tab is in: where its board's
/// `hive-workers-changed` goes.
pub fn hive_board_of(data: &AppData, tab_id: &str) -> Option<(String, String)> {
    data.windows.iter().find_map(|w| {
        w.workspaces
            .iter()
            .filter(|ws| ws.hive.is_some())
            .find(|ws| live_tab(ws, tab_id).is_some_and(|t| t.hive_role.is_some()))
            .map(|ws| (w.label.clone(), ws.id.clone()))
    })
}

/// Every Hive's board, as (window label, workspace id): a machine-wide change (the breaker) is
/// news to each.
pub fn hive_boards(data: &AppData) -> Vec<(String, String)> {
    data.windows
        .iter()
        .flat_map(|w| w.workspaces.iter().filter(|ws| ws.hive.is_some()).map(move |ws| (w.label.clone(), ws.id.clone())))
        .collect()
}

/// A Hive agent's Hive, role and Hive folder, cloned out of the state lock.
pub fn hive_agent(data: &AppData, tab_id: &str) -> Option<(Hive, String, paths::HivePaths)> {
    let (ws, tab) = find_hive_tab(data, tab_id)?;
    let hive = ws.hive.clone()?;
    let root = paths::hive_root(&hive.id)?;
    Some((hive, tab.hive_role.clone()?, paths::HivePaths::new(root)))
}

/// What a session start did: the kickoff try it claimed, and whether a worker's phase moved
/// (`launching` → `running`), which its board must hear: nothing else refetches it.
#[derive(Default)]
pub struct SessionStart {
    pub kickoff: Option<KickoffClaim>,
    pub phase_changed: bool,
}

/// A Hive agent's session started (its SessionStart hook): list it on the task, and while the
/// agent's kickoff hasn't reached it, claim the next try. A worker is found by its tab; its
/// first start also marks its setup done and its phase running. Blocks on file I/O.
pub fn session_started(
    hive: &Hive,
    role: &str,
    paths: &paths::HivePaths,
    tab_id: &str,
    session_id: &str,
    transcript_path: Option<String>,
) -> SessionStart {
    let mut phase_changed = false;
    let (task, text) = match role {
        ROLE_COORDINATOR => (paths.coordinator_task(), kickoff_text(hive, paths)),
        ROLE_WORKER => {
            let Some(w) = worker::worker_for_tab(paths, tab_id) else {
                log::warn!("hive {}: session {session_id} in tab {tab_id}, which no worker.json names", hive.id);
                return SessionStart::default();
            };
            let Ok(task) = worker::task_dir(paths, &w.task_id) else { return SessionStart::default() };
            let res = worker::update_worker(&task, |w| {
                let was = (w.setup_done, w.fresh_next, w.phase.clone());
                w.setup_done = true;
                // The fresh session it asked for has started: the next launch resumes it.
                w.fresh_next = false;
                if w.phase == worker::PHASE_LAUNCHING {
                    w.phase = worker::PHASE_RUNNING.to_string();
                    phase_changed = true;
                }
                was != (w.setup_done, w.fresh_next, w.phase.clone())
            });
            if let Err(e) = res {
                phase_changed = false;
                log::warn!("hive {}: worker {} not marked started: {e}", hive.id, w.slug);
            }
            (task, worker_kickoff_text(paths, &w))
        }
        _ => return SessionStart::default(),
    };
    let entry = launch::SessionEntry {
        session_id: session_id.to_string(),
        transcript_path,
        started_at: crate::commands::workspace::iso_now(),
    };
    if let Err(e) = launch::append_session(&task, entry) {
        log::warn!("hive {}: session {session_id} not recorded: {e}", hive.id);
    }
    let kickoff = launch::kickoff_pending(&task).then(|| KickoffClaim::take(task, text)).flatten();
    SessionStart { kickoff, phase_changed }
}

/// The invariant rules a Hive agent is primed with at every session start (§7.1). Blocks on a
/// small read: a worker's come from its `worker.json`.
pub fn priming_rules(hive: &Hive, role: &str, paths: &paths::HivePaths, tab_id: &str) -> String {
    let w = (role == ROLE_WORKER).then(|| worker::worker_for_tab(paths, tab_id)).flatten();
    brief::invariant_rules(hive, role, paths, w.as_ref())
}

/// Kickoffs being delivered now, by task folder: two hooks report one start, and a held kickoff
/// can wait half an hour, so a second start must not send it again meanwhile.
static KICKOFFS_IN_FLIGHT: std::sync::Mutex<Vec<std::path::PathBuf>> = std::sync::Mutex::new(Vec::new());

/// One try at the coordinator's kickoff. It stays pending on disk unless `delivered` is called,
/// so a try that was refused, held then dropped, or cut short by a restart is made again on the
/// coordinator's next session start.
pub struct KickoffClaim {
    pub text: String,
    task_dir: std::path::PathBuf,
}

impl KickoffClaim {
    fn take(task_dir: std::path::PathBuf, text: String) -> Option<Self> {
        let mut busy = KICKOFFS_IN_FLIGHT.lock().unwrap_or_else(|e| e.into_inner());
        if busy.contains(&task_dir) {
            return None;
        }
        busy.push(task_dir.clone());
        Some(Self { text, task_dir })
    }

    /// The agent has it: typed, or taken by its mod.
    pub fn delivered(&self) {
        if let Err(e) = launch::mark_kickoff_delivered(&self.task_dir) {
            log::warn!("hive: kickoff delivered but not recorded, so it may be sent again: {e}");
        }
    }
}

impl Drop for KickoffClaim {
    fn drop(&mut self) {
        KICKOFFS_IN_FLIGHT.lock().unwrap_or_else(|e| e.into_inner()).retain(|p| *p != self.task_dir);
    }
}

/// The first thing the coordinator is told, once: maiTerm's words, never a human's.
pub fn kickoff_text(hive: &Hive, paths: &paths::HivePaths) -> String {
    format!(
        "[maiTerm] This Hive has just been created, and you are its coordinator. Read {} first: it has your full brief, the \
         folders and your human's Instructions. Then plan the work toward the goal: {}",
        paths.coordinator_brief().display(),
        hive.goal.trim()
    )
}

/// A worker's first words from maiTerm, once per session it starts fresh.
pub fn worker_kickoff_text(paths: &paths::HivePaths, w: &worker::WorkerRecord) -> String {
    format!("[maiTerm] Read {}, then start.", paths.worker_brief(&w.slug).display())
}

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

/// A new tab in a Hive would sit behind the Hive view with nothing to show it, as the pane's
/// active tab (§5: a Hive's pane has none). The one tab made there is a reload's copy of a Hive
/// tab (`copy_of`), which `reloadTab` turns hidden; `create_tab` never makes it active.
pub fn refuse_tab_create(ws: &Workspace, copy_of: Option<&str>) -> Result<(), String> {
    if ws.hive.is_some() && !copy_of.and_then(|id| live_tab(ws, id)).is_some_and(|t| t.hive_role.is_some()) {
        return Err("A Hive's agents are started by the Hive, not as new tabs".to_string());
    }
    Ok(())
}

/// The restored tab would become the pane's active tab behind the Hive view.
pub fn refuse_tab_restore(ws: &Workspace) -> Result<(), String> {
    if ws.hive.is_some() {
        return Err("A tab can't be restored into a Hive".to_string());
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

/// A live tab with a `hive_role`, wherever it is. Refusals ask this rather than
/// `find_hive_tab`: a role without its Hive record is still nobody else's to drive.
fn is_hive_role_tab(data: &AppData, tab_id: &str) -> bool {
    data.windows
        .iter()
        .flat_map(|w| w.workspaces.iter())
        .any(|ws| live_tab(ws, tab_id).is_some_and(|t| t.hive_role.is_some()))
}

fn is_hive_workspace(data: &AppData, workspace_id: &str) -> bool {
    data.windows.iter().flat_map(|w| w.workspaces.iter()).any(|ws| ws.id == workspace_id && ws.hive.is_some())
}

/// The Overlord tools that act on their target. Two supervisors driving one agent is the
/// double-drive problem, and the coordinator already supervises its Hive (§7.3). `getTabPrompt`
/// is a read and stays; archive and the coordinator's close have their own lifecycle refusals.
pub const OVERLORD_TOOLS_REFUSING_HIVE: [&str; 7] =
    ["driveTab", "releaseDirective", "answerTabPrompt", "recoverTab", "resumeTab", "archiveTab", "closeTab"];

/// The Rust backstop for the engine's Hive skip: an Overlord tool whose `tab_id` names a Hive
/// agent is refused before it reaches the frontend. A batch's `tab_ids` are left to the
/// frontend, which refuses the Hive row (`retireGuard`, reason 'hive') and still acts on the
/// rest: one bad row is not a bad call.
pub fn refuse_overlord_target(data: &AppData, tool: &str, args: &serde_json::Value) -> Option<String> {
    if !OVERLORD_TOOLS_REFUSING_HIVE.contains(&tool) {
        return None;
    }
    let single = args.get("tab_id").and_then(|v| v.as_str());
    single.is_some_and(|id| is_hive_role_tab(data, id)).then(|| {
        format!("{tool} refused (reason 'hive'): a Hive's coordinator supervises this tab. Leave it alone.")
    })
}

/// Agent requests that would put a Hive agent in a mesh or a bridge, move one, or add a tab or a
/// mesh to a Hive workspace, refused when filed rather than shown to the human: no card should
/// offer what the Hive refuses anyway (§5: workers talk to the coordinator, not to other agents).
pub fn refuse_request(data: &AppData, kind: &str, args: &serde_json::Value) -> Result<(), String> {
    let s = |k: &str| args.get(k).and_then(|v| v.as_str());
    let hive_ws = |k: &str| s(k).is_some_and(|id| is_hive_workspace(data, id));
    let hive_tab = |k: &str| s(k).is_some_and(|id| is_hive_role_tab(data, id));
    let refusal = match kind {
        "mesh_enable" | "mesh_disable" | "mesh_link" | "mesh_unlink" if hive_ws("workspace_id") || hive_ws("with_workspace_id") => {
            "A Hive is never a mesh: its agents talk to their coordinator"
        }
        "tab_move" if hive_tab("tab_id") => "A Hive's agent can't be moved: it belongs to its Hive",
        "tab_move" if hive_ws("to_workspace_id") => "A tab can't be moved into a Hive",
        "workspace_move" if hive_ws("workspace_id") => "A Hive can't move to another window",
        "tab_create" if hive_ws("workspace_id") => "A Hive's agents are started by the Hive, not as new tabs",
        "bridge_connect" | "bridge_fork" if hive_tab("caller_tab_id") || hive_tab("target_tab_id") => {
            "A Hive's agent is never bridged: it talks to its coordinator"
        }
        _ => return Ok(()),
    };
    Err(refusal.to_string())
}

/// Whether this tab's prompt of `kind` (a doorbell kind: `permission`, `question`, `ask`…) is the
/// coordinator's to answer, so nothing pushes, toasts or notifies the human for it (§4.7). The
/// Rust twin of `hiveOwnsPrompt` (`src/lib/hive/model.ts`), asked by `ring_devices`.
///
/// False for everything until `hiveAnswerWorker` ships (step 7): before it a coordinator can't
/// answer a prompt, so every worker prompt reaches the human as today. Asked at the choke point
/// now so step 7 changes this function, not its callers.
pub fn owns_prompt(data: &AppData, tab_id: &str, kind: &str) -> bool {
    let _ = (data, tab_id, kind);
    false
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::{AppData, Pane, WindowData};

    #[test]
    fn no_prompt_is_the_coordinators_before_it_can_answer_one() {
        let d = app();
        for tab in ["coord", "w1", "plain-in-hive", "t1", "gone"] {
            for kind in ["permission", "question", "ask", "script", "idle_done", "escalation", "request", "compaction", "completion"] {
                assert!(!owns_prompt(&d, tab, kind), "{tab} {kind}");
            }
        }
    }

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
    fn a_hive_takes_no_new_or_restored_tabs_but_a_reload_copy() {
        let d = app();
        let (h, p) = (ws(&d, "hive-ws"), ws(&d, "plain-ws"));
        assert!(refuse_tab_create(h, None).is_err(), "Cmd+T in a Hive");
        assert!(refuse_tab_create(h, Some("plain-in-hive")).is_err(), "a copy of an ordinary tab is still a new tab");
        assert!(refuse_tab_create(h, Some("gone")).is_err());
        assert!(refuse_tab_create(h, Some("coord")).is_ok(), "the coordinator's reload");
        assert!(refuse_tab_create(p, None).is_ok());
        assert!(refuse_tab_restore(h).is_err());
        assert!(refuse_tab_restore(p).is_ok());
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
    fn a_hive_tab_is_found_with_its_hive_and_an_ordinary_one_is_not() {
        let d = app();
        let (h, role, paths) = hive_agent(&d, "coord").unwrap();
        assert_eq!((h.id.as_str(), role.as_str()), ("h1", ROLE_COORDINATOR));
        assert!(paths.root.ends_with("hive/h1"));
        assert!(hive_agent(&d, "plain-in-hive").is_none(), "no hive_role");
        assert!(hive_agent(&d, "t1").is_none());
        assert!(hive_agent(&d, "gone").is_none());
    }

    #[test]
    fn the_coordinator_is_kicked_off_until_one_try_is_delivered() {
        let root = std::env::temp_dir().join(format!("hive-kickoff-{}", uuid::Uuid::new_v4()));
        let paths = paths::HivePaths::new(&root);
        let hive = Hive { id: "h1".into(), goal: "Ship it".into(), ..Hive::default() };
        let start = |sid: &str| {
            let s = session_started(&hive, ROLE_COORDINATOR, &paths, "coord", sid, None);
            assert!(!s.phase_changed, "a coordinator has no worker phase");
            s.kickoff
        };

        let first = start("s1").expect("a kickoff on the first session");
        assert!(first.text.starts_with("[maiTerm] ") && first.text.contains("brief.md") && first.text.ends_with("Ship it"));
        assert!(start("s1").is_none(), "the same start reported twice, while the first try is out");
        // Refused, or held and then lost to a restart: the claim goes without `delivered`.
        drop(first);
        let retry = start("s2").expect("still pending: the next start tries again, though the session list isn't empty");
        retry.delivered();
        drop(retry);
        assert!(start("s2").is_none() && start("s3").is_none(), "delivered once, never again");
        assert!(!launch::kickoff_pending(&paths.coordinator_task()), "recorded on disk, so a restart keeps it");
        assert_eq!(launch::last_session(&paths.coordinator_task()).as_deref(), Some("s3"));
        let orphan = session_started(&hive, ROLE_WORKER, &paths, "no-such-tab", "w1", None);
        assert!(orphan.kickoff.is_none() && !orphan.phase_changed, "no worker.json names the tab");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_workers_first_start_marks_it_running_and_kicks_it_off_once() {
        let root = std::env::temp_dir().join(format!("hive-worker-start-{}", uuid::Uuid::new_v4()));
        let paths = paths::HivePaths::new(&root);
        let hive = Hive { id: "h1".into(), goal: "Ship it".into(), ..Hive::default() };
        let (_, _, mut w) = launch::tests::sample_worker_hive();
        w.fresh_next = true;
        let task = worker::task_dir(&paths, &w.task_id).unwrap();
        worker::write_worker(&task, &w).unwrap();
        let started = |sid: &str| session_started(&hive, ROLE_WORKER, &paths, "tab-w", sid, None);
        let start = |sid: &str| started(sid).kickoff;

        let first = started("s1");
        // The board fetched `launching` at the spawn: only this tells it the phase moved.
        assert!(first.phase_changed, "launching → running is news to the board");
        let claim = first.kickoff.expect("the worker's kickoff");
        assert_eq!(claim.text, format!("[maiTerm] Read {}, then start.", paths.worker_brief("auth").display()));
        let now = worker::read_worker(&task).unwrap();
        assert!(now.setup_done && !now.fresh_next, "setup never runs again; the next launch resumes");
        assert_eq!(now.phase, worker::PHASE_RUNNING);
        claim.delivered();
        drop(claim);
        assert!(start("s1").is_none(), "a resume is not told again");
        let resumed = started("s2");
        assert!(resumed.kickoff.is_none() && !resumed.phase_changed, "already running: nothing for the board");
        assert_eq!(launch::last_session(&task).as_deref(), Some("s2"));

        // A worker that reported keeps that phase when it resumes.
        worker::update_worker(&task, |w| {
            w.phase = worker::PHASE_REPORTED.into();
            true
        })
        .unwrap();
        assert!(!started("s3").phase_changed);
        assert_eq!(worker::read_worker(&task).unwrap().phase, worker::PHASE_REPORTED);

        // A restart writes `launching` again; its session start moves it, and says so.
        worker::update_worker(&task, |w| {
            w.phase = worker::PHASE_LAUNCHING.into();
            true
        })
        .unwrap();
        assert!(started("s4").phase_changed);
        assert_eq!(worker::read_worker(&task).unwrap().phase, worker::PHASE_RUNNING);
        let rules = priming_rules(&hive, ROLE_WORKER, &paths, "tab-w");
        assert!(rules.contains(&*paths.worker_brief("auth").to_string_lossy()), "{rules}");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn overlord_tools_refuse_a_hive_target_and_nothing_else() {
        let d = app();
        use serde_json::json as j;
        for tool in OVERLORD_TOOLS_REFUSING_HIVE {
            let msg = refuse_overlord_target(&d, tool, &j!({ "tabId": "ov", "tab_id": "w1" })).expect(tool);
            assert!(msg.contains("reason 'hive'") && msg.contains("coordinator supervises"), "{msg}");
            assert!(refuse_overlord_target(&d, tool, &j!({ "tab_id": "plain-in-hive" })).is_none(), "{tool}: an ordinary tab in a Hive workspace");
            assert!(refuse_overlord_target(&d, tool, &j!({ "tab_id": "t1" })).is_none(), "{tool}");
        }
        // A batch goes through: the frontend refuses the Hive row and closes the rest.
        assert!(refuse_overlord_target(&d, "closeTab", &j!({ "tab_ids": ["t1", "coord"] })).is_none(), "one Hive agent in a batch");
        assert!(refuse_overlord_target(&d, "archiveTab", &j!({ "tab_ids": ["coord", "w1"] })).is_none());
        assert!(refuse_overlord_target(&d, "getTabPrompt", &j!({ "tab_id": "coord" })).is_none(), "a read");
        assert!(refuse_overlord_target(&d, "listWorkspaces", &j!({ "tab_id": "coord" })).is_none());
    }

    #[test]
    fn requests_that_would_mesh_bridge_or_move_a_hive_are_refused_when_filed() {
        let d = app();
        use serde_json::json as j;
        let refused = |kind: &str, args: serde_json::Value| refuse_request(&d, kind, &args).is_err();
        for kind in ["mesh_enable", "mesh_disable", "mesh_unlink"] {
            assert!(refused(kind, j!({ "workspace_id": "hive-ws" })), "{kind}");
            assert!(!refused(kind, j!({ "workspace_id": "plain-ws" })), "{kind}");
        }
        assert!(refused("mesh_link", j!({ "workspace_id": "plain-ws", "with_workspace_id": "hive-ws" })));
        assert!(refused("tab_move", j!({ "tab_id": "w1", "to_workspace_id": "plain-ws" })));
        assert!(refused("tab_move", j!({ "tab_id": "t1", "to_workspace_id": "hive-ws" })));
        assert!(!refused("tab_move", j!({ "tab_id": "plain-in-hive", "to_workspace_id": "plain-ws" })));
        assert!(refused("workspace_move", j!({ "workspace_id": "hive-ws" })));
        assert!(refused("tab_create", j!({ "workspace_id": "hive-ws" })));
        assert!(!refused("tab_create", j!({ "workspace_id": "plain-ws" })));
        assert!(refused("bridge_connect", j!({ "caller_tab_id": "t1", "target_tab_id": "coord" })));
        assert!(refused("bridge_fork", j!({ "caller_tab_id": "w1", "target_tab_id": "t1" })));
        assert!(!refused("bridge_connect", j!({ "caller_tab_id": "t1", "target_tab_id": "plain-in-hive" })));
        assert!(!refused("bridge_disconnect", j!({ "caller_tab_id": "coord", "partner_tab_id": "t1" })), "closing a bridge is always allowed");
        assert!(!refused("restart", j!({ "update": false })));
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
