//! Turning a workspace into a Hive (docs/hive.md §3): what the Create sheet reads (preflight,
//! folder and tool detection) and `hive_create`.
//!
//! The sheet's checks are advice; Rust re-checks everything on Create, and the workspace's
//! eligibility again under the write lock that installs the Hive.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use tauri::State;

use super::layout::{self, FolderInfo};
use super::launch;
use super::paths::{hive_root, HivePaths};
use super::record::{Hive, HiveFolder, HiveModels, HiveSettings, CRITERIA_NEEDED, KIND_GIT, KIND_PLAIN, LANDING_BASE, LANDING_INTEGRATION, STATUS_ACTIVE};
use super::ROLE_COORDINATOR;
use crate::state::app_state::StackRuntimeRow;
use crate::state::workspace::TabType;
use crate::state::{save_state, AppData, AppState, Service, Tab, Workspace};

/// Projects' instructions limit. Over it is refused, never cut: a truncated rule reads as a
/// different rule (§3).
pub const MAX_INSTRUCTIONS: usize = 16_000;
/// The project brief, in characters: refused, never cut (the TS twin is `MAX_BRIEF`).
pub const MAX_BRIEF: usize = 32_000;
/// Files attached to the project brief: how many, and how big each.
pub const MAX_BRIEF_FILES: usize = 10;
pub const MAX_BRIEF_FILE_BYTES: u64 = 5 * 1024 * 1024;
/// The coordinator home's folder holding them.
pub const BRIEF_FILES_DIR: &str = "brief";
/// Workers at once on a new Hive (§6.8): a ceiling, not a target. The coordinator starts only what
/// the plan needs, and memory pressure and the run queue govern the machine.
pub const DEFAULT_CAP: u32 = 24;
pub const DEFAULT_BUDGET_CENTS: u64 = 1000;
pub const COORDINATOR_TAB_NAME: &str = "Queen";

/// The §7.4 checkpoints, Coordinator by default but Settings (§3.4), whose changes the human
/// approves until they hand them over. In integration mode a land is always the human's (§8.3).
pub fn default_checkpoints(landing_mode: &str) -> BTreeMap<String, String> {
    let mut out: BTreeMap<String, String> =
        ["plan", "questions", "permissions", "landing", "finals", "interfaces"].iter().map(|k| (k.to_string(), "coordinator".to_string())).collect();
    out.insert("settings".to_string(), "ask".to_string());
    if landing_mode == LANDING_INTEGRATION {
        out.insert("landing".to_string(), "ask".to_string());
    }
    out
}

/// Every Hive agent runs in Claude's sandbox, which native Windows doesn't have, the
/// coordinator included (plan decision 7). `windows` is `cfg!(windows)`, passed in for tests.
pub fn platform_refusal(windows: bool) -> Result<(), String> {
    if windows {
        return Err("A Hive needs Claude's sandbox, which native Windows doesn't have.".to_string());
    }
    Ok(())
}

/// The fresh shell a Hive replaces.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Replaceable {
    pub pane_id: String,
    pub tab_id: String,
}

fn all_tabs(ws: &Workspace) -> impl Iterator<Item = (&str, &Tab)> {
    ws.panes.iter().flat_map(|p| p.tabs.iter().map(move |t| (p.id.as_str(), t)))
}

/// What the workspace's own record says: no tabs, or one fresh, unused shell the Hive replaces
/// (§3). Whether that shell is running something is asked of its PTY separately.
pub fn structural_eligibility(ws: &Workspace) -> Result<Option<Replaceable>, String> {
    if ws.hive.is_some() {
        return Err("It's already a Hive.".to_string());
    }
    if ws.overlord {
        return Err("The Overlord's workspace can't be a Hive.".to_string());
    }
    if ws.bridge_all {
        return Err("A mesh workspace can't be a Hive: a Hive's agents talk to their queen.".to_string());
    }
    if ws.suspended {
        return Err("Resume the workspace first.".to_string());
    }
    // A Hive takes no restored tab, so they'd be stranded; one restored first would be an agent
    // resuming behind the Hive view.
    if !ws.archived_tabs.is_empty() {
        return Err("It has archived tabs. Restore or delete them first: a Hive's workspace holds only its own agents.".to_string());
    }
    let tabs: Vec<(&str, &Tab)> = all_tabs(ws).collect();
    let (pane_id, tab) = match tabs.as_slice() {
        [] => return Ok(None),
        [one] => *one,
        _ => return Err("It has more than one tab. A Hive is made from an empty workspace, or from one fresh shell it replaces.".to_string()),
    };
    let fresh = tab.tab_type == TabType::Terminal
        && tab.runtime.is_none()
        && tab.auto_resume_command.as_deref().is_none_or(|c| c.trim().is_empty())
        && tab.notes.as_deref().is_none_or(|n| n.trim().is_empty())
        && tab.service_id.is_none()
        && tab.hive_role.is_none()
        && tab.agent_bridge.is_none();
    if !fresh {
        return Err(format!("Its tab \"{}\" has been used (an agent ran in it, or it has notes). A Hive replaces only a fresh shell: close that tab, and this workspace can become one.", tab.name));
    }
    if ws.tasks.iter().any(|t| t.tab_id.as_deref() == Some(tab.id.as_str()) && !t.is_retired()) {
        return Err(format!("Its tab \"{}\" has tasks assigned.", tab.name));
    }
    Ok(Some(Replaceable { pane_id: pane_id.to_string(), tab_id: tab.id.clone() }))
}

/// The workspace's only tab when it alone keeps the workspace from becoming a Hive: everything
/// else `structural_eligibility` asks holds, the tab is a plain terminal, and closing it takes
/// nothing but the tab (no bridge, no service, no open task). Whether its shell is idle is
/// `shell_check`'s, at the caller.
pub fn lone_used_tab(ws: &Workspace) -> Option<String> {
    if ws.hive.is_some() || ws.overlord || ws.bridge_all || ws.suspended || !ws.archived_tabs.is_empty() {
        return None;
    }
    let tabs: Vec<(&str, &Tab)> = all_tabs(ws).collect();
    let [(_, tab)] = tabs.as_slice() else { return None };
    let plain = tab.tab_type == TabType::Terminal && tab.hive_role.is_none() && tab.service_id.is_none() && tab.agent_bridge.is_none();
    let tasks = ws.tasks.iter().any(|t| t.tab_id.as_deref() == Some(tab.id.as_str()) && !t.is_retired());
    (plain && !tasks && structural_eligibility(ws).is_err()).then(|| tab.id.clone())
}

const SHELL_BUSY: &str = "The shell in this workspace is running something. Stop it, or close the tab, then try again.";
const SHELL_UNREADABLE: &str = "Couldn't tell whether the shell in this workspace is running something. Close the tab, then try again.";
const WORKSPACE_CHANGED: &str = "The workspace's tabs changed while the Hive was being made. Try again.";

/// Whether closing the shell is safe, from its foreground read: `None` is no live PTY (nothing
/// to kill), `Some(None)` a foreground that couldn't be read, which is never taken for idle.
pub fn shell_verdict(at_prompt: Option<Option<bool>>) -> Result<(), String> {
    match at_prompt {
        None | Some(Some(true)) => Ok(()),
        Some(Some(false)) => Err(SHELL_BUSY.to_string()),
        Some(None) => Err(SHELL_UNREADABLE.to_string()),
    }
}

/// Closing the shell would kill what its PTY runs. Blocks on `ps`.
fn shell_check(state: &Arc<AppState>, tab_id: &str) -> Result<(), String> {
    let pty = state.tab_pty_map.read().get(tab_id).cloned().filter(|p| state.pty_registry.read().contains_key(p));
    shell_verdict(pty.map(|p| crate::pty::get_pty_foreground_job(state, &p, true).ok().and_then(|f| f.shell_at_prompt)))
}

/// Stricter than `shell_check`, for offering to close a tab (`Preflight.closable_tab`): its shell
/// at its prompt AND with no child process at all. A job sent to the background with `&` or
/// stopped with Ctrl+Z leaves the shell at its prompt, and closing the tab hangs it up too. A
/// prompt helper that lives under the shell (gitstatusd) also counts: the cost is only that the
/// button isn't offered, and the human can still close the tab. Blocks on `ps`.
fn shell_idle_and_childless(state: &Arc<AppState>, tab_id: &str) -> bool {
    if shell_check(state, tab_id).is_err() {
        return false;
    }
    let Some(pty) = state.tab_pty_map.read().get(tab_id).cloned() else { return true };
    match crate::pty::pty_child_pid_of(state, &pty) {
        Some(pid) => crate::pty::manager::child_processes(pid).is_empty(),
        // Registered without a pid: nothing to read, so nothing is offered.
        None => !state.pty_registry.read().contains_key(&pty),
    }
}

/// `<app data>`: a folder that is it, holds it or sits in it would put the state file, other
/// Hives' Rust-owned folders and secrets on an agent's reach list, and its `denyWrite` would
/// cover the coordinator's own home (§6.2).
pub(crate) fn app_data_dir() -> Option<PathBuf> {
    let d = crate::state::persistence::app_data_dir()?;
    Some(std::fs::canonicalize(&d).unwrap_or(d))
}

/// Why `path` (canonical) can't be a Hive folder, against `app_data` (canonical).
pub fn app_data_refusal(path: &Path, app_data: &Path) -> Option<&'static str> {
    if path.starts_with(app_data) {
        Some("is inside maiTerm's own data folder")
    } else if app_data.starts_with(path) {
        Some("holds maiTerm's own data folder")
    } else {
        None
    }
}

/// What the Create sheet needs before it opens (§3).
#[derive(Debug, Clone, Serialize)]
pub struct Preflight {
    pub eligible: bool,
    /// Why not, in a sentence for the sheet.
    pub reason: Option<String>,
    pub replaceable_tab: Option<Replaceable>,
    /// The folder of the tab the human was last looking at, when it's a folder on this disk.
    pub default_primary: Option<String>,
    pub default_cap: u32,
    pub platform_ok: bool,
    pub claude_ok: bool,
    pub claude_detail: Option<String>,
    /// Claude reports no login for the account the Hive's agents would run under: a warning,
    /// never a refusal. None when it is logged in or can't tell (a token can't be identified).
    pub login_warning: Option<String>,
    /// The managed account whose root the Hive's agents read trust from, for the sheet's trust
    /// warning; None for Claude's default (`~/.claude`).
    pub trust_account: Option<String>,
    /// With no managed account, the user's own `CLAUDE_CONFIG_DIR` the agents read trust from
    /// instead of `~/.claude` (`launch::TrustRoot::Dir`).
    pub trust_root_dir: Option<String>,
    /// The one used tab standing in the way, offered for the human to close: only when nothing
    /// else does and its shell is idle at its prompt, the test a shell must pass to be replaced
    /// (`lone_used_tab`, `shell_check`). Never a tab with something running in it.
    pub closable_tab: Option<String>,
}

/// The account a new Hive's agents run under, as the human knows it: the active managed Claude
/// account's label, or None for Claude's own login on this computer.
pub(crate) fn account_label(prefs: &crate::state::Preferences) -> Option<String> {
    let (_, id) = crate::accounts::active_accounts(prefs).into_iter().find(|(rt, _)| *rt == crate::accounts::Runtime::Claude)?;
    let a = prefs.managed_accounts.iter().find(|a| a.id == id)?;
    Some(if a.label.is_empty() { a.email.clone().unwrap_or_else(|| id.clone()) } else { a.label.clone() })
}

/// The sheet's warning from `claude auth status --json`'s output. Only an explicit
/// `loggedIn: false` warns: unreadable output, or a login it can't vouch for (a token, a key),
/// says nothing (docs/login.md §2.3, §6.1).
pub fn login_verdict(stdout: &[u8], account: Option<&str>) -> Option<String> {
    let v: serde_json::Value = serde_json::from_slice(stdout).ok()?;
    if v.get("loggedIn").and_then(serde_json::Value::as_bool) != Some(false) {
        return None;
    }
    Some(match account {
        Some(a) => format!(
            "Claude reports the account \"{a}\" isn't logged in, and this Hive's agents would run under it: every turn would fail. \
             Run /login in a Claude tab of that account first."
        ),
        None => "Claude reports it isn't logged in on this computer, so every turn of this Hive's agents would fail. Run /login in \
                 a Claude tab first."
            .to_string(),
    })
}

