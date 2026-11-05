//! Changing a Hive's tools after Create (docs/hive.md §3.4, docs/hive-media.md §7): Hive settings
//! → Tools. The human's alone, a window command and never an agent tool. It writes `Hive.tools`,
//! adds the generator hosts the human ticked for the newly added tools to the gallery's generator
//! hosts (the human's edit, as Budget and generation's Re-detect is), logs the change, and tells
//! the coordinator in a framed notice.
//!
//! Nothing caches `Hive.tools`: each launch reads the record from the state
//! (`launch::hive_launch_command`), and the worker's MCP servers (`settings::worker_mcp_config`),
//! its network (`settings::worker_domains`) and its brief's tool line all come from it. So a
//! worker gets the change at its next launch, and a running one keeps what it started with.
//!
//! The ids are checked against a fresh detection, never taken on the sheet's word: a newly added
//! id must be detected on this Mac now. One the Hive already has may stay though detection no
//! longer finds it (a server since dropped from the config): the launch leaves out what it can't
//! find, and the human removes it here. A setup-only program (`cli:brew`, `isSetupOnly` in
//! src/lib/hive/media.ts) is never a tool. Nothing here hands a worker a skill or a plugin:
//! skills load from user settings a worker never reads, and no plugin is a detected tool.
//!
//! The hosts offered are `create::generator_hosts` for the newly added tools only: a catalogue
//! provider's `sandbox_hosts` and a recognised native server's own url host. Never a provider's
//! `unverified_hosts` or `result_hosts`, which no sandbox gets (docs/hive-media.md §9). A removed
//! tool's hosts stay in the gallery's generator hosts, which only the human edits (plan D2).

use serde::Serialize;

use super::change::{ip_remotes, log_line, refuse_private_hosts, running_on_old};
use super::create::{generator_hosts, DetectedTool};
use super::deliver::one_line;
use super::fuses::STATUS_PAUSED;
use super::gallery::budget::{check_hosts, normalize_host, MAX_GENERATOR_HOSTS};
use super::record::{Hive, STATUS_ACTIVE, STATUS_DONE};
use super::rpc::HiveWindow;
use super::tools::{Caller, Ctx};

/// Programs maiTerm detects only to set providers up: never something a Hive makes with. Kept in
/// step with `SETUP_ONLY_TOOLS` in src/lib/hive/media.ts.
pub const SETUP_ONLY_TOOLS: [&str; 1] = ["cli:brew"];

pub fn is_setup_only(id: &str) -> bool {
    SETUP_ONLY_TOOLS.contains(&id)
}

/// How long a tool id may be, as shown in a refusal or a notice.
const SHOWN_ID: usize = 80;

/// A tool as a refusal names it to the human: its detected label, else its id without the kind
/// prefix (`mcp:elevenlabs` is "elevenlabs"), as the sheet shows it (`labelOf` in
/// src/lib/hive/toolsChange.ts). One line, escaped.
pub fn tool_name(id: &str, detected: &[DetectedTool]) -> String {
    let label = detected.iter().find(|t| t.id == id).map(|t| t.label.trim()).filter(|l| !l.is_empty());
    let bare = id.split_once(':').map(|(_, rest)| rest).filter(|r| !r.is_empty()).unwrap_or(id);
    one_line(label.unwrap_or(bare), SHOWN_ID)
}

/// What a tools change would do, before it is saved: the sheet shows it, the offered hosts ticked.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct ToolsPreview {
    /// The Hive's tools as they would be saved: trimmed, deduplicated, in the order given.
    pub tools: Vec<String>,
    pub added: Vec<String>,
    pub removed: Vec<String>,
    /// The generator hosts the added tools need that the gallery doesn't already have.
    pub hosts_offered: Vec<String>,
}

/// A tools change saved: the Hive as saved and what changed.
#[derive(Debug, Clone, Serialize)]
pub struct HiveToolsChange {
    pub hive: Hive,
    pub added: Vec<String>,
    pub removed: Vec<String>,
    /// The ticked hosts, now in the gallery's generator hosts.
    pub hosts_added: Vec<String>,
    /// Whether the coordinator's notice was delivered (false: it has no tab, or no agent took it).
    pub coordinator_told: bool,
}

/// A change checked against a record: the preview and the hosts the human ticked, normalised.
#[derive(Debug, Clone, PartialEq)]
pub struct Checked {
    pub preview: ToolsPreview,
    pub hosts_added: Vec<String>,
}

