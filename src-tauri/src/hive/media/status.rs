//! `hive_media_status` (docs/hive-media.md §5): per capability, what on this Mac already covers it,
//! and what to set up for one that nothing does.
//!
//! The row shape is an interface (a later phase draws the Tools step's panel from it), so it stays
//! exactly `{ capability, covered_by: [{ tool_id, provider, state }], recommended, alternatives }`.
//! Every kind on the wire (`state`) is a String, never an enum.
//!
//! Each source of evidence is its own small function that takes what it reads as arguments, so a
//! test feeds it fixtures instead of this Mac:
//! - `cli_hits`: a program on PATH (`detect_tools`);
//! - `proxy_hits`: a server in maiTerm's MCP proxy registry, recognised by the upstream it dials;
//! - `native_hits`: a `.claude.json` server that is not ours, by `match.hosts` then `match.names`;
//! - `plugin_hits`: a Claude Code plugin installed in any Claude config root;
//! - `comfy_hits`: ComfyUI answering on 127.0.0.1:8188, or the Comfy Desktop app installed.
//!
//! `recommend` is pure. The defaults live in the catalogue (`Provider.default_for`), never here.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use serde::Serialize;

use super::{catalogue, provider, providers_for_native, providers_for_upstream, Provider};
use crate::hive::create::DetectedTool;

/// `state` of a cover: it works now.
pub const READY: &str = "ready";
/// The server answered that its human has to sign in.
pub const NEEDS_SIGN_IN: &str = "needs_sign_in";
/// Present, but not for this Hive: a proxy server that is off, scoped to other workspaces, or
/// down. Never `ready`.
pub const UNREACHABLE: &str = "unreachable";
/// Present, but not confirmed to work: a native MCP entry (maiTerm cannot see its sign-in), a
/// proxy server it hasn't reached yet, a plugin that is switched off, an app that isn't running.
pub const INSTALLED: &str = "installed";

/// One tool that covers a capability.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Cover {
    /// What `Hive.tools` stores for it (`DetectedTool.id`): `mcp:<entry name>`, `cli:ffmpeg`,
    /// `plugin:remotion@remotion`, `comfyui`, `app:ComfyUI.app`.
    pub tool_id: String,
    /// The catalogue provider id.
    pub provider: String,
    pub state: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct MediaRow {
    pub capability: String,
    pub covered_by: Vec<Cover>,
    /// Set only when nothing covering the capability is `ready`.
    pub recommended: Option<String>,
    pub alternatives: Vec<String>,
}

/// A cover and every capability it covers (one tool usually covers several).
#[derive(Debug, Clone, PartialEq)]
pub struct Hit {
    pub cover: Cover,
    pub capabilities: Vec<String>,
}

fn hit(tool_id: &str, p: &Provider, state: &str, capabilities: Vec<String>) -> Hit {
    Hit { cover: Cover { tool_id: tool_id.to_string(), provider: p.id.clone(), state: state.to_string() }, capabilities }
}

// ─── Programs ───────────────────────────────────────────────────────────────

/// Detected programs a catalogue provider lists in `detect.clis`. A program that is there is ready.
pub fn cli_hits(detected: &[DetectedTool]) -> Vec<Hit> {
    let mut out = Vec::new();
    for t in detected.iter().filter(|t| t.kind == "cli") {
        for p in &catalogue().providers {
            for probe in p.detect.clis.iter().filter(|c| c.bin == t.label) {
                out.push(hit(&t.id, p, READY, probe.capabilities.clone()));
            }
        }
    }
    out
}

// ─── MCP proxy ──────────────────────────────────────────────────────────────

/// What `hive_media_status` needs of one server in maiTerm's MCP proxy registry.
#[derive(Debug, Clone, PartialEq)]
pub struct ProxyInput {
    pub id: String,
    /// The upstream it dials.
    pub url: String,
    pub enabled: bool,
    /// Workspaces whose tabs see its tools; empty means every workspace (`ProxyServer.workspaces`).
    pub workspaces: Vec<String>,
    /// `Status.state`: `connecting` | `connected` | `needs_sign_in` | `unreachable` | `error` | `disabled`.
    pub status: String,
}

/// The registry as the status reads it, with each server's live status.
pub fn proxy_inputs() -> Vec<ProxyInput> {
    crate::mcp_proxy::ensure_loaded();
    crate::mcp_proxy::all()
        .into_iter()
        .map(|s| {
            let status = crate::mcp_proxy::status(&s.id).state;
            ProxyInput { id: s.id, url: s.url, enabled: s.enabled, workspaces: s.workspaces, status }
        })
        .collect()
}