/// Asks Claude whether the account a Hive tab would launch under is logged in, in the
/// environment the launch line gives it when it starts Claude (`launch::check_env`): the
/// account's config root, or, with none, Claude's default, the only root the line's guard lets a
/// launch run under, and nothing that would answer instead. Cheap and bounded; any
/// failure to tell is no warning. Blocks.
fn login_check(prefs: &crate::state::Preferences) -> Option<String> {
    let cli = crate::accounts::resolve_cli(crate::accounts::Runtime::Claude.profile())?;
    let root = launch::launch_config_root(prefs);
    let status = |form: &str| {
        let mut cmd = crate::util::proc::command(&cli);
        cmd.args(["auth", "status", form]);
        for (k, v) in launch::check_env(prefs, root.as_deref()) {
            match v {
                Some(v) => cmd.env(k, v),
                None => cmd.env_remove(k),
            };
        }
        // Exit 1 is the ordinary logged-out answer, with its JSON on stdout.
        crate::util::proc::output_timeout(&mut cmd, Duration::from_secs(10)).ok()
    };
    let label = account_label(prefs);
    let json = status("--json")?;
    if let Some(w) = login_verdict(&json.stdout, label.as_deref()) {
        return Some(w);
    }
    // The JSON calls an expired login logged in; only the text form says so (`expired_verdict`).
    let claude_ai = serde_json::from_slice::<serde_json::Value>(&json.stdout).ok().is_some_and(|v| v["authMethod"] == "claude.ai");
    if !claude_ai {
        return None;
    }
    expired_verdict(&status("--text")?.stdout, label.as_deref())
}

/// The sheet's warning from `claude auth status --text`, for a login the JSON reports as
/// `loggedIn: true`. That flag is only "a token is stored" (2.1.310's `authStatus`); a
/// subscription login whose refresh token Claude found dead (stored as "") stays true there,
/// and only the text form prints `Login: Expired — log in again`, as `/status` does.
pub fn expired_verdict(stdout: &[u8], account: Option<&str>) -> Option<String> {
    let text = String::from_utf8_lossy(stdout);
    let expired = text.lines().any(|l| l.trim_start().strip_prefix("Login:").is_some_and(|v| v.trim_start().starts_with("Expired")));
    if !expired {
        return None;
    }
    Some(match account {
        Some(a) => format!(
            "Claude reports the login of the account \"{a}\" has expired, and this Hive's agents would run under it: every turn \
             would fail. Run /login in a Claude tab of that account first."
        ),
        None => "Claude reports its login on this computer has expired, so every turn of this Hive's agents would fail. Run \
                 /login in a Claude tab first."
            .to_string(),
    })
}

/// The `.claude.json` a new Hive's agents would read trust from: the root their launch line
/// names (`launch::launch_claude_json`: the active Claude account's, or `~/.claude.json` without
/// managed accounts, §6.6). Launch writes every root; this is only the sheet's warning.
pub fn trust_roots(prefs: &crate::state::Preferences) -> Vec<PathBuf> {
    launch::launch_claude_json(prefs).into_iter().collect()
}

fn local_dir(raw: &str) -> Option<String> {
    let p = crate::share::expand_tilde(raw.trim());
    (p.is_absolute() && p.is_dir()).then(|| crate::share::canon(&p).to_string_lossy().to_string())
}

/// A tab's folder as its terminal last reported it (the frontend's OSC 7 state), which leads
/// Rust's `last_cwd` after a fresh `cd`.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct LiveCwd {
    pub tab_id: String,
    pub cwd: Option<String>,
    /// The OSC 7 URL's host: another machine's folder (an ssh tab) is no Primary here.
    pub host: Option<String>,
}

/// The default Primary's candidates, in the caller's tab order: each tab's live folder when its
/// terminal reported one on this machine, then the folder Rust saved for it.
pub(crate) fn primary_candidates(tab_ids: &[String], live: &[LiveCwd], last_cwd: impl Fn(&str) -> Option<String>, local_host: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for id in tab_ids {
        let osc = live
            .iter()
            .find(|l| &l.tab_id == id)
            .filter(|l| crate::filetree::place::osc_is_local(l.host.as_deref(), local_host))
            .and_then(|l| l.cwd.clone());
        for c in osc.into_iter().chain(last_cwd(id)).filter(|c| !c.trim().is_empty()) {
            if !out.contains(&c) {
                out.push(c);
            }
        }
    }
    out
}

#[tauri::command]
pub async fn hive_preflight(
    window: tauri::Window,
    state: State<'_, Arc<AppState>>,
    workspace_id: String,
    from_tab_ids: Option<Vec<String>>,
    live_cwds: Option<Vec<LiveCwd>>,
) -> Result<Preflight, String> {
    crate::instance::hive_refusal()?;
    let label = window.label().to_string();
    let (structural, last_cwds, lone) = {
        let data = state.app_data.read();
        let win = data.window(&label).ok_or("Window not found")?;
        let ws = win.workspaces.iter().find(|w| w.id == workspace_id).ok_or("Workspace not found")?;
        let last: HashMap<String, String> = win
            .workspaces
            .iter()
            .flat_map(|w| all_tabs(w))
            .filter_map(|(_, t)| t.last_cwd.clone().map(|c| (t.id.clone(), c)))
            .collect();
        (structural_eligibility(ws), last, lone_used_tab(ws))
    };
    // In the caller's order: the window's active tab, then the one looked at before it.
    let from_cwds = primary_candidates(
        &from_tab_ids.unwrap_or_default(),
        &live_cwds.unwrap_or_default(),
        |id| last_cwds.get(id).cloned(),
        &crate::filetree::place::local_hostname(),
    );
    let prefs = state.app_data.read().preferences.clone();
    let app = state.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        let (eligible, reason, replaceable_tab) = match structural {
            Ok(Some(r)) => match shell_check(&app, &r.tab_id) {
                Ok(()) => (true, None, Some(r)),
                Err(e) => (false, Some(e), None),
            },
            Ok(None) => (true, None, None),
            Err(e) => (false, Some(e), None),
        };
        let closable_tab = lone.filter(|t| shell_idle_and_childless(&app, t));
        let claude = launch::claude_new_enough();
        let app_data = app_data_dir();
        // Never one Create would refuse: a tab sitting in ~ would otherwise propose ~.
        let default_primary = from_cwds
            .iter()
            .filter_map(|c| local_dir(c))
            .find(|d| app_data.as_deref().is_none_or(|a| app_data_refusal(Path::new(d), a).is_none()));
        let (trust_account, trust_root_dir) = match launch::trust_root(&prefs) {
            launch::TrustRoot::Account(a) => (Some(a), None),
            launch::TrustRoot::Dir(d) => (None, Some(d.display().to_string())),
            launch::TrustRoot::Default => (None, None),
        };
        Preflight {
            eligible,
            reason,
            replaceable_tab,
            default_primary,
            default_cap: DEFAULT_CAP,
            platform_ok: platform_refusal(cfg!(windows)).is_ok(),
            claude_ok: claude.is_ok(),
            claude_detail: claude.err(),
            login_warning: login_check(&prefs),
            trust_account,
            trust_root_dir,
            closable_tab,
        }
    })
    .await
    .map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn hive_detect_folder(state: State<'_, Arc<AppState>>, path: String) -> Result<FolderInfo, String> {
    let roots = trust_roots(&state.app_data.read().preferences);
    let p = crate::share::expand_tilde(path.trim());
    if !p.is_absolute() || !p.is_dir() {
        return Err(format!("No folder at {path}"));
    }
    tauri::async_runtime::spawn_blocking(move || layout::detect_folder(&p, &roots)).await.map_err(|e| e.to_string())
}

/// The sheet's Trust it (§6.6): the human's click trusts the repo they picked, in the one root
/// the Hive's agents launch under (`trust_roots`), as Claude's own dialog would. The sheet's
/// other remedy, running `claude` in the workspace's own shell, would use up the shell the Hive
/// replaces. Never reachable from an agent: a window command only. Answers the folder detected
/// again, its trust re-read from that root.
#[tauri::command]
pub async fn hive_trust_repo(state: State<'_, Arc<AppState>>, path: String) -> Result<FolderInfo, String> {
    crate::instance::hive_refusal()?;
    let roots = trust_roots(&state.app_data.read().preferences);
    let p = crate::share::expand_tilde(path.trim());
    if !p.is_absolute() || !p.is_dir() {
        return Err(format!("No folder at {path}"));
    }
    tauri::async_runtime::spawn_blocking(move || {
        let launch_json = roots.first().ok_or("Couldn't find the home folder")?;
        let info = layout::detect_folder(&p, &roots);
        if info.kind != KIND_GIT {
            return Err(format!("{} isn't the top folder of a git repository, so there's no repo to trust", info.path));
        }
        if let Some(why) = app_data_dir().and_then(|a| app_data_refusal(Path::new(&info.path), &a)) {
            return Err(format!("{} {why}", info.path));
        }
        super::trust::trust_repo(launch_json, Path::new(&info.path))?;
        Ok(layout::detect_folder(&p, &roots))
    })
    .await
    .map_err(|e| e.to_string())?
}

/// The sheet's Commit these files (§3.6): the human's click makes a repo's files its first commit.
/// A window command only, never reachable from an agent. Answers the folder detected again.
#[tauri::command]
pub async fn hive_commit_first(state: State<'_, Arc<AppState>>, path: String) -> Result<FolderInfo, String> {
    crate::instance::hive_refusal()?;
    let roots = trust_roots(&state.app_data.read().preferences);
    let p = crate::share::expand_tilde(path.trim());
    if !p.is_absolute() || !p.is_dir() {
        return Err(format!("No folder at {path}"));
    }
    tauri::async_runtime::spawn_blocking(move || {
        if let Some(why) = app_data_dir().and_then(|a| app_data_refusal(&crate::share::canon(&p), &a)) {
            return Err(format!("{} {why}", p.display()));
        }
        layout::commit_first(&p)?;
        Ok(layout::detect_folder(&p, &roots))
    })
    .await
    .map_err(|e| e.to_string())?
}

/// Something a Hive can make things with (§3 Tools, §10.3). `id` is what `Hive.tools` stores.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct DetectedTool {
    pub id: String,
    /// `"mcp"` | `"cli"` | `"skill"` | `"comfyui"`.
    pub kind: String,
    pub label: String,
    /// Where it answers, for one that's reached over the network.
    pub host: Option<String>,
}

pub const DETECTED_CLIS: [&str; 4] = ["ffmpeg", "magick", "sox", "blender"];
pub const COMFYUI_ADDR: &str = "127.0.0.1:8188";

/// maiTerm's own server isn't a tool the Hive makes things with: every agent gets it.
fn is_maiterm_server(name: &str) -> bool {
    ["maiterm", "maiterm-dev", "aiterm", "aiterm-dev"].contains(&name)
}

fn url_host(url: &str) -> Option<String> {
    let rest = url.split_once("://")?.1;
    let authority = rest.split(['/', '?', '#']).next()?;
    let host = authority.rsplit_once('@').map_or(authority, |(_, h)| h);
    (!host.is_empty()).then(|| host.to_string())
}

/// The MCP servers in one `.claude.json`'s `mcpServers`, by name.
pub fn mcp_tools(claude_json: &serde_json::Value) -> Vec<DetectedTool> {
    let Some(servers) = claude_json.get("mcpServers").and_then(|v| v.as_object()) else { return Vec::new() };
    servers
        .iter()
        .filter(|(name, _)| !is_maiterm_server(name))
        .map(|(name, entry)| DetectedTool {
            id: format!("mcp:{name}"),
            kind: "mcp".to_string(),
            label: name.clone(),
            host: entry.get("url").and_then(|u| u.as_str()).and_then(url_host),
        })
        .collect()
}

