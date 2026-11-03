//! Hive media setup (docs/hive-media.md): the catalogue of media providers and what this machine
//! already has of them.
//!
//! One data file, `resources/hive/media/catalogue.json`, replaces the generator tables that used
//! to live in `create.rs` (`KNOWN_GENERATOR_HOSTS`, `SKILL_GENERATORS`): detection, the sandbox's
//! generator hosts, the worker's MCP copy (`settings::worker_mcp_config`) and the status the
//! Tools step reads all ask this module, so they cannot disagree.
//!
//! Every persisted or wire kind in here is a String (`Route.kind`, `Provider.offer`, a cover's
//! `state`), never an enum, so a later phase adds one without breaking an older file.
//!
//! Phase 1 changes NO sandbox host: each of the five providers `KNOWN_GENERATOR_HOSTS` held keeps
//! exactly its old host set as `sandbox_hosts` (`todays_five_providers_keep_exactly_their_hosts`).
//! A provider new to the catalogue gets `sandbox_hosts` only where a host is verified; result CDNs
//! nobody has seen yet belong in `unverified_hosts`, never guessed into `sandbox_hosts` (§9).

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};

use serde::{Deserialize, Serialize};
use tauri::State;

use super::create::DetectedTool;
use crate::state::AppState;

const FILE: &str = include_str!("../../resources/hive/media/catalogue.json");

// ─── The catalogue ──────────────────────────────────────────────────────────

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Capability {
    pub id: String,
    pub name: String,
    pub means: String,
    /// One of the five the first page shows; the rest sit under "More".
    pub common: bool,
}

/// One way to get a provider. `kind` is `remote_mcp` | `brew` | `plugin` | `local_app` | `manual`
/// (`stdio_mcp` arrives with phase 5).
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Route {
    pub kind: String,
    #[serde(default)]
    pub url: String,
    /// `oauth` | `token` | `none`.
    pub auth: String,
    /// What a job spends: `api` (prepaid balance) | `plan` (subscription credits) | `free`, and
    /// the odd ones (`plan_or_wallet`, `comfy_credits`).
    pub credits: String,
    /// A tool that prices a job before it runs, if the provider has one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub quote_tool: Option<String>,
    /// `brew`: the one install line.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub command: Option<String>,
    /// `plugin`: the Claude Code plugin id, `name@marketplace`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub plugin_id: Option<String>,
}

/// A command-line tool whose presence covers some capabilities.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct CliProbe {
    pub bin: String,
    pub capabilities: Vec<String>,
}

/// How to find a provider that is not an MCP server.
#[derive(Debug, Clone, Default, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields, default)]
pub struct Detect {
    pub clis: Vec<CliProbe>,
    /// Claude Code plugin ids (`name@marketplace`) looked up in every account root.
    pub plugins: Vec<String>,
    /// A local port that answers when it runs (ComfyUI: 8188).
    pub port: Option<u16>,
    /// macOS app bundles under /Applications or ~/Applications.
    pub apps: Vec<String>,
}

/// What recognises a provider among the agent's own tools.
#[derive(Debug, Clone, Default, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields, default)]
pub struct Match {
    /// Host suffixes of an MCP server's url (`fal.ai` matches `mcp.fal.ai`).
    pub hosts: Vec<String>,
    /// Words of an MCP server's or skill's name (`fal` matches `fal-mirror`).
    pub names: Vec<String>,
    /// Skill names that use this provider (`/generate` picks its models on Kie.ai, §10.3).
    pub skills: Vec<String>,
}

fn yes() -> bool {
    true
}

