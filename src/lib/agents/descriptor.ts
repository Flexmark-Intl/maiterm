import type { AgentRuntime } from './types';

/** Probe common tool-input keys for a short detail string (runtime-neutral fallback). */
function commonDetail(toolInput: Record<string, unknown> | null): string | undefined {
  if (!toolInput) return undefined;
  const cmd = toolInput.command as string | undefined;
  if (cmd) return cmd.length > 50 ? cmd.slice(0, 47) + '...' : cmd;
  const fp = (toolInput.file_path ?? toolInput.path) as string | undefined;
  if (fp) return fp.split('/').pop() || fp;
  const pat = (toolInput.pattern ?? toolInput.query) as string | undefined;
  if (pat) return pat;
  return undefined;
}

/** Frontend per-runtime descriptor: user-facing identity + the bits the agent-state
 *  store needs that differ by runtime (tool-detail summarizer, brand, stale timeout). */
export interface RuntimeDescriptorTs {
  runtime: AgentRuntime;
  /** User-facing brand for toasts/logs. */
  displayName: string;
  /** Trigger-variable name holding the session id. */
  sessionIdVar: string;
  supportsFork: boolean;
  /** How long a reported tool may stay "active" before the indicator is auto-cleared. */
  toolStaleTimeoutMs: number;
  /** Where this runtime's MCP config lives (shown in the Preferences hints). */
  configHint: string;
  /** Build a short detail string from a tool invocation, or undefined. */
  summarizeTool(toolName: string, toolInput: Record<string, unknown> | null): string | undefined;
}

const claude: RuntimeDescriptorTs = {
  runtime: 'claude',
  displayName: 'Claude Code',
  sessionIdVar: 'claudeSessionId',
  supportsFork: true,
  toolStaleTimeoutMs: 15_000,
  configHint: '~/.claude.json',
  // The original Claude tool summarizer, verbatim.
  summarizeTool(toolName, toolInput) {
    if (!toolInput) return undefined;
    switch (toolName) {
      case 'Bash': {
        const cmd = toolInput.command as string | undefined;
        if (!cmd) return undefined;
        return cmd.length > 50 ? cmd.slice(0, 47) + '...' : cmd;
      }
      case 'Edit':
      case 'Write':
      case 'Read': {
        const fp = toolInput.file_path as string | undefined;
        if (!fp) return undefined;
        return fp.split('/').pop() || fp;
      }
      case 'Glob':
      case 'Grep':
        return toolInput.pattern as string | undefined;
      case 'Agent':
        return toolInput.description as string | undefined;
      case 'WebFetch':
      case 'WebSearch':
        return (toolInput.query ?? toolInput.url) as string | undefined;
      default:
        return undefined;
    }
  },
};

const codex: RuntimeDescriptorTs = {
  runtime: 'codex',
  displayName: 'Codex',
  sessionIdVar: 'codexSessionId',
  supportsFork: false,
  toolStaleTimeoutMs: 15_000,
  configHint: '~/.codex/config.toml',
  // Codex tools: shell/Bash (command), apply_patch/Edit/Write (file), plus MCP tools.
  summarizeTool(toolName, toolInput) {
    if (!toolInput) return undefined;
    switch (toolName) {
      case 'apply_patch':
      case 'Edit':
      case 'Write': {
        const fp = (toolInput.file_path ?? toolInput.path) as string | undefined;
        if (fp) return fp.split('/').pop() || fp;
        return commonDetail(toolInput);
      }
      default:
        return commonDetail(toolInput);
    }
  },
};

const gemini: RuntimeDescriptorTs = {
  ...codex,
  runtime: 'gemini',
  displayName: 'Gemini',
  sessionIdVar: 'geminiSessionId',
  configHint: '~/.gemini/settings.json',
};

/**
 * Every runtime's session-id trigger variable. These identify ONE live/resumable session, so
 * they must never be copied to another tab — a duplicate that inherits its source's session id
 * renders the source's conversation in maiLink and auto-resumes `--resume <same sid>` into the
 * same transcript (two agents appending to one session).
 */
export const SESSION_ID_VARS: ReadonlySet<string> = new Set(
  [claude, codex, gemini].map((d) => d.sessionIdVar),
);

/** Resolve the frontend descriptor for a runtime (defaults to Claude). */
export function getDescriptor(runtime: AgentRuntime): RuntimeDescriptorTs {
  switch (runtime) {
    case 'codex':
      return codex;
    case 'gemini':
      return gemini;
    default:
      return claude;
  }
}