fn skill_tools(skills_dir: &Path) -> Vec<DetectedTool> {
    let mut names: Vec<String> = std::fs::read_dir(skills_dir)
        .into_iter()
        .flatten()
        .flatten()
        .filter(|e| e.path().join("SKILL.md").is_file())
        .filter_map(|e| e.file_name().to_str().map(str::to_string))
        .filter(|n| !n.starts_with('.'))
        .collect();
    names.sort();
    names.into_iter().map(|n| DetectedTool { id: format!("skill:{n}"), kind: "skill".to_string(), label: n, host: None }).collect()
}

/// Hosted generators' API and media hosts, a starting point the human edits (plan D2): `key` is
/// matched against a server's or skill's name, word by word, and `suffixes` against an MCP
/// server's url host. Exact hosts only, so nothing depends on whether `*.` entries work.
pub struct KnownGenerator {
    pub key: &'static str,
    pub suffixes: &'static [&'static str],
    pub hosts: &'static [&'static str],
}

pub const KNOWN_GENERATOR_HOSTS: &[KnownGenerator] = &[
    KnownGenerator { key: "fal", suffixes: &["fal.ai", "fal.run", "fal.media"], hosts: &["fal.run", "queue.fal.run", "fal.media", "v3.fal.media"] },
    KnownGenerator { key: "kie", suffixes: &["kie.ai"], hosts: &["api.kie.ai"] },
    KnownGenerator { key: "higgsfield", suffixes: &["higgsfield.ai"], hosts: &["platform.higgsfield.ai"] },
    KnownGenerator { key: "kling", suffixes: &["klingai.com"], hosts: &["api.klingai.com"] },
    KnownGenerator { key: "wavespeed", suffixes: &["wavespeed.ai"], hosts: &["api.wavespeed.ai"] },
];

/// The `/generate` skill picks its models on Kie.ai (§10.3).
const SKILL_GENERATORS: &[(&str, &str)] = &[("generate", "kie")];

fn name_words(name: &str) -> impl Iterator<Item = String> + '_ {
    name.split(|c: char| !c.is_ascii_alphanumeric()).filter(|w| !w.is_empty()).map(|w| w.to_ascii_lowercase())
}

fn suffix_of(host: &str, suffix: &str) -> bool {
    host == suffix || host.ends_with(&format!(".{suffix}"))
}

/// The table's generators an MCP server is recognised as: by a word of its name, or by its url
/// host (lowercase, any port dropped) under one of a generator's suffixes.
fn known_mcp_generators(name: &str, host: Option<&str>) -> Vec<&'static KnownGenerator> {
    let host = host.map(|h| {
        let h = h.to_ascii_lowercase();
        h.rsplit_once(':').filter(|(_, port)| port.bytes().all(|b| b.is_ascii_digit())).map_or(h.clone(), |(host, _)| host.to_string())
    });
    KNOWN_GENERATOR_HOSTS
        .iter()
        .filter(|k| host.as_deref().is_some_and(|h| k.suffixes.iter().any(|s| suffix_of(h, s))) || name_words(name).any(|w| w == k.key))
        .collect()
}

/// Whether an `.claude.json` MCP server entry is a generator a media worker may be handed (plan
/// D1): recognised by its url host or its name, as `generator_hosts` recognises one. Anything
/// else (a github, a filesystem server) runs outside the sandbox and the gate, so a worker never
/// gets it, whatever the human ticked. A stdio entry has no url: only its name can vouch for it.
pub fn is_generator_server(name: &str, entry: &serde_json::Value) -> bool {
    let host = entry.get("url").and_then(|u| u.as_str()).and_then(url_host);
    !known_mcp_generators(name, host.as_deref()).is_empty()
}

/// The hosts a worker's sandbox needs for the generators the human chose (§14.16, plan D2): the
/// table's hosts for each chosen server or skill it recognises, plus a recognised server's own
/// url host when that host is under a generator's suffix. Any other ticked server (a github, a
/// filesystem) adds nothing: its host would become a media worker's way out, and a name alone
/// doesn't vouch for the host it points at. Lowercased, deduplicated, anything that isn't a
/// public host name dropped (a proxy entry's 127.0.0.1, a port), and so is a repo's remote
/// host, which stays denied (§7.3).
pub fn generator_hosts(detected: &[DetectedTool], chosen: &[String], remote_hosts: &[String]) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut push = |h: &str| {
        let bare = h.rsplit_once(':').filter(|(_, port)| port.bytes().all(|b| b.is_ascii_digit())).map_or(h, |(host, _)| host);
        if let Ok(n) = super::gallery::budget::normalize_host(bare) {
            let remote = remote_hosts.iter().any(|r| super::gallery::budget::host_covers(&n, &r.to_ascii_lowercase()));
            if !remote && !out.contains(&n) {
                out.push(n);
            }
        }
    };
    for t in detected.iter().filter(|t| chosen.contains(&t.id)) {
        let host = t.host.as_deref().map(str::to_ascii_lowercase);
        let known: Vec<&KnownGenerator> = match t.kind.as_str() {
            "mcp" => {
                if let Some(h) = host.as_deref().filter(|h| !known_mcp_generators("", Some(h)).is_empty()) {
                    push(h);
                }
                known_mcp_generators(&t.label, host.as_deref())
            }
            "skill" => {
                let key = SKILL_GENERATORS.iter().find(|(skill, _)| *skill == t.label).map(|(_, k)| *k);
                KNOWN_GENERATOR_HOSTS.iter().filter(|k| key == Some(k.key) || name_words(&t.label).any(|w| w == k.key)).collect()
            }
            _ => Vec::new(),
        };
        for k in known {
            k.hosts.iter().for_each(|h| push(h));
        }
    }
    out
}

/// Every chosen generator's hosts, from a fresh detection, minus the Hive's repo remotes. Blocks:
/// tool detection and git.
pub fn detect_generator_hosts(hive: &Hive, prefs: &crate::state::Preferences) -> Vec<String> {
    generator_hosts(&detect_tools(prefs), &hive.tools, &super::settings::hive_remote_hosts(hive))
}

/// Gallery settings → Re-detect: what the Hive's chosen generators need now, for the human to
/// tick in. An older Hive made before step 8 has no hosts until then. Nothing is saved here.
#[tauri::command]
pub async fn hive_detect_generator_hosts(window: tauri::Window, state: State<'_, Arc<AppState>>, workspace_id: String) -> Result<Vec<String>, String> {
    let (hive, prefs) = {
        let data = state.app_data.read();
        let ws = data.window(window.label()).and_then(|w| w.workspaces.iter().find(|w| w.id == workspace_id)).ok_or("Workspace not found")?;
        (ws.hive.clone().ok_or("That workspace isn't a Hive")?, data.preferences.clone())
    };
    tauri::async_runtime::spawn_blocking(move || detect_generator_hosts(&hive, &prefs)).await.map_err(|e| e.to_string())
}

fn detect_tools(prefs: &crate::state::Preferences) -> Vec<DetectedTool> {
    let mut out: Vec<DetectedTool> = Vec::new();
    for root in trust_roots(prefs) {
        if let Some(v) = std::fs::read(&root).ok().and_then(|b| serde_json::from_slice::<serde_json::Value>(&b).ok()) {
            out.extend(mcp_tools(&v));
        }
    }
    for cli in DETECTED_CLIS {
        if !crate::accounts::find_executables(cli).is_empty() {
            out.push(DetectedTool { id: format!("cli:{cli}"), kind: "cli".to_string(), label: cli.to_string(), host: None });
        }
    }
    if let Some(home) = dirs::home_dir() {
        out.extend(skill_tools(&home.join(".claude").join("skills")));
    }
    let comfy = COMFYUI_ADDR.parse().ok().is_some_and(|a| std::net::TcpStream::connect_timeout(&a, Duration::from_millis(300)).is_ok());
    if comfy {
        out.push(DetectedTool { id: "comfyui".to_string(), kind: "comfyui".to_string(), label: "ComfyUI".to_string(), host: Some(COMFYUI_ADDR.to_string()) });
    }
    out
}

#[tauri::command]
pub async fn hive_detect_tools(state: State<'_, Arc<AppState>>) -> Result<Vec<DetectedTool>, String> {
    let prefs = state.app_data.read().preferences.clone();
    tauri::async_runtime::spawn_blocking(move || detect_tools(&prefs)).await.map_err(|e| e.to_string())
}

/// What the Create sheet sends. Folders are as the human set them; Rust re-detects each one.
#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct HiveSpec {
    pub name: String,
    pub goal: String,
    /// Rules for every agent (`Hive.instructions`).
    pub instructions: String,
    /// The project brief, the coordinator's alone (`Hive.brief`).
    pub brief: String,
    /// Full paths of the files attached to it, copied at creation (`copy_brief_files`).
    pub brief_files: Vec<String>,
    pub folders: Vec<HiveFolder>,
    pub cap: u32,
    pub landing_mode: String,
    pub models: HiveModels,
    pub tools: Vec<String>,
    pub budget_cents: u64,
    pub push_after_land: bool,
    /// The Stack step (§3 step 3): definitions and `shared_from` references, checked again by
    /// `import_stack`.
    pub stack: Vec<Service>,
    /// "Keep this Hive's agents off the phone": every Hive tab is made `mailink_excluded`.
    pub keep_off_phone: bool,
    /// Workers are chats on the phone as well as the coordinator.
    pub workers_on_phone: bool,
}

impl Default for HiveSpec {
    fn default() -> Self {
        Self {
            name: String::new(),
            goal: String::new(),
            instructions: String::new(),
            brief: String::new(),
            brief_files: Vec::new(),
            folders: Vec::new(),
            cap: DEFAULT_CAP,
            landing_mode: LANDING_BASE.to_string(),
            models: HiveModels::default(),
            tools: Vec::new(),
            budget_cents: DEFAULT_BUDGET_CENTS,
            push_after_land: false,
            stack: Vec::new(),
            keep_off_phone: false,
            workers_on_phone: false,
        }
    }
}

/// The running service in another workspace whose command and folder this one repeats, and
/// the port it holds: a fixed-port copy here would collide with it.
fn port_holder<'a>(data: &'a AppData, runtime: &HashMap<String, StackRuntimeRow>, workspace_id: &str, s: &Service) -> Option<(&'a Workspace, &'a Service, u16)> {
    data.windows.iter().flat_map(|w| w.workspaces.iter()).filter(|w| w.id != workspace_id).find_map(|w| {
        w.stack
            .iter()
            .filter(|o| o.shared_from.is_none() && o.command.trim() == s.command.trim() && Service::expand_cwd(&o.cwd) == s.cwd)
            .filter(|o| runtime.get(&o.id).is_some_and(|r| matches!(r.status.as_str(), "starting" | "running" | "ready")))
            .find_map(|o| o.port.map(|p| (w, o, p)))
    })
}

