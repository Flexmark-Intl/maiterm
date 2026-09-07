// Shared frontend agent-runtime types; Rust identity lives in state/agent_runtime.rs.

/** Which agent runtime a tab / session belongs to. Defaults to 'claude'. */
export type AgentRuntime = 'claude' | 'codex' | 'gemini';

/** Per-session agent activity state (union UNCHANGED from the Claude shape). */
export type AgentState = 'active' | 'idle' | 'permission';

/** Aggregate agent state surfaced on a workspace dot. */
export type WorkspaceAgentState = 'permission' | 'active' | 'idle-unread' | 'idle-read';

/**
 * User-facing per-runtime descriptor. Mirrors the load-bearing fields of the Rust
 * RuntimeDescriptor. The active frontend descriptor table lives in descriptor.ts.
 */
export interface AgentRuntimeDescriptor {
  /** Brand shown in toasts/logs/picker copy, e.g. 'Claude Code' | 'Codex'. */
  displayName: string;
  /** Trigger-variable name carrying the session id, e.g. 'claudeSessionId'. */
  sessionIdVar: string;
  /** Whether maiTerm implements session forking for this runtime (currently Claude only). */
  supportsFork: boolean;
  /** How long a reported tool may stay "active" before being treated as stale. */
  toolStaleTimeoutMs: number;
  /** Config-file hint shown in the prefs UI, e.g. '~/.claude.json' | '~/.codex/config.toml'. */
  configHint: string;
}
