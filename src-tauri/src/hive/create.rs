//! Turning a workspace into a Hive (docs/hive.md §3): what the Create sheet reads (preflight,
//! folder and tool detection, the machine's cap ceiling) and `hive_create`.
//!
//! The sheet's checks are advice; Rust re-checks everything on Create, and the workspace's
//! eligibility again under the write lock that installs the Hive.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use tauri::State;

use super::layout::{self, FolderInfo};
use super::launch;
use super::paths::{hive_root, HivePaths};
use super::record::{Hive, HiveFolder, HiveModels, KIND_GIT, KIND_PLAIN, LANDING_BASE, LANDING_INTEGRATION, STATUS_ACTIVE};
use super::ROLE_COORDINATOR;
use crate::state::workspace::TabType;
use crate::state::{save_state, AppData, AppState, Tab, Workspace};

/// Projects' instructions limit. Over it is refused, never cut: a truncated rule reads as a
/// different rule (§3).
pub const MAX_INSTRUCTIONS: usize = 16_000;
pub const DEFAULT_CAP: u32 = 4;
pub const DEFAULT_BUDGET_CENTS: u64 = 1000;
pub const COORDINATOR_TAB_NAME: &str = "Coordinator";

/// The §7.4 checkpoints, all Coordinator by default. In integration mode a land is always the
/// human's (§8.3).
pub fn default_checkpoints(landing_mode: &str) -> BTreeMap<String, String> {
    let mut out: BTreeMap<String, String> =
        ["plan", "questions", "permissions", "landing", "finals", "interfaces"].iter().map(|k| (k.to_string(), "coordinator".to_string())).collect();
    if landing_mode == LANDING_INTEGRATION {
        out.insert("landing".to_string(), "ask".to_string());
    }
    out
}

/// About one worker per 16 GB above a 32 GB reserve, never more than ncpu/4, never below one
/// (§6.8; the doc's "6 on this Mac" predates the ncpu term, which gives 5).
pub fn cap_ceiling(ram_gb: u64, ncpu: u32) -> u32 {
    let by_ram = ram_gb.saturating_sub(32) / 16;
    let by_cpu = u64::from(ncpu / 4);
    by_ram.min(by_cpu).max(1) as u32
}

pub fn machine_ceiling() -> u32 {
    let mut sys = sysinfo::System::new();
    sys.refresh_memory();
    let ram_gb = sys.total_memory() / (1 << 30);
    let ncpu = std::thread::available_parallelism().map(|n| n.get() as u32).unwrap_or(4);
    cap_ceiling(ram_gb, ncpu)
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
        return Err("A mesh workspace can't be a Hive: a Hive's agents talk to their coordinator.".to_string());
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
        return Err(format!("Its tab \"{}\" has been used. A Hive replaces only a fresh shell.", tab.name));
    }
    if ws.tasks.iter().any(|t| t.tab_id.as_deref() == Some(tab.id.as_str()) && !t.is_retired()) {
        return Err(format!("Its tab \"{}\" has tasks assigned.", tab.name));
    }
    Ok(Some(Replaceable { pane_id: pane_id.to_string(), tab_id: tab.id.clone() }))
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

/// `<app data>`: a folder that is it, holds it or sits in it would put the state file, other
/// Hives' Rust-owned folders and secrets on an agent's reach list, and its `denyWrite` would
/// cover the coordinator's own home (§6.2).
fn app_data_dir() -> Option<PathBuf> {
    let d = super::paths::hives_dir()?.parent()?.to_path_buf();
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
    pub ceiling: u32,
    pub default_cap: u32,
    pub platform_ok: bool,
    pub claude_ok: bool,
    pub claude_detail: Option<String>,
}

/// The `.claude.json` a new Hive's agents would read trust from: the active Claude account's
/// root, or `~/.claude.json` without managed accounts (§6.6). Launch writes every root; this is
/// only the sheet's warning.
pub fn trust_roots(prefs: &crate::state::Preferences) -> Vec<PathBuf> {
    let active = crate::accounts::active_accounts(prefs)
        .into_iter()
        .find(|(rt, _)| *rt == crate::accounts::Runtime::Claude)
        .and_then(|(rt, id)| crate::accounts::account_root(rt, &id));
    match active {
        Some(root) => vec![root.join(".claude.json")],
        None => dirs::home_dir().map(|h| vec![h.join(".claude.json")]).unwrap_or_default(),
    }
}

fn local_dir(raw: &str) -> Option<String> {
    let p = crate::share::expand_tilde(raw.trim());
    (p.is_absolute() && p.is_dir()).then(|| crate::share::canon(&p).to_string_lossy().to_string())
}