/// The Stack step's rows (§3 step 3), checked again here: the sheet's choices are advice.
///
/// - A `shared_from` row must name a service in ANOTHER workspace that is not itself shared,
///   and it never starts here (no auto-start, no intent).
/// - A definition needs a command and an absolute folder, `{checkout}` only in env, and env
///   names a shell takes. One with a fixed port (no `{port}`) that repeats a service another
///   workspace is running on a known port is refused, naming it: the Hive would fight it for
///   the port. Any other fixed-port one is imported and runs one copy at a time.
/// - Every row is new: a fresh id, no running intent, never agent-edited (the human chose it in
///   the sheet). A name this workspace already has is left to the existing service; a name
///   twice in the step is refused.
pub fn import_stack(
    existing: &[Service],
    incoming: &[Service],
    data: &AppData,
    runtime: &HashMap<String, StackRuntimeRow>,
    workspace_id: &str,
) -> Result<Vec<Service>, String> {
    let have: HashSet<String> = existing.iter().map(|s| Service::normalize_name(&s.name)).collect();
    let mut seen: HashSet<String> = HashSet::new();
    let mut out = Vec::new();
    for row in incoming {
        let name = row.name.trim().to_string();
        if name.is_empty() {
            return Err("Every stack service needs a name.".to_string());
        }
        let key = Service::normalize_name(&name);
        if !seen.insert(key.clone()) {
            return Err(format!("Two stack services are called {name}."));
        }
        if have.contains(&key) {
            continue;
        }
        let mut s = row.clone();
        s.id = uuid::Uuid::new_v4().to_string();
        s.name = name.clone();
        s.normalized_name = key;
        s.was_running = false;
        s.running_targets.clear();
        s.agent_edited = false;
        if s.origin != "human" {
            s.origin = "suggested".to_string();
        }
        if let Some(r) = &s.shared_from {
            if r.workspace_id == workspace_id {
                return Err(format!("{name}: a service can only be shared from another workspace."));
            }
            match crate::stack::shared_source(data, r) {
                None => return Err(format!("{name}: the service it would be shared from is gone.")),
                Some((ws, src)) if src.shared_from.is_some() => {
                    return Err(format!("{name}: {} in {} is itself shared from elsewhere; share the original.", src.name, ws.name))
                }
                Some(_) => {}
            }
            s.auto_start = false;
        } else {
            if s.command.trim().is_empty() {
                return Err(format!("{name} needs a command."));
            }
            s.cwd = Service::expand_cwd(&s.cwd);
            if !Path::new(&s.cwd).is_absolute() {
                return Err(format!("{name} needs a full path for its folder."));
            }
            let needs = crate::stack::placeholders::Needs::of(&s.command, &s.env);
            if needs.checkout_in_command {
                return Err(format!("{name}: {{checkout}} can only be used in env values, not in the command."));
            }
            if let Some((k, _)) = s.env.iter().find(|(k, _)| !crate::stack::placeholders::valid_env_key(k)) {
                return Err(format!("{name}: env name {k:?} isn't one a shell takes."));
            }
            if needs.one_copy() {
                if let Some((ws, other, port)) = port_holder(data, runtime, workspace_id, &s) {
                    return Err(format!(
                        "{name} has a fixed port, and {} in {} runs the same command on :{port}. Add {{port}} to its command so the Hive runs its own copies, or share the running one.",
                        other.name, ws.name
                    ));
                }
            }
        }
        out.push(s);
    }
    Ok(out)
}

/// The `hive/<slug>` branch name part: lowercase ASCII letters and digits, any other run one
/// dash, at most 60. The TS twin (`slugify` in lib/hive/model.ts) previews it.
pub fn slugify(name: &str) -> String {
    let mut out = String::new();
    for c in name.chars() {
        if c.is_ascii_alphanumeric() {
            out.push(c.to_ascii_lowercase());
        } else if !out.is_empty() && !out.ends_with('-') {
            out.push('-');
        }
    }
    out.truncate(60);
    out.trim_end_matches('-').to_string()
}

/// The sheet's rules, checked again here (the TS twin is `validateSpec`).
pub fn validate_spec(spec: &HiveSpec) -> Result<(), String> {
    if spec.name.trim().is_empty() {
        return Err("Name the Hive.".to_string());
    }
    if slugify(&spec.name).is_empty() {
        return Err("The name needs at least one letter or digit: it names the Hive's branches.".to_string());
    }
    if spec.goal.trim().is_empty() {
        return Err("Give the Hive a goal.".to_string());
    }
    let n = spec.instructions.chars().count();
    if n > MAX_INSTRUCTIONS {
        return Err(format!("The rules for every agent are {n} characters; the limit is 16,000. Shorten them: nothing is cut."));
    }
    let n = spec.brief.chars().count();
    if n > MAX_BRIEF {
        return Err(format!("The project brief is {n} characters; the limit is 32,000. Shorten it, or attach the rest as a file."));
    }
    if spec.brief_files.len() > MAX_BRIEF_FILES {
        return Err(format!("Attach at most {MAX_BRIEF_FILES} files to the project brief."));
    }
    if spec.brief_files.iter().any(|f| !crate::share::expand_tilde(f.trim()).is_absolute()) {
        return Err("Every attached file needs a full path.".to_string());
    }
    match spec.folders.iter().filter(|f| f.primary).count() {
        0 => return Err("Pick a Primary folder.".to_string()),
        1 => {}
        _ => return Err("Only one folder can be Primary.".to_string()),
    }
    if spec.folders.iter().any(|f| !Path::new(&f.path).is_absolute()) {
        return Err("Every folder needs a full path.".to_string());
    }
    if spec.cap == 0 || spec.cap > super::change::MAX_CAP {
        return Err(format!("Workers at once is 1 to {}.", super::change::MAX_CAP));
    }
    if ![LANDING_BASE, LANDING_INTEGRATION].contains(&spec.landing_mode.as_str()) {
        return Err(format!("Unknown landing mode \"{}\"", spec.landing_mode));
    }
    for m in [&spec.models.coordinator, &spec.models.worker] {
        if !(m.runtime.is_empty() || m.runtime == "claude") {
            return Err("Only Claude runs in a Hive in this version.".to_string());
        }
    }
    Ok(())
}

/// A repo with files and no commit (the sheet's twin is in `validateFolders`).
const UNBORN_REFUSAL: &str = "the repo has no commit yet, so a worker's copy would have none of its files. Use Commit these files on its row.";

fn non_empty(s: &Option<String>) -> Option<String> {
    s.as_deref().map(str::trim).filter(|s| !s.is_empty()).map(str::to_string)
}

/// The folders as stored: each path canonical and re-detected, never trusted from the sheet.
/// `app_data` is maiTerm's data folder (canonical), which no folder may hold or sit in.
/// Blocks on git.
pub fn resolve_folders(spec: &[HiveFolder], roots: &[PathBuf], app_data: Option<&Path>) -> Result<Vec<HiveFolder>, String> {
    let mut out: Vec<HiveFolder> = Vec::new();
    for f in spec {
        let name = f.name.trim().to_string();
        let path = Path::new(&f.path);
        if !path.is_dir() {
            return Err(format!("{name}: no folder at {}", f.path));
        }
        let info = layout::detect_folder(path, roots);
        if let Some(why) = app_data.and_then(|a| app_data_refusal(Path::new(&info.path), a)) {
            return Err(format!("{name}: {} {why}, which a Hive's agents must never reach. Pick the project's own folder.", info.path));
        }
        if out.iter().any(|o| o.path == info.path) {
            return Err(format!("Two folders point at {}", info.path));
        }
        // A repo may be shared as a plain folder (read-only, no worktrees); a plain one can't
        // become a repo.
        let git = f.kind == KIND_GIT;
        if git && info.kind != KIND_GIT {
            return Err(format!("{name}: {} isn't the top folder of a git repository", info.path));
        }
        // An empty one was given its first commit by `start_new_repos`.
        if git && info.unborn {
            return Err(format!("{name}: {UNBORN_REFUSAL}"));
        }
        // Base is what a land merges into and the read copy sits on (§3, §8.1).
        let base = if git { non_empty(&f.base).or(info.base) } else { None };
        if git && base.is_none() {
            return Err(format!("{name}: pick a Base branch (the repo is on a detached HEAD), or share it as a plain folder."));
        }
        out.push(HiveFolder {
            name,
            path: info.path,
            kind: if git { KIND_GIT } else { KIND_PLAIN }.to_string(),
            base,
            setup: if git { non_empty(&f.setup) } else { None },
            land_check: if git { non_empty(&f.land_check) } else { None },
            primary: f.primary,
            local_remotes: if git { info.local_remotes } else { Vec::new() },
        });
    }
    Ok(out)
}

/// The new projects among the sheet's git folders, made ready for read copies before
/// `resolve_folders` reads them (§3.6, `layout::start_repo`): an empty folder becomes a repo on
/// its Base (`main` unless the human named another), with an empty first commit. Answers them,
/// for the caller to trust once Create succeeds. A Create that fails later leaves the repo made,
/// which is all it is: an empty first commit.
pub fn start_new_repos(spec: &[HiveFolder], app_data: Option<&Path>) -> Result<Vec<PathBuf>, String> {
    let mut empty = Vec::new();
    for f in spec.iter().filter(|f| f.kind == KIND_GIT) {
        let path = Path::new(&f.path);
        if !path.is_dir() {
            continue; // `resolve_folders` says so
        }
        let info = layout::detect_folder(path, &[]);
        // Only an empty folder: a repo with files and no commit is `resolve_folders`' refusal.
        if !info.empty || (info.kind == KIND_GIT && !info.unborn) {
            continue;
        }
        let name = f.name.trim();
        if let Some(why) = app_data.and_then(|a| app_data_refusal(Path::new(&info.path), a)) {
            return Err(format!("{name}: {} {why}, which a Hive's agents must never reach. Pick the project's own folder.", info.path));
        }
        let branch = non_empty(&f.base).or(info.base).unwrap_or_else(|| layout::NEW_REPO_BRANCH.to_string());
        if layout::start_repo(path, &branch).map_err(|e| format!("{name}: {e}"))? {
            empty.push(PathBuf::from(&info.path));
        }
    }
    Ok(empty)
}

/// A new project's repo trusted where the Hive's agents launch (§3.6): an empty folder has no
/// project settings, hooks or MCP servers of its own, which are what trusting a repo lets run, and
/// the human chose it for this Hive. A failure is the sheet's untrusted warning, never a refusal.
fn trust_new_repos(empty: &[PathBuf], roots: &[PathBuf]) {
    let Some(launch_json) = roots.first() else { return };
    for p in empty {
        if let Err(e) = super::trust::trust_repo(launch_json, p) {
            log::warn!("hive: couldn't trust the new repo {}: {e}", p.display());
        }
    }
}

pub fn build_hive(spec: &HiveSpec, id: String, folders: Vec<HiveFolder>) -> Hive {
    let mut models = spec.models.clone();
    for m in [&mut models.coordinator, &mut models.worker] {
        if m.runtime.is_empty() {
            m.runtime = "claude".to_string();
        }
    }
    let mut tools: Vec<String> = Vec::new();
    for t in &spec.tools {
        if !t.is_empty() && !tools.contains(t) {
            tools.push(t.clone());
        }
    }
    Hive {
        id,
        // One line: it's the header and the kickoff's last words.
        goal: spec.goal.split_whitespace().collect::<Vec<_>>().join(" "),
        instructions: spec.instructions.clone(),
        brief: spec.brief.trim().to_string(),
        // The copies' names, set once they're made (`copy_brief_files`).
        brief_files: Vec::new(),
        criteria_confirm: CRITERIA_NEEDED.to_string(),
        folders,
        cap: spec.cap,
        landing_mode: spec.landing_mode.clone(),
        models,
        tools,
        checkpoints: default_checkpoints(&spec.landing_mode),
        budget_cents: spec.budget_cents,
        push_after_land: spec.push_after_land,
        criteria: Vec::new(),
        status: STATUS_ACTIVE.to_string(),
        done_at: None,
        slug: slugify(&spec.name),
        created_at: crate::commands::workspace::iso_now(),
        settings: HiveSettings { keep_off_phone: spec.keep_off_phone, workers_on_phone: spec.workers_on_phone, ..HiveSettings::default() },
        ..Hive::default()
    }
}

#[derive(Debug, Clone)]
pub struct Installed {
    pub workspace: Workspace,
    pub coordinator_tab_id: String,
    /// The shell removed in the same write, found under the lock.
    pub replaced: Option<Replaceable>,
    /// The workspace as it was, for a save that fails.
    pub before: Workspace,
}