/// A proxy server's state for `workspace`. A server scoped away from the Hive's workspace answers
/// "not enabled for this workspace" to its tabs, so it is `unreachable` here, never `ready`, even
/// while the server itself is connected. With no workspace (Preferences) scope is not judged: no
/// Hive exists yet to be scoped away from.
pub fn proxy_state(s: &ProxyInput, workspace: Option<&str>) -> &'static str {
    if !s.enabled {
        return UNREACHABLE;
    }
    if let Some(ws) = workspace {
        if !s.workspaces.is_empty() && !s.workspaces.iter().any(|w| w == ws) {
            return UNREACHABLE;
        }
    }
    match s.status.as_str() {
        "connected" => READY,
        "needs_sign_in" => NEEDS_SIGN_IN,
        "unreachable" | "error" | "disabled" => UNREACHABLE,
        // `connecting`, or a state a later build adds: present, not confirmed.
        _ => INSTALLED,
    }
}

/// Proxy servers recognised by the upstream they dial (never by their name or their loopback
/// entry url, docs/hive-media.md §5). `tool_id` is the `.claude.json` entry's name, which is what
/// the Tools step ticks.
pub fn proxy_hits(servers: &[ProxyInput], workspace: Option<&str>) -> Vec<Hit> {
    let mut out = Vec::new();
    for s in servers {
        let tool_id = format!("mcp:{}", crate::mcp_proxy::entries::entry_name(&s.id));
        let state = proxy_state(s, workspace);
        for p in providers_for_upstream(Some(&s.url)) {
            out.push(hit(&tool_id, p, state, p.capabilities.clone()));
        }
    }
    out
}

// ─── Native .claude.json servers ────────────────────────────────────────────

/// A `.claude.json` server that is NOT one of ours, recognised as `create::is_generator_server`
/// recognises it: by its url host, else a word of its name (a stdio entry has only its name). Its
/// sign-in is invisible to maiTerm, so it is `installed`, never `ready`. A proxied entry is read
/// from the registry instead (`proxy_hits`), and one carrying our marker that is not provably ours
/// reaches nothing we can name.
pub fn native_hits(detected: &[DetectedTool]) -> Vec<Hit> {
    let mut out = Vec::new();
    for t in detected.iter().filter(|t| t.kind == "mcp" && t.proxy.is_none()) {
        let host = t.host.as_deref();
        for p in providers_for_native(&t.label, host) {
            out.push(hit(&t.id, p, INSTALLED, p.capabilities.clone()));
        }
    }
    out
}

// ─── Claude Code plugins ────────────────────────────────────────────────────

/// The Claude config root whose plugins the coordinator really loads: the one it launches under
/// (`install::coordinator_config_root`). That is the active managed Claude account's root, read through
/// its links to `~/.claude` (and an unlinked real `plugins` folder in it is what it loads), else
/// the human's exported `CLAUDE_CONFIG_DIR`, else `~/.claude`. It is the root
/// `install::plugin_root` installs into, so install, verify and launch can't disagree. A plugin
/// found anywhere else (an account switched off, an exported root a managed account shadows) is
/// not the coordinator's, so it is not reported. Workers load no user plugin at all.
pub fn plugin_roots(prefs: &crate::state::Preferences) -> Vec<PathBuf> {
    plugin_roots_for(prefs, crate::accounts::user_claude_root(), dirs::home_dir().as_deref())
}

/// `plugin_roots` with the human's exported root and home given.
pub fn plugin_roots_for(prefs: &crate::state::Preferences, users_own: Option<&Path>, home: Option<&Path>) -> Vec<PathBuf> {
    crate::hive::media::install::coordinator_config_root(prefs, users_own, home).into_iter().collect()
}

/// The plugins one config root has installed, as `(id, enabled)`. Claude Code records them in
/// `<root>/plugins/installed_plugins.json`: `{ "version": 2, "plugins": { "name@marketplace":
/// [ { "scope", "installPath", ... } ] } }` (version 1 holds one object instead of the array).
/// A plugin counts when it has a `user` (or `managed`) install whose folder is still there: a
/// `project` or `local` install loads only in the folder it names, which is no Hive agent's. It
/// is `enabled` unless `<root>/settings.json` sets `enabledPlugins[id]` to false.
pub fn installed_plugins(root: &Path) -> Vec<(String, bool)> {
    let read = |p: PathBuf| std::fs::read(p).ok().and_then(|b| serde_json::from_slice::<serde_json::Value>(&b).ok());
    let Some(file) = read(root.join("plugins").join("installed_plugins.json")) else { return Vec::new() };
    let Some(plugins) = file.get("plugins").and_then(|p| p.as_object()) else { return Vec::new() };
    let settings = read(root.join("settings.json"));
    let mut out = Vec::new();
    for (id, installs) in plugins {
        let installs: Vec<&serde_json::Value> = match installs {
            serde_json::Value::Array(a) => a.iter().collect(),
            o @ serde_json::Value::Object(_) => vec![o],
            _ => continue,
        };
        let live = installs.iter().any(|i| {
            let scope = i.get("scope").and_then(|s| s.as_str()).unwrap_or("user");
            let there = i.get("installPath").and_then(|p| p.as_str()).is_some_and(|p| Path::new(p).is_dir());
            matches!(scope, "user" | "managed") && there
        });
        if !live {
            continue;
        }
        let off = settings.as_ref().and_then(|s| s.get("enabledPlugins")).and_then(|e| e.get(id)).and_then(|v| v.as_bool()) == Some(false);
        out.push((id.clone(), !off));
    }
    out
}

