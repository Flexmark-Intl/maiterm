//! The capability map in a Hive agent's brief (docs/hive-media.md §10): what the Hive can make,
//! with which tool, what that spends, and what isn't set up. It replaces the raw "Tools this Hive
//! has: mcp:fal, cli:ffmpeg" line in the coordinator's brief and every media worker's.
//!
//! Everything said is joined from `Hive.tools` and the catalogue: a provider's name, its
//! capabilities (in catalogue order), its route's `credits` and `quote_tool`, and its results'
//! retention. A server is named as the reader's OWN MCP config names it (a worker's `mcp.json`,
//! written by `settings::worker_mcp_config`; the coordinator's account `.claude.json`), so a dev
//! build's `fal-dev` is written `fal-dev`, never a name this module makes up.
//!
//! A worker is never told it has a plugin or a skill: it loads neither (`--setting-sources
//! project`, docs/hive.md §6.7). Those, and local ComfyUI (a worker's localhost is closed), are on
//! the coordinator's map only.

use serde_json::Value;

use super::{catalogue, providers_for_skill, Provider, Route};
use crate::hive::deliver::one_line;

/// Who reads the map. The coordinator asks for a missing capability with `hiveRequestCapability`
/// and holds the plugins and skills; a worker reports a gap to its coordinator.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Reader {
    Coordinator,
    Worker,
}

/// One MCP server in an agent's config, as the catalogue recognises it.
#[derive(Debug, Clone, PartialEq)]
pub struct Server {
    /// Its name in that config: the key of its `mcpServers` entry.
    pub name: String,
    pub provider: &'static Provider,
}

/// The servers in `config`'s `mcpServers` that the Hive chose (`mcp:<name>` in `tools`) and the
/// catalogue recognises (`create::generator_providers`), in the order of `tools`.
pub fn servers(config: Option<&Value>, tools: &[String]) -> Vec<Server> {
    let Some(entries) = config.and_then(|c| c.get("mcpServers")).and_then(Value::as_object) else { return Vec::new() };
    let mut out: Vec<Server> = Vec::new();
    for name in tools.iter().filter_map(|t| t.strip_prefix("mcp:")) {
        if out.iter().any(|s| s.name == name) {
            continue;
        }
        let Some(entry) = entries.get(name) else { continue };
        if let Some(p) = crate::hive::create::generator_providers(name, entry).into_iter().next() {
            out.push(Server { name: name.to_string(), provider: p });
        }
    }
    out
}

/// What the map is drawn from.
pub struct MapInput<'a> {
    pub reader: Reader,
    /// `Hive.tools`.
    pub tools: &'a [String],
    /// The generator servers in the reader's own MCP config.
    pub own: &'a [Server],
    /// The generator servers the Hive has, in the account config its agents launch under. For the
    /// coordinator these are its own. A worker without generators (a reviewer, an art director)
    /// is told that other workers have them, and "Not set up" never lists what they cover.
    pub hive: &'a [Server],
}

/// The map's text, and every `Hive.tools` id it names (the coordinator lists the rest as other
/// tools).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Map {
    pub lines: Vec<String>,
    pub listed: Vec<String>,
    /// The coordinator of a Hive with no media tool at all: one line, not a five-capability
    /// "Not set up" list that would invite needless requests.
    pub no_media: bool,
}

/// What a coordinator whose Hive has no media tool is told, in place of the map.
pub const NO_MEDIA: &str = "This Hive has no media tools. If the goal needs images, video, voice or music, ask with hiveRequestCapability; never substitute.";

const SHOWN_NAME: usize = 80;

fn cap_index(id: &str) -> usize {
    catalogue().capabilities.iter().position(|c| c.id == id).unwrap_or(usize::MAX)
}

/// Capability ids in catalogue order.
fn in_order<'a>(caps: impl IntoIterator<Item = &'a str>) -> Vec<&'a str> {
    let mut v: Vec<&str> = Vec::new();
    for c in caps {
        if !v.contains(&c) {
            v.push(c);
        }
    }
    v.sort_by_key(|c| cap_index(c));
    v
}

/// A name from a config or `Hive.tools`, safe inside the brief's quotes.
fn shown(name: &str) -> String {
    one_line(name, SHOWN_NAME).replace('"', "'")
}