/// Install a laid-out Hive into its workspace, in one write: the record, the name, the fresh
/// shell it replaces taken out, and the coordinator's hidden tab in the first pane. The tab is
/// never made active: it isn't in the strip, and a pane whose active tab is hidden shows
/// nothing (§5). `expected` is the shell whose PTY was checked; any other answer under the lock
/// means the tabs changed meanwhile, and nothing is installed.
pub fn install_hive(
    data: &mut AppData,
    label: &str,
    workspace_id: &str,
    hive: Hive,
    stack: Vec<Service>,
    name: &str,
    expected: Option<&Replaceable>,
) -> Result<Installed, String> {
    let win = data.window_mut(label).ok_or("Window not found")?;
    let ws = win.workspaces.iter_mut().find(|w| w.id == workspace_id).ok_or("Workspace not found")?;
    let replaced = structural_eligibility(ws)?;
    if replaced.as_ref() != expected {
        return Err(WORKSPACE_CHANGED.to_string());
    }
    if ws.panes.is_empty() {
        return Err("The workspace has no pane".to_string());
    }
    let before = ws.clone();
    if let Some(r) = &replaced {
        if let Some(pane) = ws.panes.iter_mut().find(|p| p.id == r.pane_id) {
            let at = pane.tabs.iter().position(|t| t.id == r.tab_id).unwrap_or(0);
            pane.tabs.retain(|t| t.id != r.tab_id);
            if pane.active_tab_id.as_deref() == Some(r.tab_id.as_str()) {
                pane.active_tab_id = crate::commands::workspace::pick_active_after_close(&pane.tabs, at);
            }
        }
    }
    let mut tab = Tab::new(COORDINATOR_TAB_NAME.to_string());
    tab.custom_name = true;
    tab.hive_role = Some(ROLE_COORDINATOR.to_string());
    // Before its launch: the coordinator's first turn is never a chat on the phone either.
    tab.mailink_excluded = hive.settings.keep_off_phone;
    let coordinator_tab_id = tab.id.clone();
    ws.panes[0].tabs.push(tab);
    ws.hive = Some(hive);
    // `import_stack` checked them against this workspace's names; one added since is kept.
    for s in stack {
        if !ws.stack.iter().any(|e| e.normalized_name == s.normalized_name || Service::normalize_name(&e.name) == s.normalized_name) {
            ws.stack.push(s);
        }
    }
    let name = name.trim();
    if !name.is_empty() {
        ws.name = name.to_string();
    }
    Ok(Installed { workspace: ws.clone(), coordinator_tab_id, replaced, before })
}

