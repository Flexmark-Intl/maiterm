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
use std::sync::OnceLock;

use serde::{Deserialize, Serialize};

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
    /// A tool that reads the account's balance, if the provider has one. It prices nothing: the
    /// panel does not show it, and phase 3's verify step is its reader.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub balance_tool: Option<String>,
    /// `brew`: the one install line.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub command: Option<String>,
    /// `plugin`: the Claude Code plugin id, `name@marketplace`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub plugin_id: Option<String>,
    /// `plugin`: what `claude plugin marketplace add` takes for that marketplace (`owner/repo`
    /// or a URL). Only a source someone has confirmed: a plugin route without one is refused by
    /// the install (`install::plan`), which points the human at the guide instead of guessing.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub marketplace: Option<String>,
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
    /// The host suffixes under which a recognised native server's OWN url host is added to a
    /// worker's sandbox (the five providers' old table, unchanged). Empty for a provider that is
    /// only recognised: its server runs outside the sandbox and its host is no one's way out.
    pub own_hosts: Vec<String>,
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
    /// A models line per capability, for a provider whose models differ by capability (fal's video
    /// models are not its music models). The panel shows it under that capability, else `models_note`.
    #[serde(default)]
    pub capability_models: HashMap<String, String>,
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

    /// Whether `host` is under one of its `own_hosts` suffixes: the hosts the old table matched a
    /// server by, the only ones that make its `sandbox_hosts` a worker's. `match.hosts` is wider
    /// (kling.ai) and only recognises.
    pub fn owns_host(&self, host: &str) -> bool {
        let host = bare_host(host);
        self.matches.own_hosts.iter().any(|s| suffix_of(&host, s))
    }

    /// Whether a word of `name` is one of its `match.names`.
    pub fn named_by(&self, name: &str) -> bool {
        name_words(name).any(|w| self.matches.names.contains(&w))
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
        for cap in p.capabilities.iter().chain(&p.default_for).chain(p.capability_notes.keys()).chain(p.capability_models.keys()).chain(p.detect.clis.iter().flat_map(|c| &c.capabilities)) {
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

pub fn capability(id: &str) -> Option<&'static Capability> {
    catalogue().capabilities.iter().find(|c| c.id == id)
}

/// The capabilities the first page shows (video, image, voice, music, finish), in catalogue order.
pub fn common_capabilities() -> impl Iterator<Item = &'static Capability> {
    catalogue().capabilities.iter().filter(|c| c.common)
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

/// Whether a native MCP server's own url host may become a worker sandbox host: it is under an
/// `own_hosts` suffix. Recognition (`match.hosts`) is wider than this on purpose; phase 1 adds no
/// sandbox host a server's host did not already yield.
pub fn is_sandbox_own_host(host: &str) -> bool {
    catalogue().providers.iter().any(|p| p.owns_host(host))
}

/// The providers a PROXIED server is recognised as: by the host of the upstream it dials, and by
/// nothing else. Its name is the human's (or an agent's) to choose and says nothing about what
/// it reaches: a proxied "fal" that points elsewhere is not fal, and a proxied "video" that
/// points at fal is. `None` (the registry no longer has it) recognises nothing.
pub fn providers_for_upstream(upstream_url: Option<&str>) -> Vec<&'static Provider> {
    let Some(host) = upstream_url.and_then(super::create::url_host) else { return Vec::new() };
    providers_for_host(&host)
}

/// The providers whose `match.hosts` has a suffix of `host` (any case, any port). No name is
/// consulted.
pub fn providers_for_host(host: &str) -> Vec<&'static Provider> {
    let host = bare_host(host);
    catalogue().providers.iter().filter(|p| host_matches(p, &host)).collect()
}

/// The providers a skill uses (`/generate` → Kie.ai), by the skill's exact name or a word of the
/// provider's names in it, as the table did.
pub fn providers_for_skill(skill: &str) -> Vec<&'static Provider> {
    catalogue().providers.iter().filter(|p| p.matches.skills.iter().any(|s| s == skill) || name_words(skill).any(|w| p.matches.names.contains(&w))).collect()
}

pub mod install;
pub mod status;
pub mod view;
pub mod whisper;

#[cfg(test)]
mod tests;