/// The route an MCP server is reached by: its first MCP route (remote now, stdio from phase 5).
fn mcp_route(p: &Provider) -> Option<&Route> {
    p.routes.iter().find(|r| r.kind.ends_with("_mcp"))
}

/// Credits that aren't dollars the budget counts: a plan's or a provider's own credits.
fn plan_like(credits: &str) -> bool {
    matches!(credits, "plan" | "plan_or_wallet" | "comfy_credits")
}

/// What a job spends, then (where it can) how to price it first.
fn money(p: &Provider, route: &Route, reader: Reader) -> String {
    let name = &p.name;
    let mut s = match route.credits.as_str() {
        "api" => "Prepaid API credit.".to_string(),
        "plan" => format!("Spends your human's {name} plan credits."),
        "plan_or_wallet" => format!("Spends your human's {name} plan credits or wallet balance."),
        "comfy_credits" => format!("Spends your human's {name} credits."),
        "free" | "none" => "Free.".to_string(),
        _ => String::new(),
    };
    if plan_like(&route.credits) {
        s.push_str(match reader {
            Reader::Coordinator => " The budget can't see these credits, so count generations instead: give each worker the number it may make.",
            Reader::Worker => " The budget can't see these credits, so your coordinator counts generations instead: make no more than your brief allows.",
        });
    }
    if let Some(q) = route.quote_tool.as_deref().filter(|q| !q.trim().is_empty()) {
        s.push_str(&format!(" Price a job with {q} before running it."));
    }
    s.trim_start().to_string()
}

/// How its results come back: a route's own note (a server maiTerm runs itself saves files into
/// its output folder, which `galleryAdd` accepts, while a worker's shell can't reach its result
/// URLs), else how long its result URLs last.
fn results(p: &Provider, reader: Reader) -> Option<String> {
    if let Some(note) = mcp_route(p).and_then(|r| r.results_note.as_deref()).map(str::trim).filter(|n| !n.is_empty()) {
        return Some(note.to_string());
    }
    if p.result_hosts.is_empty() && p.retention.trim().is_empty() {
        return None;
    }
    let expire = if p.retention.trim().is_empty() { "expire".to_string() } else { format!("expire after {}", p.retention.trim()) };
    let into = match reader {
        Reader::Worker => " into scratch/",
        Reader::Coordinator => "",
    };
    Some(format!("Results are public URLs that {expire}: download each{into} at once, then galleryAdd it."))
}