/// What `tools` would change on `hive`, or why it can't. `detected` is a fresh detection;
/// `remote_hosts` the hosts the Hive's repos push to, which no host offered may reach.
pub fn preview(hive: &Hive, detected: &[DetectedTool], tools: &[String], remote_hosts: &[String]) -> Result<ToolsPreview, String> {
    if hive.status == STATUS_DONE {
        return Err("This Hive is done: a message to its queen wakes it first.".to_string());
    }
    let mut next: Vec<String> = Vec::new();
    for id in tools.iter().map(|t| t.trim()).filter(|t| !t.is_empty()) {
        if is_setup_only(id) {
            return Err(format!(
                "\u{201c}{}\u{201d} is what maiTerm sets media providers up with, not a tool a Hive makes things with.",
                tool_name(id, detected)
            ));
        }
        let had = hive.tools.iter().any(|t| t == id);
        if !had && !detected.iter().any(|t| t.id == id) {
            return Err(format!("This Mac has no tool called \u{201c}{}\u{201d}: set it up, then open Hive settings again.", tool_name(id, detected)));
        }
        if !next.iter().any(|t| t == id) {
            next.push(id.to_string());
        }
    }
    let added: Vec<String> = next.iter().filter(|t| !hive.tools.contains(t)).cloned().collect();
    let mut removed: Vec<String> = Vec::new();
    for t in hive.tools.iter().filter(|t| !next.contains(t)) {
        if !removed.contains(t) {
            removed.push(t.clone());
        }
    }
    if added.is_empty() && removed.is_empty() {
        return Err("Nothing would change: the Hive already has exactly these tools.".to_string());
    }
    let have: Vec<String> = hive.settings.gallery.generator_hosts.iter().filter_map(|h| normalize_host(h).ok()).collect();
    let hosts_offered = generator_hosts(detected, &added, remote_hosts).into_iter().filter(|h| !have.contains(h)).collect();
    Ok(ToolsPreview { tools: next, added, removed, hosts_offered })
}

/// The change as it would land on `hive`: the preview, and `add_hosts` checked to be among the
/// hosts offered (a host for anything else is Budget and generation's), never a remote, and
/// within `MAX_GENERATOR_HOSTS` with the ones the gallery has.
pub fn check(hive: &Hive, detected: &[DetectedTool], tools: &[String], add_hosts: &[String], remote_hosts: &[String]) -> Result<Checked, String> {
    let preview = preview(hive, detected, tools, remote_hosts)?;
    let have: Vec<String> = hive.settings.gallery.generator_hosts.iter().filter_map(|h| normalize_host(h).ok()).collect();
    let mut ticked: Vec<String> = Vec::new();
    for h in add_hosts {
        let n = normalize_host(h)?;
        // Added meanwhile (Budget and generation): already there, nothing to add.
        if have.contains(&n) {
            continue;
        }
        if !preview.hosts_offered.contains(&n) {
            return Err(format!(
                "{n} isn\u{2019}t a host the added tools need ({}): add any other host in Budget and generation.",
                if preview.hosts_offered.is_empty() { "they need none".to_string() } else { preview.hosts_offered.join(", ") }
            ));
        }
        if !ticked.contains(&n) {
            ticked.push(n);
        }
    }
    let hosts_added = check_hosts(&ticked, remote_hosts)?;
    if hive.settings.gallery.generator_hosts.len() + hosts_added.len() > MAX_GENERATOR_HOSTS {
        return Err(format!("At most {MAX_GENERATOR_HOSTS} generator hosts: remove some in Budget and generation first."));
    }
    Ok(Checked { preview, hosts_added })
}

/// A checked change written onto the record.
pub fn apply_to(hive: &mut Hive, c: &Checked) {
    hive.tools = c.preview.tools.clone();
    hive.settings.gallery.generator_hosts.extend(c.hosts_added.iter().cloned());
}

/// The lines the settings log and the coordinator's notice carry, every id `one_line`d (a tool id
/// is an MCP server's name from a config file: data, never maiTerm's words).
pub fn change_lines(c: &Checked) -> Vec<String> {
    let ids = |v: &[String]| v.iter().map(|t| one_line(t, SHOWN_ID)).collect::<Vec<_>>().join(", ");
    let mut lines = Vec::new();
    if !c.preview.added.is_empty() {
        lines.push(format!("Tools added: {}", ids(&c.preview.added)));
    }
    if !c.preview.removed.is_empty() {
        lines.push(format!("Tools removed: {}", ids(&c.preview.removed)));
    }
    if !c.hosts_added.is_empty() {
        lines.push(format!("Media workers may also reach (generator hosts): {}", c.hosts_added.join(", ")));
    }
    lines
}

/// The coordinator's notice: what changed, and when it reaches whom.
pub fn notice(c: &Checked, running: Option<&str>) -> String {
    let mut s = "[maiTerm] Your human changed this Hive\u{2019}s tools in Hive settings:".to_string();
    for l in change_lines(c) {
        s.push_str("\n- ");
        s.push_str(&l);
    }
    s.push_str(
        "\nWorkers get the new list at their next launch: a running worker keeps the tools it started with. Your brief\u{2019}s \
         list follows at your own next launch.",
    );
    if c.preview.added.iter().any(|t| t.starts_with("skill:")) {
        s.push_str(" A skill loads in your session only: never tell a worker to load one.");
    }
    if let Some(r) = running {
        s.push('\n');
        s.push_str(r);
    }
    s
}

