import type {
  OverlordCondition,
  OverlordGate,
  OverlordLedgerOutcome,
  OverlordRule,
  OverlordStep,
} from '$lib/tauri/types';

/**
 * Shared presentation vocabulary for the Overlord surfaces.
 *
 * The rules ARE the product surface (docs/overlord.md §5), and a rule the human can't
 * read at a glance is a rule they can't trust with the human's own authority. Every
 * screen renders the same phrasing from here — board, preferences editor, approval
 * prompt — so a rule never reads differently depending on where you meet it.
 */

// ── Conditions ────────────────────────────────────────────────────────────────

/** Full clause: "context reaches 55%". Reads after the word "When". */
export function describeCondition(when: OverlordCondition): string {
  switch (when.event) {
    case 'context_pct': return `context reaches ${when.at_or_above}%`;
    case 'turn_end': return 'a turn ends';
    case 'commit': return 'a commit lands';
    case 'tab_idle': return `a tab idles ${fmtMinutes(when.minutes)}`;
    case 'task_stale': return `a task goes stale ${when.days}d`;
    case 'agent_unready': return 'an agent is running but unbound';
    case 'no_todo_list': return 'real work is not on the task list';
    case 'permission_pending': return `a permission waits ${fmtMinutes(when.minutes)}`;
    case 'directive_unacked': return `a directive goes unacked ${fmtMinutes(when.minutes)}`;
  }
}

/** Two-word chip label for dense rows. */
export function conditionChip(when: OverlordCondition): string {
  switch (when.event) {
    case 'context_pct': return `${when.at_or_above}% CTX`;
    case 'turn_end': return 'TURN END';
    case 'commit': return 'COMMIT';
    case 'tab_idle': return `IDLE ${when.minutes}M`;
    case 'task_stale': return `STALE ${when.days}D`;
    case 'agent_unready': return 'UNREADY';
    case 'no_todo_list': return 'UNTRACKED';
    case 'permission_pending': return `PERM ${when.minutes}M`;
    case 'directive_unacked': return `UNACKED ${when.minutes}M`;
  }
}

/** The signal each condition reads — shown as a hint so a rule's cost is legible. */
export function conditionSource(when: OverlordCondition): string {
  switch (when.event) {
    case 'context_pct':
    case 'tab_idle': return 'transcript tail · cached';
    case 'turn_end':
    case 'permission_pending': return 'agent hooks';
    case 'commit': return 'git commit tool calls · Claude only';
    case 'no_todo_list': return 'maiTerm tasks · every runtime';
    case 'task_stale': return 'board timers';
    case 'agent_unready': return 'agent state + liveness probe';
    case 'directive_unacked': return 'directive ledger';
  }
}

/** What kind of thing a deck escalation card is. Every card used to be chipped
 *  "escalation", which made a tab that hit a wall look identical to one that asked for a
 *  decision and to a step that timed out — three different next moves under one word. */
export function escalationLabel(kind: string): string {
  switch (kind) {
    case 'blocked': return 'blocked';
    case 'step_timeout': return 'timed out';
    case 'directive_unacked': return 'unacked';
    case 'agent_report': return 'from agent';
    default: return 'escalation';
  }
}

// ── Steps & gates ─────────────────────────────────────────────────────────────

export function describeGate(gate: OverlordGate | null | undefined): string {
  if (!gate) return 'then move on';
  switch (gate.until) {
    case 'turn_end': return 'wait for the turn to finish';
    case 'ack': return 'wait for an ack';
    case 'context_below': return `wait until context drops under ${gate.pct}%`;
    case 'idle_ms': return `wait for ${Math.round(gate.ms / 1000)}s of quiet`;
  }
}

export function gateChip(gate: OverlordGate | null | undefined): string {
  if (!gate) return 'NO WAIT';
  switch (gate.until) {
    case 'turn_end': return 'TURN';
    case 'ack': return 'ACK';
    case 'context_below': return `< ${gate.pct}%`;
    case 'idle_ms': return `${Math.round(gate.ms / 1000)}s QUIET`;
  }
}

/** One-line summary of what a rule actually does, for collapsed rows. */
export function sequenceSummary(sequence: OverlordStep[]): string {
  if (!sequence.length) return 'no steps';
  const slash = sequence.filter((s) => s.kind === 'slash');
  const talk = sequence.filter((s) => s.kind === 'process');
  const parts: string[] = [];
  if (talk.length) parts.push(`${talk.length} directive${talk.length === 1 ? '' : 's'}`);
  if (slash.length) parts.push(slash.map((s) => s.text).join(' '));
  return parts.join(' + ');
}

export function scopeLabel(rule: OverlordRule, nameOf: (id: string) => string): string {
  if (!rule.workspaces.length) return 'every workspace';
  if (rule.workspaces.length === 1) return nameOf(rule.workspaces[0]);
  return `${rule.workspaces.length} workspaces`;
}

