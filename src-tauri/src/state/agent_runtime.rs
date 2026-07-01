//! Agent-runtime abstraction (Stage 2 — core types).
//!
//! The spine every multi-agent seam resolves through. Stage 2 is PURELY
//! ADDITIVE: only Claude is wired in, and Claude must behave byte-identically.
//! Codex/Gemini descriptor rows exist as inert-but-valid placeholders so later
//! stages can fill them in without touching the type surface.

use serde::{Deserialize, Serialize};

/// Which agent runtime a tab / MCP connection / session belongs to.
///
/// Detected per-connection from the MCP `initialize` `clientInfo.name`; defaults
/// to Claude (NEVER Codex) so an unrecognized client never misroutes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum AgentRuntime {
    #[default]
    Claude,
    Codex,
    Gemini,
}

#[allow(dead_code)]
impl AgentRuntime {
    /// Parse a persisted/serialized runtime key. Returns `None` for unknown keys.
    pub fn from_key(s: &str) -> Option<Self> {
        match s {
            "claude" => Some(Self::Claude),
            "codex" => Some(Self::Codex),
            "gemini" => Some(Self::Gemini),
            _ => None,
        }
    }

    /// The stable serialized key for this runtime (inverse of `from_key`).
    pub fn as_key(self) -> &'static str {
        match self {
            Self::Claude => "claude",
            Self::Codex => "codex",
            Self::Gemini => "gemini",
        }
    }

    /// Per-connection detection from the MCP `initialize` `clientInfo.name`.
    /// Substring match on a lowercased name; default Claude (NEVER Codex).
    pub fn detect(client_info_name: Option<&str>) -> Self {
        match client_info_name.map(|n| n.to_ascii_lowercase()) {
            Some(n) if n.contains("codex") => Self::Codex,
            Some(n) if n.contains("gemini") => Self::Gemini,
            _ => Self::Claude,
        }
    }

    /// Whether this runtime's dormancy is inferred from PTY/process lifecycle
    /// (Codex/Gemini) rather than a `SessionEnd` hook (Claude). The dormancy
    /// reaper only ever inspects runtimes for which this is true, so Claude is
    /// never polled and stays byte-identical.
    pub fn uses_pty_dormancy(self) -> bool {
        matches!(descriptor(self).dormancy, DormancySource::PtyExitOrPrompt)
    }

    /// Process-name basenames the runtime's CLI appears as in `ps`, for the reaper.
    pub fn agent_process_names(self) -> &'static [&'static str] {
        descriptor(self).agent_process_names
    }
}

/// How a runtime's hooks are installed on disk.
#[allow(dead_code)]
pub enum HookConfigKind {
    /// `~/.claude/settings.json` hook entries (Claude).
    ClaudeSettingsJson,
    /// `~/.codex/hooks.json` + shim (Codex).
    CodexHooksJson,
}

/// What signals that an agent session has gone dormant.
#[allow(dead_code)]
pub enum DormancySource {
    /// A `SessionEnd` hook fires (Claude).
    SessionEndHook,
    /// Inferred from PTY exit or returning to a shell prompt (Codex/Gemini).
    PtyExitOrPrompt,
}

/// Static, per-runtime capability/identity table. One `&'static` row per runtime.
#[allow(dead_code)]
pub struct RuntimeDescriptor {
    pub runtime: AgentRuntime,
    /// User-facing brand, e.g. "Claude Code" / "Codex". Drives toast/log/picker copy.
    pub display_name: &'static str,
    /// Base MCP server name; flavor suffix (`-dev`) applied via `mcp_server_name()`.
    pub mcp_server_base: &'static str,
    /// The `initialize` `clientInfo.name` match key for this runtime.
    pub client_info_name: &'static str,
    /// Trigger-variable name carrying the session id, e.g. "claudeSessionId".
    pub session_id_var: &'static str,
    /// Auth header used for the IDE/MCP connection.
    pub auth_header: &'static str,
    /// Whether the runtime supports forking a session (Claude:true, Codex:false — LOCKED).
    pub supports_fork: bool,
    /// Whether the runtime's CLI rewrites its own MCP config and so needs periodic
    /// re-assertion (Claude:true; Codex:false).
    pub needs_mcp_reassert: bool,
    pub hook_config: HookConfigKind,
    pub dormancy: DormancySource,
    /// Process-name basenames the runtime's CLI appears as in `ps` (argv0 or comm).
    /// Used by the dormancy reaper to tell "agent still running in the tab's PTY
    /// tree" from "agent exited". Empty for runtimes whose dormancy is hook-driven.
    pub agent_process_names: &'static [&'static str],
    /// How long a reported tool may stay "active" before being treated as stale.
    pub tool_stale_timeout_ms: u64,
}