/// Catalogue providers whose `detect.plugins` is installed in any of `roots` (one hit per plugin,
/// however many roots hold it; ready in any root where it is enabled).
pub fn plugin_hits(roots: &[PathBuf]) -> Vec<Hit> {
    let mut found: Vec<(String, bool)> = Vec::new();
    for root in roots {
        for (id, enabled) in installed_plugins(root) {
            match found.iter_mut().find(|(i, _)| *i == id) {
                Some(f) => f.1 |= enabled,
                None => found.push((id, enabled)),
            }
        }
    }
    let mut out = Vec::new();
    for p in &catalogue().providers {
        for plugin in &p.detect.plugins {
            if let Some((_, enabled)) = found.iter().find(|(i, _)| i == plugin) {
                out.push(hit(&format!("plugin:{plugin}"), p, if *enabled { READY } else { INSTALLED }, p.capabilities.clone()));
            }
        }
    }
    out
}

// ─── ComfyUI ────────────────────────────────────────────────────────────────

/// Where macOS apps live: /Applications and ~/Applications. Empty elsewhere: Comfy Desktop's
/// presence is only looked for on a Mac, and skipped quietly on any other system.
pub fn app_dirs() -> Vec<PathBuf> {
    if !cfg!(target_os = "macos") {
        return Vec::new();
    }
    std::iter::once(PathBuf::from("/Applications")).chain(dirs::home_dir().map(|h| h.join("Applications"))).collect()
}

/// ComfyUI answering on 127.0.0.1:8188 (`detect_tools` reports it as `comfyui`) is ready; the
/// Comfy Desktop app installed but not running is `installed`.
pub fn comfy_hits(detected: &[DetectedTool], app_dirs: &[PathBuf]) -> Vec<Hit> {
    let mut out = Vec::new();
    for p in &catalogue().providers {
        if p.detect.port.is_none() && p.detect.apps.is_empty() {
            continue;
        }
        if p.detect.port.is_some() && detected.iter().any(|t| t.kind == "comfyui") {
            out.push(hit("comfyui", p, READY, p.capabilities.clone()));
            continue;
        }
        for app in &p.detect.apps {
            if app_dirs.iter().any(|d| d.join(app).exists()) {
                out.push(hit(&format!("app:{app}"), p, INSTALLED, p.capabilities.clone()));
                break;
            }
        }
    }
    out
}

// ─── Recommendation ─────────────────────────────────────────────────────────

/// Whether maiTerm may recommend `p`: offered at all (not `detect_only`), not marked
/// `recommendable: false`, and set up by something other than a guide (a `manual` route is the
/// human's to do alone).
fn recommendable(p: &Provider) -> bool {
    p.offer != "detect_only" && p.recommendable && p.has_automatic_route()
}