/**
 * Repair the guards a condition would otherwise contradict.
 *
 * Three conditions describe a state the DEFAULT guards exclude by definition, so a rule
 * carrying both is born dead. The human editor applied this coupling and the MCP path did
 * not — `AGENT_RULE_GUARDS` handled only `agent_unready` — so an agent-proposed
 * `permission_pending` or `directive_unacked` rule arrived unfireable, and the approval
 * modal cheerfully asked the human to approve it.
 *
 * This is the repair; `ruleWarnings` below is the same knowledge stated as a diagnosis.
 * They must agree — every warning here has a matching clause there.
 */
export function guardsForCondition(
  guards: OverlordRule['guards'],
  event: OverlordCondition['event'],
): OverlordRule['guards'] {
  // The condition MEANS no agent is running; requiring a live REPL makes it unfireable.
  if (event === 'agent_unready') return { ...guards, require_live_repl: false };
  // The condition needs the agent stopped at a prompt, which `idle` excludes.
  if (event === 'permission_pending') {
    const st = guards.agent_state ?? ['idle'];
    return st.includes('permission') ? guards : { ...guards, agent_state: [...st, 'permission'] };
  }
  // The condition needs an outstanding directive; "serialize" requires there be none.
  if (event === 'directive_unacked') return { ...guards, only_if_no_outstanding: false };
  return guards;
}

/** Guard combinations that make a condition unreachable — surfaced inline so a rule
 *  can never be silently dead (the engine inverts agent_state for agent_unready). */
export function ruleWarnings(rule: OverlordRule): string[] {
  const out: string[] = [];
  const states = rule.guards.agent_state ?? ['idle'];
  if (rule.when.event === 'permission_pending' && !states.includes('permission')) {
    out.push('This never fires: the condition needs the agent in “permission”, but the guard excludes it.');
  }
  if (rule.when.event === 'directive_unacked' && rule.guards.only_if_no_outstanding) {
    out.push('This never fires: the condition needs an outstanding directive, but “serialize” requires none.');
  }
  if (rule.when.event === 'agent_unready' && rule.guards.require_live_repl) {
    out.push('This never fires: the condition means no agent is running, but a live REPL is required.');
  }
  if (!rule.sequence.length || rule.sequence.every((s) => !s.text.trim())) {
    out.push('No directive text — nothing would be sent.');
  }
  if (rule.when.event === 'commit' || rule.when.event === 'no_todo_list') {
    const nonClaude = rule.sequence.some((s) => s.runtimes?.some((r) => r !== 'claude'));
    if (nonClaude) out.push('This signal is only detected for Claude tabs today.');
  }
  return out;
}

// ── Ledger ────────────────────────────────────────────────────────────────────

export function outcomeTone(outcome: OverlordLedgerOutcome): 'good' | 'warn' | 'bad' | 'muted' {
  switch (outcome) {
    case 'sent':
    case 'acked': return 'good';
    case 'timed_out':
    case 'aborted':
    case 'proposed': return 'warn';
    case 'blocked_no_repl':
    case 'blocked_guard': return 'bad';
    case 'skipped_runtime': return 'muted';
  }
}

export function outcomeLabel(outcome: OverlordLedgerOutcome): string {
  switch (outcome) {
    case 'sent': return 'sent';
    case 'acked': return 'acked';
    case 'timed_out': return 'timed out';
    case 'aborted': return 'aborted';
    case 'proposed': return 'proposed';
    case 'blocked_no_repl': return 'no live repl';
    case 'blocked_guard': return 'guard blocked';
    case 'skipped_runtime': return 'skipped';
  }
}

// ── Time ──────────────────────────────────────────────────────────────────────

function fmtMinutes(m: number): string {
  if (m < 60) return `${m}m`;
  const h = Math.floor(m / 60);
  const rem = m % 60;
  return rem ? `${h}h${rem}m` : `${h}h`;
}

/** Compact age: 2d, 4h, 12m, now. */
export function fmtAge(at: number | string | undefined | null): string {
  if (at === undefined || at === null) return '—';
  const ms = Date.now() - (typeof at === 'number' ? at : Date.parse(at));
  if (!Number.isFinite(ms) || ms < 0) return '—';
  const d = Math.floor(ms / 86_400_000);
  if (d > 0) return `${d}d`;
  const h = Math.floor(ms / 3_600_000);
  if (h > 0) return `${h}h`;
  const m = Math.floor(ms / 60_000);
  if (m > 0) return `${m}m`;
  return 'now';
}

/** Cooldown in human units for the rules editor. */
export function fmtSeconds(s: number): string {
  if (s <= 0) return 'none';
  if (s < 60) return `${s}s`;
  if (s < 3600) return `${Math.round(s / 60)}m`;
  return `${+(s / 3600).toFixed(1)}h`;
}