/// The map, as brief lines (each without its leading "- "; a continuation starts with two spaces).
pub fn map(input: &MapInput) -> Map {
    let reader = input.reader;
    let tools = input.tools;
    let mut out = Map::default();
    // (sort key, lines) per entry; sorted by the first capability each covers.
    let mut entries: Vec<(usize, Vec<String>)> = Vec::new();
    let mut covered: Vec<&str> = Vec::new();

    // MCP servers this agent really has.
    for s in input.own {
        let p = s.provider;
        let caps = in_order(p.capabilities.iter().map(String::as_str));
        covered.extend(&caps);
        let mut head = format!("{}: {} (MCP \"{}\").", caps.join(", "), p.name, shown(&s.name));
        let mut lines = Vec::new();
        if let Some(route) = mcp_route(p) {
            let m = money(p, route, reader);
            if !m.is_empty() {
                head.push(' ');
                head.push_str(&m);
            }
        }
        lines.push(head);
        if let Some(r) = results(p, reader) {
            lines.push(format!("  {r}"));
        }
        entries.push((caps.first().map_or(usize::MAX, |c| cap_index(c)), lines));
        out.listed.push(format!("mcp:{}", s.name));
    }

    // The Hive's servers this agent doesn't have (a worker that doesn't generate).
    for s in input.hive.iter().filter(|s| !input.own.iter().any(|o| o.name == s.name)) {
        let caps = in_order(s.provider.capabilities.iter().map(String::as_str));
        covered.extend(&caps);
        entries.push((caps.first().map_or(usize::MAX, |c| cap_index(c)), vec![format!("{}: other workers make these with {}; you can't run it.", caps.join(", "), s.provider.name)]));
    }

    // Programs, grouped: capabilities whose programs are the same share one line.
    let mut groups: Vec<(Vec<&str>, Vec<String>)> = Vec::new();
    {
        let mut per_cap: Vec<(&str, Vec<String>)> = Vec::new();
        for id in tools {
            let Some(bin) = id.strip_prefix("cli:") else { continue };
            let mut hit = false;
            for probe in catalogue().providers.iter().flat_map(|p| &p.detect.clis).filter(|c| c.bin == bin) {
                hit = true;
                let label = probe.name.clone().unwrap_or_else(|| probe.bin.clone());
                for cap in &probe.capabilities {
                    match per_cap.iter_mut().find(|(c, _)| c == cap) {
                        Some((_, v)) if !v.contains(&label) => v.push(label.clone()),
                        Some(_) => {}
                        None => per_cap.push((cap.as_str(), vec![label.clone()])),
                    }
                }
            }
            if hit && !out.listed.contains(id) {
                out.listed.push(id.clone());
            }
        }
        per_cap.sort_by_key(|(c, _)| cap_index(c));
        for (cap, progs) in per_cap {
            covered.push(cap);
            match groups.iter_mut().find(|(_, p)| *p == progs) {
                Some((caps, _)) => caps.push(cap),
                None => groups.push((vec![cap], progs)),
            }
        }
    }
    for (caps, progs) in groups {
        entries.push((cap_index(caps[0]), vec![format!("{}: {}.", caps.join(", "), progs.join(", "))]));
    }

    // The coordinator's own: plugins, skills and local ComfyUI, which no worker has. They count as
    // covered for the coordinator only: a worker can't use them, so its "Not set up" still lists
    // their capabilities and sends it to its coordinator rather than to a substitute.
    for id in tools {
        let found: Vec<(&'static Provider, String)> = if let Some(plugin) = id.strip_prefix("plugin:") {
            catalogue().providers.iter().filter(|p| p.detect.plugins.iter().any(|x| x == plugin)).map(|p| (p, format!("your Claude Code plugin {}", shown(plugin)))).collect()
        } else if let Some(skill) = id.strip_prefix("skill:") {
            providers_for_skill(skill).into_iter().map(|p| (p, format!("your skill {}", shown(skill)))).collect()
        } else if id == "comfyui" {
            catalogue().providers.iter().filter(|p| p.detect.port.is_some() && p.routes.iter().any(|r| r.kind == "local_app")).map(|p| (p, "on this Mac".to_string())).collect()
        } else {
            Vec::new()
        };
        for (p, how) in found {
            let caps = in_order(p.capabilities.iter().map(String::as_str));
            if reader == Reader::Coordinator {
                covered.extend(&caps);
                entries.push((caps.first().map_or(usize::MAX, |c| cap_index(c)), vec![format!("{}: {}, {how} (yours only: workers don't have it).", caps.join(", "), p.name)]));
                if !out.listed.contains(id) {
                    out.listed.push(id.clone());
                }
            }
        }
    }

    if covered.is_empty() && reader == Reader::Coordinator {
        out.lines = vec![NO_MEDIA.to_string()];
        out.no_media = true;
        return out;
    }
    entries.sort_by_key(|(k, _)| *k);
    out.lines = entries.into_iter().flat_map(|(_, l)| l).collect();

    let missing: Vec<&str> = super::common_capabilities().map(|c| c.id.as_str()).filter(|c| !covered.contains(c)).collect();
    if !missing.is_empty() {
        let what = match reader {
            Reader::Coordinator => "If the goal needs it, ask with hiveRequestCapability; never substitute.",
            Reader::Worker => "If your task needs it, stop and report it to your coordinator; never substitute.",
        };
        out.lines.push(format!("Not set up: {}. {what}", missing.join(", ")));
    }
    out
}

/// The map as a brief's bullet: the heading, then each line one level in.
/// A coordinator whose Hive has no media tool gets the one line instead.
pub fn bullet(m: &Map) -> String {
    if m.no_media {
        return format!("- {NO_MEDIA}\n");
    }
    let mut s = String::from("- What this Hive can make, and with what:\n");
    for l in &m.lines {
        match l.strip_prefix("  ") {
            Some(cont) => s.push_str(&format!("    {cont}\n")),
            None => s.push_str(&format!("  - {l}\n")),
        }
    }
    s
}

#[cfg(test)]
mod tests;