fn offer_list() -> String {
    "list".to_string()
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Provider {
    pub id: String,
    pub name: String,
    /// When its facts were last checked: providers change monthly (§2.5).
    pub checked: String,
    /// `recommend` (offered as a default) | `list` (offered as an alternative) | `detect_only`
    /// (recognised when present, never set up).
    #[serde(default = "offer_list")]
    pub offer: String,
    /// D1: shown first to someone who already pays for it (detected, or chosen).
    #[serde(default)]
    pub subscriber_first: bool,
    /// `false`: listed, but never the recommendation.
    #[serde(default = "yes")]
    pub recommendable: bool,
    pub capabilities: Vec<String>,
    /// The capabilities this provider is THE default for.
    #[serde(default)]
    pub default_for: Vec<String>,
    #[serde(default)]
    pub models_note: String,
    /// A note per capability, for a claim that holds for one only (or isn't verified yet).
    #[serde(default)]
    pub capability_notes: HashMap<String, String>,
    pub routes: Vec<Route>,
    #[serde(default)]
    pub detect: Detect,
    /// Where a result URL points: what a worker's shell downloads from.
    pub result_hosts: Vec<String>,
    /// Hosts of the provider's API, for a shell that calls it itself.
    pub api_hosts: Vec<String>,
    /// What a media worker's sandbox is given when the Hive uses this provider (§9).
    pub sandbox_hosts: Vec<String>,
    /// Hosts nobody has confirmed from a real job. Never added to a sandbox.
    pub unverified_hosts: Vec<String>,
    /// How long a result URL lives.
    pub retention: String,
    pub signup_url: String,
    pub billing_url: String,
    pub guide: Vec<String>,
    #[serde(rename = "match")]
    pub matches: Match,
    pub source_urls: Vec<String>,
}

impl Provider {
    pub fn covers(&self, capability: &str) -> bool {
        self.capabilities.iter().any(|c| c == capability)
    }

    /// Whether setting it up needs nothing but the human's click and sign-in: anything but a guide.
    pub fn has_automatic_route(&self) -> bool {
        self.routes.iter().any(|r| r.kind != "manual")
    }

    /// Its first remote MCP route.
    pub fn mcp_route(&self) -> Option<&Route> {
        self.routes.iter().find(|r| r.kind == "remote_mcp")
    }
}

#[derive(Debug, Clone, Default, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Catalogue {
    pub version: u32,
    pub checked: String,
    pub capabilities: Vec<Capability>,
    pub providers: Vec<Provider>,
}

pub fn parse(text: &str) -> Result<Catalogue, String> {
    let c: Catalogue = serde_json::from_str(text).map_err(|e| e.to_string())?;
    let mut seen = HashSet::new();
    for p in &c.providers {
        if !seen.insert(p.id.as_str()) {
            return Err(format!("provider {} appears twice", p.id));
        }
        for cap in p.capabilities.iter().chain(&p.default_for).chain(p.capability_notes.keys()).chain(p.detect.clis.iter().flat_map(|c| &c.capabilities)) {
            if !c.capabilities.iter().any(|k| &k.id == cap) {
                return Err(format!("provider {} names unknown capability {cap}", p.id));
            }
        }
    }
    Ok(c)
}

/// The shipped catalogue. A file that doesn't parse is a build defect the tests catch; at runtime
/// it is logged and the catalogue is empty, which recognises no generator (fails closed).
pub fn catalogue() -> &'static Catalogue {
    static CATALOGUE: OnceLock<Catalogue> = OnceLock::new();
    CATALOGUE.get_or_init(|| {
        parse(FILE).unwrap_or_else(|e| {
            log::error!("hive: media catalogue: {e}");
            Catalogue::default()
        })
    })
}

pub fn provider(id: &str) -> Option<&'static Provider> {
    catalogue().providers.iter().find(|p| p.id == id)
}

// ─── Recognising a provider ─────────────────────────────────────────────────

/// The words of a name, lowercased: `kling-video` is `kling`, `video`.
pub fn name_words(name: &str) -> impl Iterator<Item = String> + '_ {
    name.split(|c: char| !c.is_ascii_alphanumeric()).filter(|w| !w.is_empty()).map(|w| w.to_ascii_lowercase())
}

/// `host` lowercased with any `:port` dropped.
pub fn bare_host(host: &str) -> String {
    let h = host.to_ascii_lowercase();
    h.rsplit_once(':').filter(|(_, port)| port.bytes().all(|b| b.is_ascii_digit())).map_or(h.clone(), |(host, _)| host.to_string())
}

fn suffix_of(host: &str, suffix: &str) -> bool {
    host == suffix || host.ends_with(&format!(".{suffix}"))
}

fn host_matches(p: &Provider, host: &str) -> bool {
    p.matches.hosts.iter().any(|s| suffix_of(host, s))
}

/// The providers a NATIVE MCP server is recognised as: by its url host (any port), else by a word
/// of its name. A stdio entry has no url, so only its name can vouch for it.
pub fn providers_for_native(name: &str, host: Option<&str>) -> Vec<&'static Provider> {
    let host = host.map(bare_host);
    catalogue().providers.iter().filter(|p| host.as_deref().is_some_and(|h| host_matches(p, h)) || name_words(name).any(|w| p.matches.names.contains(&w))).collect()
}

