import type { AgentRuntime } from './types';

/** Trigger-variable name holding the session id for a runtime. */
export function sessionIdVar(runtime: AgentRuntime): string {
  switch (runtime) {
    case 'codex': return 'codexSessionId';
    case 'gemini': return 'geminiSessionId';
    default: return 'claudeSessionId';
  }
}

/**
 * How a runtime forks a session, since they do not agree on the shape.
 *
 * Claude appends `--fork-session` to a resume, so a fork command is a resume command plus a
 * flag. Codex has a distinct `codex fork SESSION_ID` subcommand, so a fork command REPLACES the
 * resume verb. Modelling only the flag is why Codex forking was inert everywhere the flag was
 * the test (review C6).
 */
interface ForkSpec {
  /** Does `cmd` already fork? Such a command must never be reused as a resume command: it
   *  would re-fork the ORIGINAL session on every restore, losing this tab's own conversation
   *  and pinning the wrong id. */
  isFork(cmd: string): boolean;
  /** Rewrite a resume command into one that forks the same session, or null when it doesn't
   *  apply to this command. */
  toFork(cmd: string): string | null;
  /** Spawn command that forks `sessionId` into a fresh session. */
  build(sessionId: string): string;
}

const CODEX_FORK_RE = /(^|\s)codex\s+fork(\s|$)/;
const CODEX_RESUME_RE = /(^|\s)codex\s+resume(\s|$)/;

const FORK_SPECS: Partial<Record<AgentRuntime, ForkSpec>> = {
  claude: {
    isFork: (cmd) => cmd.includes('--fork-session'),
    toFork: (cmd) => `${cmd} --fork-session`,
    build: (sessionId) => `claude --resume ${sessionId} --fork-session`,
  },
  codex: {
    isFork: (cmd) => CODEX_FORK_RE.test(cmd),
    // Swap the verb rather than appending: `codex resume <id> --fork` is not a thing.
    toFork: (cmd) => (CODEX_RESUME_RE.test(cmd) ? cmd.replace(/(^|\s)codex\s+resume(\s|$)/, '$1codex fork$2') : null),
    build: (sessionId) => `codex fork ${sessionId}`,
  },
};

/** Whether maiTerm can fork a session for this runtime at all. */
export function supportsFork(runtime: AgentRuntime): boolean {
  return !!FORK_SPECS[runtime];
}

/** The spawn command that forks `sessionId`, or null if the runtime has no fork. */
export function buildForkCommand(runtime: AgentRuntime, sessionId: string): string | null {
  return FORK_SPECS[runtime]?.build(sessionId) ?? null;
}

/** Rewrite a resume command so it forks instead, or null when that isn't possible. */
export function toForkCommand(runtime: AgentRuntime, cmd: string): string | null {
  const spec = FORK_SPECS[runtime];
  if (!spec || spec.isFork(cmd)) return null;
  return spec.toFork(cmd);
}

/** True if `cmd` is a fork-spawn command for this runtime (must never be reused as a resume command). */
export function isForkCommand(runtime: AgentRuntime, cmd: string | null | undefined): boolean {
  const spec = FORK_SPECS[runtime];
  return !!spec && !!cmd && spec.isFork(cmd);
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
