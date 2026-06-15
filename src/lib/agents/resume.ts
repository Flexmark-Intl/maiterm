import type { AgentRuntime } from './types';

/** Trigger-variable name holding the session id for a runtime. */
export function sessionIdVar(runtime: AgentRuntime): string {
  switch (runtime) {
    case 'codex': return 'codexSessionId';
    case 'gemini': return 'geminiSessionId';
    default: return 'claudeSessionId';
  }
}

/** The launch flag that forks a session, or null if the runtime can't fork. */
export function forkFlag(runtime: AgentRuntime): string | null {
  return runtime === 'claude' ? '--fork-session' : null;
}

/** True if `cmd` is a fork-spawn command for this runtime (must never be reused as a resume command). */
export function isForkCommand(runtime: AgentRuntime, cmd: string | null | undefined): boolean {
  const flag = forkFlag(runtime);
  return !!flag && !!cmd && cmd.includes(flag);
}

/** The default auto-resume command template for a runtime (uses the %<runtime>SessionId trigger var). */
export function getResumeCommand(runtime: AgentRuntime): string {
  switch (runtime) {
    case 'codex': return 'codex resume %codexSessionId';
    case 'gemini': return 'gemini --resume %geminiSessionId';
    default: return 'claude --resume %claudeSessionId';
  }
}