#[tauri::command]
pub async fn hive_preflight(
    window: tauri::Window,
    state: State<'_, Arc<AppState>>,
    workspace_id: String,
    from_tab_ids: Option<Vec<String>>,
) -> Result<Preflight, String> {
    let label = window.label().to_string();
    let (structural, from_cwds) = {
        let data = state.app_data.read();
        let win = data.window(&label).ok_or("Window not found")?;
        let ws = win.workspaces.iter().find(|w| w.id == workspace_id).ok_or("Workspace not found")?;
        // In the caller's order: the window's active tab, then the one looked at before it.
        let from_cwds: Vec<String> = from_tab_ids
            .unwrap_or_default()
            .iter()
            .filter_map(|id| win.workspaces.iter().flat_map(|w| all_tabs(w)).find(|(_, t)| &t.id == id).and_then(|(_, t)| t.last_cwd.clone()))
            .collect();
        (structural_eligibility(ws), from_cwds)
    };
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
        let claude = launch::claude_new_enough();
        let app_data = app_data_dir();
        // Never one Create would refuse: a tab sitting in ~ would otherwise propose ~.
        let default_primary = from_cwds
            .iter()
            .filter_map(|c| local_dir(c))
            .find(|d| app_data.as_deref().is_none_or(|a| app_data_refusal(Path::new(d), a).is_none()));
        Preflight {
            eligible,
            reason,
            replaceable_tab,
            default_primary,
            ceiling: machine_ceiling(),
            default_cap: DEFAULT_CAP,
            platform_ok: platform_refusal(cfg!(windows)).is_ok(),
            claude_ok: claude.is_ok(),
            claude_detail: claude.err(),
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
    pub instructions: String,
    pub folders: Vec<HiveFolder>,
    pub cap: u32,
    /// The human confirmed a cap above the machine's ceiling.
    pub cap_confirmed: bool,
    pub landing_mode: String,
    pub models: HiveModels,
    pub tools: Vec<String>,
    pub budget_cents: u64,
    pub push_after_land: bool,
}

impl Default for HiveSpec {
    fn default() -> Self {
        Self {
            name: String::new(),
            goal: String::new(),
            instructions: String::new(),
            folders: Vec::new(),
            cap: DEFAULT_CAP,
            cap_confirmed: false,
            landing_mode: LANDING_BASE.to_string(),
            models: HiveModels::default(),
            tools: Vec::new(),
            budget_cents: DEFAULT_BUDGET_CENTS,
            push_after_land: false,
        }
    }
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
pub fn validate_spec(spec: &HiveSpec, ceiling: u32) -> Result<(), String> {
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
        return Err(format!("The instructions are {n} characters; the limit is 16,000. Shorten them: nothing is cut."));
    }
    match spec.folders.iter().filter(|f| f.primary).count() {
        0 => return Err("Pick a Primary folder.".to_string()),
        1 => {}
        _ => return Err("Only one folder can be Primary.".to_string()),
    }
    if spec.folders.iter().any(|f| !Path::new(&f.path).is_absolute()) {
        return Err("Every folder needs a full path.".to_string());
    }
    if spec.cap == 0 {
        return Err("Workers at once must be at least 1.".to_string());
    }
    if spec.cap > ceiling && !spec.cap_confirmed {
        return Err(format!("{} workers at once is above this computer's ceiling of {ceiling}. Confirm it to go ahead.", spec.cap));
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
    let coordinator_tab_id = tab.id.clone();
    ws.panes[0].tabs.push(tab);
    ws.hive = Some(hive);
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

#[tauri::command]
pub async fn hive_create(
    window: tauri::Window,
    state: State<'_, Arc<AppState>>,
    workspace_id: String,
    spec: HiveSpec,
) -> Result<HiveCreated, String> {
    platform_refusal(cfg!(windows))?;
    let label = window.label().to_string();
    let ceiling = machine_ceiling();
    validate_spec(&spec, ceiling)?;
    let (replaceable, prefs) = {
        let data = state.app_data.read();
        let ws = data.window(&label).and_then(|w| w.workspaces.iter().find(|w| w.id == workspace_id)).ok_or("Workspace not found")?;
        (structural_eligibility(ws)?, data.preferences.clone())
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
        let folders = resolve_folders(&spec_for_disk.folders, &trust_roots(&prefs), app_data_dir().as_deref())?;
        let hive = build_hive(&spec_for_disk, uuid::Uuid::new_v4().to_string(), folders);
        layout::validate(&hive)?;
        let root = hive_root(&hive.id).ok_or("Couldn't find the app data folder")?;
        layout::create_layout(&root, &hive).inspect_err(|_| {
            // Its read copies are already taken back; the fixed folders are this Hive's alone.
            let _ = std::fs::remove_dir_all(&root);
        })?;
        let paths = HivePaths::new(&root);
        if let Err(e) = launch::write_record(&paths.coordinator_task(), &launch::coordinator_record(&hive, &paths)) {
            layout::undo_layout(&root, &hive);
            return Err(e);
        }
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
        install_hive(&mut data, &label, &workspace_id, hive.clone(), &spec.name, replaceable.as_ref()).map(|r| (r, data.clone()))
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
    fn the_ceiling_follows_ram_and_cpus() {
        assert_eq!(cap_ceiling(128, 20), 5, "this Mac: RAM allows 6, ncpu/4 is 5");
        assert_eq!(cap_ceiling(32, 10), 1, "nothing above the reserve still allows one");
        assert_eq!(cap_ceiling(16, 8), 1);
        assert_eq!(cap_ceiling(64, 16), 2);
        assert_eq!(cap_ceiling(256, 64), 14);
        assert_eq!(cap_ceiling(256, 2), 1, "ncpu/4 rounds to zero, still one");
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

    #[test]
    fn install_never_makes_the_coordinator_active() {
        // An empty workspace: its pane had nothing active and still has nothing active.
        let mut d = app_with(empty_ws());
        let got = install_hive(&mut d, "main", "ws", sample(), " Ship it ", None).unwrap();
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
        let got = install_hive(&mut d, "main", "ws", sample(), "", Some(&shell_ref)).unwrap();
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
        assert!(install_hive(&mut d, "main", "ws", sample(), "X", None).is_err());
        assert_eq!(serde_json::to_string(&d).unwrap(), before);
        assert!(install_hive(&mut d, "main", "gone", sample(), "X", None).is_err());
        assert!(install_hive(&mut d, "other-window", "ws", sample(), "X", None).is_err());
    }

    #[test]
    fn install_refuses_a_shell_other_than_the_one_it_checked() {
        // Empty when checked; a fresh shell opened during the layout (Cmd+T behind the sheet).
        let mut d = app_with(ws_with_shell());
        let before = serde_json::to_string(&d).unwrap();
        assert_eq!(install_hive(&mut d, "main", "ws", sample(), "X", None).unwrap_err(), WORKSPACE_CHANGED);
        // The checked shell closed and another opened in its place.
        let mut other = the_shell(&ws_with_shell());
        other.tab_id = "sh-before".into();
        assert_eq!(install_hive(&mut d, "main", "ws", sample(), "X", Some(&other)).unwrap_err(), WORKSPACE_CHANGED);
        assert_eq!(serde_json::to_string(&d).unwrap(), before, "nothing installed");
        // The checked shell closed, nothing opened.
        let mut d = app_with(empty_ws());
        assert!(install_hive(&mut d, "main", "ws", sample(), "X", Some(&other)).is_err());
    }

    #[test]
    fn a_failed_save_puts_the_workspace_back_name_and_shell_included() {
        let start = ws_with_shell();
        let shell_ref = the_shell(&start);
        let mut d = app_with(start);
        let before = serde_json::to_string(&d).unwrap();
        let got = install_hive(&mut d, "main", "ws", sample(), "Ship it", Some(&shell_ref)).unwrap();
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
    fn the_spec_is_validated_like_the_sheet() {
        assert_eq!(validate_spec(&spec(), 5), Ok(()));
        let bad = |f: &dyn Fn(&mut HiveSpec)| {
            let mut s = spec();
            f(&mut s);
            validate_spec(&s, 5).err()
        };
        assert!(bad(&|s| s.instructions = "x".repeat(MAX_INSTRUCTIONS)).is_none(), "16,000 exactly is allowed");
        let over = bad(&|s| s.instructions = "é".repeat(MAX_INSTRUCTIONS + 1)).unwrap();
        assert!(over.contains("16001") && over.contains("nothing is cut"), "counted in characters, refused: {over}");
        assert!(bad(&|s| s.folders[0].primary = false).unwrap().contains("Primary"));
        assert!(bad(&|s| s.folders.push(HiveFolder { name: "api".into(), path: "/r/api".into(), primary: true, ..HiveFolder::default() })).is_some());
        assert!(bad(&|s| s.folders[0].path = "relative/web".into()).is_some());
        assert!(bad(&|s| s.goal = "  ".into()).is_some());
        assert!(bad(&|s| s.name = "!!!".into()).unwrap().contains("letter or digit"));
        assert!(bad(&|s| s.cap = 0).is_some());
        assert!(bad(&|s| s.cap = 6).unwrap().contains("ceiling of 5"));
        assert!(bad(&|s| { s.cap = 6; s.cap_confirmed = true; }).is_none(), "confirmed");
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
        assert!(h.checkpoints.values().all(|v| v == "coordinator") && h.checkpoints.len() == 6);
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
}
