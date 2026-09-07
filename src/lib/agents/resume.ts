import type { AgentRuntime } from './types';

/** Trigger-variable name holding the session id for a runtime. */
export function sessionIdVar(runtime: AgentRuntime): string {
  switch (runtime) {
    case 'codex': return 'codexSessionId';
    case 'gemini': return 'geminiSessionId';
    default: return 'claudeSessionId';
  }
}

/** The implemented appendable fork flag. Codex uses a subcommand, not this flag model. */
export function forkFlag(runtime: AgentRuntime): string | null {
  return runtime === 'claude' ? '--fork-session' : null;
}

/** True if `cmd` is a fork-spawn command for this runtime (must never be reused as a resume command). */
export function isForkCommand(runtime: AgentRuntime, cmd: string | null | undefined): boolean {
  const flag = forkFlag(runtime);
  return !!flag && !!cmd && cmd.includes(flag);
}

/** Options that change the shape of a runtime's launch command. */
export interface ResumeCommandOptions {
  /** Codex only: add `--dangerously-bypass-hook-trust` (the `codex_hooks_bypass_trust`
   *  preference). Codex refuses to run a hook definition it has not been shown and trusted, and
   *  maiTerm's hook command changes whenever the auth token does, so a user who accepts that
   *  risk can skip the prompt. Applied ONLY to a launch maiTerm builds itself — it can never
   *  affect a codex the user starts by hand. */
  bypassHookTrust?: boolean;
}

/** The default auto-resume command template for a runtime (uses the %<runtime>SessionId trigger var). */
export function getResumeCommand(runtime: AgentRuntime, opts?: ResumeCommandOptions): string {
  switch (runtime) {
    case 'codex':
      return opts?.bypassHookTrust
        ? 'codex resume --dangerously-bypass-hook-trust %codexSessionId'
        : 'codex resume %codexSessionId';
    case 'gemini': return 'gemini --resume %geminiSessionId';
    default: return 'claude --resume %claudeSessionId';
  }
}
