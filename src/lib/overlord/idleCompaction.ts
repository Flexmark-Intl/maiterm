import type { AgentRuntime, AgentState } from '$lib/agents/types';
import type { OverlordRule } from '$lib/tauri/types';

/**
 * When Claude's idle compaction may be replaced by the checkpoint ritual (docs/overlord.md §7).
 *
 * Claude reports its idle compaction and its threshold compaction with one `auto` trigger, and
 * blocking the threshold one would leave a full conversation with nowhere to go. So everything
 * here has to say this is the idle one, and anything unknown refuses: a refusal costs only what
 * Claude was about to do anyway.
 */

/** Claude fires at 54 minutes of a one hour cache, so a younger session is not its idle case. */
export const IDLE_COMPACTION_MIN_AGE_MS = 50 * 60_000;
/** Claude won't idle-compact within this long of its human acting, so input here means this
 *  compaction is their prompt's. */
export const IDLE_COMPACTION_HUMAN_MS = 60_000;

/** Whether this rule ends in a `/compact` this runtime runs. Without one, blocking would leave
 *  the session uncompacted while its cache runs out, the one outcome worse than doing nothing. */
export function ruleCompacts(rule: OverlordRule, runtime: AgentRuntime): boolean {
  return rule.sequence.some(
    (s) => s.kind === 'slash' && s.text.trim().startsWith('/compact') && (!s.runtimes || s.runtimes.includes(runtime)),
  );
}

export interface IdleCompactionFacts {
  now: number;
  /** The engine is running in this window, on a tab it supervises. */
  supervised: boolean;
  /** A proposal waits for a click, and nobody is there; the cache ends in about six minutes. */
  proposeMode: boolean;
  rule: OverlordRule | null;
  runtime: AgentRuntime;
  state: AgentState | undefined;
  /** A ritual or a directive already owns this tab. */
  occupied: boolean;
  lastTurnTs: number | undefined;
  lastHumanInputAt: number | undefined;
}

/** Why this compaction goes ahead as Claude meant it, or null when the ritual takes over. */
export function idleCompactionRefusal(f: IdleCompactionFacts): string | null {
  if (!f.supervised) return 'not supervised here';
  if (f.proposeMode) return 'propose mode';
  if (!f.rule) return 'no checkpoint rule';
  if (!ruleCompacts(f.rule, f.runtime)) return 'the rule does not compact';
  if (f.state !== 'idle') return 'the agent is not idle';
  if (f.occupied) return 'the tab is already being driven';
  if (f.lastTurnTs === undefined) return 'no last turn on record';
  if (f.now - f.lastTurnTs < IDLE_COMPACTION_MIN_AGE_MS) return 'the last turn is too recent';
  if (f.lastHumanInputAt !== undefined && f.now - f.lastHumanInputAt < IDLE_COMPACTION_HUMAN_MS) {
    return 'the human just used the tab';
  }
  return null;
}