/// An install that reached memory but not the disk: the workspace goes back to what it was, its
/// name and shell included, so no later save writes half a Create.
pub fn undo_install(data: &mut AppData, label: &str, workspace_id: &str, hive_id: &str, before: Workspace) {
    let ws = data.window_mut(label).and_then(|w| w.workspaces.iter_mut().find(|w| w.id == workspace_id));
    if let Some(ws) = ws.filter(|ws| ws.hive.as_ref().is_some_and(|h| h.id == hive_id)) {
        *ws = before;
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct HiveCreated {
    pub workspace: Workspace,
    pub coordinator_tab_id: String,
    /// The fresh shell the Hive replaced, already gone from `workspace`: the frontend drops its
    /// mirror, which unmounts its terminal and kills the PTY.
    pub replace_tab: Option<Replaceable>,
}

/// The project brief's attached files, copied into the coordinator's home (`brief/`), so it reads
/// them in its own folder as they were when the Hive was made. Their names there, in order: a
/// name already taken gets a number before its extension.
pub fn copy_brief_files(sources: &[String], home: &Path) -> Result<Vec<String>, String> {
    if sources.is_empty() {
        return Ok(Vec::new());
    }
    let dir = home.join(BRIEF_FILES_DIR);
    std::fs::create_dir_all(&dir).map_err(|e| format!("Couldn't make the project brief's folder: {e}"))?;
    let mut names: Vec<String> = Vec::new();
    for s in sources {
        let src = crate::share::expand_tilde(s.trim());
        let meta = std::fs::metadata(&src).map_err(|_| format!("The attached file {} isn't there.", src.display()))?;
        if !meta.is_file() {
            return Err(format!("{} isn't a file: attach files, not folders.", src.display()));
        }
        if meta.len() > MAX_BRIEF_FILE_BYTES {
            return Err(format!("{} is over 5 MB: attach a smaller file, or put it in one of the Hive's folders.", src.display()));
        }
        let name = src.file_name().map(|n| n.to_string_lossy().to_string()).filter(|n| !n.is_empty()).ok_or_else(|| format!("{} has no file name.", src.display()))?;
        let (stem, ext) = match name.rsplit_once('.') {
            Some((s, e)) if !s.is_empty() => (s.to_string(), format!(".{e}")),
            _ => (name.clone(), String::new()),
        };
        let mut candidate = name.clone();
        let mut n = 2;
        // Asked of the disk too: APFS folds case and Unicode form, so `Spec.md` and `spec.md`
        // would be one file there.
        while names.contains(&candidate) || std::fs::symlink_metadata(dir.join(&candidate)).is_ok() {
            candidate = format!("{stem}-{n}{ext}");
            n += 1;
        }
        std::fs::copy(&src, dir.join(&candidate)).map_err(|e| format!("Couldn't copy {}: {e}", src.display()))?;
        names.push(candidate);
    }
    Ok(names)
}

#[tauri::command]
pub async fn hive_create(
    window: tauri::Window,
    app_handle: tauri::AppHandle,
    state: State<'_, Arc<AppState>>,
    workspace_id: String,
    spec: HiveSpec,
) -> Result<HiveCreated, String> {
    platform_refusal(cfg!(windows))?;
    crate::instance::hive_refusal()?;
    let label = window.label().to_string();
    validate_spec(&spec)?;
    let (replaceable, prefs, stack) = {
        let data = state.app_data.read();
        let ws = data.window(&label).and_then(|w| w.workspaces.iter().find(|w| w.id == workspace_id)).ok_or("Workspace not found")?;
        let replaceable = structural_eligibility(ws)?;
        let stack = import_stack(&ws.stack, &spec.stack, &data, &state.stack_runtime.read(), &workspace_id)?;
        (replaceable, data.preferences.clone(), stack)
    };

    let app = state.inner().clone();
    let spec_for_disk = spec.clone();
    let shell = replaceable.as_ref().map(|r| r.tab_id.clone());
    let (hive, root) = tauri::async_runtime::spawn_blocking(move || -> Result<(Hive, PathBuf), String> {
        if let Some(t) = &shell {
            shell_check(&app, t)?;
        }
        // A coordinator that can't start is a broken Hive: refuse here rather than at launch.
        launch::claude_new_enough()?;
        let roots = trust_roots(&prefs);
        let empty = start_new_repos(&spec_for_disk.folders, app_data_dir().as_deref())?;
        let folders = resolve_folders(&spec_for_disk.folders, &roots, app_data_dir().as_deref())?;
        let mut hive = build_hive(&spec_for_disk, uuid::Uuid::new_v4().to_string(), folders);
        // The queued commands it starts with, read from the folders (§6.8); edited in settings.
        hive.settings.run_queue = super::queue::detect_commands(&hive.folders);
        // Its generators' hosts, fixed here and edited by the human, never re-derived at launch: a
        // server added to a config file later must not widen a sandbox on its own (plan D2).
        hive.settings.gallery.generator_hosts = detect_generator_hosts(&hive, &prefs);
        layout::validate(&hive)?;
        let root = hive_root(&hive.id).ok_or("Couldn't find the app data folder")?;
        layout::create_layout(&root, &hive).inspect_err(|_| {
            // Its read copies are already taken back; the fixed folders are this Hive's alone.
            let _ = std::fs::remove_dir_all(&root);
        })?;
        let paths = HivePaths::new(&root);
        match copy_brief_files(&spec_for_disk.brief_files, &paths.coordinator_home()) {
            Ok(names) => hive.brief_files = names,
            Err(e) => {
                layout::undo_layout(&root, &hive);
                return Err(e);
            }
        }
        if let Err(e) = launch::write_record(&paths.coordinator_task(), &launch::coordinator_record(&hive, &paths)) {
            layout::undo_layout(&root, &hive);
            return Err(e);
        }
        trust_new_repos(&empty, &roots);
        Ok((hive, root))
    })
    .await
    .map_err(|e| e.to_string())??;

    // The layout took seconds: ask the shell again just before it's closed.
    let app = state.inner().clone();
    let shell = replaceable.as_ref().map(|r| r.tab_id.clone());
    let recheck = tauri::async_runtime::spawn_blocking(move || shell.map_or(Ok(()), |t| shell_check(&app, &t)))
        .await
        .map_err(|e| e.to_string())
        .and_then(|r| r);

    let hive_id = hive.id.clone();
    let installed = recheck.and_then(|()| {
        let mut data = state.app_data.write();
        install_hive(&mut data, &label, &workspace_id, hive.clone(), stack, &spec.name, replaceable.as_ref()).map(|r| (r, data.clone()))
    });
    let saved = installed.and_then(|(r, snapshot)| match save_state(&snapshot) {
        Ok(()) => Ok(r),
        Err(e) => {
            undo_install(&mut state.app_data.write(), &label, &workspace_id, &hive_id, r.before);
            Err(e)
        }
    });
    let Installed { workspace, coordinator_tab_id, replaced, .. } = match saved {
        Ok(r) => r,
        Err(e) => {
            // Never saved, so nothing refers to it: take the disk side back too.
            tauri::async_runtime::spawn_blocking(move || layout::undo_layout(&root, &hive));
            return Err(e);
        }
    };
    if let Some(r) = &replaced {
        let _ = state.scrollback_db.delete(&r.tab_id);
    }
    if let Some(root) = hive_root(&hive_id) {
        super::memory::watch(state.inner(), &app_handle, &hive_id, &HivePaths::new(root));
    }
    log::info!("hive {hive_id}: created in workspace {workspace_id} (coordinator tab {coordinator_tab_id})");
    Ok(HiveCreated { workspace, coordinator_tab_id, replace_tab: replaced })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::{Task, WindowData};

    fn task(tab: &str, status: &str) -> Task {
        serde_json::from_value(serde_json::json!({
            "id": "t1", "title": "t", "status": status, "tab_id": tab, "origin": "human", "created_at": "", "updated_at": ""
        }))
        .unwrap()
    }

    fn shell(id: &str) -> Tab {
        let mut t = Tab::new("Terminal".into());
        t.id = id.into();
        t
    }

    fn app_with(ws: Workspace) -> AppData {
        let mut win = WindowData::new("main".into());
        win.workspaces = vec![ws];
        AppData { windows: vec![win], ..AppData::default() }
    }

    fn empty_ws() -> Workspace {
        let mut ws = Workspace::new("Plain".into());
        ws.id = "ws".into();
        ws.panes[0].tabs.clear();
        ws.panes[0].active_tab_id = None;
        ws
    }

    fn ws_with_shell() -> Workspace {
        let mut ws = empty_ws();
        ws.panes[0].tabs = vec![shell("sh")];
        ws.panes[0].active_tab_id = Some("sh".into());
        ws
    }

    fn sample() -> Hive {
        Hive { id: "h1".into(), slug: "ship".into(), goal: "Ship".into(), ..Hive::default() }
    }

    #[test]
    fn native_windows_is_refused() {
        assert!(platform_refusal(true).unwrap_err().contains("Windows"));
        assert!(platform_refusal(false).is_ok());
    }

    #[test]
    fn an_empty_workspace_or_one_fresh_shell_is_eligible() {
        assert_eq!(structural_eligibility(&empty_ws()), Ok(None));
        let ws = ws_with_shell();
        assert_eq!(structural_eligibility(&ws), Ok(Some(Replaceable { pane_id: ws.panes[0].id.clone(), tab_id: "sh".into() })));
        // A done task on it doesn't hold it.
        let mut ws = ws_with_shell();
        ws.tasks.push(task("sh", "done"));
        assert!(structural_eligibility(&ws).is_ok());
    }

    #[test]
    fn a_used_or_busy_workspace_is_not() {
        let refused = |f: &dyn Fn(&mut Workspace)| {
            let mut ws = ws_with_shell();
            f(&mut ws);
            structural_eligibility(&ws).is_err()
        };
        assert!(refused(&|ws| ws.panes[0].tabs.push(shell("two"))), "two tabs");
        assert!(refused(&|ws| ws.panes[0].tabs[0].runtime = Some(crate::state::AgentRuntime::Claude)), "an agent ran");
        assert!(refused(&|ws| ws.panes[0].tabs[0].auto_resume_command = Some("claude --resume x".into())));
        assert!(refused(&|ws| ws.panes[0].tabs[0].notes = Some("keep this".into())));
        assert!(refused(&|ws| ws.panes[0].tabs[0].tab_type = TabType::Editor));
        assert!(refused(&|ws| ws.panes[0].tabs[0].service_id = Some("svc".into())), "a service tab is a tab");
        assert!(refused(&|ws| ws.tasks.push(task("sh", "active"))));
        assert!(refused(&|ws| ws.bridge_all = true), "a mesh");
        assert!(refused(&|ws| ws.overlord = true));
        assert!(refused(&|ws| ws.suspended = true));
        assert!(refused(&|ws| ws.hive = Some(sample())), "already one");
        assert!(!refused(&|ws| ws.panes[0].tabs[0].notes = Some("  \n".into())), "blank notes are none");
    }

    fn the_shell(ws: &Workspace) -> Replaceable {
        Replaceable { pane_id: ws.panes[0].id.clone(), tab_id: "sh".into() }
    }

    /// A new Hive waits for its criteria to be confirmed; one from before that, with no state
    /// recorded, never does.
    #[test]
    fn a_new_hive_keeps_its_brief_and_waits_for_confirmed_criteria() {
        let hive = build_hive(&HiveSpec { brief: "  The plan.\n".into(), ..HiveSpec::default() }, "h1".into(), Vec::new());
        assert_eq!((hive.brief.as_str(), hive.criteria_confirm.as_str()), ("The plan.", CRITERIA_NEEDED));
        assert!(hive.criteria_unconfirmed());
        let old: Hive = serde_json::from_value(serde_json::json!({ "id": "h0", "goal": "g" })).unwrap();
        assert!(!old.criteria_unconfirmed(), "a Hive from before the gate isn't held by it");
        let saved = serde_json::to_value(&old).unwrap();
        assert!(saved.get("brief").is_none() && saved.get("criteria_confirm").is_none(), "nothing new written for it");
    }

    /// "Keep this Hive's agents off the phone": the coordinator is excluded before its launch,
    /// and a worker's tab is made excluded too. Off by default.
    #[test]
    fn keep_off_phone_excludes_every_hive_tab_as_it_is_made() {
        let spec = HiveSpec { name: "Ship".into(), keep_off_phone: true, ..HiveSpec::default() };
        let hive = build_hive(&spec, "h1".into(), Vec::new());
        assert!(hive.settings.keep_off_phone);
        let mut d = app_with(empty_ws());
        let got = install_hive(&mut d, "main", "ws", hive.clone(), Vec::new(), "Ship", None).unwrap();
        let coord = got.workspace.panes[0].tabs.iter().find(|t| t.id == got.coordinator_tab_id).unwrap();
        assert!(coord.mailink_excluded, "excluded in the same write that made it");
        let w = super::super::worker::WorkerRecord { title: "API".into(), ..Default::default() };
        assert!(super::super::tools::worker_tab(&w, &hive).mailink_excluded);

        let plain = build_hive(&HiveSpec::default(), "h2".into(), Vec::new());
        assert!(!plain.settings.keep_off_phone);
        let mut d = app_with(empty_ws());
        let got = install_hive(&mut d, "main", "ws", plain.clone(), Vec::new(), "", None).unwrap();
        assert!(!got.workspace.panes[0].tabs.iter().any(|t| t.mailink_excluded), "the coordinator is a chat on the phone");
        assert!(super::super::tools::worker_tab(&w, &plain).mailink_excluded, "by default a worker isn't");
        let shown = build_hive(&HiveSpec { workers_on_phone: true, ..HiveSpec::default() }, "h3".into(), Vec::new());
        assert!(!super::super::tools::worker_tab(&w, &shown).mailink_excluded, "workers shown when the human chose it");
        let both = build_hive(&HiveSpec { workers_on_phone: true, keep_off_phone: true, ..HiveSpec::default() }, "h4".into(), Vec::new());
        assert!(super::super::tools::worker_tab(&w, &both).mailink_excluded, "keep off the phone wins");
        // A Create sheet from before the options sends no field: both off.
        let old: HiveSpec = serde_json::from_value(serde_json::json!({ "name": "x" })).unwrap();
        assert!(!old.keep_off_phone && !old.workers_on_phone);
    }

    /// Only Claude's explicit "not logged in" warns; a login it can't vouch for, or output that
    /// isn't its JSON, says nothing.
    #[test]
    fn the_login_check_warns_only_on_an_explicit_logged_out() {
        let out = br#"{"loggedIn": false, "authMethod": "none", "apiProvider": "firstParty"}"#;
        let w = login_verdict(out, None).unwrap();
        assert!(w.contains("isn't logged in") && w.contains("/login"), "{w}");
        assert!(login_verdict(out, Some("Work")).unwrap().contains("\"Work\""));
        assert!(login_verdict(br#"{"loggedIn": true, "authMethod": "claude.ai", "email": "a@b"}"#, None).is_none());
        assert!(login_verdict(br#"{"loggedIn": true, "authMethod": "oauth_token"}"#, None).is_none(), "a token: can't tell, no warning");
        assert!(login_verdict(br#"{"authMethod": "none"}"#, None).is_none(), "no answer is no claim");
        assert!(login_verdict(b"error: unknown command", None).is_none());
    }

    /// The second live test: `/status` said "Login: Expired — log in again" and the JSON said
    /// logged in. The text form is 2.1.310's `NPn` lines, `label: value`.
    #[test]
    fn an_expired_login_warns_from_the_text_form() {
        let expired = "Login: Expired \u{2014} log in again\nOrganization: Acme\nEmail: a@b\n".as_bytes();
        let w = expired_verdict(expired, None).unwrap();
        assert!(w.contains("expired") && w.contains("/login"), "{w}");
        assert!(expired_verdict(expired, Some("Work")).unwrap().contains("\"Work\""));
        assert!(expired_verdict(b"Login method: Claude Max account\nOrganization: Acme\nEmail: a@b\n", None).is_none(), "a live login");
        assert!(expired_verdict(b"Not logged in. Run claude auth login to authenticate.\n", None).is_none(), "the JSON's case");
        assert!(expired_verdict(b"Organization: Expired Things Ltd\n", None).is_none(), "only the Login line counts");
        assert!(expired_verdict(b"", None).is_none());
    }

    /// The second live test: the default Primary lagged a fresh `cd`, since `last_cwd` is saved later.
    #[test]
    fn the_default_primary_prefers_the_live_folder_on_this_machine() {
        let ids = vec!["active".to_string(), "before".to_string()];
        let last = |id: &str| match id {
            "active" => Some("/r/old".to_string()),
            "before" => Some("/r/prev".to_string()),
            _ => None,
        };
        let live = |id: &str, cwd: &str, host: Option<&str>| LiveCwd { tab_id: id.into(), cwd: Some(cwd.into()), host: host.map(String::from) };
        assert_eq!(primary_candidates(&ids, &[], last, "mybox"), vec!["/r/old", "/r/prev"], "nothing live: Rust's");
        assert_eq!(
            primary_candidates(&ids, &[live("active", "/r/new", Some("mybox.local"))], last, "MyBox"),
            vec!["/r/new", "/r/old", "/r/prev"],
            "the live folder first, Rust's after it should the live one not be usable"
        );
        assert_eq!(primary_candidates(&ids, &[live("active", "/r/new", None)], last, "mybox")[0], "/r/new", "no host is this machine");
        assert_eq!(
            primary_candidates(&ids, &[live("active", "/home/u/app", Some("nova"))], last, "mybox"),
            vec!["/r/old", "/r/prev"],
            "an ssh tab's folder is another machine's"
        );
        assert_eq!(primary_candidates(&ids, &[live("active", "/r/old", None)], last, "mybox"), vec!["/r/old", "/r/prev"], "once each");
    }

    /// The sheet's trust read and the login check use the root the launch line names, never an
    /// inherited `CLAUDE_CONFIG_DIR`.
    #[test]
    fn the_sheet_reads_the_launchs_own_root() {
        let none = crate::state::Preferences::default();
        assert_eq!(trust_roots(&none), dirs::home_dir().map(|h| h.join(".claude.json")).into_iter().collect::<Vec<_>>());
        assert_eq!(trust_roots(&none), launch::launch_claude_json(&none).into_iter().collect::<Vec<_>>());
    }

    #[test]
    fn install_never_makes_the_coordinator_active() {
        // An empty workspace: its pane had nothing active and still has nothing active.
        let mut d = app_with(empty_ws());
        let got = install_hive(&mut d, "main", "ws", sample(), Vec::new(), " Ship it ", None).unwrap();
        let (ws, coord) = (got.workspace, got.coordinator_tab_id);
        assert_eq!(ws.panes[0].active_tab_id, None);
        let tab = ws.panes[0].tabs.iter().find(|t| t.id == coord).unwrap();
        assert_eq!(tab.hive_role.as_deref(), Some(ROLE_COORDINATOR));
        assert!(tab.is_strip_hidden());
        assert_eq!(ws.name, "Ship it");
        assert_eq!(ws.hive.as_ref().map(|h| h.id.as_str()), Some("h1"));
        assert_eq!(got.replaced, None);
        // The returned copy is what's in the state.
        let stored = &d.window("main").unwrap().workspaces[0];
        assert_eq!(stored.hive, ws.hive);
        assert_eq!(stored.panes[0].tabs.len(), 1);

        // With a shell to replace: it goes in the same write, and the pane is left with nothing
        // active rather than the hidden coordinator.
        let start = ws_with_shell();
        let shell_ref = the_shell(&start);
        let mut d = app_with(start);
        let got = install_hive(&mut d, "main", "ws", sample(), Vec::new(), "", Some(&shell_ref)).unwrap();
        assert_eq!(got.replaced, Some(shell_ref));
        assert_eq!(got.workspace.panes[0].active_tab_id, None);
        assert_eq!(got.workspace.panes[0].tabs.iter().map(|t| t.id.as_str()).collect::<Vec<_>>(), vec![got.coordinator_tab_id.as_str()]);
        assert_eq!(got.workspace.name, "Plain", "a blank name keeps the workspace's");
        assert_eq!(got.before.panes[0].active_tab_id.as_deref(), Some("sh"));
    }

    #[test]
    fn install_rechecks_eligibility_and_changes_nothing_when_refused() {
        let mut ws = ws_with_shell();
        ws.panes[0].tabs.push(shell("opened-meanwhile"));
        let mut d = app_with(ws);
        let before = serde_json::to_string(&d).unwrap();
        assert!(install_hive(&mut d, "main", "ws", sample(), Vec::new(), "X", None).is_err());
        assert_eq!(serde_json::to_string(&d).unwrap(), before);
        assert!(install_hive(&mut d, "main", "gone", sample(), Vec::new(), "X", None).is_err());
        assert!(install_hive(&mut d, "other-window", "ws", sample(), Vec::new(), "X", None).is_err());
    }

    #[test]
    fn install_refuses_a_shell_other_than_the_one_it_checked() {
        // Empty when checked; a fresh shell opened during the layout (Cmd+T behind the sheet).
        let mut d = app_with(ws_with_shell());
        let before = serde_json::to_string(&d).unwrap();
        assert_eq!(install_hive(&mut d, "main", "ws", sample(), Vec::new(), "X", None).unwrap_err(), WORKSPACE_CHANGED);
        // The checked shell closed and another opened in its place.
        let mut other = the_shell(&ws_with_shell());
        other.tab_id = "sh-before".into();
        assert_eq!(install_hive(&mut d, "main", "ws", sample(), Vec::new(), "X", Some(&other)).unwrap_err(), WORKSPACE_CHANGED);
        assert_eq!(serde_json::to_string(&d).unwrap(), before, "nothing installed");
        // The checked shell closed, nothing opened.
        let mut d = app_with(empty_ws());
        assert!(install_hive(&mut d, "main", "ws", sample(), Vec::new(), "X", Some(&other)).is_err());
    }

    #[test]
    fn a_failed_save_puts_the_workspace_back_name_and_shell_included() {
        let start = ws_with_shell();
        let shell_ref = the_shell(&start);
        let mut d = app_with(start);
        let before = serde_json::to_string(&d).unwrap();
        let got = install_hive(&mut d, "main", "ws", sample(), Vec::new(), "Ship it", Some(&shell_ref)).unwrap();
        undo_install(&mut d, "main", "ws", "other-hive", got.before.clone());
        assert_ne!(serde_json::to_string(&d).unwrap(), before, "only its own Hive is taken back");
        undo_install(&mut d, "main", "ws", "h1", got.before);
        assert_eq!(serde_json::to_string(&d).unwrap(), before);
        assert_eq!(d.window("main").unwrap().workspaces[0].name, "Plain");
    }

    #[test]
    fn only_a_shell_read_at_its_prompt_is_idle() {
        assert!(shell_verdict(None).is_ok(), "no live PTY: nothing to kill");
        assert!(shell_verdict(Some(Some(true))).is_ok());
        assert_eq!(shell_verdict(Some(Some(false))).unwrap_err(), SHELL_BUSY);
        assert_eq!(shell_verdict(Some(None)).unwrap_err(), SHELL_UNREADABLE, "can't tell is never idle");
    }

    /// Only a used tab that alone stands in the way is offered for closing; a fresh shell, two
    /// tabs, a bridged tab, one with open tasks or a mesh workspace never is.
    #[test]
    fn only_a_lone_used_tab_is_offered_for_closing() {
        let used = |f: &dyn Fn(&mut Tab)| {
            let mut ws = empty_ws();
            let mut t = shell("sh");
            t.runtime = Some(crate::state::AgentRuntime::Claude);
            f(&mut t);
            ws.panes[0].tabs = vec![t];
            ws
        };
        assert_eq!(lone_used_tab(&used(&|_| {})), Some("sh".to_string()));
        assert_eq!(lone_used_tab(&used(&|t| t.runtime = None)), None, "a fresh shell is replaced, not closed");
        assert_eq!(lone_used_tab(&used(&|t| t.agent_bridge = serde_json::from_value(serde_json::json!({ "partner_tab_id": "p", "partner_label": "P" })).ok())), None);
        let mut two = used(&|_| {});
        two.panes[0].tabs.push(shell("other"));
        assert_eq!(lone_used_tab(&two), None);
        let mut mesh = used(&|_| {});
        mesh.bridge_all = true;
        assert_eq!(lone_used_tab(&mesh), None);
        let mut tasked = used(&|_| {});
        tasked.tasks.push(task("sh", "active"));
        assert_eq!(lone_used_tab(&tasked), None);
    }

    #[test]
    fn archived_tabs_make_a_workspace_ineligible() {
        let mut ws = empty_ws();
        ws.archived_tabs.push(shell("old"));
        assert!(structural_eligibility(&ws).unwrap_err().contains("archived"));
    }

    #[test]
    fn no_folder_may_hold_or_sit_in_the_app_data_folder() {
        let app = Path::new("/Users/x/Library/Application Support/com.aiterm.dev");
        assert!(app_data_refusal(Path::new("/Users/x"), app).unwrap().contains("holds"));
        assert!(app_data_refusal(Path::new("/"), app).is_some());
        assert!(app_data_refusal(app, app).is_some());
        assert!(app_data_refusal(&app.join("hive/h1/coordinator/home"), app).unwrap().contains("inside"));
        assert!(app_data_refusal(Path::new("/Users/x/code/web"), app).is_none());
        assert!(app_data_refusal(Path::new("/Users/x/Library/Application Support/com.aiterm.dev2"), app).is_none(), "by component, not prefix");
    }

    fn spec() -> HiveSpec {
        HiveSpec {
            name: "Ship it".into(),
            goal: "Ship the thing".into(),
            folders: vec![HiveFolder { name: "web".into(), path: "/r/web".into(), kind: KIND_GIT.into(), primary: true, ..HiveFolder::default() }],
            ..HiveSpec::default()
        }
    }

    #[test]
    fn brief_files_are_copied_into_the_coordinators_home_under_names_of_their_own() {
        let base = std::env::temp_dir().join(format!("hive-brief-{}", uuid::Uuid::new_v4()));
        let (a, b, home) = (base.join("a"), base.join("b"), base.join("home"));
        std::fs::create_dir_all(&a).unwrap();
        std::fs::create_dir_all(&b).unwrap();
        std::fs::write(a.join("plan.md"), "first").unwrap();
        std::fs::write(b.join("plan.md"), "second").unwrap();
        std::fs::write(a.join("Makefile"), "m").unwrap();
        let s = |p: &Path| p.to_string_lossy().to_string();
        assert_eq!(copy_brief_files(&[], &home).unwrap(), Vec::<String>::new());
        assert!(!home.exists(), "nothing attached, no folder");
        let names = copy_brief_files(&[s(&a.join("plan.md")), s(&b.join("plan.md")), s(&a.join("Makefile"))], &home).unwrap();
        assert_eq!(names, vec!["plan.md", "plan-2.md", "Makefile"]);
        assert_eq!(std::fs::read_to_string(home.join("brief/plan-2.md")).unwrap(), "second", "each its own copy");
        std::fs::write(b.join("PLAN.md"), "third").unwrap();
        // True on APFS's default (case-insensitive), false on a case-sensitive disk such as CI's.
        let folds_case = home.join("brief/PLAN.md").exists();
        let more = copy_brief_files(&[s(&b.join("PLAN.md"))], &home).unwrap();
        assert_eq!(more != vec!["PLAN.md"], folds_case, "a name the disk already holds, whatever its case, gets a number");
        assert_eq!(std::fs::read_to_string(home.join("brief/plan.md")).unwrap(), "first", "nothing overwritten");
        assert!(copy_brief_files(&[s(&a)], &home).unwrap_err().contains("isn't a file"));
        assert!(copy_brief_files(&[s(&a.join("gone.md"))], &home).unwrap_err().contains("isn't there"));
        let _ = std::fs::remove_dir_all(&base);
    }

    // `/r/web` is absolute only on Unix; no Hive is made on Windows (§14.16).
    #[cfg(unix)]
    #[test]
    fn the_spec_is_validated_like_the_sheet() {
        assert_eq!(validate_spec(&spec()), Ok(()));
        let bad = |f: &dyn Fn(&mut HiveSpec)| {
            let mut s = spec();
            f(&mut s);
            validate_spec(&s).err()
        };
        assert!(bad(&|s| s.instructions = "x".repeat(MAX_INSTRUCTIONS)).is_none(), "16,000 exactly is allowed");
        let over = bad(&|s| s.instructions = "é".repeat(MAX_INSTRUCTIONS + 1)).unwrap();
        assert!(over.contains("16001") && over.contains("nothing is cut"), "counted in characters, refused: {over}");
        assert!(bad(&|s| s.brief = "é".repeat(MAX_BRIEF)).is_none(), "32,000 exactly is allowed");
        assert!(bad(&|s| s.brief = "x".repeat(MAX_BRIEF + 1)).unwrap().contains("32001"));
        assert!(bad(&|s| s.brief_files = vec!["/a/plan.md".into(); MAX_BRIEF_FILES + 1]).unwrap().contains("at most 10"));
        assert!(bad(&|s| s.brief_files = vec!["plan.md".into()]).unwrap().contains("full path"));
        assert!(bad(&|s| s.folders[0].primary = false).unwrap().contains("Primary"));
        assert!(bad(&|s| s.folders.push(HiveFolder { name: "api".into(), path: "/r/api".into(), primary: true, ..HiveFolder::default() })).is_some());
        assert!(bad(&|s| s.folders[0].path = "relative/web".into()).is_some());
        assert!(bad(&|s| s.goal = "  ".into()).is_some());
        assert!(bad(&|s| s.name = "!!!".into()).unwrap().contains("letter or digit"));
        assert!(bad(&|s| s.cap = 0).is_some());
        assert!(bad(&|s| s.cap = crate::hive::change::MAX_CAP + 1).unwrap().contains("1 to 64"));
        assert!(bad(&|s| s.cap = crate::hive::change::MAX_CAP).is_none(), "no machine ceiling: memory pressure governs");
        assert!(bad(&|s| s.landing_mode = "someday".into()).is_some());
        assert!(bad(&|s| s.models.worker.runtime = "codex".into()).is_some());
    }

    #[test]
    fn slugs_name_a_branch() {
        assert_eq!(slugify("Ship it!"), "ship-it");
        assert_eq!(slugify("  --Session  store: reads--  "), "session-store-reads");
        assert_eq!(slugify("Café Ünïcode"), "caf-n-code");
        assert_eq!(slugify("!!!"), "");
        assert_eq!(slugify(&"a-".repeat(40)).len(), 59, "cut to 60, then the trailing dash");
        let s = slugify("Ship it");
        let h = Hive { slug: s, landing_mode: LANDING_BASE.into(), ..Hive::default() };
        assert!(layout::validate(&h).is_ok());
    }

    #[test]
    fn the_hive_takes_its_defaults() {
        let mut s = spec();
        s.goal = "Ship\n  the   thing".into();
        s.tools = vec!["cli:ffmpeg".into(), "cli:ffmpeg".into(), "".into()];
        s.models.coordinator.runtime = String::new();
        let h = build_hive(&s, "h1".into(), s.folders.clone());
        assert_eq!(h.goal, "Ship the thing");
        assert_eq!(h.slug, "ship-it");
        assert_eq!(h.tools, vec!["cli:ffmpeg".to_string()]);
        assert_eq!((h.models.coordinator.runtime.as_str(), h.models.coordinator.model.as_str(), h.models.coordinator.effort.as_str()), ("claude", "opus", "medium"));
        assert_eq!((h.models.worker.model.as_str(), h.models.worker.effort.as_str()), ("sonnet", "high"));
        assert_eq!((h.cap, h.budget_cents, h.status.as_str()), (DEFAULT_CAP, 1000, STATUS_ACTIVE));
        assert_eq!(h.checkpoints.len(), 7);
        assert!(h.checkpoints.iter().all(|(k, v)| v == if k == "settings" { "ask" } else { "coordinator" }), "the human approves settings changes");
        assert_eq!(default_checkpoints(LANDING_INTEGRATION).get("landing").map(String::as_str), Some("ask"), "integration mode always asks");
    }

    #[test]
    fn folders_are_re_detected_not_taken_from_the_sheet() {
        let base = crate::share::canon(&{
            let t = std::env::temp_dir().join(format!("hive-create-{}", uuid::Uuid::new_v4()));
            std::fs::create_dir_all(&t).unwrap();
            t
        });
        let web = base.join("web");
        std::fs::create_dir_all(&web).unwrap();
        let git = |args: &[&str]| {
            let ok = std::process::Command::new("git")
                .current_dir(&web)
                .env("GIT_CONFIG_GLOBAL", "/dev/null")
                .env("GIT_CONFIG_NOSYSTEM", "1")
                .args(["-c", "user.email=t@example.com", "-c", "user.name=t", "-c", "commit.gpgsign=false"])
                .args(args)
                .status()
                .unwrap()
                .success();
            assert!(ok, "git {args:?}");
        };
        git(&["init", "-q", "-b", "trunk"]);
        std::fs::write(web.join("a"), "a").unwrap();
        git(&["add", "-A"]);
        git(&["commit", "-q", "--no-verify", "-m", "init"]);
        let plain = base.join("brand");
        std::fs::create_dir_all(&plain).unwrap();

        let f = |name: &str, path: &Path, kind: &str| HiveFolder {
            name: name.into(),
            path: path.to_string_lossy().into(),
            kind: kind.into(),
            primary: name == "web",
            setup: Some("  ".into()),
            local_remotes: vec!["/forged".into()],
            ..HiveFolder::default()
        };
        let got = resolve_folders(&[f("web", &web.join("."), KIND_GIT), f("brand", &plain, KIND_PLAIN)], &[], None).unwrap();
        assert_eq!(got[0].path, web.to_string_lossy(), "canonical");
        assert_eq!(got[0].base.as_deref(), Some("trunk"), "the current branch by default");
        assert_eq!(got[0].setup, None, "blank is none");
        assert!(got[0].local_remotes.is_empty(), "read from the repo, never from the sheet");
        assert_eq!((got[1].kind.as_str(), got[1].base.clone()), (KIND_PLAIN, None));

        assert!(resolve_folders(&[f("brand", &plain, KIND_GIT)], &[], None).unwrap_err().contains("isn't the top folder"));
        assert!(resolve_folders(&[f("web", &web, KIND_GIT), f("again", &web, KIND_PLAIN)], &[], None).unwrap_err().contains("Two folders"));
        assert!(resolve_folders(&[f("gone", &base.join("nope"), KIND_PLAIN)], &[], None).is_err());
        // A repo shared as a plain folder is allowed: read-only, no read copy.
        assert_eq!(resolve_folders(&[f("web", &web, KIND_PLAIN)], &[], None).unwrap()[0].kind, KIND_PLAIN);

        // maiTerm's data folder (here a scratch one): holding it or sitting in it is refused.
        let app_data = base.join("app-data");
        std::fs::create_dir_all(app_data.join("hive")).unwrap();
        let refuse = |p: &Path| resolve_folders(&[f("x", p, KIND_PLAIN)], &[], Some(&app_data)).unwrap_err();
        assert!(refuse(&base).contains("holds maiTerm's own data folder"), "the folder above it, like ~");
        assert!(refuse(&app_data.join("hive")).contains("inside"));
        assert!(resolve_folders(&[f("brand", &plain, KIND_PLAIN)], &[], Some(&app_data)).is_ok(), "a sibling is fine");

        // A detached HEAD has no current branch: the human names a Base or shares it plain.
        git(&["checkout", "-q", "--detach"]);
        let mut detached = f("web", &web, KIND_GIT);
        assert!(resolve_folders(std::slice::from_ref(&detached), &[], None).unwrap_err().contains("pick a Base branch"));
        detached.base = Some("trunk".into());
        assert_eq!(resolve_folders(&[detached], &[], None).unwrap()[0].base.as_deref(), Some("trunk"));
        assert!(resolve_folders(&[f("web", &web, KIND_PLAIN)], &[], None).is_ok());
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn tools_come_from_the_root_and_skip_maiterm_itself() {
        let v = serde_json::json!({ "mcpServers": {
            "maiterm": { "type": "http", "url": "http://127.0.0.1:8765/mcp" },
            "maiterm-dev": {},
            "fal": { "type": "http", "url": "https://user:pw@mcp.fal.ai/mcp?x=1" },
            "local": { "command": "npx", "args": ["x"] }
        }});
        let tools = mcp_tools(&v);
        assert_eq!(tools.iter().map(|t| t.id.as_str()).collect::<Vec<_>>(), vec!["mcp:fal", "mcp:local"]);
        assert_eq!(tools[0].host.as_deref(), Some("mcp.fal.ai"));
        assert_eq!(tools[1].host, None);
        assert!(mcp_tools(&serde_json::json!({})).is_empty());
    }

    /// Plan D2: a chosen server's url host plus the table's hosts for what it recognises; an
    /// unchosen server, a local one, a proxy's loopback and a repo remote add nothing.
    #[test]
    fn generator_hosts_come_from_the_chosen_tools_and_the_table() {
        let home = std::env::temp_dir().join(format!("hive-gen-hosts-{}", uuid::Uuid::new_v4().simple()));
        std::fs::create_dir_all(&home).unwrap();
        std::fs::write(
            home.join(".claude.json"),
            r#"{ "mcpServers": {
                "fal": { "type": "http", "url": "https://mcp.fal.ai/mcp" },
                "local": { "command": "npx", "args": ["x"] },
                "higgs": { "type": "http", "url": "https://mcp.higgsfield.ai/mcp" },
                "proxied": { "type": "http", "url": "http://127.0.0.1:8765/mcp/x/abc" },
                "kling-video": { "command": "kling-mcp" },
                "github": { "type": "http", "url": "https://api.githubcopilot.com/mcp/" },
                "fal-mirror": { "type": "http", "url": "https://mcp.example.com/fal" }
            } }"#,
        )
        .unwrap();
        let v: serde_json::Value = serde_json::from_slice(&std::fs::read(home.join(".claude.json")).unwrap()).unwrap();
        let mut detected = mcp_tools(&v);
        detected.push(DetectedTool { id: "skill:generate".into(), kind: "skill".into(), label: "generate".into(), host: None });
        detected.push(DetectedTool { id: "cli:ffmpeg".into(), kind: "cli".into(), label: "ffmpeg".into(), host: None });
        let chosen = |ids: &[&str]| ids.iter().map(|s| s.to_string()).collect::<Vec<_>>();

        let hosts = generator_hosts(&detected, &chosen(&["mcp:fal", "mcp:local", "cli:ffmpeg"]), &[]);
        assert_eq!(hosts, vec!["mcp.fal.ai", "fal.run", "queue.fal.run", "fal.media", "v3.fal.media"]);
        // A remote host is never a generator host, however it was found.
        let hosts = generator_hosts(&detected, &chosen(&["mcp:fal"]), &["queue.fal.run".to_string()]);
        assert!(!hosts.contains(&"queue.fal.run".to_string()) && hosts.contains(&"fal.run".to_string()), "{hosts:?}");
        // By url suffix, by name word, the skill's table entry; a loopback proxy adds nothing.
        let hosts = generator_hosts(&detected, &chosen(&["mcp:higgs", "mcp:kling-video", "skill:generate", "mcp:proxied"]), &[]);
        assert_eq!(hosts, vec!["mcp.higgsfield.ai", "platform.higgsfield.ai", "api.klingai.com", "api.kie.ai"]);
        assert!(generator_hosts(&detected, &[], &[]).is_empty(), "nothing chosen, nothing reached");
        // A ticked server that is no generator adds nothing: a github one's host would be a
        // media worker's way out. One recognised by name alone adds the table's hosts, never its own.
        assert!(generator_hosts(&detected, &chosen(&["mcp:github", "mcp:local", "mcp:proxied"]), &[]).is_empty());
        assert_eq!(generator_hosts(&detected, &chosen(&["mcp:fal-mirror"]), &[]), vec!["fal.run", "queue.fal.run", "fal.media", "v3.fal.media"]);
        let _ = std::fs::remove_dir_all(&home);
    }

    fn svc(name: &str, command: &str, cwd: &str) -> Service {
        serde_json::from_value(serde_json::json!({
            "id": format!("id-{name}"), "name": name, "command": command, "cwd": cwd,
            "origin": "suggested", "created_at": "", "updated_at": ""
        }))
        .unwrap()
    }

    /// A human's workspace "Main" running `web` (`npm run dev` in /p, :5173) and `postgres`.
    fn running_elsewhere() -> (AppData, HashMap<String, StackRuntimeRow>, Service, Service) {
        let mut main = Workspace::new("Main".into());
        let mut web = svc("web", "npm run dev", "/p");
        web.port = Some(5173);
        let pg = svc("postgres", "postgres -D data", "/p");
        main.stack = vec![web.clone(), pg.clone()];
        let mut data = app_with(empty_ws());
        data.windows[0].workspaces.push(main);
        let mut runtime = HashMap::new();
        runtime.insert(web.id.clone(), serde_json::from_value(serde_json::json!({"service_id": web.id, "status": "ready"})).unwrap());
        (data, runtime, web, pg)
    }

    // Unix paths (`/p`): not absolute on Windows, where a Hive is refused anyway.
    #[cfg(unix)]
    #[test]
    fn the_stack_step_imports_definitions_fresh_and_references_infrastructure() {
        let (data, runtime, _web, pg) = running_elsewhere();
        let main_id = data.windows[0].workspaces[1].id.clone();
        let mut db = svc("db", "", "/p");
        db.shared_from = Some(crate::state::workspace::ServiceRef { workspace_id: main_id, service_id: pg.id.clone() });
        db.auto_start = true;
        let mut dev = svc(" Dev ", "npm run dev -- --port {port}", "/p");
        dev.was_running = true;
        dev.running_targets = vec!["branch:x".into()];
        dev.agent_edited = true;
        dev.origin = "agent".into();
        let api = svc("api", "cargo run", "/p/api");
        let got = import_stack(&[], &[db, dev, api], &data, &runtime, "ws").unwrap();
        assert_eq!(got.iter().map(|s| s.name.as_str()).collect::<Vec<_>>(), vec!["db", "Dev", "api"]);
        assert!(got.iter().all(|s| !s.id.starts_with("id-")), "every row gets a fresh id");
        assert!(!got[0].auto_start && got[0].shared_from.is_some(), "a reference never starts here");
        let d = &got[1];
        assert!(!d.was_running && d.running_targets.is_empty() && !d.agent_edited && d.origin == "suggested", "{d:?}");
        assert_eq!(d.normalized_name, "dev");
    }

    // Unix paths (`/p`): not absolute on Windows, where a Hive is refused anyway.
    #[cfg(unix)]
    #[test]
    fn a_fixed_port_that_another_workspace_holds_is_refused_and_named() {
        let (data, mut runtime, web, _) = running_elsewhere();
        let copy = svc("web", "npm run dev", "/p");
        let err = import_stack(&[], std::slice::from_ref(&copy), &data, &runtime, "ws").unwrap_err();
        assert!(err.contains("web in Main") && err.contains(":5173") && err.contains("{port}"), "{err}");
        // With {port} it runs its own copies; elsewhere stopped, the fixed port is free.
        assert!(import_stack(&[], &[svc("web", "npm run dev -- --port {port}", "/p")], &data, &runtime, "ws").is_ok());
        runtime.get_mut(&web.id).unwrap().status = "stopped".into();
        assert_eq!(import_stack(&[], &[copy], &data, &runtime, "ws").unwrap().len(), 1);
    }

    // Unix paths (`/p`): not absolute on Windows, where a Hive is refused anyway.
    #[cfg(unix)]
    #[test]
    fn bad_stack_rows_are_refused_and_existing_names_kept() {
        let (data, runtime, _, pg) = running_elsewhere();
        let refused = |s: Service| import_stack(&[], &[s], &data, &runtime, "ws").unwrap_err();
        assert!(refused(svc("x", "ls {checkout}", "/p")).contains("{checkout}"));
        assert!(refused(svc("x", "", "/p")).contains("needs a command"));
        assert!(refused(svc("x", "run", "relative")).contains("full path"));
        let mut env = svc("x", "run", "/p");
        env.env = vec![("A-B".into(), "1".into())];
        assert!(refused(env).contains("env name"));
        let mut own = svc("x", "", "/p");
        own.shared_from = Some(crate::state::workspace::ServiceRef { workspace_id: "ws".into(), service_id: pg.id.clone() });
        assert!(refused(own).contains("another workspace"));
        let mut gone = svc("x", "", "/p");
        gone.shared_from = Some(crate::state::workspace::ServiceRef { workspace_id: "nope".into(), service_id: "nope".into() });
        assert!(refused(gone).contains("gone"));
        assert!(import_stack(&[], &[svc("a", "x", "/p"), svc("A.", "y", "/p")], &data, &runtime, "ws").unwrap_err().contains("Two"));
        let kept = import_stack(&[svc("api", "old", "/p")], &[svc("API", "new", "/p"), svc("web2", "x", "/p")], &data, &runtime, "ws").unwrap();
        assert_eq!(kept.iter().map(|s| s.name.as_str()).collect::<Vec<_>>(), vec!["web2"], "the workspace's own api wins");
    }

    #[test]
    fn install_puts_the_imported_stack_in_the_same_write() {
        let mut d = app_with(empty_ws());
        let got = install_hive(&mut d, "main", "ws", sample(), vec![svc("web", "x", "/p")], "Ship", None).unwrap();
        assert_eq!(got.workspace.stack.len(), 1);
        assert!(got.before.stack.is_empty(), "undo restores the stack too");
    }
}
