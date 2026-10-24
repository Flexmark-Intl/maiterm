//! The pilot override (docs/hive.md §6.8): `MAITERM_INSTANCE=<name>` makes a DEBUG build an
//! isolated instance that runs beside the dev and installed maiTerms without touching either.
//!
//! It gets its own data folder (`com.aiterm.dev.<name>`, through `persistence::app_data_slug`, so
//! its own instance lock, state, ssh sockets and logs), and it makes no global write: no
//! `~/.claude.json` entry, hooks, skill or Codex config, no MCP proxy entries, no browser-link
//! files, no deshittify rules, no SSH bridge (whose setup rewrites a host's shared Codex config
//! and hooks). Its agents therefore have no maiTerm MCP of their own, and their priming says so:
//! it is a test subject, not an agent host. So it runs no Hive either, whose agents would reach
//! its MCP through their own `--mcp-config` and need folder trust in `~/.claude.json`. Its tabs
//! never carry the parent tab's `MAITERM_PORT`/`MAITERM_AUTH` (`pty::manager`). maiLink stays
//! off unless `MAITERM_MAILINK_PORT` names a port; `MAITERM_MCP_PORT` pins its server's port,
//! and a pinned port that is taken refuses the launch.
//!
//! Release builds never read any of this, so the installed app can't be moved by an inherited
//! variable.

use std::sync::OnceLock;

pub const INSTANCE_ENV: &str = "MAITERM_INSTANCE";
pub const MCP_PORT_ENV: &str = "MAITERM_MCP_PORT";
pub const MAILINK_PORT_ENV: &str = "MAITERM_MAILINK_PORT";

/// Ports an override may never name: the dev server's, the installed maiTerm's maiLink and the
/// port it used until 2026-10-20 (where a kill-by-port quit maiTerm).
pub const RESERVED_PORTS: [u16; 3] = [1420, 8765, 27365];

/// `[a-z0-9-]{1,32}`: it ends up in a folder name, a bundle identifier and a socket path.
pub fn valid_name(name: &str) -> bool {
    (1..=32).contains(&name.len()) && name.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
}

/// What `MAITERM_INSTANCE` asks for: `Ok(None)` for a normal launch (and always in release).
pub fn parse_name(debug: bool, raw: Option<&str>) -> Result<Option<String>, String> {
    match raw {
        _ if !debug => Ok(None),
        None => Ok(None),
        Some(n) if valid_name(n) => Ok(Some(n.to_string())),
        Some(n) => Err(format!("{INSTANCE_ENV}={n:?} is not a valid instance name (1-32 of a-z, 0-9 and -)")),
    }
}

fn parsed() -> &'static Result<Option<String>, String> {
    static PARSED: OnceLock<Result<Option<String>, String>> = OnceLock::new();
    PARSED.get_or_init(|| parse_name(cfg!(debug_assertions), std::env::var(INSTANCE_ENV).ok().as_deref()))
}

/// The instance name, when this is an isolated instance.
pub fn name() -> Option<&'static str> {
    parsed().as_ref().ok().and_then(|n| n.as_deref())
}

pub fn is_isolated() -> bool {
    name().is_some()
}

/// The data folder's name for this launch. An invalid name gets a folder no real instance can
/// have ('!' is outside the charset): `run` refuses to start on one, and anything that asks
/// before it never lands in dev's folder.
pub fn slug_for(debug: bool, instance: &Result<Option<String>, String>) -> String {
    match (debug, instance) {
        (false, _) => "com.aiterm.app".to_string(),
        (true, Ok(None)) => "com.aiterm.dev".to_string(),
        (true, Ok(Some(n))) => format!("com.aiterm.dev.{n}"),
        (true, Err(_)) => "com.aiterm.dev.!invalid".to_string(),
    }
}

pub fn slug() -> String {
    slug_for(cfg!(debug_assertions), parsed())
}

/// Why this launch must not start, checked first thing in `run`.
pub fn startup_refusal() -> Option<String> {
    if let Err(e) = parsed() {
        return Some(e.clone());
    }
    match mcp_port() {
        Err(e) => return Some(e),
        // Without its own server the instance's tabs would hold no port of their own, so it
        // doesn't start at all rather than run as a different test subject than asked for.
        Ok(Some(p)) => {
            if let Some(why) = pinned_port_taken(p) {
                return Some(why);
            }
        }
        Ok(None) => {}
    }
    mailink_port().err()
}

/// Why the pinned MCP port can't be bound on loopback, or None (the probe is released at once;
/// `prepare_server` binds it for real).
pub fn pinned_port_taken(port: u16) -> Option<String> {
    std::net::TcpListener::bind(("127.0.0.1", port))
        .err()
        .map(|e| format!("{MCP_PORT_ENV}={port} is not free: {e}"))
}

/// `MAITERM_PORT` for an isolated instance's tabs when it has no server. Never empty and never a
/// real port: the other maiTerms' global hooks run when it is theirs OR empty, and this
/// instance's tabs usually inherit the parent tab's (the pilot launches it from a worker tab).
pub const NO_SERVER_PORT: &str = "0";

/// A port override, read only by an isolated instance.
pub fn parse_port(var: &str, raw: Option<&str>) -> Result<Option<u16>, String> {
    let Some(raw) = raw else { return Ok(None) };
    match raw.trim().parse::<u16>() {
        Ok(p) if p >= 1024 && !RESERVED_PORTS.contains(&p) => Ok(Some(p)),
        Ok(p) if RESERVED_PORTS.contains(&p) => Err(format!("{var}={p} is reserved (1420, 8765 and 27365 belong to the other maiTerms)")),
        _ => Err(format!("{var}={raw:?} is not a port from 1024 to 65535")),
    }
}