/// The providers a PROXIED server is recognised as: by the host of the upstream it dials, and by
/// nothing else. Its name is the human's (or an agent's) to choose and says nothing about what
/// it reaches: a proxied "fal" that points elsewhere is not fal, and a proxied "video" that
/// points at fal is. `None` (the registry no longer has it) recognises nothing.
pub fn providers_for_upstream(upstream_url: Option<&str>) -> Vec<&'static Provider> {
    let Some(host) = upstream_url.and_then(super::create::url_host).map(|h| bare_host(&h)) else { return Vec::new() };
    catalogue().providers.iter().filter(|p| host_matches(p, &host)).collect()
}

/// The providers a skill uses (`/generate` → Kie.ai), by the skill's exact name or a word of the
/// provider's names in it, as the table did.
pub fn providers_for_skill(skill: &str) -> Vec<&'static Provider> {
    catalogue().providers.iter().filter(|p| p.matches.skills.iter().any(|s| s == skill) || name_words(skill).any(|w| p.matches.names.contains(&w))).collect()
}

// ─── Status ─────────────────────────────────────────────────────────────────

pub const READY: &str = "ready";
pub const NEEDS_SIGN_IN: &str = "needs_sign_in";
pub const UNREACHABLE: &str = "unreachable";
pub const INSTALLED: &str = "installed";

/// One tool that covers a capability.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Cover {
    /// What `Hive.tools` would store for it (`mcp:fal`, `cli:ffmpeg`), or `plugin:<id>` / `app:<bundle>`.
    pub tool_id: String,
    pub provider: String,
    /// `ready` | `needs_sign_in` | `unreachable` | `installed` (there, but nothing could check it).
    pub state: String,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct CapabilityRow {
    pub capability: String,
    pub covered_by: Vec<Cover>,
    /// Set only while nothing usable covers the capability.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub recommended: Option<String>,
    pub alternatives: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct MediaStatus {
    /// One row per catalogue capability, in catalogue order.
    pub rows: Vec<CapabilityRow>,
    /// Homebrew is installed: the `brew` route needs it.
    pub brew: bool,
}

/// What the proxy registry holds for one server, as detection needs it.
#[derive(Debug, Clone, PartialEq)]
pub struct ProxyFacts {
    pub enabled: bool,
    /// `mcp_proxy::Status.state`.
    pub state: String,
    /// Workspaces whose tabs see the server; empty means every workspace.
    pub workspaces: Vec<String>,
}

/// Everything detection reads, gathered once (`gather`) so the rest is pure.
#[derive(Debug, Clone, Default)]
pub struct Facts {
    /// `create::detect_tools`: MCP servers (a proxied one carries its upstream), CLIs, skills, ComfyUI.
    pub tools: Vec<DetectedTool>,
    /// The proxy's servers by id.
    pub proxy: HashMap<String, ProxyFacts>,
    /// Command-line tools found, by name.
    pub clis: HashSet<String>,
    /// Installed Claude Code plugin ids, across every account root.
    pub plugins: HashSet<String>,
    pub comfy_running: bool,
    pub apps: HashSet<String>,
    pub brew: bool,
}

/// A proxied server's state for one Hive. A server scoped away from the Hive's workspace is
/// `unreachable` for it, never `ready`: its tools are not served there. No workspace (the
/// Preferences pane) applies no scope.
pub fn proxy_state(f: Option<&ProxyFacts>, workspace: Option<&str>) -> &'static str {
    let Some(f) = f else { return UNREACHABLE };
    if !f.enabled || f.state == "disabled" {
        return UNREACHABLE;
    }
    if let Some(w) = workspace {
        if !f.workspaces.is_empty() && !f.workspaces.iter().any(|x| x == w) {
            return UNREACHABLE;
        }
    }
    match f.state.as_str() {
        "connected" => READY,
        "needs_sign_in" => NEEDS_SIGN_IN,
        "unreachable" | "error" => UNREACHABLE,
        // `connecting` and anything newer: present, not known to work.
        _ => INSTALLED,
    }
}

fn rank(state: &str) -> u8 {
    match state {
        READY => 0,
        INSTALLED => 1,
        NEEDS_SIGN_IN => 2,
        _ => 3,
    }
}

fn usable(state: &str) -> bool {
    state == READY || state == INSTALLED
}

