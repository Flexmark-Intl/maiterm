//! Per-runtime on-disk registration. The MCP/IDE HTTP server is shared (one port,
//! one auth); only the on-disk config a CLI discovers differs per runtime. Each
//! Registrar owns its runtime's install/reassert/unregister. ClaudeRegistrar
//! delegates to the existing lockfile.rs machinery verbatim.
use crate::state::{AgentRuntime, Preferences};
use super::lockfile;

pub trait Registrar: Send + Sync {
    /// The runtime this registrar serves, used by preference refresh dispatch.
    #[allow(dead_code)]
    fn runtime(&self) -> AgentRuntime;
    /// Whether this runtime's integration is enabled in preferences.
    fn enabled(&self, prefs: &Preferences) -> bool;
    /// Write all on-disk registration (config/MCP entry + hooks + skill) for the live server.
    fn install(&self, port: u16, auth: &str, workspace_folders: &[String], prefs: &Preferences);
    /// Re-assert config after drift. Currently implemented only by ClaudeRegistrar.
    fn reassert_if_drifted(&self, port: u16, auth: &str, prefs: &Preferences);
    /// Remove all on-disk registration (app exit).
    fn unregister(&self, port: u16, auth: &str);
}

pub struct ClaudeRegistrar;
impl Registrar for ClaudeRegistrar {
    fn runtime(&self) -> AgentRuntime { AgentRuntime::Claude }
    fn enabled(&self, prefs: &Preferences) -> bool { prefs.claude_ide }
    fn install(&self, port: u16, auth: &str, workspace_folders: &[String], prefs: &Preferences) {
        if let Err(e) = lockfile::write_lockfile(port, auth, workspace_folders.to_vec(), prefs.claude_hooks) {
            log::warn!("Failed to write Claude Code lock file: {}", e);
        }
    }
    fn reassert_if_drifted(&self, port: u16, auth: &str, prefs: &Preferences) {
        if let Err(e) = lockfile::ensure_mcp_settings(port, auth) {
            log::warn!("MCP settings re-assert failed: {}", e);
        }
        // Hooks live in ~/.claude/settings.json, which is just as clobber-prone
        // (claude CLI rewrites; an SSH-bridge setup script landing in a local
        // shell overwrites them with dead remote-tunnel ports).
        if prefs.claude_hooks {
            if let Err(e) = lockfile::ensure_hook_settings(port, auth) {
                log::warn!("Hook settings re-assert failed: {}", e);
            }
        }
    }
    fn unregister(&self, port: u16, auth: &str) {
        lockfile::delete_lockfile(port, auth);
    }
}

/// All known registrars (used for exit cleanup — unregister regardless of current pref).
/// Codex installs when prefs.codex_ide is on (default true).
/// Exit cleanup currently unregisters both unconditionally; Codex shared-hook ownership
/// remains an open issue in docs/codex-integration-review.md.
pub fn all_registrars() -> Vec<Box<dyn Registrar>> {
    vec![Box::new(ClaudeRegistrar), Box::new(super::codex::CodexRegistrar)]
}
/// Registrars whose integration is currently enabled (used for install + reassert).
pub fn enabled_registrars(prefs: &Preferences) -> Vec<Box<dyn Registrar>> {
    all_registrars().into_iter().filter(|r| r.enabled(prefs)).collect()
}