/// What to set up for `capability`, and the alternatives, given what already covers it.
///
/// - Something `ready` covers it: nothing to recommend, no alternatives.
/// - Otherwise the catalogue's default (`default_for`, first in catalogue order).
/// - But a subscriber provider (`subscriber_first`: Higgsfield, Kling, Runway) that is among
///   `covered` in any state (detected) or in `pays_for` (the human's "I already pay for…") is
///   promoted to the recommendation for the capabilities it covers, and the default is then the
///   FIRST alternative: the human has the subscription, but fal is the answer for a newcomer.
/// - A `manual` or not-offered provider is never recommended, and never an alternative.
pub fn recommend(capability: &str, covered: &[Cover], pays_for: &[String]) -> (Option<String>, Vec<String>) {
    if covered.iter().any(|c| c.state == READY) {
        return (None, Vec::new());
    }
    let candidates: Vec<&Provider> = catalogue().providers.iter().filter(|p| p.covers(capability) && recommendable(p)).collect();
    let default = candidates.iter().find(|p| p.default_for.iter().any(|c| c == capability)).copied();
    let promoted: Vec<&Provider> =
        candidates.iter().filter(|p| p.subscriber_first && (pays_for.contains(&p.id) || covered.iter().any(|c| c.provider == p.id))).copied().collect();

    let recommended = promoted.first().copied().or(default);
    let mut alternatives: Vec<&Provider> = Vec::new();
    // fal first behind a promoted subscriber provider, then the other subscriber ones.
    if recommended.is_some_and(|r| promoted.first().is_some_and(|p| p.id == r.id)) {
        alternatives.extend(default);
        alternatives.extend(promoted.iter().skip(1));
    }
    // Then the rest, those offered as defaults before those only listed; catalogue order within.
    let rest = candidates.iter().filter(|p| p.offer == "recommend").chain(candidates.iter().filter(|p| p.offer != "recommend"));
    for p in rest {
        if !alternatives.iter().any(|a| a.id == p.id) && recommended.is_none_or(|r| r.id != p.id) {
            alternatives.push(p);
        }
    }
    (recommended.map(|p| p.id.clone()), alternatives.into_iter().map(|p| p.id.clone()).collect())
}

/// Every catalogue capability's row, in catalogue order, from the hits of all the sources.
pub fn rows(hits: &[Hit], pays_for: &[String]) -> Vec<MediaRow> {
    // Only ids the catalogue knows: anything else the caller sends is dropped, not trusted.
    let pays_for: Vec<String> = pays_for.iter().filter(|id| provider(id).is_some()).cloned().collect();
    catalogue()
        .capabilities
        .iter()
        .map(|cap| {
            let mut seen: HashSet<(String, String)> = HashSet::new();
            let covered_by: Vec<Cover> = hits
                .iter()
                .filter(|h| h.capabilities.iter().any(|c| c == &cap.id))
                .filter(|h| seen.insert((h.cover.tool_id.clone(), h.cover.provider.clone())))
                .map(|h| h.cover.clone())
                .collect();
            let (recommended, alternatives) = recommend(&cap.id, &covered_by, &pays_for);
            MediaRow { capability: cap.id.clone(), covered_by, recommended, alternatives }
        })
        .collect()
}

/// The whole status: reads this Mac, then `rows`. Blocks (ports, files, executables): call it off
/// the main thread. `workspace` is a Hive's workspace id, or `None` for Preferences.
pub fn status(prefs: &crate::state::Preferences, workspace: Option<&str>, pays_for: &[String]) -> Vec<MediaRow> {
    rows(&hits(prefs, workspace), pays_for)
}

/// Every source's hits on this Mac: what `status` turns into rows, and what Verify re-detects
/// (`verify.rs`). Blocks: call it off the main thread.
pub fn hits(prefs: &crate::state::Preferences, workspace: Option<&str>) -> Vec<Hit> {
    let detected = crate::hive::create::detect_tools(prefs);
    let mut hits = cli_hits(&detected);
    hits.extend(proxy_hits(&proxy_inputs(), workspace));
    hits.extend(native_hits(&detected));
    hits.extend(plugin_hits(&plugin_roots(prefs)));
    hits.extend(comfy_hits(&detected, &app_dirs()));
    hits
}

/// The Tools step's media panel and Preferences → Hive: one row per capability. `workspace_id` is
/// a Hive's workspace (its proxy scope is judged), or none for Preferences; `already_pay_for` is
/// provider ids the human says they already pay for (D1), which promotes the subscriber ones.
#[tauri::command]
pub async fn hive_media_status(
    window: tauri::Window,
    state: tauri::State<'_, std::sync::Arc<crate::state::AppState>>,
    workspace_id: Option<String>,
    already_pay_for: Option<Vec<String>>,
) -> Result<Vec<MediaRow>, String> {
    let prefs = {
        let data = state.app_data.read();
        if let Some(id) = &workspace_id {
            data.window(window.label()).and_then(|w| w.workspaces.iter().find(|w| &w.id == id)).ok_or("Workspace not found")?;
        }
        data.preferences.clone()
    };
    let pays_for = already_pay_for.unwrap_or_default();
    let rows = tauri::async_runtime::spawn_blocking({
        let ws = workspace_id.clone();
        move || status(&prefs, ws.as_deref(), &pays_for)
    })
    .await
    .map_err(|e| e.to_string())?;
    // The panel re-reads after every setup step: a watched ask it shows Ready is finished now.
    crate::hive::tool_setup::on_status(workspace_id.as_deref(), &rows);
    Ok(rows)
}

#[cfg(test)]
mod tests;