/// Every cover found, by capability. Pure: all I/O is in `Facts`.
fn covers(cat: &Catalogue, f: &Facts, workspace: Option<&str>) -> HashMap<String, Vec<Cover>> {
    let mut out: HashMap<String, Vec<Cover>> = HashMap::new();
    let mut add = |capabilities: &[String], tool_id: &str, p: &Provider, state: &str| {
        for c in capabilities {
            let cover = Cover { tool_id: tool_id.to_string(), provider: p.id.clone(), state: state.to_string() };
            let list = out.entry(c.clone()).or_default();
            if !list.contains(&cover) {
                list.push(cover);
            }
        }
    };
    for t in &f.tools {
        match t.kind.as_str() {
            "mcp" => {
                let (providers, state) = match &t.proxy {
                    Some(r) => (providers_for_upstream(r.upstream.as_deref()), proxy_state(f.proxy.get(&r.id), workspace)),
                    // A native server: nothing can say whether it is signed in.
                    None => (providers_for_native(&t.label, t.host.as_deref()), INSTALLED),
                };
                for p in providers {
                    add(&p.capabilities, &t.id, p, state);
                }
            }
            "skill" => {
                for p in providers_for_skill(&t.label) {
                    add(&p.capabilities, &t.id, p, INSTALLED);
                }
            }
            _ => {}
        }
    }
    for p in &cat.providers {
        for cli in &p.detect.clis {
            if f.clis.contains(&cli.bin) {
                add(&cli.capabilities, &format!("cli:{}", cli.bin), p, READY);
            }
        }
        for id in p.detect.plugins.iter().filter(|id| f.plugins.contains(*id)) {
            add(&p.capabilities, &format!("plugin:{id}"), p, INSTALLED);
        }
        if p.detect.port.is_some() && f.comfy_running {
            add(&p.capabilities, "comfyui", p, READY);
        } else {
            for app in p.detect.apps.iter().filter(|a| f.apps.contains(*a)) {
                add(&p.capabilities, &format!("app:{app}"), p, INSTALLED);
            }
        }
    }
    out
}

/// The provider to recommend for a capability nothing usable covers, and the rest to offer.
///
/// In order, the first is recommended:
/// 1. a provider already tried here (it covers the capability but isn't usable: finish its sign-in);
/// 2. one the human already pays for: detected (`subscriber_first`: Higgsfield, Kling, Runway),
///    or named in `already_pay` (D1; a later "I already pay for…" choice);
/// 3. the capability's default (fal for video, image, lipsync, upscale, 3d, and for now music and
///    sfx, D3), when it is offered.
///
/// Nothing in 1–3 leaves the capability with no recommendation: the listed providers are only
/// alternatives. `detect_only` and `recommendable: false` providers are never offered.
pub fn recommend(cat: &Catalogue, capability: &str, covered: &[Cover], present: &HashSet<String>, already_pay: &[String]) -> (Option<String>, Vec<String>) {
    if covered.iter().any(|c| usable(&c.state)) {
        return (None, Vec::new());
    }
    let candidates: Vec<&Provider> = cat.providers.iter().filter(|p| p.covers(capability) && p.offer != "detect_only").collect();
    let tried = |p: &Provider| covered.iter().any(|c| c.provider == p.id);
    let pays = |p: &Provider| already_pay.contains(&p.id) || (p.subscriber_first && present.contains(&p.id));
    let default = |p: &Provider| p.default_for.iter().any(|c| c == capability) && p.recommendable;
    let mut ordered: Vec<&Provider> = Vec::new();
    for stage in [&tried as &dyn Fn(&Provider) -> bool, &pays, &default] {
        let next: Vec<&Provider> = candidates.iter().copied().filter(|p| stage(p) && !ordered.iter().any(|o| o.id == p.id)).collect();
        ordered.extend(next);
    }
    let head = ordered.len();
    // Then everything else: a provider maiTerm sets up before a guide the human follows alone,
    // and one listed-but-not-recommended last.
    let mut rest: Vec<&Provider> = candidates.iter().copied().filter(|p| !ordered.iter().any(|o| o.id == p.id)).collect();
    rest.sort_by_key(|p| (!p.recommendable, !p.has_automatic_route()));
    ordered.extend(rest);
    let ids: Vec<String> = ordered.iter().map(|p| p.id.clone()).collect();
    // `recommended` only when a provider of stages 1-3 leads; otherwise everything is an alternative.
    if head > 0 {
        (ids.first().cloned(), ids.into_iter().skip(1).collect())
    } else {
        (None, ids)
    }
}