/// The human's tools change for the Hive `caller` names, checked against the record as it is
/// under the write lock, saved, logged and told to the coordinator.
pub(crate) async fn set_tools<W: HiveWindow>(
    ctx: &Ctx<'_, W>,
    caller: &Caller,
    detected: &[DetectedTool],
    remote_hosts: &[String],
    tools: &[String],
    add_hosts: &[String],
) -> Result<HiveToolsChange, String> {
    let (hive, c) = write_tools(ctx, caller, detected, remote_hosts, tools, add_hosts).await?;
    log_line(caller, "human", &change_lines(&c), None);
    let p = caller.paths.clone();
    let workers = super::tools::blocking(move || Ok(super::worker::list_workers(&p))).await.unwrap_or_default();
    let text = notice(&c, running_on_old(&workers).as_deref());
    let coordinator_told = match super::tools::coordinator_tab(ctx.data, caller) {
        Some(tab) => {
            let sent = ctx.window.deliver(&tab, text, "Hive tools").await;
            log::info!("hive {}: tools notice to the coordinator: {}", caller.hive.id, sent.describe());
            matches!(sent, super::deliver::Sent::Delivered)
        }
        None => false,
    };
    Ok(HiveToolsChange { hive, added: c.preview.added, removed: c.preview.removed, hosts_added: c.hosts_added, coordinator_told })
}

/// A tools change checked against the record as it is under the write lock, and saved: the one
/// write behind Hive settings → Tools and an approved `tool_setup` card (`hive::tool_setup`),
/// which each log it and tell the coordinator in their own words. A host is saved only if it was
/// looked up first (`refuse_private_hosts`).
pub(crate) async fn write_tools<W: HiveWindow>(
    ctx: &Ctx<'_, W>,
    caller: &Caller,
    detected: &[DetectedTool],
    remote_hosts: &[String],
    tools: &[String],
    add_hosts: &[String],
) -> Result<(Hive, Checked), String> {
    let current = super::autonomy::current_hive(ctx, caller).ok_or("This Hive is gone.")?;
    // Refused before any lookup: the lookups only run for hosts that would be saved.
    let first = check(&current, detected, tools, add_hosts, remote_hosts)?;
    // A host added by any path must not answer this computer or its network (§3.4).
    refuse_private_hosts(&first.hosts_added, false, &ip_remotes(remote_hosts)).await?;
    let mut outcome: Result<Checked, String> = Err("This Hive is gone.".to_string());
    let hive = super::finish::write_status(ctx, caller, &[STATUS_ACTIVE, STATUS_PAUSED], None, |h| {
        // Only hosts that were looked up are saved.
        outcome = check(h, detected, tools, add_hosts, remote_hosts).and_then(|c| {
            if c.hosts_added.iter().any(|x| !first.hosts_added.contains(x)) {
                return Err("The Hive\u{2019}s generator hosts changed meanwhile: open Hive settings again.".to_string());
            }
            apply_to(h, &c);
            Ok(c)
        });
    })?;
    let c = outcome?;
    Ok((hive, c))
}

/// The window's own Hive in `workspace_id`, with a fresh detection and its remotes. Blocks
/// (detection, git) off the async thread.
async fn gather(window: &tauri::Window, state: &std::sync::Arc<crate::state::AppState>, workspace_id: &str) -> Result<(Caller, Vec<DetectedTool>, Vec<String>), String> {
    let (caller, prefs) = {
        let data = state.app_data.read();
        let caller = super::tools::caller_for_workspace(&data, workspace_id).filter(|c| c.label == window.label()).ok_or("That workspace isn't a Hive in this window")?;
        (caller, data.preferences.clone())
    };
    let hive = caller.hive.clone();
    let (detected, remotes) = super::tools::blocking(move || Ok((super::create::detect_tools(&prefs), super::settings::hive_remote_hosts(&hive)))).await?;
    Ok((caller, detected, remotes))
}

/// Hive settings → Tools, before Save: what the picked tools would add and remove, and the hosts
/// the added ones need. Nothing is saved.
#[tauri::command]
pub async fn hive_tools_change_preview(
    window: tauri::Window,
    state: tauri::State<'_, std::sync::Arc<crate::state::AppState>>,
    workspace_id: String,
    tools: Vec<String>,
) -> Result<ToolsPreview, String> {
    let (caller, detected, remotes) = gather(&window, state.inner(), &workspace_id).await?;
    preview(&caller.hive, &detected, &tools, &remotes)
}

/// Hive settings → Tools → Save (docs/hive.md §3.4): the human's change. `add_hosts` are the
/// offered hosts the human left ticked.
#[tauri::command]
pub async fn hive_set_tools(
    window: tauri::Window,
    state: tauri::State<'_, std::sync::Arc<crate::state::AppState>>,
    app: tauri::AppHandle,
    workspace_id: String,
    tools: Vec<String>,
    add_hosts: Vec<String>,
) -> Result<HiveToolsChange, String> {
    let (caller, detected, remotes) = gather(&window, state.inner(), &workspace_id).await?;
    let ctx = super::tools::app_ctx(state.inner(), &app, &caller);
    set_tools(&ctx, &caller, &detected, &remotes, &tools, &add_hosts).await
}

#[cfg(test)]
mod tests;
