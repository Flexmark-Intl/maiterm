// Agent-runtime core types (Stage 2 TS mirror of src-tauri/src/state/agent_runtime.rs).
//
// PURELY ADDITIVE: these types are imported nowhere yet. The runtime/state unions
// are byte-for-byte the existing Claude shapes — only Claude is wired in today.

/** Which agent runtime a tab / session belongs to. Defaults to 'claude'. */
export type AgentRuntime = 'claude' | 'codex' | 'gemini';

/** Per-session agent activity state (union UNCHANGED from the Claude shape). */
export type AgentState = 'active' | 'idle' | 'permission';

/** Aggregate agent state surfaced on a workspace dot. */
export type WorkspaceAgentState = 'permission' | 'active' | 'idle-unread' | 'idle-read';

/**
 * User-facing per-runtime descriptor. Mirrors the load-bearing fields of the Rust
 * RuntimeDescriptor that the frontend consumes. No instances are created here —
 * later stages build the descriptor table and wire it up.
 */
export interface AgentRuntimeDescriptor {
  /** Brand shown in toasts/logs/picker copy, e.g. 'Claude Code' | 'Codex'. */
  displayName: string;
  /** Trigger-variable name carrying the session id, e.g. 'claudeSessionId'. */
  sessionIdVar: string;
  /** Whether the runtime supports forking a session (claude:true, codex:false — LOCKED). */
  supportsFork: boolean;
  /** How long a reported tool may stay "active" before being treated as stale. */
  toolStaleTimeoutMs: number;
  /** Config-file hint shown in the prefs UI, e.g. '~/.claude.json' | '~/.codex/config.toml'. */
  configHint: string;
}