/// One row per capability. `workspace` is the Hive's workspace (None in Preferences, which
/// applies no proxy scope); `already_pay` provider ids the human said they subscribe to.
pub fn build_status(cat: &Catalogue, f: &Facts, workspace: Option<&str>, already_pay: &[String]) -> MediaStatus {
    let mut covered = covers(cat, f, workspace);
    // Providers present in any state: a subscriber found anywhere is promoted for every capability.
    let present: HashSet<String> = covered.values().flatten().map(|c| c.provider.clone()).collect();
    let order = |id: &str| cat.providers.iter().position(|p| p.id == id).unwrap_or(usize::MAX);
    let rows = cat
        .capabilities
        .iter()
        .map(|cap| {
            let mut list = covered.remove(&cap.id).unwrap_or_default();
            let subscriber = |c: &Cover| provider(&c.provider).is_some_and(|p| p.subscriber_first);
            list.sort_by_key(|c| (rank(&c.state), !subscriber(c), order(&c.provider)));
            let (recommended, alternatives) = recommend(cat, &cap.id, &list, &present, already_pay);
            CapabilityRow { capability: cap.id.clone(), covered_by: list, recommended, alternatives }
        })
        .collect();
    MediaStatus { rows, brew: f.brew }
}

// ─── Gathering the facts ────────────────────────────────────────────────────

/// Every Claude config root a Hive's agent might run under: each managed Claude account's, and
/// the user's own (`CLAUDE_CONFIG_DIR` as they exported it, else `~/.claude`). A plugin installed
/// under one account is not installed under another (the farm shares `plugins/` only where it
/// links it), so Remotion is looked for in all of them.
pub fn claude_roots(prefs: &crate::state::Preferences) -> Vec<PathBuf> {
    let mut out: Vec<PathBuf> = prefs
        .managed_accounts
        .iter()
        .filter(|a| a.runtime == crate::accounts::Runtime::Claude.slug())
        .filter_map(|a| crate::accounts::account_root(crate::accounts::Runtime::Claude, &a.id))
        .collect();
    out.extend(crate::accounts::user_claude_root().map(Path::to_path_buf));
    out.extend(dirs::home_dir().map(|h| h.join(".claude")));
    let mut seen = HashSet::new();
    out.retain(|p| seen.insert(p.clone()));
    out
}

/// The plugin ids (`name@marketplace`) in one root's `plugins/installed_plugins.json`.
pub fn installed_plugins(root: &Path) -> Vec<String> {
    std::fs::read(root.join("plugins").join("installed_plugins.json"))
        .ok()
        .and_then(|b| serde_json::from_slice::<serde_json::Value>(&b).ok())
        .and_then(|v| v.get("plugins").and_then(|p| p.as_object()).map(|p| p.keys().cloned().collect()))
        .unwrap_or_default()
}

fn app_present(bundle: &str) -> bool {
    Path::new("/Applications").join(bundle).exists() || dirs::home_dir().is_some_and(|h| h.join("Applications").join(bundle).exists())
}

/// Read the machine. Blocks: file, PATH and a local port probe.
pub fn gather(prefs: &crate::state::Preferences) -> Facts {
    let cat = catalogue();
    let tools = super::create::detect_tools(prefs);
    let mut clis: HashSet<String> = tools.iter().filter(|t| t.kind == "cli").map(|t| t.label.clone()).collect();
    for bin in cat.providers.iter().flat_map(|p| &p.detect.clis).map(|c| c.bin.as_str()).chain(["brew"]) {
        if !clis.contains(bin) && !crate::accounts::find_executables(bin).is_empty() {
            clis.insert(bin.to_string());
        }
    }
    let brew = clis.contains("brew");
    crate::mcp_proxy::ensure_loaded();
    let proxy = crate::mcp_proxy::all()
        .into_iter()
        .map(|s| {
            let facts = ProxyFacts { enabled: s.enabled, state: crate::mcp_proxy::status(&s.id).state, workspaces: s.workspaces.clone() };
            (s.id, facts)
        })
        .collect();
    let plugins = claude_roots(prefs).iter().flat_map(|r| installed_plugins(r)).collect();
    let apps = cat.providers.iter().flat_map(|p| &p.detect.apps).filter(|a| app_present(a)).cloned().collect();
    let comfy_running = tools.iter().any(|t| t.kind == "comfyui");
    Facts { tools, proxy, clis, plugins, comfy_running, apps, brew }
}

/// What the Tools step reads (§5): one row per capability, with what covers it, its state, and a
/// recommendation where it is missing. `workspace_id` is the Hive's workspace, or none for
/// Preferences. `already_pay`: provider ids the human said they subscribe to.
#[tauri::command]
pub async fn hive_media_status(state: State<'_, Arc<AppState>>, workspace_id: Option<String>, already_pay: Option<Vec<String>>) -> Result<MediaStatus, String> {
    let prefs = state.app_data.read().preferences.clone();
    let already_pay = already_pay.unwrap_or_default();
    tauri::async_runtime::spawn_blocking(move || build_status(catalogue(), &gather(&prefs), workspace_id.as_deref(), &already_pay)).await.map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests;