fn port_env(var: &str) -> Result<Option<u16>, String> {
    if !is_isolated() {
        return Ok(None);
    }
    parse_port(var, std::env::var(var).ok().as_deref())
}

/// The MCP server's pinned port, or `Ok(None)` for a random one as always.
pub fn mcp_port() -> Result<Option<u16>, String> {
    port_env(MCP_PORT_ENV)
}

/// maiLink's port in an isolated instance (`Ok(None)`: maiLink stays off there).
pub fn mailink_port() -> Result<Option<u16>, String> {
    port_env(MAILINK_PORT_ENV)
}

/// Why maiLink can't start in this instance, or None.
pub fn mailink_refusal() -> Option<String> {
    match (is_isolated(), mailink_port()) {
        (false, _) => None,
        (true, Ok(Some(_))) => None,
        (true, Ok(None)) => Some(format!("maiLink is off in an isolated instance unless {MAILINK_PORT_ENV} names a port")),
        (true, Err(e)) => Some(e),
    }
}

/// Why a write to config another maiTerm owns (`~/.claude.json`, hooks, Codex, browser link)
/// must be skipped, or None.
pub fn global_write_refusal() -> Option<String> {
    name().map(|n| format!("isolated instance {n:?} makes no global writes"))
}

/// Why no Hive is created or launched here, or None. A Hive agent runs under the real Claude
/// config, whose folder trust (`~/.claude.json`) the launch must write, and it reaches this
/// instance's MCP through its own `--mcp-config`, so it would be an agent host after all.
pub fn hive_refusal() -> Result<(), String> {
    hive_refusal_for(name())
}

pub fn hive_refusal_for(name: Option<&str>) -> Result<(), String> {
    match name {
        None => Ok(()),
        Some(n) => Err(format!(
            "This is the isolated maiTerm test instance {n:?}, which hosts no agents and writes no Claude config, \
             so it can't run a Hive. Use the dev or installed maiTerm for that."
        )),
    }
}

/// The priming an isolated instance's agents get instead of maiTerm's standing instructions:
/// the maiTerm tools they see, if any, belong to another maiTerm.
pub fn agent_priming(name: &str) -> String {
    format!(
        "\n\nThis is an isolated maiTerm test instance ({name}). It registers no tools for you: \
         any maiterm tool you can see belongs to a different maiTerm, so do not use maiterm tools \
         from this tab."
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_normal_launch_keeps_its_data_folder() {
        assert_eq!(slug_for(true, &Ok(None)), "com.aiterm.dev");
        assert_eq!(slug_for(false, &Ok(None)), "com.aiterm.app");
        // Release never reads the variable at all.
        assert_eq!(parse_name(false, Some("pilot")), Ok(None));
        assert_eq!(slug_for(false, &parse_name(false, Some("pilot"))), "com.aiterm.app");
    }

    #[test]
    fn an_instance_gets_its_own_folder_and_a_bad_name_never_gets_devs() {
        assert_eq!(slug_for(true, &parse_name(true, Some("w-3"))), "com.aiterm.dev.w-3");
        for bad in ["", "Pilot", "a b", "../x", "a.b", "x;y", &"a".repeat(33)] {
            let p = parse_name(true, Some(bad));
            assert!(p.is_err(), "{bad:?}");
            let slug = slug_for(true, &p);
            assert_ne!(slug, "com.aiterm.dev", "{bad:?}");
            assert!(!valid_name(slug.trim_start_matches("com.aiterm.dev.")), "{bad:?} lands on a real instance's folder");
        }
        assert!(valid_name(&"a".repeat(32)));
    }

    #[test]
    fn a_port_override_refuses_the_other_maiterms_ports() {
        assert_eq!(parse_port(MCP_PORT_ENV, None), Ok(None));
        assert_eq!(parse_port(MCP_PORT_ENV, Some("41234")), Ok(Some(41234)));
        for bad in ["8765", "27365", "1420", "80", "0", "70000", "x", ""] {
            assert!(parse_port(MCP_PORT_ENV, Some(bad)).is_err(), "{bad}");
        }
    }

    #[test]
    fn a_taken_pinned_port_refuses_the_launch() {
        let held = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = held.local_addr().unwrap().port();
        assert!(pinned_port_taken(port).is_some_and(|w| w.contains(&port.to_string())));
        drop(held);
        assert_eq!(pinned_port_taken(port), None);
        // The no-server sentinel can't pass a `[ "$MAITERM_PORT" = "<port>" ] || [ -z … ]` gate.
        assert!(!NO_SERVER_PORT.is_empty() && NO_SERVER_PORT.parse::<u16>() == Ok(0));
    }

    #[test]
    fn an_isolated_instance_runs_no_hive() {
        assert_eq!(hive_refusal_for(None), Ok(()));
        assert!(hive_refusal_for(Some("p1")).is_err_and(|e| e.contains("\"p1\"") && e.contains("Hive")));
    }

    #[test]
    fn the_isolated_priming_survives_the_session_start_hook() {
        // Echoed inside a single-quoted shell string by the SessionStart command hook.
        let p = agent_priming("w-1");
        assert!(!p.contains('\'') && p.contains("w-1"), "{p}");
    }

    #[test]
    fn a_normal_launch_reads_no_override() {
        // The test binary runs without MAITERM_INSTANCE: nothing here may change behaviour.
        if std::env::var(INSTANCE_ENV).is_err() {
            assert!(!is_isolated());
            assert_eq!(mcp_port(), Ok(None));
            assert_eq!(mailink_refusal(), None);
            assert_eq!(global_write_refusal(), None);
            assert_eq!(crate::state::persistence::app_data_slug(), if cfg!(debug_assertions) { "com.aiterm.dev" } else { "com.aiterm.app" });
        }
    }
}