/// Claude Code — the only fully-wired runtime in Stage 2.
pub static CLAUDE_DESC: RuntimeDescriptor = RuntimeDescriptor {
    runtime: AgentRuntime::Claude,
    display_name: "Claude Code",
    mcp_server_base: "maiterm",
    client_info_name: "Claude Code",
    session_id_var: "claudeSessionId",
    auth_header: "x-claude-code-ide-authorization",
    supports_fork: true,
    needs_mcp_reassert: true,
    hook_config: HookConfigKind::ClaudeSettingsJson,
    dormancy: DormancySource::SessionEndHook,
    // Claude dormancy is hook-driven (SessionEnd), so the reaper never inspects it.
    agent_process_names: &["claude"],
    tool_stale_timeout_ms: 15_000,
};

/// Codex — inert placeholder row. Valid but read by nothing yet.
#[allow(dead_code)]
pub static CODEX_DESC: RuntimeDescriptor = RuntimeDescriptor {
    runtime: AgentRuntime::Codex,
    display_name: "Codex",
    mcp_server_base: "maiterm",
    client_info_name: "codex",
    session_id_var: "codexSessionId",
    auth_header: "Authorization",
    supports_fork: false,
    needs_mcp_reassert: false,
    hook_config: HookConfigKind::CodexHooksJson,
    dormancy: DormancySource::PtyExitOrPrompt,
    // The OpenAI Codex CLI (codex-rs) runs as a native `codex` binary.
    agent_process_names: &["codex"],
    tool_stale_timeout_ms: 15_000,
};

/// Gemini — inert placeholder row. Valid but read by nothing yet.
#[allow(dead_code)]
pub static GEMINI_DESC: RuntimeDescriptor = RuntimeDescriptor {
    runtime: AgentRuntime::Gemini,
    display_name: "Gemini",
    mcp_server_base: "maiterm",
    client_info_name: "gemini",
    session_id_var: "geminiSessionId",
    auth_header: "Authorization",
    supports_fork: false,
    needs_mcp_reassert: false,
    hook_config: HookConfigKind::CodexHooksJson,
    dormancy: DormancySource::PtyExitOrPrompt,
    agent_process_names: &["gemini"],
    tool_stale_timeout_ms: 15_000,
};

/// Resolve the static descriptor row for a runtime.
#[allow(dead_code)]
pub fn descriptor(rt: AgentRuntime) -> &'static RuntimeDescriptor {
    match rt {
        AgentRuntime::Codex => &CODEX_DESC,
        AgentRuntime::Gemini => &GEMINI_DESC,
        AgentRuntime::Claude => &CLAUDE_DESC,
    }
}

/// The MCP server name for this build flavor. ALL runtimes share one server name
/// today, so the runtime argument is currently ignored; it exists so call sites
/// read correctly when per-runtime names land. Returns `&'static str` so it is a
/// drop-in for the existing `cfg!(debug_assertions)` ternaries.
pub fn mcp_server_name(_rt: AgentRuntime) -> &'static str {
    if cfg!(debug_assertions) {
        "maiterm-dev"
    } else {
        "maiterm"
    }
}

/// Env markers identifying an agent-spawned shell. Scrubbed from spawned PTYs so a
/// nested terminal doesn't inherit an outer agent's environment. A freshly launched
/// `claude` re-sets the ones it needs (CLAUDECODE, session id, entrypoint, …) for its
/// own children, so scrubbing them here changes no Claude behavior.
///
/// CLAUDE_CODE_CHILD_SESSION is the critical one: if a resumed session inherits it,
/// Claude comes up as a *child* session and silently stops writing its transcript to
/// disk — the tab's chat history never persists. It leaks in when maiTerm is launched
/// from inside a Claude session (e.g. the local deploy script's `open`), so it MUST be
/// stripped before the auto-resumed `claude` sees it.
pub const AGENT_ENV_MARKERS: &[&str] = &[
    "CLAUDECODE",
    "CLAUDE_CODE_CHILD_SESSION",
    "CLAUDE_CODE_SESSION_ID",
    "CLAUDE_CODE_ENTRYPOINT",
    "CLAUDE_CODE_EXECPATH",
    "CODEX_SANDBOX",
    "CODEX_SANDBOX_NETWORK_DISABLED",
    "GEMINI_CLI",
];
