import * as commands from '$lib/tauri/commands';
import type { OverlordTabFacts } from '$lib/tauri/commands';
import type {
  OverlordCondition,
  OverlordGuards,
  OverlordLedgerEntry,
  OverlordLedgerOutcome,
  OverlordRule,
  OverlordStep,
  Task,
  TaskStatus,
  Tab,
  Workspace,
} from '$lib/tauri/types';
import type { AgentState } from '$lib/agents/types';
import { workspacesStore, tabDisplayName, navigateToTab } from '$lib/stores/workspaces.svelte';
import { resumePane } from '$lib/stores/resumeGate.svelte';
import { terminalsStore } from '$lib/stores/terminals.svelte';
import { claudeStateStore, resumeCommandFor } from '$lib/stores/agentState.svelte';
import { preferencesStore } from '$lib/stores/preferences.svelte';
import { bracketedPasteSubmit } from '$lib/utils/agentPrompt';
import { dispatch } from '$lib/stores/notificationDispatch';
import { seedDefaultOverlordRules } from '$lib/overlord/defaults';
import { guardsForCondition } from '$lib/overlord/format';
import { getVariables, interpolateVariables, setVariable } from '$lib/stores/triggers.svelte';
import { tasksStore } from '$lib/stores/tasks.svelte';
import { findImportedDuplicate, isInFlight, isParked, isRetired, makeTask, normalizeTitle, statusFromAgent, type TaskRow } from '$lib/tasks/model';
import { error as logError, info as logInfo, warn as logWarn } from '@tauri-apps/plugin-log';

/**
 * Overlord-the-engine (docs/overlord.md §2) — the deterministic, headless, per-window
 * supervisor. A rules engine over cheap state signals: context gauges, turn boundaries,
 * commit/todo facts from the transcript tail, agent state from hooks. No model in the
 * loop; the pinned Overlord *agent* (S4) is a separate consumer that wakes only for
 * escalations and conversation.
 *
 * Authority model (§3): directives are injected RAW — indistinguishable from the human
 * typing — so every safety property is mechanical and lives here, at the injection
 * boundary: require_live_repl, only_if_no_outstanding, max_per_hour, and the verbatim
 * ledger. There is no envelope to fall back on.
 */

const TICK_MS = 5_000;
/** Max wait for a step's pre-injection window (state + quiet) before aborting the ritual. */
const INJECTABLE_WAIT_CAP_MS = 5 * 60_000;
/** A commit older than this at first observation never fires the commit event. */
const COMMIT_FRESH_MS = 15 * 60_000;
/** Quiet a terminal must have been before the agent may put it away. `stopped` means no AGENT
 *  process, which is also what a shell part-way through a build looks like — so recency of
 *  output and keystrokes is the discriminator, and the irreversible verb asks for more of it. */
const ARCHIVE_QUIET_MS = 60_000;
const CLOSE_QUIET_MS = 5 * 60_000;
/** no_todo_list only nags sessions with real work in them. */
const NO_TODO_MIN_CONTEXT_TOKENS = 30_000;
const NO_TODO_RECENT_TURN_MS = 30 * 60_000;
/** Done board rows are swept after this long (same hygiene as completed mesh topics). */
const TASK_DONE_RETENTION_MS = 48 * 60 * 60 * 1000;
const GATE_POLL_MS = 1_000;
/** A directive nobody acknowledged for this long is reported (§8's TTL sweep). Longer than
 *  a working turn, shorter than the drive watch that covers driveTab's own directives. */
const DIRECTIVE_UNACKED_MS = 10 * 60_000;
/**
 * Hard ceiling on a non-ritual directive, whatever the tab is doing.
 *
 * The idle clock (`lastActiveAt`) is what stops a working agent being reported — but it can
 * be pinned at zero indefinitely, and the tabs where that happens are the ones most likely
 * to be genuinely stuck. Two ways, both real:
 *
 * - The track-request's only practical clearing path is `f.last_turn_ts > od.sentAt`, and
 *   `overlord_tab_facts` returns no facts at all for a Codex/Gemini SSH tab or one whose
 *   bridge is down — while `scanWorkspaces` will still ask such a tab to start tracking.
 *   The directive can then never clear, and a working agent resets the idle clock forever.
 * - A tab whose `Stop` hook is lost (agent killed mid-turn, bridge dropped mid-turn) stays
 *   `active` permanently: the stale timer in `agentState` re-sets the same state rather than
 *   timing it out.
 *
 * Either way the outstanding slot is held for the life of the tab, blocking every
 * `only_if_no_outstanding` rule and refusing every `driveTab`. Suppressing on idleness alone
 * removed the only notice the operator ever got that this had happened.
 */
const DIRECTIVE_MAX_OUTSTANDING_MS = 30 * 60_000;
/** How long an agent-only escalation waits for an agent that no longer exists. */
const AGENT_ESCALATION_TTL_MS = 30 * 60_000;
/** turn_end fallback (see awaitGate): how long after injection, and how long the PTY must
 *  have been silent, before an idle tab counts as having finished a sub-poll turn. */
const TURN_FALLBACK_MIN_MS = 4_000;
const TURN_FALLBACK_QUIET_MS = 3_000;

/**
 * Triage "run all" pacing.
 *
 * Every action this fires ends with an agent starting a turn, so clearing a deck of forty
 * signals in one click means forty concurrent API streams against one org's rate limit.
 * Three separate brakes, because they bound different things:
 *
 *   STAGGER — one second between individual sends, so a wave is a ramp, not a spike.
 *   GAP     — a pause between waves, holding the sustained *start* rate down.
 *   HOLD    — the only one that bounds CONCURRENCY: after the gap, keep waiting while the
 *             fleet still has a wave's worth of rituals in flight. A gap alone just spreads
 *             the starts; turns run for minutes, so without this the waves stack. Capped so
 *             one wedged ritual can't strand the run forever.
 */
const TRIAGE_BATCH = 10;
const TRIAGE_STAGGER_MS = 1_000;
const TRIAGE_GAP_MS = 5_000;
const TRIAGE_HOLD_CAP_MS = 2 * 60_000;

export interface OutstandingDirective {
  id: string;
  ruleId: string | null;
  tabId: string;
  stepIndex: number;
  text: string;
  sentAt: number;
  /** 'ack' gates resolve via replyToOverlord; others resolve inside the ritual loop. */
  acked: boolean;
  /** Raised the unacked-TTL escalation once. The directive keeps sitting there, and
   *  re-raising every tick would bury the queue in one stuck tab. */
  unackedNotified?: boolean;
  /** Last tick this tab was seen `active` since the directive went out. The unacked TTL
   *  measures from `max(sentAt, lastActiveAt)`, so its clock only runs while the agent is
   *  NOT demonstrably working — see the tick's escalate site. */
  lastActiveAt?: number;
}

export interface OverlordProposal {
  id: string;
  ruleId: string;
  ruleName: string;
  tabId: string;
  tabName: string;
  workspaceId: string;
  workspaceName: string;
  /** First step's text — what the human is approving. */
  preview: string;
  stepCount: number;
  createdAt: number;
}

export interface OverlordEscalation {
  id: string;
  ts: number;
  tabId: string;
  workspaceId: string;
  ruleId: string | null;
  kind:
    | 'step_timeout'
    | 'blocked'
    | 'directive_unacked'
    | 'permission_stuck'
    | 'agent_report'
    /** A tab answered a directive Overlord typed. For the AGENT, not the human — the deck
     *  filters these out, since a normal answer is not a problem needing triage. */
    | 'drive_reply'
    /** The human handed a board task to the agent to carry ("Send" on the card). */
    | 'task_handoff'
    /** The human deleted a task and the owning tab could not be told directly — the agent
     *  relays it, so the tab stops believing in work that is off the board. */
    | 'task_dropped'
    /** A `/maiterm init` was typed at an unbound tab and the tab never bound. `recoverTab`
     *  can only report that it TYPED the line; whether the agent on the far side was in a
     *  state to receive it is knowable only afterwards, and only by watching. This carries
     *  that verdict back to whoever asked for the recovery. */
    | 'rebind_failed';
  detail: string;
  /** The board task this is about (`task_handoff`), so the card's "Sent" receipt can be
   *  withdrawn if the handoff is ever swept undelivered. */
  taskId?: string;
  /** Consumed by listEscalations (S4); stays visible on the board until dismissed. */
  read: boolean;
}

interface RitualRun {
  runId: string;
  ruleId: string;
  ruleName: string;
  tabId: string;
  stepIndex: number;
  stepCount: number;
  startedAt: number;
  aborted: boolean;
  /** True for an `agent_unready` rule. Its whole premise is that maiTerm has NO binding
   *  for the tab's agent, so the pre-injection window must invert: wait for the absence of
   *  an agent state, not the presence of one. Without this the ritual fires, spins the
   *  full 5-minute injectable cap, and wedges the tab's serialization slot the whole time. */
  targetsUnready: boolean;
  /** ms epoch of the ritual's start, then of each injection — the human-input abort
   *  baseline. Any keystroke in the tab newer than this aborts the ritual (§7),
   *  including during the wait BETWEEN steps. */
  lastInjectionAt: number;
}

/** One change in a proposeRuleChanges batch (docs/overlord.md §10). */
export interface OverlordRuleChange {
  op: 'create' | 'update' | 'rescope' | 'enable' | 'disable' | 'delete';
  rule?: Partial<OverlordRule>;
  rule_id?: string;
  patch?: Partial<OverlordRule>;
  workspaces?: string[];
}

export interface PendingRuleChangeBatch {
  id: string;
  tabId: string;
  rationale: string;
  changes: OverlordRuleChange[];
}

/** What one scan pass found. `silent` are running tabs with nothing on the task list —
 *  the candidates a census would ask, already filtered by the ask-cooldown. */
/** Why a tab under context pressure is or isn't being checkpointed — the deck's copy AND
 *  which button it offers, so the two can't drift apart. */
export type CheckpointState =
  | { kind: 'running'; step: number; steps: number }
  | { kind: 'proposed' }
  | { kind: 'no_rule' }
  | { kind: 'cooling'; minutes: number }
  | { kind: 'busy' }
  /** A card is showing, but this tab's own applicable rule fires higher up. The deck's
   *  threshold is one window-wide number (the lowest enabled `context_pct`, scope
   *  ignored); `checkpointRuleFor` respects workspace scope. So a workspace-scoped rule at
   *  40% anywhere in the window raised a card for a tab in a DIFFERENT workspace at 42%
   *  and told it a checkpoint was coming — while the rule that governs it needs 55%. */
  | { kind: 'below_rule'; at: number }
  | { kind: 'ready' };

/** An agent tab whose tracked work is finished and which has gone quiet — a candidate for
 *  archiving (recoverable) or closing (not). */
export interface SpentTab {
  tabId: string;
  name: string;
  done: number;
  parked: number;
  lastActivity?: number;
}

/** Live progress of a triage "run all" pass, for the deck's progress bar. */
export interface TriageRunProgress {
  total: number;
  done: number;
  sent: number;
  skipped: number;
  wave: number;
  waves: number;
  /** `waiting` is the pacing gap; `holding` is waiting on the fleet to drain;
   *  `verifying` is watching the re-bound tabs actually come back. */
  phase: 'running' | 'waiting' | 'holding' | 'verifying';
  /** Epoch ms the pacing gap ends — a countdown, so a paused deck isn't mistaken for a
   *  hung one. Null while running or holding (a hold has no predictable end). */
  resumeAt: number | null;
  /** What is being sent right now, for the progress line. */
  label: string | null;
}

/** What a "run all" pass actually achieved.
 *
 *  `sent` is delivery; `bound` is OUTCOME. They are different numbers and the gap between
 *  them is the interesting one: a `/maiterm init` typed at a tab whose agent is gone is
 *  delivered perfectly and achieves nothing. `silent` counts exactly those — re-binds that
 *  landed and were never answered. Zero after a cancelled run, which has no verdict. */
export interface TriageResult {
  sent: number;
  skipped: number;
  /** Re-bind targets that registered before the run gave up watching. */
  bound: number;
  /** Re-bind targets that never came back — each now a `stopped` card wanting a restart. */
  silent: number;
}

export interface ScanSummary {
  at: number;
  tabsSeen: number;
  /** Tabs with tasks in flight on the maiTerm list. */
  mirrored: number;
  /** How many tabs contributed an import from Claude Code's own task store this pass. */
  fromStore: number;
  adopted: number;
  /** Tabs that have tasks and have completed all of them. */
  finished: number;
  silent: string[];
  /** Set once an ask-to-track pass has run against this scan's silent set. */
  asked?: number;
}

/** Latest replyToOverlord report per tab, for the board. */
export interface AgentReport {
  tabId: string;
  kind: string;
  state: string;
  summary: string;
  task?: string;
  ts: number;
}

/** Persisted (per-tab trigger variable) marker that the Overlord agent has been primed
 *  with its doctrine — same restart-survival trick as MESH_ONBOARDED_VAR. */
const OVERLORD_PRIMED_VAR = 'overlordPrimed';
/** Bump when the doctrine's CONTRACT changes, not when its wording is tidied. The marker
 *  is persisted, so an already-primed agent is never re-primed at the same version — and
 *  an agent running last version's doctrine believes last version's rules. v2 added the
 *  promise that driven tabs' replies come back on their own. v5 added task handoffs
 *  (task_handoff / task_dropped), which the agent must recognise to act on. v6 added the tab
 *  lifecycle — archiveTab/closeTab/recoverTab/resumeWorkspace and the state vocabulary that
 *  says which to use; an agent on v5 believes it cannot put a finished session away. v7
 *  reverses what the rendered ruleset MEANS: an agent on v6 reads it as a playbook to
 *  improvise from and hand-drives sequences the engine is already running, which is the
 *  whole reason v7 exists — so this is a contract change however much it looks like wording.
 *  v8 adds exemption: an agent on v7 sees `overlordExempt` tabs in listWorkspaces with no
 *  rule about them and will try to drive one — refused, but it will then raise the refusal to
 *  the human, which is the opposite of what exempting the tab asked for. */
const DOCTRINE_VERSION = '8';

/** Escalation kinds addressed to the Overlord AGENT rather than the human. The deck hides
 *  these, so nobody will ever dismiss one — `consumeEscalations` therefore DELETES them on
 *  delivery instead of marking them read, or they accumulate for the life of the window. */
const AGENT_ONLY_ESCALATIONS = new Set<OverlordEscalation['kind']>([
  'drive_reply',
  'permission_stuck',
  'task_handoff',
  'task_dropped',
  // The human already has this one: the tab reclassifies to `stopped` in the same pass, so
  // the deck raises its own re-bind card. This copy is addressed to the agent, which asked
  // for the recovery and is the only party holding the belief that it worked.
  'rebind_failed',
]);

/** Escalations that carry a HUMAN's board action to the agent rather than the engine's own
 *  judgement. They outlive exemption of the tab they name (see `sweepClosedTabs`). */
const HUMAN_BOARD_ESCALATIONS = new Set<OverlordEscalation['kind']>(['task_handoff', 'task_dropped']);

/**
 * Kinds where a second card for the same tab is the SAME fact restated, so `escalate`
 * refreshes the open one instead of appending. One chatty agent otherwise stacks a card per
 * report, and they linger: a human-addressed escalation is marked read by the agent's pull,
 * never deleted, so nothing disposes of the duplicates.
 *
 * **Deliberately just `blocked`, and the reasoning for every exclusion matters more than
 * the inclusion** — a wider net was proposed and would have been a worse bug than the one it
 * fixed:
 *
 * - `agent_report` is a distinct ASK each time. "A or B?" and "may I delete this?" are two
 *   questions from one tab, and collapsing them means the human answers the second and never
 *   sees the first.
 * - `step_timeout` is a distinct EVENT — this rule, this step, this moment. Two rules
 *   timing out on one tab are two facts, and the ritual has already aborted for each.
 * - `directive_unacked` fires once PER DIRECTIVE — `unackedNotified` is latched on the
 *   `OutstandingDirective`, not on the tab, so `clearOutstanding` followed by a new directive
 *   can raise a second card while the first is still up. That is correct: two directives are
 *   two facts with different text, and collapsing them would hide one.
 * - Everything in AGENT_ONLY_ESCALATIONS is DELETED on delivery rather than marked read, so
 *   it does not accumulate on the board at all. `permission_stuck` in particular must keep
 *   stacking — `permissionHandoff` is a list per tab precisely because one open gate is
 *   escalated again each time a `permission_pending` rule re-fires, and withdrawal has to
 *   retract all of them. `task_handoff`/`task_dropped` are per-TASK, and `sendTaskToOverlord`
 *   raises the former with `tabId: ''` for an unassigned task — so (tabId, kind) would
 *   collapse every unassigned handoff in the window onto one card.
 */
const DEDUPED_ESCALATIONS = new Set<OverlordEscalation['kind']>(['blocked']);

/** `replyToOverlord` states that mean a tab is no longer blocked, so its `blocked` card can
 *  be withdrawn. The other side of the declared enum, spelled out rather than inferred with
 *  `!== 'blocked'` — see the call site for why the difference is load-bearing. */
const RECOVERED_STATES = new Set(['working', 'done', 'idle']);

/** Guards an agent-created rule gets, whatever it asked for — the field-tier rule (§10):
 *  guards are human-only, unreachable from the MCP surface. Exported so the approval modal
 *  can warn about the rule the human would ACTUALLY get, not the one the agent asked for. */
export const AGENT_RULE_GUARDS = {
  agent_state: ['idle'] as ('idle' | 'active' | 'permission')[],
  min_quiet_ms: 3000,
  require_live_repl: true,
  max_per_hour: 1,
  only_if_no_outstanding: true,
};

/** Stable identity for a proposed change, for the don't-re-pitch-rejections set. */
function changeKey(c: OverlordRuleChange): string {
  return JSON.stringify([c.op, c.rule_id ?? c.rule?.name ?? '', c.patch ?? c.workspaces ?? c.rule?.when ?? null]);
}

function describeChange(c: OverlordRuleChange): string {
  switch (c.op) {
    case 'create': return `create "${c.rule?.name ?? 'unnamed'}"`;
    case 'update': return `update ${c.rule_id}`;
    case 'rescope': return `rescope ${c.rule_id} → ${c.workspaces?.length ? `${c.workspaces.length} workspace(s)` : 'global'}`;
    default: return `${c.op} ${c.rule_id}`;
  }
}

function matchRule(rules: OverlordRule[], key: string | undefined): OverlordRule | undefined {
  if (!key) return undefined;
  return rules.find((r) => r.id === key || r.default_id === key);
}

/**
 * Why a change cannot be applied, or null if it can.
 *
 * This exists because the answer used to be discarded. `applyRuleChange` returned the list
 * UNCHANGED on every one of these, and the caller counted that as success — so a `create`
 * missing `when` or `sequence` was ledgered "rule change approved", reported approved to the
 * agent, shown as approved to the human, and produced no rule. Nothing anywhere said no.
 *
 * Checked at PROPOSE time as well as apply time, so an agent gets a correctable error
 * instead of a false success, and the human is never asked to approve something unbuildable.
 */
function changeProblem(c: OverlordRuleChange, rules: OverlordRule[]): string | null {
  if (c.op === 'create') {
    if (!c.rule) return 'op "create" needs a `rule` object.';
    const missing: string[] = [];
    if (!c.rule.name) missing.push('name');
    if (!c.rule.when?.event) missing.push('when.event');
    if (!c.rule.sequence?.length) missing.push('sequence (at least one step)');
    if (missing.length) return `op "create" is missing: ${missing.join(', ')}.`;
    const bad = c.rule.sequence!.findIndex((s) => !s?.text || (s.kind !== 'process' && s.kind !== 'slash'));
    return bad === -1 ? null : `op "create": sequence[${bad}] needs a "kind" of "process" or "slash" and a non-empty "text".`;
  }
  if (!c.rule_id) return `op "${c.op}" needs rule_id (a rule id or a default_id).`;
  if (c.op === 'update' && !c.patch) return 'op "update" needs a `patch` object.';
  // Checked against the ruleset as it stands. A change naming a rule that isn't there was
  // the other silent no-op: matchRule found nothing and the list came back untouched.
  return matchRule(rules, c.rule_id)
    ? null
    : `op "${c.op}": no rule matches rule_id ${JSON.stringify(c.rule_id)}.`;
}

/** Apply one approved change, or null if it could not be applied. Enforces the field tiers
 *  mechanically: guards are never taken from the agent (created rules get AGENT_RULE_GUARDS;
 *  update patches have any guards stripped), and applied edits set user_modified so seeding
 *  won't overwrite. Returning null rather than the unchanged list is what lets the caller
 *  tell "applied" from "silently did nothing" — see `changeProblem`. */
function applyRuleChange(rules: OverlordRule[], c: OverlordRuleChange): OverlordRule[] | null {
  switch (c.op) {
    case 'create': {
      if (!c.rule?.name || !c.rule.when || !c.rule.sequence?.length) return null;
      const rule: OverlordRule = {
        id: crypto.randomUUID(),
        name: c.rule.name,
        description: c.rule.description ?? null,
        enabled: c.rule.enabled ?? true,
        workspaces: c.rule.workspaces ?? [],
        cooldown: c.rule.cooldown ?? 1800,
        origin: 'proposed',
        user_modified: true,
        when: c.rule.when,
        // Guards are chosen here, never taken from the agent (§10 field tiers) — but the
        // default set contradicts three conditions outright, so the coupled guard is
        // repaired the same way the human editor repairs it. This handled `agent_unready`
        // alone, which meant an agent-proposed `permission_pending` or `directive_unacked`
        // rule arrived permanently unfireable and the approval modal asked the human to
        // approve it anyway.
        guards: guardsForCondition(AGENT_RULE_GUARDS, c.rule.when.event),
        sequence: c.rule.sequence,
        supersedes: c.rule.supersedes,
      };
      return [rule, ...rules];
    }
    case 'update': {
      const target = matchRule(rules, c.rule_id);
      if (!target || !c.patch) return null;
      const { guards: _guards, id: _id, default_id: _did, ...patch } = c.patch;
      // Guards never come from the agent — but changing the CONDITION can contradict the
      // guards already on the rule, which would leave an existing, working rule dead after
      // an approved edit. Repair the coupled guard here, exactly as create does.
      const guards = patch.when ? guardsForCondition(target.guards, patch.when.event) : target.guards;
      return rules.map((r) => (r.id === target.id ? { ...r, ...patch, guards, user_modified: true } : r));
    }
    case 'rescope': {
      const target = matchRule(rules, c.rule_id);
      if (!target) return null;
      return rules.map((r) => (r.id === target.id ? { ...r, workspaces: c.workspaces ?? [], user_modified: true } : r));
    }
    case 'enable':
    case 'disable': {
      const target = matchRule(rules, c.rule_id);
      if (!target) return null;
      return rules.map((r) => (r.id === target.id ? { ...r, enabled: c.op === 'enable' } : r));
    }
    case 'delete': {
      const target = matchRule(rules, c.rule_id);
      if (!target) return null;
      return rules.filter((r) => r.id !== target.id);
    }
  }
}

function createOverlordStore() {
  // ── Reactive surfaces (board + gauges + queues) ─────────────────────────────
  let facts = $state<Map<string, OverlordTabFacts>>(new Map());
  let proposals = $state<OverlordProposal[]>([]);
  let escalations = $state<OverlordEscalation[]>([]);
  let recentLedger = $state<OverlordLedgerEntry[]>([]);
  let running = $state(false);
  let agentReports = $state<Map<string, AgentReport>>(new Map());
  let lastScan = $state<ScanSummary | null>(null);
  let scanning = $state(false);
  let pendingRuleChanges = $state<PendingRuleChangeBatch | null>(null);
  let triageRun = $state<TriageRunProgress | null>(null);
  let triageCancelled = false;
  // Resolver for the MCP proposeRuleChanges round trip (the modal answers it).
  let ruleChangeResolver:
    | ((res: { approved: string[]; rejected: string[]; failed?: string[]; pending?: boolean }) => void)
    | null = null;
  // Rituals and outstanding directives live in plain Maps (engine-internal, mutated from
  // async loops). This counter is the reactivity bridge for the board — same bump()
  // pattern as agentMesh. Every mutation of those maps calls bumpLive().
  let liveVersion = $state(0);
  // What Overlord already pitched and the human rejected — refuse re-pitches this session.
  const rejectedChangeKeys = new Set<string>();

  // ── Engine bookkeeping (non-reactive) ───────────────────────────────────────
  const prevAgentState = new Map<string, AgentState | undefined>();
  const prevCommitTs = new Map<string, number | undefined>();

  /**
   * Edges waiting to be consumed, per tab.
   *
   * An edge is a MOMENT — a turn ended, a commit landed — but every guard is about a
   * WINDOW: the agent idle, the PTY quiet for 3s, no ritual running, no outstanding
   * directive. The two rarely coincide on the same 5s tick, and the edge used to be
   * recomputed from a single-tick state delta and then dropped, so it was thrown away
   * exactly when the guards were most likely to reject it:
   *
   *   - `commit` is recorded when the git `tool_use` block is emitted, i.e. MID-TURN,
   *     when the agent is by definition not idle and the PTY is not quiet. So
   *     `review_after_commit` — a DEFAULT rule — could effectively never fire.
   *   - a `turn_end` inside the quiet window (any turn ending in the 3s before a tick)
   *     was lost permanently.
   *
   * Latching fixes both: the edge is re-offered on every tick until a rule consumes it,
   * the human overtakes it, or it ages past EDGE_LATCH_MS. The rule still runs no earlier
   * than its guards allow — it just no longer misses its one chance.
   *
   * What latching does NOT change: `consumeEdge` spends the edge for the event the winning
   * rule matched, so a SECOND rule on the same event and tab is still starved by the
   * one-fire-per-tab `break`. That is deliberate — directives serialize per tab anyway —
   * and unchanged from before.
   */
  const pendingEdges = new Map<string, { turnEnded?: number; committed?: number }>();
  /** How long a latched edge stays offerable. Long enough to outlast a working turn and
   *  a cooldown wait; short enough that a rule enabled tomorrow doesn't fire on today's
   *  commit. */
  const EDGE_LATCH_MS = 5 * 60_000;
  const permissionSince = new Map<string, number>();
  const lastFiredAt = new Map<string, number>(); // `${ruleId}|${tabId}` → ms

  /**
   * Tabs Overlord has typed a directive into and is waiting to hear back from.
   *
   * Overlord injects raw text with no envelope (§3), so the agent on the other end answers
   * in its own terminal exactly as it would answer the human — it has no reason to call
   * `replyToOverlord` unless the directive asked it to. `replyToOverlord` is voluntary;
   * this is not. Without it, "give me more info" gets answered into a void: the agent does
   * the work, writes the reply, and nothing carries it back.
   *
   * `baseline` is the tab's `last_turn_ts` at send time, NOT the wall clock — an SSH tab's
   * transcript is written on the remote host, so the two clocks are not comparable.
   */
  interface DriveWatch {
    baseline: number;
    sentAt: number;
    text: string;
    /** Raised the permission escalation once — the prompt sits until a human answers, and
     *  re-escalating every tick would bury the supervisor in its own alarm. */
    permissionNotified?: boolean;
    /** How many times a reply read was actually ATTEMPTED, and how many of those threw.
     *  Without these the expiry escalation cannot tell "this machine never had a transcript
     *  to read" from "it read fine and the agent said nothing" — and it used to name the
     *  first as the cause every time, whichever had happened. */
    reads: number;
    readErrors: number;
  }
  const driveWatch = new Map<string, DriveWatch>();
  /** Give up harvesting after the same window driveTab's own directive cleanup uses. */
  const DRIVE_WATCH_MS = 15 * 60_000;
  /** Enough of the answer to act on. The rest stays in the tab, which Overlord can drive
   *  again — the point is to keep the supervisor's context small (§2). */
  const DRIVE_REPLY_MAX = 4000;
  const fireLog = new Map<string, number[]>(); // `${ruleId}|${tabId}` → recent fire ts
  const outstanding = new Map<string, OutstandingDirective>(); // tabId → directive
  // Escalations raised but not yet announced to the agent (it was busy/absent at the
  // time). The tick keeps retrying the wake nudge until one lands.
  const unNudged = new Set<string>();
  /** Why a tab that was an agent has no live agent state (docs/overlord.md §9.4).
   *
   *  "Dormant" conflates two situations with opposite remedies, and treating them as one
   *  is why the triage deck could only ever print advice:
   *
   *    unbound — the agent process IS running; maiTerm just has no MCP binding for it
   *              (the usual case after a restart, resume or fork). One `/maiterm init`
   *              fixes it, and that is something Overlord can simply do.
   *    stopped — nothing is running; the tab is sitting at a shell. Re-initializing would
   *              type a slash command into bash. The remedy is to relaunch the agent,
   *              which is a bigger action and stays opt-in.
   */
  type UnreadyKind = 'unbound' | 'stopped';
  const liveness = new Map<string, { kind: UnreadyKind; at: number }>();
  /** Tabs typed `/maiterm init` at, waiting to see whether a binding actually arrives. */
  const rebindWatch = new Map<string, number>(); // tabId → sentAt
  /** Tabs where it demonstrably did not: treated as `stopped` from then on, whatever the
   *  process probe says (see the classification in `probeLiveness`). */
  const rebindFailed = new Set<string>();
  /** How long a re-bind gets. An agent that is alive and merely unbound answers a slash
   *  command in a second or two; this is slack for a busy TUI, not for a resume. */
  const REBIND_VERIFY_MS = 45_000;
  /** The one line that re-binds a running agent. Watching is keyed on THIS TEXT having been
   *  typed, never on the rule that typed it: `agent_unready` is a selectable event with a
   *  free-text sequence, so a rule whose step says anything else would otherwise be watched
   *  for a binding it cannot produce. */
  const REBIND_COMMAND = '/maiterm init';

  /** Board tasks the human handed to the agent ("Send" on a card) → when. In memory only,
   *  like `escalations` itself: the handoff IS the escalation, and once the agent has
   *  consumed it the mark is only a receipt for the human. */
  const handedOff = new Map<string, number>(); // taskId → ms epoch

  const rituals = new Map<string, RitualRun>(); // tabId → active ritual
  let ticker: ReturnType<typeof setInterval> | null = null;
  let ticking = false;

  const sleep = (ms: number) => new Promise<void>((r) => setTimeout(r, ms));
  function bumpLive() { liveVersion++; }

  function setOutstanding(tabId: string, d: OutstandingDirective) { outstanding.set(tabId, d); bumpLive(); }
  function clearOutstanding(tabId: string) { if (outstanding.delete(tabId)) bumpLive(); }

  // ── Tab / workspace helpers ─────────────────────────────────────────────────

  /**
   * Exempt from Overlord, by the tab's own flag or its workspace's (docs/overlord.md §11).
   * Checked here, at the ONE enumeration every engine path starts from, so exemption is not
   * a test each consumer has to remember: the tick (rules, facts, task mirror, census), the
   * liveness probe, `spentTabs`, `rulesForTab` — an exempt tab is simply not an agent tab
   * as far as the engine can see. The agent's tools ask `isExemptTab` explicitly, because
   * they take a tab id from outside and have to say WHY they refused.
   */
  function tabExempt(tab: Tab, ws: Workspace): boolean {
    return !!tab.overlord_exempt || !!ws.overlord_exempt;
  }

  function isExemptTab(tabId: string): boolean {
    for (const ws of workspacesStore.workspaces) {
      for (const pane of ws.panes) {
        const tab = pane.tabs.find((t) => t.id === tabId);
        if (tab) return tabExempt(tab, ws);
      }
    }
    return false;
  }

  const EXEMPT_DETAIL =
    'That tab is exempt from Overlord — the human marked it, or its workspace, exempt. ' +
    'Leave it alone: nothing here will drive, recover, answer, archive or close it.';

  function agentTabs(): { tab: Tab; ws: Workspace }[] {
    const out: { tab: Tab; ws: Workspace }[] = [];
    for (const ws of workspacesStore.workspaces) {
      for (const pane of ws.panes) {
        for (const tab of pane.tabs) {
          if ((tab.tab_type ?? 'terminal') !== 'terminal') continue;
          if (!tab.runtime) continue; // never hosted an agent → not supervised
          if (tabExempt(tab, ws)) continue; // the human said hands off
          out.push({ tab, ws });
        }
      }
    }
    return out;
  }

  function workspaceForTab(tabId: string): Workspace | null {
    for (const ws of workspacesStore.workspaces) {
      for (const pane of ws.panes) {
        if (pane.tabs.some((t) => t.id === tabId)) return ws;
      }
    }
    return null;
  }

  /** This window's Overlord workspace, if one exists. */
  function overlordWorkspace(): Workspace | null {
    return workspacesStore.workspaces.find((w) => w.overlord) ?? null;
  }

  // ── Ledger (§3) — verbatim, append-through to the persisted ring ────────────

  function ledger(
    tabId: string,
    ruleId: string | null,
    origin: OverlordLedgerEntry['origin'],
    stepIndex: number,
    step: Pick<OverlordStep, 'kind' | 'text'>,
    outcome: OverlordLedgerOutcome,
  ) {
    const entry: OverlordLedgerEntry = {
      id: crypto.randomUUID(),
      ts: new Date().toISOString(),
      tab_id: tabId,
      workspace_id: workspaceForTab(tabId)?.id ?? '',
      rule_id: ruleId,
      origin,
      step_index: stepIndex,
      text: step.text,
      kind: step.kind,
      outcome,
    };
    recentLedger = [...recentLedger.slice(-199), entry];
    commands.appendOverlordLedger([entry]).catch((e) => logError(`overlord: ledger append failed: ${e}`));
  }

  // ── Rule scoping + supersedes (§6) ──────────────────────────────────────────

  /**
   * A rule has a sequence when a step has TEXT, not when a step exists. The rules editor
   * persists "New rule" with one blank step the moment it is added, so every ruleset passes
   * through that state — and a blank step still submits (`bracketedPasteSubmit('')` wraps
   * nothing and presses Enter). Every place that asks "can this rule run" asks this; the
   * three that counted steps each found the blank rule a different way (a global "New rule ·
   * off" in every Trigger menu; the deck's Checkpoint button silently running it instead
   * of the real checkpoint rule).
   */
  function hasRunnableSequence(rule: OverlordRule): boolean {
    return rule.sequence.some((s) => s.text.trim() !== '');
  }

  /**
   * Whether a permission prompt on the tab stops this rule. The tab is `ready` there —
   * registered, process alive — but a rule wanting `idle` would sit in `waitInjectable` for
   * its whole cap (5 min) holding the ritual slot: strip frozen at 1/N, every other rule on
   * the tab blocked, having told the human it started. Nothing resolves that but the human
   * answering the prompt, so the human-initiated paths refuse up front. A rule whose guard
   * admits `permission` was written to fire there and goes through to the handoff.
   */
  function permissionBlocks(rule: OverlordRule, tabId: string): boolean {
    return mappedState(tabId) === 'permission' && !(rule.guards.agent_state ?? ['idle']).includes('permission');
  }

  /** Enabled rules that apply to a tab's workspace, with supersedes resolved. */
  function rulesForWorkspace(wsId: string): OverlordRule[] {
    const inScope = preferencesStore.overlordRules.filter(
      (r) => r.enabled && (r.workspaces.length === 0 || r.workspaces.includes(wsId)),
    );
    const suppressed = new Set<string>();
    for (const r of inScope) {
      for (const key of r.supersedes ?? []) suppressed.add(key);
    }
    return inScope.filter((r) => !suppressed.has(r.id) && !(r.default_id && suppressed.has(r.default_id)));
  }

  // ── Guard evaluation (§3) — mechanical, at the tool boundary ───────────────

  function mappedState(tabId: string): AgentState | undefined {
    return claudeStateStore.getState(tabId)?.state;
  }

  function guardsPassSync(rule: OverlordRule, tabId: string, now: number): boolean {
    const g = rule.guards;
    // cooldown per (rule, tab)
    const key = `${rule.id}|${tabId}`;
    const last = lastFiredAt.get(key) ?? 0;
    if (rule.cooldown > 0 && now - last < rule.cooldown * 1000) return false;
    // agent_state (default idle-only). agent_unready rules are the exception: the
    // condition MEANS "no live agent state", so requiring one would make the rule
    // dead-on-arrival — for those, pass only when no state exists.
    const st = mappedState(tabId);
    if (rule.when.event === 'agent_unready') {
      if (st) return false;
    } else {
      const allowed = g.agent_state ?? ['idle'];
      if (!st || !allowed.includes(st)) return false;
    }
    // min_quiet_ms (default 3000)
    const quiet = g.min_quiet_ms ?? 3000;
    const lastOut = terminalsStore.getLastOutputAt(tabId) ?? 0;
    if (now - lastOut < quiet) return false;
    // only_if_no_outstanding: serialize per tab (rituals count as outstanding)
    if (g.only_if_no_outstanding && (outstanding.has(tabId) || rituals.has(tabId))) return false;
    // max_per_hour per (rule, tab)
    if (g.max_per_hour !== undefined) {
      const log = (fireLog.get(key) ?? []).filter((t) => now - t < 3_600_000);
      fireLog.set(key, log);
      if (log.length >= g.max_per_hour) return false;
    }
    return true;
  }

  /**
   * Why `require_live_repl` is or isn't satisfied — not just whether.
   *
   * The two halves are different facts and were being collapsed into one boolean:
   *   * a live agent PROCESS in the tty — without it a directive lands in bash;
   *   * a REGISTERED session — `claudeState` is fed only by hooks, and a hook cannot name
   *     its tab until `initSession` binds the connection.
   *
   * A tab resumed from a previous session has the first and not the second, and the merged
   * boolean reported that as "no live REPL" — a phrase that means the terminal is dead. It
   * sent the supervisor, and then the human, hunting for a dead terminal that was never
   * dead. `unbound` is a different problem with a different, one-line remedy.
   */
  type ReplState = 'ready' | 'unbound' | 'stopped' | 'unknown' | 'no_terminal';

  async function replState(tabId: string): Promise<ReplState> {
    const inst = terminalsStore.get(tabId);
    if (!inst) return 'no_terminal';
    if (claudeStateStore.getState(tabId)) {
      // Registered — but still confirm something is running, or the directive lands in bash.
      try {
        const live = await commands.getAgentLiveness(inst.ptyId);
        return live.agent_running || live.ssh_foreground ? 'ready' : 'stopped';
      } catch {
        return 'stopped';
      }
    }
    // Unregistered. This tick's BATCHED probe already classified it, so read that rather
    // than firing a per-call process sweep — `get_agent_liveness` TTL-caches the sweep but
    // not the per-call BFS, which is why the batch exists. `null` means the tab could not
    // be classified at all (its pane isn't mounted), which is not the same as dead.
    const kind = unreadyKind(tabId);
    return kind === 'unbound' ? 'unbound' : kind === 'stopped' ? 'stopped' : 'unknown';
  }

  /**
   * The require_live_repl hard precondition (§3): registered AND running.
   *
   * WATCH ITEM (docs/overlord.md §4.0): "registered" used to imply the agent had taken a
   * turn, because only initSession could create the session mapping. The SessionStart hook
   * now creates it at PROCESS START, so this can be true while Claude is still replaying a
   * resumed transcript and not yet reading input — a directive injected then may go unread,
   * silently, which is the failure this guard exists to prevent. If that turns out to bite,
   * add one condition: the tab has been `idle` at least once (a completed turn), the same
   * fact agentDelivery already models for mesh and bridge.
   */
  async function hasLiveRepl(tabId: string): Promise<boolean> {
    return (await replState(tabId)) === 'ready';
  }

  function humanTypedSince(tabId: string, sinceMs: number): boolean {
    const at = terminalsStore.getLastUserInputAt(tabId);
    return at !== undefined && at > sinceMs;
  }

  // ── Ritual executor (§7) — a gated sequence state machine ──────────────────

  async function waitInjectable(run: RitualRun, guards: OverlordGuards): Promise<boolean> {
    const allowed = guards.agent_state ?? ['idle'];
    const quiet = guards.min_quiet_ms ?? 3000;
    const t0 = Date.now();
    while (Date.now() - t0 < INJECTABLE_WAIT_CAP_MS) {
      if (run.aborted) return false;
      // Human typed into the tab since our last injection (or ritual start) — their
      // tab now, even if their turn already finished (§7). Without this, a human turn
      // between steps gets waited out and the next step steamrolls their conversation.
      if (humanTypedSince(run.tabId, run.lastInjectionAt)) return false;
      const st = mappedState(run.tabId);
      const lastOut = terminalsStore.getLastOutputAt(run.tabId) ?? 0;
      const stateOk = run.targetsUnready ? st === undefined : !!st && allowed.includes(st);
      if (stateOk && Date.now() - lastOut >= quiet) return true;
      await sleep(500);
    }
    return false;
  }

  async function awaitGate(
    run: RitualRun,
    step: OverlordStep,
    directive: OutstandingDirective,
  ): Promise<'ok' | 'timeout' | 'aborted'> {
    const gate = step.await;
    if (!gate) return 'ok';
    const deadline = Date.now() + (step.timeout_seconds ?? 600) * 1000;
    let sawActive = false;
    while (Date.now() < deadline) {
      if (run.aborted) return 'aborted';
      if (humanTypedSince(run.tabId, directive.sentAt)) return 'aborted';
      const st = mappedState(run.tabId);
      switch (gate.until) {
        case 'turn_end': {
          // Wait for the directive's turn to start, then finish. 'permission' pauses the
          // ritual (it resumes when the gate clears) — deliberately not a failure.
          if (st === 'active') sawActive = true;
          if (sawActive && st === 'idle') return 'ok';
          // A turn shorter than one poll never shows 'active', which would strand the
          // ritual until its timeout. The fallback proof must be something the DIRECTIVE
          // ITSELF cannot satisfy: last_turn_ts counts plain user lines, and the injected
          // directive is one, so it advances on delivery rather than on completion (and on
          // an SSH tab it carries the remote clock). PTY silence is the honest signal — a
          // working TUI agent repaints continuously.
          if (!sawActive && st === 'idle') {
            const lastOut = terminalsStore.getLastOutputAt(run.tabId) ?? 0;
            if (Date.now() - directive.sentAt >= TURN_FALLBACK_MIN_MS
              && Date.now() - lastOut >= TURN_FALLBACK_QUIET_MS) return 'ok';
          }
          break;
        }
        case 'ack': {
          if (directive.acked) return 'ok';
          break;
        }
        case 'context_below': {
          const f = facts.get(run.tabId);
          // A fresh usage line only appears on the NEXT turn after /compact, so also
          // accept a compaction boundary newer than the injection as proof it landed.
          if (f?.context_pct !== undefined && f.context_pct < gate.pct) return 'ok';
          if (f?.last_compact_ts !== undefined && f.last_compact_ts > directive.sentAt) return 'ok';
          break;
        }
        case 'idle_ms': {
          const lastOut = Math.max(terminalsStore.getLastOutputAt(run.tabId) ?? 0, directive.sentAt);
          if (Date.now() - lastOut >= gate.ms) return 'ok';
          break;
        }
      }
      await sleep(GATE_POLL_MS);
    }
    return 'timeout';
  }

  /** One board notice at a time per tab (see `deleteTask`). Rejections are swallowed on
   *  the stored tail so one failed notice can't poison the next. */
  const noticeChain = new Map<string, Promise<unknown>>();
  function serializeNotice(tabId: string, fn: () => Promise<boolean>): Promise<boolean> {
    const prev = noticeChain.get(tabId) ?? Promise.resolve();
    const next = prev.then(fn, fn);
    noticeChain.set(tabId, next.catch(() => {}));
    return next;
  }

  /**
   * Type a board notice at a tab with the HUMAN's authority. Returns whether it landed.
   *
   * Shared by every board action that has something to tell the tab carrying a task, so
   * they cannot drift on the one thing that is easy to get wrong here — WHEN it is safe to
   * type. Idle only, and re-checked AFTER the liveness round trip:
   *
   * `hasLiveRepl` says a process is alive; it says nothing about whether the tab can take
   * typed text. It returns true for `active` (the notice would land mid-turn, which every
   * rule-driven injection waits to avoid) and for `permission` — the one state driveTab,
   * runSequence and answerPrompt all refuse, because a permission prompt is a keystroke
   * menu where pasted prose is swallowed or corrupts the selection.
   *
   * Serialized per tab, because `bracketedPasteSubmit` is write → settle → CR: two notices
   * for the same tab clicked inside that window would merge into one prompt and leave a
   * stray carriage return behind. The checks run INSIDE the chain, so the second notice
   * re-tests a tab the first one has just typed into.
   *
   * A notice is not a directive: it asks for nothing back, so it deliberately does NOT take
   * the tab's outstanding slot, which would block every `only_if_no_outstanding` rule behind
   * it for something nobody is waiting on.
   */
  async function noticeToTab(tabId: string, text: string, what: string): Promise<boolean> {
    const inst = terminalsStore.get(tabId);
    return serializeNotice(tabId, async () => {
      if (!inst) return false;
      if (mappedState(tabId) !== 'idle') return false;
      if (!(await hasLiveRepl(tabId))) return false;
      // State can move during the liveness round trip.
      if (mappedState(tabId) !== 'idle') return false;
      if (Date.now() - (terminalsStore.getLastOutputAt(tabId) ?? 0) < 1500) return false;
      try {
        await bracketedPasteSubmit(inst.ptyId, text);
        return true;
      } catch (e) {
        logError(`overlord: ${what} notice failed for ${tabId.slice(0, 8)}: ${e}`);
        return false;
      }
    });
  }

  /** Record this tick's edges and report which are still offerable (§ `pendingEdges`). */
  function latchEdges(
    tabId: string,
    now: number,
    turnEnded: boolean,
    committed: boolean,
  ): { turnEnded: boolean; committed: boolean } {
    const e = pendingEdges.get(tabId) ?? {};
    if (turnEnded) e.turnEnded = now;
    if (committed) e.committed = now;
    // Expire, and drop anything the human has overtaken.
    //
    // Holding an edge for minutes reopens a hole §7 closes for rituals: `waitInjectable`
    // only compares keystrokes against the RUNNING ritual's baseline, so typing that
    // happened before the rule fired is invisible to it. A latched commit could therefore
    // fire "review that commit" into a conversation where the human had already said
    // "revert it, wrong branch". Keystrokes after an edge was stamped mean the human has
    // taken the tab somewhere else, and the edge is stale — which is the same judgement
    // §7 makes between ritual steps, applied to the window before the first one.
    if (e.turnEnded !== undefined && (now - e.turnEnded >= EDGE_LATCH_MS || humanTypedSince(tabId, e.turnEnded))) {
      delete e.turnEnded;
    }
    if (e.committed !== undefined && (now - e.committed >= EDGE_LATCH_MS || humanTypedSince(tabId, e.committed))) {
      delete e.committed;
    }
    if (e.turnEnded === undefined && e.committed === undefined) {
      pendingEdges.delete(tabId);
      return { turnEnded: false, committed: false };
    }
    pendingEdges.set(tabId, e);
    return { turnEnded: e.turnEnded !== undefined, committed: e.committed !== undefined };
  }

  /** Spend the edge a rule just fired on, so it fires once per edge and not once per
   *  tick for as long as the latch lives. Other rules on other edges keep theirs. */
  function consumeEdge(tabId: string, event: OverlordRule['when']['event']) {
    const e = pendingEdges.get(tabId);
    if (!e) return;
    if (event === 'turn_end') delete e.turnEnded;
    else if (event === 'commit') delete e.committed;
    else return;
    if (e.turnEnded === undefined && e.committed === undefined) pendingEdges.delete(tabId);
  }

  // ── maiLink mirror (docs/mailink-protocol.md §13) ───────────────────────────
  /** The phone is served a SNAPSHOT of this engine, published from here, never by asking this
   *  webview — Rust cannot read a frontend store, and an occluded screen throttles this
   *  window's timers, which is exactly when a phone is in use. Rust stamps version/window and
   *  gates rows on tab designation; `asOf` is set here, at build time, so the phone can render
   *  how old what it is looking at is. Only human-addressed escalations cross: an agent-only
   *  row would light a badge nothing on the phone can clear. Debounced, so the mutation paths
   *  can call `scheduleMirror()` freely. */
  /** Every rule the phone could fire, with the tabs it would be offered on. One entry per rule;
   *  a rule nothing can run is omitted rather than sent with an empty list.
   *
   *  Grouped by WORKSPACE, and `rulesForTab` is called once per workspace rather than once per
   *  tab: the scope it filters on is the tab's workspace, and it scans `agentTabs()` internally
   *  to find it — per tab that is O(tabs²) every publish, which at fleet size (hundreds of tabs)
   *  is real main-thread work every 5 s. Calling it through a representative tab keeps ONE
   *  implementation of the predicate, which is the thing worth protecting here. */
  function rulesForPhone(): { id: string; name: string; enabled: boolean; appliesTo: string[] }[] {
    const byWorkspace = new Map<string, string[]>();
    for (const { tab, ws } of agentTabs()) {
      const tabs = byWorkspace.get(ws.id);
      if (tabs) tabs.push(tab.id);
      else byWorkspace.set(ws.id, [tab.id]);
    }
    const byRule = new Map<string, { id: string; name: string; enabled: boolean; appliesTo: string[] }>();
    for (const tabIds of byWorkspace.values()) {
      for (const r of overlordStore.rulesForTab(tabIds[0])) {
        const row = byRule.get(r.id) ?? { id: r.id, name: r.name, enabled: r.enabled, appliesTo: [] };
        row.appliesTo.push(...tabIds);
        byRule.set(r.id, row);
      }
    }
    // Re-sort: `rulesForTab` sorts per workspace, but the union comes out in Map INSERTION
    // order, so a disabled rule first seen in workspace A landed above an enabled one from
    // workspace B — the inverse of the desktop menu, and the opposite of what §13.2 promises.
    return [...byRule.values()].sort(
      (a, b) => Number(b.enabled) - Number(a.enabled) || a.name.localeCompare(b.name),
    );
  }

  /** Terminal tabs in this window's Overlord workspace, if it has one. */
  function overlordWorkspaceTabIds(): string[] {
    const ws = workspacesStore.workspaces.find((w) => w.overlord);
    if (!ws) return [];
    return ws.panes.flatMap((p) => p.tabs.filter((t) => (t.tab_type ?? 'terminal') === 'terminal').map((t) => t.id));
  }

  let mirrorTimer: ReturnType<typeof setTimeout> | null = null;
  function publishMirror() {
    mirrorTimer = null;
    // The module-level store — initialised long before any timer can fire.
    const s = overlordStore;
    // Published EVERY tick, changed or not: `asOf` is the phone's only liveness signal, and a
    // "skip if unchanged" made a quiet awake desktop indistinguishable from a sleeping one (an
    // idle board froze at launch). ~5 s of desktop clock per window is the cost.
    const snapshot = $state.snapshot({
      // `running` means the engine is ON for this window: rehydrate() starts the ticker in every
      // window regardless (tick() no-ops when disabled), so the preference is the truth here.
      running: running && preferencesStore.overlordEnabled,
      escalations: s.humanEscalations,
      proposals: s.proposals,
      agentReports: [...agentReports.values()],
      outstandingDirectives: s.outstandingDirectives,
      ritualProgress: s.ritualProgress,
      spentTabs: s.spentTabs,
      pendingRuleChanges: s.pendingRuleChanges,
      lastScan: s.lastScan,
      // What the composer dock's "Run an Overlord rule on this tab" menu offers, RESOLVED to
      // tab ids rather than sent as a scope the phone would have to re-match. The predicate is
      // three things at once — the tab is an agent tab (terminal, has a runtime, not exempt),
      // the rule has a runnable sequence, and its `workspaces` is empty or contains the tab's
      // workspace — and a second implementation of that would drift from this one. Disabled
      // rules are included, as on the desktop, sorted last; `enabled` says which.
      //
      // Gated on the preference, like every desktop surface that offers this menu (ComposerDock,
      // the tab context menu, the board behind its sidebar accessor). Overlord is OFF by default
      // and the engine seeds default rules and ticks regardless, so publishing unconditionally
      // offered the phone a menu that exists nowhere on that desktop — and firing from it would
      // have pasted rule text into an agent whose human never turned the supervisor on.
      rules: preferencesStore.overlordEnabled ? rulesForPhone() : [],
      // The tabs in this window's Overlord WORKSPACE. Empty means the supervisor's own
      // conversation is not reachable from the phone (it is a `board` tab, or the human turned
      // its availability off, or expose-all is off and it was never marked native) — which the
      // phone should SAY, rather than render an empty section as if there were nothing to show.
      agentTabIds: overlordWorkspaceTabIds(),
    }) as Record<string, unknown>;
    commands
      .publishOverlordSnapshot({ ...snapshot, asOf: Date.now() })
      .catch((e) => logWarn(`overlord: maiLink mirror publish failed: ${e}`));
  }
  function scheduleMirror() {
    if (mirrorTimer) return;
    mirrorTimer = setTimeout(publishMirror, 750);
  }

  function escalate(
    tabId: string,
    ruleId: string | null,
    kind: OverlordEscalation['kind'],
    detail: string,
    taskId?: string,
  ): string {
    // The same fact restated (see DEDUPED_ESCALATIONS). Refresh the open card in place so
    // the board carries the agent's LATEST reason rather than its first — a stale card is a
    // subtler lie than a duplicated one, and the duplicate at least tells you it is old.
    //
    // **`ts` is NOT refreshed.** It means "raised at", and on a deduped card that is the
    // more useful of the two readings: a tab restating `blocked` every 30s for two hours
    // would otherwise render as "30s" forever on both the deck and the phone, and how long
    // this has been true is most of what the human wants from the card. Stacked duplicates
    // used to convey that, loudly — losing the duration while fixing the noise would trade
    // one bad reading for another. The card reads "raised 2h ago, latest reason: …".
    //
    // `read` is preserved, so this does not re-ring the doorbell or re-deliver to the agent.
    // Deduping exists to cut noise; resetting it would restore the noise by another route.
    // The human is NOT the party that loses out: `publishMirror` sends `humanEscalations`,
    // which filters on kind and not on `read`, and the deck renders read cards until they
    // are dismissed — so both human surfaces show the updated detail. Only the Overlord
    // agent misses it, and only after it has already been told this tab is blocked.
    //
    // DO NOT "fix" that by adding `unNudged.add(open.id)` here. It is silently INERT for a
    // read card: `wakeOverlordAgent` prunes every id that is not `!read` before it does
    // anything, so the line would look like a fix and do nothing.
    const open = DEDUPED_ESCALATIONS.has(kind)
      ? escalations.find((e) => e.tabId === tabId && e.kind === kind)
      : undefined;
    if (open) {
      escalations = escalations.map((e) =>
        e.id === open.id
          ? {
              ...e,
              detail,
              ruleId,
              taskId: taskId ?? e.taskId,
              // Re-resolved because a tab can be dragged between workspaces, and a deduped
              // card is never re-created — so without this the row keeps the workspace it
              // was first raised in, for good. Falls back to the stored value rather than
              // `''`, since a lookup miss must not blank a field that was right.
              workspaceId: workspaceForTab(tabId)?.id ?? e.workspaceId,
            }
          : e,
      );
      scheduleMirror();
      return open.id;
    }
    const id = crypto.randomUUID();
    escalations = [
      ...escalations,
      {
        id,
        ts: Date.now(),
        tabId,
        workspaceId: workspaceForTab(tabId)?.id ?? '',
        ruleId,
        kind,
        detail,
        taskId,
        read: false,
      },
    ];
    scheduleMirror();
    unNudged.add(id);
    void wakeOverlordAgent();
    return id;
  }

  /**
   * Tabs the agent has been told are stuck at a prompt, so the ask can be WITHDRAWN when the
   * gate clears.
   *
   * A permission prompt is the one thing Overlord may not answer, so it hands the tab to the
   * agent and waits. But the human usually just answers it in the tab — and nothing told
   * anyone. An undelivered handoff still rang the doorbell and sent the agent chasing a gate
   * that was already open; a delivered one left it working, or holding a question it had put
   * to the human, on a problem that no longer exists. Both are the queue-vs-derived split
   * (§3.1): the `permission` CARD is derived and self-clears, this handoff is a queue and
   * had nobody to clear it.
   */
  const permissionHandoff = new Map<string, { escalationIds: string[]; ruleId: string | null }>();

  /**
   * A LIST per tab, not one entry. One open gate can be escalated more than once — a
   * `permission_pending` rule re-fires on the same still-open prompt every cooldown, because
   * `permissionSince` stays set for as long as it sits — and remembering only the newest left
   * the earlier asks queued, to be delivered later about a gate that had long since opened.
   * Withdrawing one of N is not withdrawing.
   */
  function recordPermissionHandoff(tabId: string, escalationId: string, ruleId: string | null) {
    const existing = permissionHandoff.get(tabId);
    if (existing) existing.escalationIds.push(escalationId);
    else permissionHandoff.set(tabId, { escalationIds: [escalationId], ruleId });
  }

  /** Forget one id, dropping the tab's entry once nothing is left to withdraw. */
  function forgetPermissionHandoff(tabId: string, escalationId: string) {
    const h = permissionHandoff.get(tabId);
    if (!h) return;
    h.escalationIds = h.escalationIds.filter((id) => id !== escalationId);
    if (!h.escalationIds.length) permissionHandoff.delete(tabId);
  }

  /**
   * Retract every ask still sitting in the queue for this tab and forget it. Returns how many
   * had ALREADY been pulled by the agent — the ones a correction is owed for — or null if
   * this tab had no handoff at all.
   *
   * Dropping the map entry alone is not enough: the un-pulled asks would still be delivered,
   * about a gate that is no longer there.
   */
  function withdrawPermissionHandoff(tabId: string): { delivered: number; ruleId: string | null } | null {
    const h = permissionHandoff.get(tabId);
    if (!h) return null;
    permissionHandoff.delete(tabId);
    const queued = new Set(h.escalationIds.filter((id) => escalations.some((e) => e.id === id)));
    if (queued.size) {
      escalations = escalations.filter((e) => !queued.has(e.id));
      for (const id of queued) unNudged.delete(id);
      logInfo(
        `overlord: withdrew ${queued.size} undelivered permission handoff(s) for ` +
          `${tabDisplayName(tabId)} — the gate cleared before the agent read them`,
      );
    }
    return { delivered: h.escalationIds.length - queued.size, ruleId: h.ruleId };
  }

  /**
   * Drop a tab's open `blocked` cards, because the tab just said it isn't.
   *
   * The queue-vs-derived split again (§3.1), and the same shape `permissionHandoff` names:
   * a `blocked` card is DERIVED from a condition that changes on its own, but it was stored
   * as a queue only a human could empty. `blocked` is not in AGENT_ONLY_ESCALATIONS, so the
   * agent's pull marks it read rather than deleting it, and the only exits were a human
   * dismissing it or the tab dying. A later report overwrites `agentReports` and never
   * touched this list — so an agent that hit a wall, said so, and then carried on left a
   * card asserting it needed a human, for the life of the window. On the phone, where the
   * inbox badges rows off `escalations[].tabId`, that is a permanently flagged conversation.
   *
   * Withdrawal rather than deriving the lane from `agentReports`: deriving is the cleaner
   * end state but stops `blocked` being an escalation at all, which changes what the mirror
   * publishes — and the maiLink client anchors its Recover button to
   * `kind === 'blocked' || kind === 'step_timeout'`, so it would lose recoverTab for exactly
   * the case it exists to serve. See the board task before attempting it.
   *
   * **Only the bare-blocked path.** `needs_human` / `kind: 'escalate'` files `agent_report`,
   * which stays a queue: an agent can raise a question and go do other work, and the
   * question does not stop needing an answer because the asker got unblocked. Withdrawing
   * there would clear a card the human never saw, on the strength of the agent's own
   * subsequent activity.
   */
  function withdrawBlocked(tabId: string) {
    const gone = escalations.filter((e) => e.kind === 'blocked' && e.tabId === tabId);
    if (!gone.length) return;
    const ids = new Set(gone.map((e) => e.id));
    escalations = escalations.filter((e) => !ids.has(e.id));
    for (const id of ids) unNudged.delete(id);
    scheduleMirror();
    logInfo(
      `overlord: withdrew ${ids.size} blocked card(s) for ${tabDisplayName(tabId)} — it reported a non-blocked state`,
    );
  }

  /** Does this window still have an Overlord agent tab that could ever pull the queue? */
  function hasOverlordAgentTab(): boolean {
    const ws = overlordWorkspace();
    if (!ws) return false;
    return ws.panes.some((p) => p.tabs.some((t) => !!t.runtime));
  }

  /**
   * Drop agent-only escalations nobody can deliver.
   *
   * They are hidden from the human deck by design, so the human can never dismiss one;
   * `consumeEscalations` is their only disposal, and only the agent calls it. Close the
   * agent tab after driving a few tabs and the queue grows for the life of the window,
   * with the doorbell retrying each one on every tick forever.
   *
   * Only swept while there is no agent tab at all. A live agent that is merely busy will
   * pull them when it comes up for air, and throwing away an answer it asked for would be
   * worse than keeping it.
   */
  function sweepUndeliverableEscalations(now: number) {
    if (hasOverlordAgentTab()) return;
    const dead = escalations.filter(
      (e) => AGENT_ONLY_ESCALATIONS.has(e.kind) && !e.read && now - e.ts > AGENT_ESCALATION_TTL_MS,
    );
    if (!dead.length) return;
    const ids = new Set(dead.map((e) => e.id));
    escalations = escalations.filter((e) => !ids.has(e.id));
    for (const id of ids) unNudged.delete(id);
    // Withdraw the card's "Sent" receipt for any handoff being thrown away — a receipt
    // that outlives the thing it is a receipt for is worse than no receipt at all.
    let withdrew = false;
    for (const e of dead) {
      if (e.taskId && handedOff.delete(e.taskId)) withdrew = true;
      // Thrown away undelivered, so there is nothing to stand down from later.
      forgetPermissionHandoff(e.tabId, e.id);
    }
    if (withdrew) bumpLive();
    logInfo(`overlord: dropped ${ids.size} undeliverable agent escalation(s) — no agent tab in this window`);
  }

  /**
   * Forget every tab this window no longer has.
   *
   * Nothing watched for a tab going away. The fleet-derived signals — pressure, permission,
   * unready — self-clear because they are DERIVED from the workspace tree, so they simply
   * stop being produced. Proposals and escalations are QUEUES, and had nobody to clear
   * them: closing a tab left its card sitting on triage, most visibly a "Re-bind a running
   * agent" proposal whose Send button pointed at a PTY that no longer exists.
   *
   * Also drops the tab's engine bookkeeping, which only ever grew — a long-lived window
   * that opens and closes tabs leaked an entry per tab across a dozen maps.
   */
  /**
   * Release everything the engine holds for a tab that is no longer supervised — closed, or
   * EXEMPTED (docs/overlord.md §11). Exemption is the same event from the engine's side:
   * `agentTabs()` stops seeing the tab, but a ritual mid-sequence, an outstanding directive,
   * a drive watch reading its transcript, a proposal card offering to type into it, all
   * outlive that filter unless something lets go of them. The human reaches for "exempt"
   * exactly while one of those is happening, so leaving them to run out is leaving the
   * exemption silently false for minutes.
   */
  function sweepClosedTabs() {
    const present = new Set<string>();
    const live = new Set<string>();
    for (const ws of workspacesStore.workspaces) {
      for (const pane of ws.panes) {
        for (const tab of pane.tabs) {
          present.add(tab.id);
          if (!tabExempt(tab, ws)) live.add(tab.id);
        }
      }
    }
    // An empty tree means the window is still loading, not that every tab was closed.
    // Sweeping on it would throw away the whole queue at startup.
    if (!present.size) return;

    const deadProposals = proposals.filter((p) => !live.has(p.tabId));
    // An escalation dies with its tab — but an exempt tab is still HERE, and exemption is
    // about supervision, not the work: the human's own board actions on it ("Do it",
    // "Send", a deleted task) are relays the human just clicked, and sweeping them turned a
    // "Sent" receipt back into "Send" five seconds later with the supervisor already woken
    // to find nothing. Those survive exemption; the engine's own (permission handoffs,
    // drive replies, re-bind failures) do not. And an escalation with NO tab — "Send" on an
    // unassigned task passes '' — belongs to nobody's tab and must not die with one.
    const deadEscalations = escalations.filter((e) => {
      if (!e.tabId) return false;
      if (!present.has(e.tabId)) return true;
      return !live.has(e.tabId) && !HUMAN_BOARD_ESCALATIONS.has(e.kind);
    });
    // Seeded from every per-tab map, not just the four with visible symptoms. A tab can sit
    // in one of these and NO other: the ritual path records a permission handoff and returns
    // before `setOutstanding`, and its `rituals` entry is dropped in the same `finally` — so
    // closing that tab left a handoff whose tab was gone, which `sweepResolvedPermissionHandoffs`
    // then read as a prompt somebody answered.
    const deadTabs = new Set<string>([
      ...deadProposals.map((p) => p.tabId),
      ...deadEscalations.map((e) => e.tabId),
      ...[
        ...outstanding.keys(), ...liveness.keys(), ...driveWatch.keys(), ...rituals.keys(),
        ...permissionHandoff.keys(), ...permissionSince.keys(), ...prevAgentState.keys(),
        ...pendingEdges.keys(),
      ],
    ].filter((id) => !live.has(id)));
    if (!deadProposals.length && !deadEscalations.length && !deadTabs.size) return;

    if (deadProposals.length) proposals = proposals.filter((p) => live.has(p.tabId));
    if (deadEscalations.length) {
      // Remove exactly what was judged dead above. Filtering on `live` here again would
      // delete the spared ones too — the human's relay and the tabless '' — but only on
      // ticks where something ELSE was dead, and without clearing their `handedOff`
      // receipt: a "Sent" that never reverts, for a handoff the agent never received.
      const deadIds = new Set(deadEscalations.map((e) => e.id));
      escalations = escalations.filter((e) => !deadIds.has(e.id));
      for (const e of deadEscalations) {
        unNudged.delete(e.id);
        // Same reason as the sweep above: a "Sent" receipt must not outlive its errand.
        if (e.taskId) handedOff.delete(e.taskId);
      }
    }

    for (const id of deadTabs) {
      // Abort rather than delete — the ritual's own loop owns its map entry and clears it
      // in its finally, and yanking it from under a running loop is how you get one that
      // keeps injecting into a tab nobody is watching.
      const run = rituals.get(id);
      if (run) run.aborted = true;
      prevAgentState.delete(id);
      prevCommitTs.delete(id);
      pendingEdges.delete(id);
      permissionSince.delete(id);
      // The tab is gone, so "stand down, it was answered" would be a lie about why.
      permissionHandoff.delete(id);
      outstanding.delete(id);
      liveness.delete(id);
      rebindWatch.delete(id);
      rebindFailed.delete(id);
      // A watch on a closed tab expires into "no reply could be read", which is true and
      // useless: the human closed it. Drop it without the escalation.
      driveWatch.delete(id);
      noticeChain.delete(id);
      primedAgents.delete(id);
      if (agentReports.has(id)) {
        agentReports.delete(id);
        agentReports = new Map(agentReports);
      }
      // These two are keyed `${ruleId}|${tabId}`, so they need the scan.
      for (const k of [...lastFiredAt.keys()]) if (k.endsWith(`|${id}`)) lastFiredAt.delete(k);
      for (const k of [...fireLog.keys()]) if (k.endsWith(`|${id}`)) fireLog.delete(k);
    }

    bumpLive();
    logInfo(
      `overlord: swept ${deadTabs.size} closed or exempt tab(s) — dropped ${deadProposals.length} ` +
        `proposal(s), ${deadEscalations.length} escalation(s)`,
    );
  }

  /**
   * The gate cleared — usually because the human answered it in the tab, which is the normal
   * way a permission prompt ends. Withdraw the ask.
   *
   * Undelivered: delete it, and the agent never learns it was asked. Delivered: it is acting
   * on this right now, so it is owed a correction — including the case that prompted this,
   * where it had already put the question to the human and is sitting blocked on an answer
   * that will never come, because the human answered the tab instead.
   */
  function sweepResolvedPermissionHandoffs() {
    for (const tabId of [...permissionHandoff.keys()]) {
      const st = mappedState(tabId);
      if (st === 'permission') continue;
      const w = withdrawPermissionHandoff(tabId);
      // Nothing reached the agent, so it never learns it was asked.
      if (!w || !w.delivered) continue;
      // Say WHICH way it cleared. A prompt also stops being a prompt when the agent behind it
      // exits, and "it was answered, the tab is moving again" would be a confident account of
      // something nobody checked — the exact habit this subsystem keeps having to unlearn.
      const answered = st === 'idle' || st === 'active';
      escalate(
        tabId,
        w.ruleId,
        'permission_stuck',
        answered
          ? `Stand down on ${tabDisplayName(tabId)} — the prompt you were told about was ` +
              `answered and that tab is working again, so nothing is needed from you. You did ` +
              `not answer it through maiTerm, so it was answered in the tab. If you put the ` +
              `question to a human, it is moot: drop it rather than waiting on an answer that ` +
              `is not coming.`
          : `Stand down on ${tabDisplayName(tabId)} — it is no longer at that prompt, but it ` +
              `has no live agent state either, so the gate went away with the agent rather ` +
              `than being answered. Nothing there will act on a directive until it is ` +
              `restarted. Drop any question you raised about it.`,
      );
      logInfo(
        `overlord: permission handoff for ${tabDisplayName(tabId)} was already delivered — ` +
          `told the agent to stand down (${answered ? 'answered' : `state=${st ?? 'none'}`})`,
      );
    }
  }

  /** One-line doorbell into the Overlord agent's PTY (§9.1) — content stays behind
   *  the listEscalations pull, keeping the agent's transcript lean. An agent that is merely
   *  busy or unmounted → the queue waits (the engine runs regardless; §2 agent lifecycle).
   *  NO agent tab at all → agent-only items are swept after 30 min rather than waiting
   *  forever, so nothing may be accepted here on the promise that it will be delivered. */
  /** One doorbell at a time. Everything before the first `await` here is synchronous, so N
   *  escalations raised in one synchronous loop — which is now the ordinary case, since a
   *  restart leaves several tabs unbound and their re-bind watches expire in the same probe
   *  pass — all read `idle` and an unread queue, then all reach `bracketedPasteSubmit` on the
   *  SAME supervisor PTY. That helper writes the text, settles ~100ms, then writes the CR
   *  separately, so the later pastes land inside that window: the supervisor's input box gets
   *  the nudge concatenated N times, submitted by the first CR, followed by stray Enters. */
  let waking = false;
  async function wakeOverlordAgent() {
    if (waking) return;
    if (unNudged.size === 0) return;
    // The human may have cleared the queue while the agent was busy — an escalation
    // dismissed from the board must not still ring "0 escalations pending" later.
    for (const id of [...unNudged]) {
      if (!escalations.some((e) => e.id === id && !e.read)) unNudged.delete(id);
    }
    if (unNudged.size === 0) return;
    const ws = overlordWorkspace();
    if (!ws) return;
    waking = true;
    try {
      for (const pane of ws.panes) {
        for (const tab of pane.tabs) {
          if (!tab.runtime) continue;
          if (mappedState(tab.id) !== 'idle') continue;
          if (!(await hasLiveRepl(tab.id))) continue;
          const inst = terminalsStore.get(tab.id);
          if (!inst) return;
          const unread = escalations.filter((e) => !e.read);
          const n = unread.length;
          // Word it for what's actually queued: calling a tab's answer an "escalation" makes
          // the agent open it braced for a problem.
          const what = unread.every((e) => e.kind === 'drive_reply')
            ? `${n} repl${n === 1 ? 'y' : 'ies'} from tabs you drove`
            : `${n} Overlord item${n === 1 ? '' : 's'} pending`;
          try {
            await bracketedPasteSubmit(inst.ptyId, `${what} — call listEscalations.`);
            unNudged.clear();
          } catch (e) {
            logError(`overlord: wake nudge failed: ${e}`);
          }
          return;
        }
      }
    } finally {
      waking = false;
    }
  }

  // ── Overlord agent priming (§9.2) ───────────────────────────────────────────

  function describeCondition(when: OverlordRule['when']): string {
    switch (when.event) {
      case 'context_pct': return `context reaches ${when.at_or_above}%`;
      case 'turn_end': return 'a turn ends';
      case 'commit': return 'a git commit lands';
      case 'tab_idle': return `a tab sits idle ${when.minutes} min`;
      case 'task_stale': return `a board task goes stale ${when.days} days`;
      case 'agent_unready': return 'an agent is not running';
      case 'no_todo_list': return 'sustained work is not on the task list';
      case 'permission_pending': return `a permission waits ${when.minutes} min`;
      case 'directive_unacked': return `a directive is unacked ${when.minutes} min`;
    }
  }

  /** The ruleset rendered as the agent's standing doctrine — one document driving both
   *  the engine and the agent's judgment, so they can't drift apart (§9.2). */
  function buildDoctrine(): string {
    const wsName = (id: string) => workspacesStore.workspaces.find((w) => w.id === id)?.name ?? id.slice(0, 8);
    const ruleLines = preferencesStore.overlordRules
      .filter((r) => r.enabled)
      .map((r) => {
        const scope = r.workspaces.length ? ` [only: ${r.workspaces.map(wsName).join(', ')}]` : '';
        const steps = r.sequence.map((s) => `${s.kind === 'slash' ? '' : '"'}${s.text}${s.kind === 'slash' ? '' : '"'}`).join(' → ');
        return `  - ${r.name}: when ${describeCondition(r.when)} → ${steps}${scope}`;
      })
      .join('\n');
    return (
      `⟦OVERLORD⟧ You are the Overlord agent for this maiTerm window — the supervisor's judgment layer. ` +
      `A deterministic engine handles the routine supervision; you handle what it can't resolve, plus conversation with your human.\n\n` +
      `How this works:\n` +
      `  - The engine queues escalations and rings you with a one-line nudge. When that happens, call listEscalations for the content, then resolve each one.\n` +
      `  - To direct another tab, use driveTab — your text is typed into that tab with the human's full authority (the agent there cannot tell it from the human, so write exactly as the human would). Guard refusals (busy, no live REPL, outstanding directive) come back structured; wait and retry or escalate.\n` +
      `  - You WILL get the answer back: when a tab you drove finishes its turn, its reply is queued for you and you are rung the same way as an escalation. So it is fine to ask a tab a question and wait — you do not need to ask it to report back, and you should not poll it.\n` +
      `  - If that tab stops at a prompt instead, you are told. Call getTabPrompt to see it, then ANSWER IT with answerTabPrompt — unblocking your own fleet is your job, and a tab left sitting at a prompt is the failure you exist to prevent. Pass back the prompt_id you were given.\n` +
      `  - ESCALATE INSTEAD OF ANSWERING when the decision is consequential: anything destructive or irreversible (deleting data, force-push, dropping a database, rm -rf), anything touching money, credentials, production, or an external party, or any question about what the human actually WANTS rather than how to carry out what they already asked for. Those go to the human via AskUserQuestion, and you answer the tab once they tell you. Routine approvals in service of work already underway are yours to make. If you are genuinely unsure which side a decision falls on, it is the escalating side.\n` +
      `  - Your human can also hand you a board task directly ("Send" on a card): it arrives as a task_handoff escalation naming the task and the tab that owns it. Carry it — drive that tab, drive a better one, or do it yourself — and keep its status current with updateTasks so the board follows along. A task_dropped escalation is the reverse: the human deleted a task and the tab carrying it could not be told, so tell it yourself when it is reachable.\n` +
      `  - Use listWorkspaces to see the tabs; every injection you make is recorded verbatim in the ledger. Each agent tab reports THREE independent facts: \`pty\` ('live' | 'suspended' | 'none' — the terminal underneath), \`state\` (what the agent is doing, meaningful only over a live pty), and \`loaded\` (whether anything can reach it at all). Read all three. An 'idle' agent with loaded:false is healthy and undrivable; a suspended tab is not a dead one.\n` +
      `  - Every state has one action, and you have all of them: 'idle'/'active' → driveTab · 'permission' → getTabPrompt + answerTabPrompt · 'unbound' or 'stopped' → recoverTab (re-binds or restarts, chosen from the process state) · pty 'suspended' → resumeTab · a tab in a suspended WORKSPACE → resumeWorkspace · a tab in that workspace's archivedTabs[] → restoreArchivedTab. Nothing in this window has to stay stuck.\n` +
      `  - Four things that get confused, and are reported separately: a SUSPENDED TAB (pty:'suspended') sits in the pane tree of an ACTIVE workspace with its terminal killed — suspending every tab but the active one is routine, so most of these are perfectly ordinary; a SUSPENDED WORKSPACE parks all of its tabs at once; ARCHIVED tabs are lifted out of the pane tree entirely; loaded:false only means the pane is not mounted right now, and can be true of a tab whose agent is alive and working. Never describe one as another — say which one you mean.\n` +
      `  - A tab or workspace marked \`overlordExempt: true\` in listWorkspaces is off limits: the human exempted it. The engine runs nothing on it, it has no card, and every tool refuses it with reason 'exempt'. Do not drive, recover, answer, archive, close or resume it, and do not raise it to the human — being left alone is what they asked for.\n` +
      `  - Judging a tab's age: listWorkspaces gives \`lastTurnAt\` (its last real turn) and \`contextPct\` for agent tabs whose session could be resolved, plus \`suspendedAt\` for suspended ones. Both are absent — not zero — when there is no readable transcript for that tab (no live session and no remembered session id), which is itself worth knowing: nothing can be read back from that tab. Archived tabs carry \`archivedAt\`, and getTabNotes reads an archived tab's notes without restoring it — read those before deciding what a session was for.\n` +
      `  - Finished sessions: archiveTab when there is any chance of coming back to it — a bug in what it built, or follow-up work — which keeps the scrollback, cwd and ssh context and restores. closeTab ONLY when the session is definitively over or a fresh one would do just as well; it is irreversible and keeps nothing. Prefer archiving whenever you are unsure. Both refuse a tab that is still working, and both are ledgered. deleteArchivedTab prunes the archive itself when an archived session is no longer worth keeping.\n` +
      `  - When you find yourself hand-issuing the same directive repeatedly, propose a rule with proposeRuleChanges (batched; the human approves each change). Never re-propose a rejected change.\n` +
      `  - Reaching your human: AskUserQuestion ONLY — never print questions to the terminal or write status notes.\n\n` +
      // This heading used to read "improvise with these same thresholds and phrasings when
      // asked to check on tabs by hand", which handed the agent a list of sequences with no
      // hint that anything else runs them. It read as a playbook, so the agent executed one
      // by hand — the engine fired the same rule mid-way through, and the human got every
      // directive twice (2026-08-31, `checkpoint_at_context_pressure`). The ruleset is here
      // as a description of what is ALREADY handled, not as instructions.
      `Standing doctrine — THE ENGINE RUNS THESE RULES ITSELF, automatically, without you. ` +
      `They are listed so you know what is already being taken care of and can speak about it in the same terms. ` +
      `They are NOT a playbook for you to carry out. Never hand-drive a sequence a rule below already owns: ` +
      `the engine will fire it too, and your human then gets every directive twice. ` +
      `Borrow their thresholds and phrasing only when checking on something by hand that no rule covers:\n` +
      `${ruleLines || '  (no rules enabled yet)'}\n\n` +
      `Nothing to do right now? Check in with your human briefly, then stay silent until an escalation or instruction arrives.`
    );
  }

  /** Prime the Overlord agent tab with its doctrine — idempotent within a session and
   *  across restarts (persisted var), mirroring the mesh tryPrime mechanics. */
  const primedAgents = new Set<string>();
  async function tryPrimeOverlordAgent() {
    const ws = overlordWorkspace();
    if (!ws) return;
    for (const pane of ws.panes) {
      for (const tab of pane.tabs) {
        if ((tab.tab_type ?? 'terminal') !== 'terminal' || !tab.runtime) continue;
        if (primedAgents.has(tab.id)) continue;
        if (mappedState(tab.id) !== 'idle') continue;
        primedAgents.add(tab.id); // mark before await so a racing tick can't double-prime
        if (getVariables(tab.id)?.get(OVERLORD_PRIMED_VAR) === DOCTRINE_VERSION) continue;
        if (!(await hasLiveRepl(tab.id))) { primedAgents.delete(tab.id); continue; }
        const inst = terminalsStore.get(tab.id);
        if (!inst) { primedAgents.delete(tab.id); continue; }
        try {
          await bracketedPasteSubmit(inst.ptyId, buildDoctrine());
          await setVariable(tab.id, OVERLORD_PRIMED_VAR, DOCTRINE_VERSION);
          logInfo(`overlord: primed agent tab ${tab.id.slice(0, 8)} with doctrine`);
        } catch (e) {
          primedAgents.delete(tab.id);
          logError(`overlord: agent priming failed: ${e}`);
        }
        return;
      }
    }
  }

  /** Is this tab the Overlord agent (a tab inside the Overlord workspace)? Gates the
   *  supervisor-only MCP tools — supervised agents never get them. */
  function isOverlordAgentTab(tabId: string): boolean {
    return workspaceForTab(tabId)?.overlord === true;
  }

  /**
   * Run a rule's sequence on a tab because a human asked — the Checkpoint button, the
   * fleet card's Trigger menu, the composer bar's.
   *
   * Bypasses the rate limiters and the `when` clause — `cooldown` and `max_per_hour` exist
   * to stop the ENGINE nagging, and a human clicking is the override they are guarding
   * against being unable to make. The mechanical guards are NOT bypassed: `runSequence`
   * still re-checks the live REPL at every step and `waitInjectable` still holds for the
   * agent-state and quiet window, so clicking while the agent is mid-turn queues the
   * sequence rather than typing over its output.
   *
   * The readiness test depends on the rule. An `agent_unready` rule is FOR an unbound tab —
   * its sequence re-binds one — so fired at a bound agent it would type `/maiterm init` into
   * a session that already has one, and fired at a stopped tab it would type into bash.
   * Every other rule needs the agent bound and running. Same distinction the deck and
   * driveTab make: an unbound tab is not a dead one.
   */
  async function fireRuleNow(rule: OverlordRule, tabId: string): Promise<{ started: boolean; reason?: string }> {
    if (rituals.has(tabId)) return { started: false, reason: 'already_running' };
    if (outstanding.has(tabId)) return { started: false, reason: 'outstanding' };
    const repl = await replState(tabId);
    const want: ReplState = rule.when.event === 'agent_unready' ? 'unbound' : 'ready';
    if (repl !== want) return { started: false, reason: `tab_${repl}` };
    if (permissionBlocks(rule, tabId)) return { started: false, reason: 'tab_permission' };
    void runSequence($state.snapshot(rule) as OverlordRule, tabId, 'human');
    logInfo(`overlord: manual fire of "${rule.name}" on ${tabId.slice(0, 8)}`);
    return { started: true };
  }

  /** Run a rule's sequence against a tab. All ledger writes for the run happen here. */
  async function runSequence(rule: OverlordRule, tabId: string, origin: OverlordLedgerEntry['origin']) {
    if (rituals.has(tabId)) return;
    const run: RitualRun = {
      runId: crypto.randomUUID(),
      ruleId: rule.id,
      ruleName: rule.name,
      tabId,
      targetsUnready: rule.when.event === 'agent_unready',
      stepIndex: 0,
      stepCount: rule.sequence.length,
      startedAt: Date.now(),
      aborted: false,
      lastInjectionAt: Date.now(),
    };
    rituals.set(tabId, run);
    bumpLive();
    const key = `${rule.id}|${tabId}`;
    lastFiredAt.set(key, Date.now());
    fireLog.set(key, [...(fireLog.get(key) ?? []), Date.now()]);
    const runtime = workspacesStore.getTabRuntime(tabId);
    try {
      for (let i = 0; i < rule.sequence.length; i++) {
        const step = rule.sequence[i];
        // A step with no text would still submit — `bracketedPasteSubmit` wraps the empty
        // string and presses Enter, sending whatever the human had half-typed at the agent.
        // The rules editor persists "New rule" with one blank step before anything is typed
        // into it, so this is a state every ruleset passes through, not a corrupt one.
        if (!step.text.trim()) {
          logWarn(`overlord: rule "${rule.name}" step ${i + 1} has no text; skipped`);
          continue;
        }
        // Runtime capability check (§6): a slash command a runtime doesn't know lands
        // as garbage in a live REPL — skip and ledger, never inject.
        if (step.kind === 'slash' && step.runtimes && !step.runtimes.includes(runtime)) {
          ledger(tabId, rule.id, origin, i, step, 'skipped_runtime');
          continue;
        }
        // Re-verify the hard precondition at each injection, not just at fire time.
        if (rule.guards.require_live_repl && !(await hasLiveRepl(tabId))) {
          ledger(tabId, rule.id, origin, i, step, 'blocked_no_repl');
          return;
        }
        if (!(await waitInjectable(run, rule.guards))) {
          ledger(tabId, rule.id, origin, i, step, 'aborted');
          return;
        }
        if (humanTypedSince(tabId, run.lastInjectionAt)) {
          // Human typed into this tab since our last injection — their tab, their turn (§7).
          ledger(tabId, rule.id, origin, i, step, 'aborted');
          return;
        }
        // Exempted since the run started. The tick's sweep aborts the run too, but the wait
        // just above can hold for minutes and return the instant the agent goes quiet — the
        // human who exempted the tab while watching Overlord type into it gets the next
        // step anyway unless this sits AFTER the wait and directly before the paste.
        if (isExemptTab(tabId)) {
          ledger(tabId, rule.id, origin, i, step, 'aborted');
          return;
        }
        // Stopped at a permission prompt. `driveTab` refuses this exact state — "there is
        // nothing to type a directive into" — and it is right: a permission prompt is a
        // keystroke menu, so pasted prose is swallowed or corrupts the selection. Rules
        // reached the opposite verdict only because a `permission_pending` rule must list
        // `permission` in agent_state to fire at all, and waitInjectable then accepted it.
        //
        // One injection tool, one verdict (§2). The engine can't answer a prompt — that
        // needs a runtime-specific keystroke and a judgement about consequence — so it
        // hands the tab to the agent, which has getTabPrompt/answerTabPrompt for exactly
        // this. That is also what makes `permission_pending` rules useful rather than
        // merely fireable: the rule becomes "if a permission sits this long, wake the
        // supervisor", which is the thing the human actually wanted.
        if (mappedState(tabId) === 'permission') {
          ledger(tabId, rule.id, origin, i, step, 'blocked_guard');
          const escalationId = escalate(
            tabId,
            rule.id,
            'permission_stuck',
            `"${rule.name}" fired on ${tabDisplayName(tabId)}, which is stopped at a prompt — ` +
              `nothing can be typed there. Call getTabPrompt on that tab, then answerTabPrompt ` +
              `(escalate to the human first if the decision is consequential). The rule wanted ` +
              `to say: ${JSON.stringify(step.text.slice(0, 160))}.`,
          );
          recordPermissionHandoff(tabId, escalationId, rule.id);
          return;
        }
        const inst = terminalsStore.get(tabId);
        if (!inst) {
          ledger(tabId, rule.id, origin, i, step, 'blocked_no_repl');
          return;
        }
        try {
          await bracketedPasteSubmit(inst.ptyId, step.text);
        } catch (e) {
          logError(`overlord: injection failed for tab ${tabId.slice(0, 8)}: ${e}`);
          ledger(tabId, rule.id, origin, i, step, 'blocked_guard');
          return;
        }
        run.lastInjectionAt = Date.now();
        const directive: OutstandingDirective = {
          id: crypto.randomUUID(),
          ruleId: rule.id,
          tabId,
          stepIndex: i,
          text: step.text,
          sentAt: run.lastInjectionAt,
          acked: false,
        };
        if (step.await) setOutstanding(tabId, directive);
        run.stepIndex = i;
        bumpLive();
        ledger(tabId, rule.id, origin, i, step, 'sent');
        const res = await awaitGate(run, step, directive);
        // A rule re-binding an unready tab gets the same verification `recoverTab` gets:
        // both type the identical line at the identical tab for the identical reason, and
        // only one of them was watching, so a rule-driven re-bind that never took ended at
        // a silent `timed_out` (`reinit_unbound_agent`'s `on_timeout` is `continue`).
        //
        // Two things this must NOT do, both found in review. It is keyed on the text typed,
        // not on `run.targetsUnready` — that flag is the rule's EVENT and says nothing about
        // the step, so a custom agent_unready rule with any other prose would have been
        // watched for a binding it could never produce, then declared failed. That verdict
        // is not cosmetic: `rebindFailed` pins the tab to `stopped`, which is the one state
        // that stops `reinit_unbound_agent` from ever firing there again and turns the card's
        // only remaining action into a resume typed at a live agent.
        //
        // And it arms AFTER this step's gate rather than at injection, so the rule's own
        // tolerance gets the first say. Armed at injection it set a 45s verdict against a
        // step the rule gives 120s, and an init turn slow to reach its initSession call —
        // rate-limit backoff, a remote agent still replaying its transcript — was declared
        // failed while the ritual was still well inside its budget and about to succeed.
        //
        // `aborted` is excluded, and it is the branch that made this guarantee a half one.
        // A gate aborts when the HUMAN types into the tab — likely, when they are looking at
        // the tab they just watched a command appear in — and the init then never got the
        // tolerance this line was moved here to give it. Arming anyway declared a re-bind
        // failed 45s after an interruption, which pins the tab to `stopped` and offers a
        // resume typed at a live agent. Only a gate that ran to its own conclusion, or that
        // timed out having given the step its full budget, has anything to say about whether
        // the binding took. Trimmed, because the step text is a free textarea in the rules
        // editor and a trailing space would silently drop the verification while still
        // re-binding — the exact silent-failure this whole path exists to end.
        if (res !== 'aborted' && step.text.trim() === REBIND_COMMAND) rebindWatch.set(tabId, Date.now());
        if (outstanding.get(tabId)?.id === directive.id) clearOutstanding(tabId);
        if (res === 'aborted') {
          ledger(tabId, rule.id, origin, i, step, 'aborted');
          return;
        }
        if (res === 'timeout') {
          ledger(tabId, rule.id, origin, i, step, 'timed_out');
          const behavior = step.on_timeout ?? 'abort';
          if (behavior === 'continue') continue;
          if (behavior === 'notify_human') {
            dispatch('Overlord', `"${rule.name}" stalled on ${tabDisplayName(tabId)} — step ${i + 1} timed out.`, 'error', { tabId });
          } else if (behavior === 'escalate_to_overlord') {
            escalate(tabId, rule.id, 'step_timeout', `"${rule.name}" step ${i + 1} (${step.text.slice(0, 80)}) timed out on ${tabDisplayName(tabId)}.`);
          }
          return;
        }
        if (directive.acked) ledger(tabId, rule.id, origin, i, step, 'acked');
      }
    } finally {
      rituals.delete(tabId);
      if (outstanding.get(tabId)?.ruleId === rule.id) outstanding.delete(tabId);
      bumpLive();
    }
  }

  // ── Condition evaluation (§5) ───────────────────────────────────────────────

  /**
   * Conditions describing a STATE, so they still mean something when re-read later.
   *
   * `turn_end` and `commit` describe a moment that has already passed. Re-checking those
   * would invalidate every edge proposal the instant it was queued — they get the age
   * backstop below instead.
   */
  const RECHECKABLE_EVENTS = new Set<OverlordCondition['event']>([
    'context_pct', 'tab_idle', 'task_stale', 'agent_unready',
    'no_todo_list', 'permission_pending', 'directive_unacked',
  ]);

  /** How long an edge proposal stays offerable. Long enough to survive a lunch break;
   *  short enough that a queue left overnight doesn't fire on yesterday's commit, by which
   *  time "review that commit" names something the agent has long since moved past. */
  const PROPOSAL_STALE_MS = 4 * 3_600_000;

  /**
   * Whether a queued proposal still describes reality.
   *
   * A proposal is a SNAPSHOT: it records what was true when the rule matched, and then sits
   * on the board until a human decides. Nothing re-read it. A compaction staged at 23:31,
   * when the tab was genuinely over 55%, was still on the board at 09:58 — after the tab had
   * compacted at 01:38 and dropped to 8% — offering to compact it again. Approving it would
   * have thrown away a tab's entire working context to save nothing.
   *
   * Checked on the tick so the card DISAPPEARS when it stops being true, and again at fire
   * time so the gap between rendering a card and clicking it can't be exploited.
   */
  function proposalStillHolds(p: OverlordProposal, now: number): boolean {
    // Exempted after the card was raised — very plausibly BECAUSE of the card. The sweep
    // drops it on the next tick; this is for "Send it" and "Run all" clicked before then.
    if (isExemptTab(p.tabId)) return false;
    const rule = preferencesStore.overlordRules.find((r) => r.id === p.ruleId);
    // Rule deleted or switched off while the proposal waited.
    if (!rule || !rule.enabled) return false;
    if (!RECHECKABLE_EVENTS.has(rule.when.event)) {
      if (now - p.createdAt > PROPOSAL_STALE_MS) return false;
      // A `commit` proposal names THE last commit. Once another one lands it names the
      // wrong one, and the directive ("review that commit") points at history.
      if (rule.when.event === 'commit') {
        const last = facts.get(p.tabId)?.last_commit_ts;
        if (last !== undefined && last > p.createdAt) return false;
      }
      return true;
    }
    const tab = workspaceForTab(p.tabId)
      ?.panes.flatMap((pane) => pane.tabs)
      .find((t) => t.id === p.tabId);
    if (!tab) return false;
    return conditionFires($state.snapshot(rule) as OverlordRule, tab, now, false, false);
  }

  /** Drop proposals that have stopped being true. Runs on the tick, so the board shows what
   *  is the case now rather than what was the case when the sweep ran. */
  function sweepStaleProposals(now: number) {
    const dead = proposals.filter((p) => !proposalStillHolds(p, now));
    if (!dead.length) return;
    const ids = new Set(dead.map((p) => p.id));
    proposals = proposals.filter((p) => !ids.has(p.id));
    for (const p of dead) {
      logInfo(
        `overlord: dropped stale proposal "${p.ruleName}" for ${p.tabName} ` +
          `(queued ${Math.round((now - p.createdAt) / 60_000)}m ago — no longer applies)`,
      );
    }
  }

  function conditionFires(rule: OverlordRule, tab: Tab, now: number, turnEnded: boolean, committed: boolean): boolean {
    const w = rule.when;
    const f = facts.get(tab.id);
    switch (w.event) {
      case 'context_pct':
        return f?.context_pct !== undefined && f.context_pct >= w.at_or_above;
      case 'turn_end':
        return turnEnded;
      case 'commit':
        return committed;
      case 'tab_idle': {
        if (mappedState(tab.id) !== 'idle') return false;
        const last = f?.last_turn_ts;
        return last !== undefined && now - last >= w.minutes * 60_000;
      }
      case 'task_stale':
        // Parked tasks are exempt. A backlog item is SUPPOSED to sit untouched for months
        // — that is the whole point of having one — so ageing it into a stale signal would
        // make the parking lot a source of directives instead of the thing that keeps them
        // out of the way.
        return tasksForTab(tab.id).some(
          (t) => isInFlight(t) && now - Date.parse(t.updated_at) >= w.days * 86_400_000,
        );
      case 'agent_unready':
        // Narrowed to the case a directive can actually fix: the agent is running but
        // unbound. Firing on a tab sitting at a shell would type `/maiterm init` into
        // bash — noise in the user's terminal, and no closer to recovery.
        return unreadyKind(tab.id) === 'unbound';
      case 'no_todo_list': {
        // "No maiTerm tasks", not "no Claude todos". Reading the runtime's private store
        // meant this could never fire for Codex or Gemini, which have no such store —
        // the rule was silently Claude-only. Against our own tasks it works everywhere.
        //
        // It also removes the ambiguity Claude Code created by deleting its files on
        // completion, which made "finished everything" indistinguishable from "never
        // tracked" and would have nudged an agent to START a list at the moment it
        // finished one. Our finished rows persist, so having ANY task — done included —
        // is proof the tab tracks its work.
        //
        // Overlord's own placeholder rows don't count: a scan stands those up for every
        // running tab, so counting them would suppress the rule everywhere.
        // Parked work doesn't count as tracking what you're doing now — a tab whose only
        // tasks are shelved for next month is, for this rule's purposes, untracked.
        if (tasksForTab(tab.id).some((t) => t.origin !== 'overlord' && !isParked(t.status))) return false;
        if ((f?.context_used ?? 0) < NO_TODO_MIN_CONTEXT_TOKENS) return false;
        return f?.last_turn_ts !== undefined && now - f.last_turn_ts < NO_TODO_RECENT_TURN_MS;
      }
      case 'permission_pending': {
        const since = permissionSince.get(tab.id);
        return since !== undefined && now - since >= w.minutes * 60_000;
      }
      case 'directive_unacked': {
        const d = outstanding.get(tab.id);
        return !!d && !d.acked && now - d.sentAt >= w.minutes * 60_000;
      }
    }
  }

  function propose(rule: OverlordRule, tabId: string) {
    if (proposals.some((p) => p.ruleId === rule.id && p.tabId === tabId)) return;
    const ws = workspaceForTab(tabId);
    proposals = [
      ...proposals,
      {
        id: crypto.randomUUID(),
        ruleId: rule.id,
        ruleName: rule.name,
        tabId,
        tabName: tabDisplayName(tabId),
        workspaceId: ws?.id ?? '',
        workspaceName: ws?.name ?? '',
        preview: rule.sequence[0]?.text ?? '',
        stepCount: rule.sequence.length,
        createdAt: Date.now(),
      },
    ];
    // Stamp the cooldown so an undecided proposal isn't re-raised every tick.
    lastFiredAt.set(`${rule.id}|${tabId}`, Date.now());
    ledger(tabId, rule.id, 'rule', 0, rule.sequence[0] ?? { kind: 'process', text: '' }, 'proposed');
  }

  // ── Board rows (§11) — owned by the tasks store ────────────────────────────
  //
  // Overlord does not own task state; maiTerm does (docs/tasks.md). The engine is one
  // writer among several — the human's panel, agents over MCP, and the Claude task-store
  // importer write the same rows. So every helper below goes through `tasksStore`, which
  // owns persistence: a mutation persists the workspace it touched, and there is no
  // separate "did the board change" bookkeeping to keep in sync.

  /** Every task in this window, tagged with its workspace — the board's flat view. */
  function allTasks(): TaskRow[] {
    const out: TaskRow[] = [];
    for (const ws of workspacesStore.workspaces) {
      for (const t of tasksStore.forWorkspace(ws.id)) out.push({ ...t, workspace_id: ws.id });
    }
    return out;
  }

  /** Every task in this window belonging to `tabId`, regardless of workspace. */
  function tasksForTab(tabId: string): Task[] {
    const ws = workspaceForTab(tabId);
    return ws ? tasksStore.forWorkspace(ws.id).filter((t) => t.tab_id === tabId) : [];
  }

  /** Every task finished: Claude Code sweeps the files, leaving a tracked-but-empty store.
   *  Close out the tab's imported rows instead of leaving them frozen mid-flight — a
   *  half-done row that nothing will ever update becomes a false "stale" card in a few days. */
  function closeOutMirrorRows(tabId: string, now: number): boolean {
    const ws = workspaceForTab(tabId);
    if (!ws) return false;
    const stamp = new Date(now).toISOString();
    return tasksStore.mutate(ws.id, (list) => {
      let changed = false;
      const next = list.map((t) => {
        // Parked rows are left alone: if a human shelved an imported task, closing it out
        // as "done" would erase that decision and claim work happened that didn't.
        if (t.origin === 'imported' && t.tab_id === tabId && isInFlight(t)) {
          changed = true;
          return { ...t, status: 'done' as TaskStatus, updated_at: stamp };
        }
        return t;
      });
      return changed ? next : null;
    });
  }

  /** Import the tab's runtime task list into maiTerm's own store. One-way: rows are marked
   *  `imported` and never written back to the runtime (docs/tasks.md §2). Matching is by
   *  normalized title within the tab, which is also what keeps an agent that ALSO uses
   *  createTasks from double-recording the same work. */
  function syncMirrorTasks(tabId: string, f: OverlordTabFacts, now: number): boolean {
    if (!f.todos) return false;
    const ws = workspaceForTab(tabId);
    if (!ws) return false;
    const stamp = new Date(now).toISOString();
    const items = f.todos.filter((i) => i?.content);
    return tasksStore.mutate(ws.id, (list) => {
      let changed = false;
      const next = [...list];
      for (const item of items) {
        const status = statusFromAgent(item.status, item.blocked);
        // Same dedup helper the MCP createTasks path uses — a tab running BOTH its own
        // runtime todo list and createTasks must converge on one row, not record it twice.
        // Grouping-agnostic on purpose: the runtime's list has no workstreams, but a
        // human may have filed this row into one, and the importer must keep recognizing
        // it or it re-imports the same task every tick.
        const dup = findImportedDuplicate(next, item.content, tabId);
        const idx = dup ? next.indexOf(dup) : -1;
        if (idx >= 0) {
          // The importer only ever drives rows it owns. Once a row belongs to the agent
          // (createTasks) or the human, this must not touch its status: the runtime's
          // private store is a stale copy the agent has been told to stop maintaining, so
          // re-reading it every 5s would drag a task the agent just marked done — or the
          // human just dragged to review — straight back to whatever the file still says.
          if (next[idx].origin !== 'imported') continue;
          // A RETIRED row is closed out and stays closed out, even though the importer owns
          // it. The origin rule asks "who may drive this row"; this asks "is this row still
          // running at all", and the runtime's private store cannot answer that — it is a
          // stale file the agent was told to stop maintaining, so it still lists the item
          // as pending. Without this the 5s tick drags a `dropped` row straight back to
          // `todo`, which defeats the entire point of retracting one: the agent drops work
          // it decided against and the board re-files it as live, forever. Same defect for
          // `done`, where it re-opened a row somebody had just closed.
          if (isRetired(next[idx].status)) continue;
          // Reclaim a row this session left in the backlog when its previous tab closed.
          // Without taking ownership back, every completion path stays out of reach —
          // closeOutMirrorRows, the present-set sweep and tasksForTab are all tab-scoped —
          // so the list would sit unassigned and "active" forever while the tab that owns
          // it is simultaneously judged untracked and nagged to start a list.
          const reclaimed = !next[idx].tab_id;
          if (reclaimed || next[idx].status !== status) {
            next[idx] = { ...next[idx], tab_id: tabId, status, updated_at: stamp };
            changed = true;
          }
        } else {
          next.push(makeTask({ title: item.content, status, tab_id: tabId, origin: 'imported' }, stamp));
          changed = true;
        }
      }
      // The runtime's list is authoritative for ITS OWN rows: an imported item that is no
      // longer on it is finished as far as the agent is concerned. Only `imported` rows
      // are swept — a human's or another agent's task must never vanish because some
      // runtime rewrote its private list.
      // Titles the runtime still lists — imported rows outside this set are finished.
      const present = new Set(items.map((i) => normalizeTitle(i.content)));
      for (let i = 0; i < next.length; i++) {
        const t = next[i];
        if (
          t.origin === 'imported' &&
          t.tab_id === tabId &&
          isInFlight(t) &&
          !present.has(t.normalized_title)
        ) {
          next[i] = { ...t, status: 'done' as TaskStatus, updated_at: stamp };
          changed = true;
        }
      }
      return changed ? next : null;
    });
  }

  /** Age out RETIRED rows so the board doesn't accumulate history — finished (`done`) and
   *  retracted (`dropped`) alike. Both are off the board for good, and a retracted row has
   *  even less reason to linger than a finished one: it records work that never happened.
   *
   *  Human-authored tasks are exempt: someone typed those, and silently deleting them two
   *  days later is a surprise. Everything machine-authored is swept — imported mirrors,
   *  agent-created tasks, and Overlord's own placeholders alike. */
  function sweepRetiredTasks(now: number): boolean {
    let changed = false;
    for (const ws of workspacesStore.workspaces) {
      const swept = tasksStore.mutate(ws.id, (list) => {
        const next = list.filter(
          (t) =>
            !isRetired(t.status) ||
            t.origin === 'human' ||
            now - Date.parse(t.updated_at) < TASK_DONE_RETENTION_MS,
        );
        // (Parked rows are never retired, so the sweep cannot reach them — a shelved idea
        // must survive indefinitely or the backlog stops being a place to put things.)
        return next.length === list.length ? null : next;
      });
      changed = changed || swept;
    }
    return changed;
  }

  // ── Scan & census: populating the board from what's already running ─────────
  //
  // The board's automatic feeders are the agent itself (createTasks) and the Claude-store
  // importer. A tab that has learned neither — a Codex tab that was never primed, a tab
  // working without recording anything — is invisible. Scanning closes that gap in two
  // deliberately separate phases:
  //
  //   ADOPT   free, silent, safe to repeat — import a runtime's own list where one exists,
  //           otherwise stand up one placeholder row per running tab. No injection at all.
  //   ASK     opt-in, one directive per untracked tab, asking it to START RECORDING its
  //           work with createTasks — which then keeps the board current on its own,
  //           forever. Never automatic: writing into 40 transcripts must not be a side
  //           effect of opening a board.
  //
  // Both are idempotent. Adopt matches existing rows on (origin, tab_id) so a re-scan
  // updates instead of duplicating; census stamps a persisted per-tab timestamp so a
  // re-scan never re-asks the same tab inside the cooldown.

  /** Persisted per-tab marker (trigger variable, like MESH_ONBOARDED_VAR) recording when
   *  this tab was last asked to start tracking its work. */
  const TRACK_ASK_VAR = 'overlordTrackAskAt';
  const TRACK_ASK_COOLDOWN_MS = 12 * 60 * 60 * 1000;

  /** "Keep this session" is a decision, so it persists like one — same trigger-variable
   *  mechanism as the track-ask marker, and it holds for a week rather than a tick. */
  const SPENT_KEEP_VAR = 'overlordKeepTabAt';
  const SPENT_KEEP_MS = 7 * 24 * 60 * 60 * 1000;
  /** How long a finished tab must have been quiet before it is offered for archiving.
   *  Finishing the last task is not the moment to suggest packing the session away — the
   *  human is usually still reading the result. */
  const SPENT_IDLE_MS = 30 * 60_000;

  /**
   * Read the Keep marker for a tab that may have no mounted TerminalPane.
   *
   * `getVariables` is backed by a map the trigger store builds on mount and clears on
   * destroy, so for a suspended or never-visited workspace it returns undefined — which is
   * exactly the population this feature offers up, since a finished session is precisely
   * the kind of tab nobody has open. Falling back to the tab's own persisted record is what
   * makes a week-long "keep this" survive the restart it is meant to survive.
   */
  function keptAt(tab: Tab): number {
    const live = getVariables(tab.id)?.get(SPENT_KEEP_VAR);
    const raw = live ?? tab.trigger_variables?.[SPENT_KEEP_VAR];
    const n = Number(raw ?? 0);
    return Number.isFinite(n) ? n : 0;
  }

  /** What a silent tab is asked to do.
   *
   *  Deliberately NOT "tell me in one line what you're working on". A one-line answer is a
   *  snapshot that is stale the moment it arrives, it collapses an agent juggling several
   *  threads of work into a single string, and it has to be re-asked forever. Asking the
   *  agent to record its work as maiTerm tasks costs the same single turn and then feeds
   *  the board continuously and for free — structured, multi-task, always current, and
   *  editable by the human.
   *
   *  This used to ask for the runtime's OWN todo list, which made the census Claude-only:
   *  Codex has no task store and Gemini none at all, so their answer could never reach the
   *  board. createTasks is available to every runtime over the same MCP bridge. */
  const TRACK_REQUEST_TEXT =
    'Overlord check — this tab is doing multi-step work that is not on the task list, so ' +
    'nothing here is being tracked. Please call createTasks now with what you are actually ' +
    'working on (several items if several things are in flight), and keep the statuses ' +
    'current with updateTasks as you go. Then carry on with what you were doing — nothing ' +
    "else is needed. If you have no task tools, instead call replyToOverlord once with " +
    "kind:'status' and `task` set to a one-line description.";

  /** Find this tab's Overlord-owned board row, in ANY state. Matching must include
   *  `done`: filtering it out made "mark done" un-sticky — the next scan couldn't see the
   *  finished row and pushed a fresh duplicate beside it. */
  function overlordRowFor(tabId: string): Task | undefined {
    return tasksForTab(tabId).find((t) => t.origin === 'overlord');
  }

  /** Is this tab's real work already on the board — recorded by the agent (createTasks) or
   *  imported from its runtime's own list? Once it is, the scan's placeholder is redundant:
   *  the tab's work is there in detail, and a stand-in titled with the tab name is noise. */
  function hasMirrorRows(tabId: string): boolean {
    // Must use the SAME test as the scan's `tracked` and `no_todo_list`. When it didn't,
    // a tab whose only tasks were parked was judged untracked by those two (so it got
    // nudged to start tracking) while this said it was represented — so its census answer
    // was thrown away and it was re-nudged every 12 hours, forever.
    return tasksForTab(tabId).some((t) => t.origin !== 'overlord' && isInFlight(t));
  }

  /** Board rows are only rendered for non-Overlord workspaces, so creating one for the
   *  supervisor's own tab produces an invisible row that still counts in the segment
   *  badge and eventually surfaces as a phantom "stale" card. */
  function isBoardableTab(tabId: string): boolean {
    const ws = workspaceForTab(tabId);
    return !!ws && !ws.overlord && !isExemptTab(tabId);
  }

  /** SCAN path: stand up a placeholder row for a running tab, or keep an existing one
   *  fresh. Deliberately does NOT rewrite `state` or `title` on an existing row — a scan
   *  is an observation, and silently reverting a card the human dragged to `review`
   *  (or retitled by answering a census) makes the board untrustworthy. */
  function adoptRow(tabId: string, tabName: string, live: AgentState): 'created' | 'refreshed' | 'skipped' {
    if (!isBoardableTab(tabId)) return 'skipped';
    const ws = workspaceForTab(tabId);
    if (!ws) return 'skipped';
    const existing = overlordRowFor(tabId);
    if (existing) {
      // A closed-out row stays closed out — finished or retracted alike; a scan is an
      // observation and must not resurrect either.
      if (isRetired(existing.status)) return 'skipped';
      // The placeholder's title is a COPY of the tab name taken at first scan, so renaming
      // the tab left the board showing a name that exists nowhere else in the app — every
      // other surface (ledger, triage chips, task tab-chips) resolves live through
      // tabDisplayName. Re-sync it here.
      //
      // Unless the tab has since REPORTED: titleRow replaces the placeholder with the
      // agent's own one-line summary, and it is written alongside `agentReports`, so that
      // map is the reliable "this title is real work, not a stand-in" marker. Clobbering it
      // with a tab name would throw away the only thing the agent said about itself.
      const patch: Partial<Task> = {};
      if (existing.origin === 'overlord' && !agentReports.has(tabId) && existing.title !== tabName) {
        patch.title = tabName;
      }
      // Bump recency so a tab that is demonstrably alive never ages into "stale".
      tasksStore.update(ws.id, existing.id, patch);
      return 'refreshed';
    }
    tasksStore.add(ws.id, {
      title: tabName,
      status: taskStateForAgent(live),
      tab_id: tabId,
      origin: 'overlord',
    });
    return 'created';
  }

  /** REPLY path: a runtime with no task tool answered via replyToOverlord, so retitle its
   *  placeholder
   *  (or create one). Never touches a row the human already finished. */
  function titleRow(tabId: string, title: string): boolean {
    if (!isBoardableTab(tabId)) return false;
    // If the mirror already carries this tab's real todos, a one-line summary row on top
    // of them is noise, not information.
    if (hasMirrorRows(tabId)) return false;
    const ws = workspaceForTab(tabId);
    if (!ws) return false;
    const existing = overlordRowFor(tabId);
    if (existing) {
      if (isRetired(existing.status) || existing.title === title) return false;
      return tasksStore.update(ws.id, existing.id, { title });
    }
    tasksStore.add(ws.id, {
      title,
      status: taskStateForAgent(mappedState(tabId)),
      tab_id: tabId,
      origin: 'overlord',
    });
    return true;
  }

  /** Retire a tab's placeholder once its real tasks have taken over. Without this a tab
   *  that starts tracking after being scanned keeps its stand-in row forever — frozen, then
   *  permanently "stale", and a live target for task_stale rules. */
  function retirePlaceholderIfMirrored(tabId: string): boolean {
    if (!hasMirrorRows(tabId)) return false;
    const ws = workspaceForTab(tabId);
    const existing = overlordRowFor(tabId);
    if (!ws || !existing || isRetired(existing.status)) return false;
    return tasksStore.remove(ws.id, existing.id);
  }

  /** Tabs that were agents, still have a live PTY, but report no agent state. */
  function dormantCandidates(): { tab: Tab; ptyId: string }[] {
    const out: { tab: Tab; ptyId: string }[] = [];
    for (const { tab } of agentTabs()) {
      if (claudeStateStore.getState(tab.id)) continue;
      const inst = terminalsStore.get(tab.id);
      if (inst) out.push({ tab, ptyId: inst.ptyId });
    }
    return out;
  }

  /** Refresh the dormancy classification. Batched into one pass and only for candidates —
   *  this walks the process tree, and doing it per tab per tick is the shape that froze
   *  the UI once already (mesh readiness pinwheel). */
  async function probeLiveness(now: number) {
    const candidates = dormantCandidates();
    // Drop entries for tabs that are no longer candidates, so a recovered tab stops
    // being reported as unready the moment its agent comes back.
    const live = new Set(candidates.map((c) => c.tab.id));
    // `liveness` is a plain Map, so every mutation path has to bump the reactivity
    // counter itself. Without it the deck's classification — and the count on the run-all
    // button, which is a promise about what the click will do — only refreshed when some
    // unrelated state happened to change.
    let changed = false;
    for (const id of [...liveness.keys()]) {
      if (!live.has(id)) { liveness.delete(id); changed = true; }
    }
    // A tab that came back (or closed) settles its re-bind verdict — it is no longer a
    // candidate, so nothing below would ever clear these.
    for (const id of [...rebindWatch.keys()]) if (!live.has(id)) rebindWatch.delete(id);
    for (const id of [...rebindFailed]) if (!live.has(id)) rebindFailed.delete(id);
    // Re-binds that never landed. `/maiterm init` is a cheap, safe thing to type at an
    // agent that is running unbound — and a no-op typed at a shell prompt, which is what
    // an SSH tab whose REMOTE agent has exited looks like from here (see the classification
    // below). Waiting for the outcome is the only way to tell those apart from this side.
    for (const [id, sentAt] of [...rebindWatch]) {
      if (now - sentAt < REBIND_VERIFY_MS) continue;
      rebindWatch.delete(id);
      rebindFailed.add(id);
      changed = true;
      logInfo(`overlord: re-bind on ${id.slice(0, 8)} did not take — reclassifying as stopped`);
      // Carry the verdict back to whoever asked for the recovery. `recoverTab` can only ever
      // report that it TYPED `/maiterm init`; whether the far side was in a state to receive
      // it is knowable only here, seconds later. Without this the caller keeps `sent: true`
      // and nothing ever corrects it — the Payment Server tab sat unbound for 1h45m on
      // 2026-08-29 after a recovery that reported success, and only came back when an app
      // restart respawned it. A recovery that silently didn't happen is worse than one that
      // fails loudly.
      escalate(
        id,
        null,
        'rebind_failed',
        `The /maiterm init sent to ${tabDisplayName(id)} did not take — ${REBIND_VERIFY_MS / 1000}s later ` +
          `that tab still has not registered, so it is NOT recovered however the recoverTab call read. ` +
          `It is now classified 'stopped' rather than 'unbound' on the evidence: the cheap remedy was ` +
          `typed and watched, and nothing happened. Common causes are an agent still mid-resume when the ` +
          `line was typed (especially over SSH, where a remote agent that is replaying its transcript ` +
          `swallows it), or a remote agent that has actually exited, leaving ssh in the foreground at a ` +
          `shell prompt. Call recoverTab on it again — now that it reads 'stopped' that types the ` +
          `runtime's resume command and relaunches the agent, rather than re-typing an init that has ` +
          `already been shown not to work.`,
      );
    }
    if (!candidates.length) {
      if (changed) bumpLive();
      return;
    }
    try {
      const res = await commands.getAgentLivenessBatch(candidates.map((c) => c.ptyId));
      for (const { tab, ptyId } of candidates) {
        const l = res[ptyId];
        if (!l) continue;
        // ssh_foreground stands in for a remote agent we cannot see in the local process
        // tree — an SSH tab with a live session is treated as unbound, which is the case
        // that actually happens after a restart.
        //
        // But it is a GUESS, and a systematically wrong one for a whole class: `ssh` being
        // the foreground job says nothing about what runs on the far side, so an SSH tab
        // sitting at a REMOTE SHELL PROMPT — agent long gone — is indistinguishable from a
        // live remote agent that merely lost its binding. Both read `unbound`, both get
        // `/maiterm init`, and for the dead one that is a line of junk typed at bash. It
        // can never be classified `stopped`, so the remedy that would actually fix it
        // (replay auto-resume) is unreachable, and every run-all re-types the same no-op.
        //
        // `rebindFailed` is the correction, and it is evidence rather than inference: we
        // typed the cheap remedy, watched, and it did not take. Verdict wins over the
        // guess until the tab is no longer dormant.
        const kind: UnreadyKind = rebindFailed.has(tab.id)
          ? 'stopped'
          : l.agent_running || l.ssh_foreground
            ? 'unbound'
            : 'stopped';
        if (liveness.get(tab.id)?.kind !== kind) changed = true;
        liveness.set(tab.id, { kind, at: now });
      }
    } catch (e) {
      logError(`overlord: liveness probe failed: ${e}`);
    }
    if (changed) bumpLive();
  }

  /**
   * The triage worklist, shared by the run-all button's label and the run itself.
   *
   * `superseded` is the collision that made this a shared function: the default
   * `reinit_unbound_agent` rule fires on the same `agent_unready` signal the re-bind reads,
   * so every unbound tab produces BOTH a re-bind job and a proposal whose sequence is the
   * identical `/maiterm init`. Running both types it twice — or, once the re-bind lands and
   * the tab is no longer unready, leaves `waitInjectable` spinning for its full five-minute
   * cap while holding the tab's ritual lock, blocking every `only_if_no_outstanding` rule
   * and stalling the run's own drain check on rituals that will never inject.
   *
   * The re-bind wins: it is the same remedy delivered by the path that actually works on an
   * unbound tab (it bypasses guards that are false by definition there).
   */
  function triageJobs(): { rebinds: string[]; proposalIds: string[]; superseded: string[] } {
    const rebinds: string[] = [];
    for (const [tabId, e] of liveness) if (e.kind === 'unbound') rebinds.push(tabId);
    const rebinding = new Set(rebinds);
    const proposalIds: string[] = [];
    const superseded: string[] = [];
    for (const p of proposals) {
      const rule = preferencesStore.overlordRules.find((r) => r.id === p.ruleId);
      if (rebinding.has(p.tabId) && rule?.when.event === 'agent_unready') {
        superseded.push(p.id);
        continue;
      }
      proposalIds.push(p.id);
    }
    return { rebinds, proposalIds, superseded };
  }

  /** The enabled `context_pct` rule that would checkpoint this tab — lowest threshold
   *  first, since that is the one that fires. */
  function checkpointRuleFor(tabId: string): OverlordRule | null {
    const ws = workspaceForTab(tabId);
    let best: OverlordRule | null = null;
    for (const r of preferencesStore.overlordRules) {
      if (!r.enabled || r.when.event !== 'context_pct' || !hasRunnableSequence(r)) continue;
      if (r.workspaces.length && (!ws || !r.workspaces.includes(ws.id))) continue;
      const at = r.when.at_or_above;
      if (!best || at < (best.when as { at_or_above: number }).at_or_above) best = r;
    }
    return best;
  }

  /**
   * Deliver any answers to directives Overlord typed — the return leg of `driveTab`.
   *
   * A tab has answered when its `last_turn_ts` has moved past the baseline we recorded at
   * send time AND it is no longer mid-turn (harvesting while `active` would capture half a
   * thought). The reply is queued as an escalation so it rides the existing doorbell and
   * `listEscalations` plumbing, tagged `drive_reply` so the human's deck ignores it: an
   * agent answering a question is not a problem needing triage.
   */
  async function harvestDriveReplies(now: number) {
    for (const [tabId, w] of [...driveWatch]) {
      const st = mappedState(tabId);

      // Stopped at a permission prompt. This is NOT an answer, and it is the case the
      // naive "turn moved on" test gets wrong: the tool_use block that RAISED the prompt
      // is itself an assistant turn (see real_turn_ts), so `last_turn_ts` has already
      // advanced while the agent sits blocked. Harvesting here would capture the
      // half-sentence before the tool call, drop the watch, and guarantee the real reply
      // is never seen.
      //
      // Overlord also has to be TOLD. It cannot answer a permission prompt — that is what
      // the prompt exists to prevent — but a supervisor that silently waits forever on a
      // tab stopped behind a gate is the failure this whole return leg exists to remove.
      if (st === 'permission') {
        if (!w.permissionNotified) {
          w.permissionNotified = true;
          const escalationId = escalate(
            tabId,
            null,
            'permission_stuck',
            `${tabDisplayName(tabId)} is stopped at a prompt and cannot continue until it is ` +
              `answered. It was working on your directive ${JSON.stringify(w.text.slice(0, 120))}. ` +
              `Call getTabPrompt on that tab to see what it is asking, then answerTabPrompt — ` +
              `unless the decision is consequential (destructive or irreversible, money, ` +
              `credentials, production, an external party, or a question about what the human ` +
              `WANTS rather than how to do what they already asked), in which case put it to ` +
              `the human with AskUserQuestion first. Its reply reaches you once it finishes. ` +
              `If the human answers the prompt in the tab before you get to it, you will be ` +
              `told to stand down — so do not treat this as a question you must resolve.`,
          );
          recordPermissionHandoff(tabId, escalationId, null);
        }
        // Pause the give-up clock. A prompt can sit for hours, and the directive is not
        // stale — it is waiting on a human. Capped at `now`, so the worst case after a long
        // block is a fresh full window rather than an instant expiry the moment it resumes.
        w.sentAt = Math.min(now, w.sentAt + TICK_MS);
        continue;
      }
      // Left the gate — so a LATER gate on the same directive is a new event that must be
      // reported again. One directive routinely trips several ("run the tests" → approve
      // npm, then approve git commit); latching the flag for the directive's lifetime meant
      // only the first was ever escalated, and since the branch above also freezes the
      // give-up clock, the tab sat at gate two forever with Overlord never told and doctrine
      // telling it not to poll.
      w.permissionNotified = false;

      if (now - w.sentAt > DRIVE_WATCH_MS) {
        driveWatch.delete(tabId);
        // Release the slot HERE, not a second later. `driveTab` arms a `setTimeout` for
        // `DRIVE_WATCH_MS + 1000` to do this, which leaves a window — about one tick in five
        // — where the watch is gone but the directive is not: the tick's unacked check three
        // lines down then sees no drive watch, no ritual, and a directive 15 min old, and
        // raises `directive_unacked` as a SECOND card about the directive `drive_reply` has
        // just reported. Same text-equality guard the timeout uses, so a directive sent
        // since this watch began is left alone. The timeout stays as the backstop for paths
        // where this loop does not run (Overlord switched off mid-flight).
        const od = outstanding.get(tabId);
        if (od && od.text === w.text) clearOutstanding(tabId);
        // Never expire silently. The doctrine promises "you WILL get the answer back", so a
        // watch that gives up owes the supervisor a word — otherwise it waits forever on a
        // reply that is never coming.
        //
        // It owes an OBSERVATION, not a theory. This used to assert one cause every time —
        // "its transcript may not be readable from this machine (an SSH tab's transcript
        // lives on the remote host)" — whichever of the three had actually happened. That
        // guess was read as a finding: it reached the supervisor as the explanation, was
        // relayed to the human as fact, and became the stated rationale for a rule change,
        // all without anything ever checking whether that tab's transcript was readable.
        // Everything needed to tell the cases apart is right here.
        const ts = facts.get(tabId)?.last_turn_ts;
        const why = !ts
          ? `This machine has no readable transcript for that tab, so no reply of any kind ` +
            `can be read from it. On an SSH tab the transcript lives on the remote host and ` +
            `is mirrored here only for Claude, and only while that tab's bridge is up.`
          : w.reads === 0
            ? `Its transcript is readable here, and it has recorded no turn since the ` +
              `directive was typed — so the directive may never have landed in its input, ` +
              `or nothing is running in that tab.`
            : w.readErrors === w.reads
              ? `Its transcript moved, but every attempt to read the reply failed.`
              : `Its transcript moved and was read ${w.reads} time${w.reads === 1 ? '' : 's'}, ` +
                `but no reply text came back — it may have answered with tool calls only, or ` +
                `be narrating in a form the reader skips.`;
        escalate(
          tabId,
          null,
          'drive_reply',
          `No reply could be read from ${tabDisplayName(tabId)} for your directive ` +
            `${JSON.stringify(w.text.slice(0, 120))}. ${why} Do not keep waiting — ask the ` +
            `tab directly with driveTab, and tell it to answer you with replyToOverlord.`,
        );
        logInfo(
          `overlord: drive watch expired unread for ${tabId.slice(0, 8)} ` +
            `(last_turn_ts=${ts ?? 'none'} reads=${w.reads} errors=${w.readErrors})`,
        );
        continue;
      }
      const ts = facts.get(tabId)?.last_turn_ts;
      if (!ts || ts <= w.baseline) continue;
      if (st === 'active') continue;

      let reply: string | null = null;
      let readFailed = false;
      w.reads++;
      try {
        reply = await commands.getAgentReplySince(tabId, w.baseline);
      } catch (e) {
        readFailed = true;
        w.readErrors++;
        logError(`overlord: reply read failed for ${tabId.slice(0, 8)}: ${e}`);
      }
      // KEEP the watch on an empty or failed read and try again next tick. A turn can be
      // recorded before the agent's prose lands (the pasted directive is itself a user turn,
      // so `last_turn_ts` moves on delivery), and deleting here on the first unproductive
      // read permanently ended the return leg for a reply that arrived seconds later.
      if (readFailed || !reply?.trim()) continue;
      driveWatch.delete(tabId);

      // The answer is proof the directive completed. Clearing here matters beyond
      // tidiness: a driveTab directive otherwise holds the tab's outstanding slot for a
      // full 15 minutes after it was already served, blocking every rule with
      // only_if_no_outstanding and every further driveTab at that tab.
      clearOutstanding(tabId);

      // Truncate from the FRONT, keeping the tail. An agent narrates as it works and states
      // its conclusion last, so cutting the end throws away the answer and hands the
      // supervisor the preamble — with no way to fetch the rest, since driving the tab again
      // starts a new turn.
      const clipped =
        reply.length > DRIVE_REPLY_MAX
          ? `[…earlier narration truncated…]\n\n${reply.slice(-DRIVE_REPLY_MAX)}`
          : reply;
      escalate(
        tabId,
        null,
        'drive_reply',
        `${tabDisplayName(tabId)} answered your directive ${JSON.stringify(w.text.slice(0, 120))}:\n\n${clipped}`,
      );
      logInfo(`overlord: harvested reply from ${tabId.slice(0, 8)} (${reply.length} chars)`);
    }
  }

  function unreadyKind(tabId: string): UnreadyKind | null {
    void liveVersion; // see probeLiveness — `liveness` is a plain Map
    return liveness.get(tabId)?.kind ?? null;
  }

  /**
   * Is it SAFE to take this tab out of the window? Returns the refusal, or null to proceed.
   *
   * The safety half of what `spentTabs` asks. The other half — tracked tasks, one of them
   * done, quiet 30 minutes, not marked Keep — is about whether a tab looks FINISHED, which
   * is what makes it worth putting on the deck unprompted. That half is deliberately not
   * applied to `archiveTab`/`closeTab`: the agent is making a judgement the human asked it to
   * make ("this session is definitively over"), and a tab with nothing on the task board can
   * be just as over. What it may never do is take away a tab that is still working.
   *
   * `unbound` is the one that bites: no agent state does NOT mean no agent. An unbound tab's
   * process is alive and merely unregistered, and archiving or closing destroys the
   * TerminalPane, which kills the PTY — so it would silently kill a running agent. Only a tab
   * positively classified `stopped` has actually exited; `null` means not yet classified, and
   * the answer there is to wait, not to guess.
   */
  function retireGuard(
    tabId: string,
    /** Seconds of quiet required first. 0 for the deck's `spentTabs`, which applies its own
     *  far stricter 30-minute idle test; non-zero for the agent's tools, which do not. */
    quietMs = 0,
    /** May a tab with no live terminal be retired? The deck says no — it only OFFERS what it
     *  can see is finished, and a parked tab cannot be probed. The agent's tools say yes: a
     *  suspended tab has no process to kill, which makes it the SAFEST thing to put away, and
     *  refusing it (`not_classified`, "resume its workspace and retry") sent the agent off to
     *  respawn a session purely so it could wait for it to idle and then kill it again. */
    allowParked = false,
  ): { reason: string; detail: string } | null {
    if (isExemptTab(tabId)) return { reason: 'exempt', detail: EXEMPT_DETAIL };
    if (!isBoardableTab(tabId)) {
      return {
        reason: 'not_boardable',
        detail: 'That tab is not in a supervised workspace in this window (or it is the Overlord agent\'s own tab).',
      };
    }
    if (outstanding.has(tabId) || rituals.has(tabId)) {
      return { reason: 'outstanding_directive', detail: 'That tab still owes an answer to a directive. Let it land first.' };
    }
    const inst = terminalsStore.get(tabId);
    if (!inst && !terminalsStore.isSpawning(tabId)) {
      // Nothing is running: no PTY to kill, no agent to end. Every check below exists to
      // protect a live process, so they have nothing to say about this tab.
      if (allowParked) return null;
      return {
        reason: 'not_classified',
        detail: 'That tab has no live terminal, so nothing could probe it. Resume it (resumeTab) if you need to see what it was doing.',
      };
    }
    // Output still arriving, or the human still typing. `stopped` means no AGENT process —
    // which is also exactly what a shell running a build looks like, and closeTab has no
    // undo. The deck never needed this because it demands 30 minutes of quiet first.
    if (quietMs > 0) {
      const now = Date.now();
      const busySince = Math.max(
        terminalsStore.getLastOutputAt(tabId) ?? 0,
        terminalsStore.getLastUserInputAt(tabId) ?? 0,
      );
      if (busySince && now - busySince < quietMs) {
        return {
          reason: 'tab_in_use',
          detail: `That terminal produced output or took a keystroke ${Math.round((now - busySince) / 1000)}s ago — something is running in it, or somebody is using it. A tab with no agent is not necessarily an idle one. Wait until it has been quiet for ${Math.round(quietMs / 1000)}s.`,
        };
      }
    }
    const st = mappedState(tabId);
    if (st === 'active') {
      return { reason: 'agent_busy', detail: 'That agent is mid-turn. Wait for it to finish and retry.' };
    }
    if (st === 'permission') {
      return {
        reason: 'awaiting_permission',
        detail: 'That tab is stopped at a prompt. Answer it (getTabPrompt/answerTabPrompt) before deciding it is finished — the work it was doing is not over, it is waiting.',
      };
    }
    if (!st) {
      const kind = unreadyKind(tabId);
      if (kind === 'unbound') {
        return {
          reason: 'agent_running_unbound',
          detail: 'An agent IS running in that tab; it just has not run /maiterm init. Archiving or closing kills its PTY, so this would end a live session. Use recoverTab to re-bind it, then decide.',
        };
      }
      if (kind !== 'stopped') {
        return {
          reason: 'not_classified',
          detail: 'That tab has a live terminal that has not been classified yet — the liveness probe has not reached it. Retry in a few seconds rather than guessing.',
        };
      }
    }
    return null;
  }

  function taskStateForAgent(st: AgentState | undefined): TaskStatus {
    return st === 'permission' ? 'blocked' : 'active';
  }

  // ── The tick ────────────────────────────────────────────────────────────────

  async function tick() {
    if (ticking) return;
    if (!preferencesStore.overlordEnabled) return;
    ticking = true;
    try {
      const pairs = agentTabs();
      const now = Date.now();
      // 1) Poll facts for every supervised tab (batched; cached server-side).
      if (pairs.length) {
        try {
          const res = await commands.getOverlordTabFacts(pairs.map((p) => p.tab.id));
          facts = new Map(Object.entries(res));
        } catch (e) {
          logError(`overlord: facts poll failed: ${e}`);
        }
      } else {
        facts = new Map();
      }
      // Classify dormant tabs BEFORE rules evaluate, so agent_unready reads this tick's
      // state rather than the previous one's.
      await probeLiveness(now);
      // Then collect answers to directives already sent — this clears outstanding slots,
      // so running it before the rule loop lets a tab that just replied be driven again
      // this tick instead of waiting for the next one.
      await harvestDriveReplies(now);
      for (const { tab, ws } of pairs) {
        const f = facts.get(tab.id);
        const st = mappedState(tab.id);
        // Edge bookkeeping
        const prevSt = prevAgentState.get(tab.id);
        const turnEnded = prevSt === 'active' && st !== 'active' && st !== undefined;
        prevAgentState.set(tab.id, st);
        if (st === 'permission') {
          if (!permissionSince.has(tab.id)) permissionSince.set(tab.id, now);
        } else {
          permissionSince.delete(tab.id);
        }
        const prevCommit = prevCommitTs.get(tab.id);
        const committed =
          f?.last_commit_ts !== undefined &&
          prevCommit !== undefined &&
          f.last_commit_ts !== prevCommit &&
          now - f.last_commit_ts < COMMIT_FRESH_MS;
        if (f?.last_commit_ts !== undefined || prevCommit === undefined) {
          prevCommitTs.set(tab.id, f?.last_commit_ts ?? 0);
        }
        // Hold both edges until a rule spends one or they age out — the guards almost
        // never pass on the same tick the edge appears (see `pendingEdges`).
        const edges = latchEdges(tab.id, now, turnEnded, committed);
        // A driveTab directive with no ritual watching it is spent once the target's
        // turn demonstrably ran (or it was acked) — otherwise it locks the tab.
        const od = outstanding.get(tab.id);
        // The unacked clock only runs while the tab is NOT working. Stamped before the
        // branches below read it, and mutated in place like `acked`/`unackedNotified`.
        if (od && st === 'active') od.lastActiveAt = now;
        const unackedFor = od ? now - Math.max(od.sentAt, od.lastActiveAt ?? 0) : 0;
        if (od && od.ruleId === null && (od.acked || (f?.last_turn_ts !== undefined && f.last_turn_ts > od.sentAt))) {
          clearOutstanding(tab.id);
        } else if (
          od &&
          !od.acked &&
          !od.unackedNotified &&
          !driveWatch.has(tab.id) &&
          !rituals.has(tab.id) &&
          (unackedFor >= DIRECTIVE_UNACKED_MS ||
            now - od.sentAt >= DIRECTIVE_MAX_OUTSTANDING_MS)
        ) {
          // The TTL sweep the design called for on day one. A directive Overlord typed and
          // nobody answered is the supervisor's blind spot: it holds the tab's outstanding
          // slot, blocks every `only_if_no_outstanding` rule behind it, and reports nothing.
          //
          // Three things have to be true, and TWO of them were missing — the card fired at
          // tabs that were demonstrably alive, three times in four days, every one of them
          // working on the very directive it was reported for.
          //
          // - **No drive watch.** A driveTab directive has its own return leg, reporting
          //   both the answer and its own expiry, so escalating here would double-report it.
          // - **No ritual.** This was the real defect. `DIRECTIVE_UNACKED_MS` is 600_000 and
          //   the step gate's own deadline is `(step.timeout_seconds ?? 600) * 1000` — the
          //   SAME 600 seconds. Two independent timers on one directive, racing, and the
          //   one that won reported the more alarming fact (observed firing at 9m58s). A
          //   mid-ritual directive is not a directive with no feedback channel: `awaitGate`
          //   is watching it and the rule author chose its `on_timeout`. Every `ruleId !==
          //   null` directive comes from a ritual, so this is the guard that matters.
          // - **The clock only runs while the tab isn't working** (`lastActiveAt` above),
          //   OR the directive has been outstanding past the absolute ceiling. Not "never
          //   while active": a directive swallowed by a mid-turn paste leaves a tab active
          //   on something else entirely, and that is a live failure class here
          //   (`reinit_unbound_agent` is instrumented, not cured). Measuring idle time still
          //   surfaces it once the tab goes quiet, instead of never.
          //
          //   The ceiling is not belt-and-braces, it is the other half. Review found that
          //   the idle clock alone can be pinned at zero for the life of a tab — see
          //   DIRECTIVE_MAX_OUTSTANDING_MS — and that those are precisely the tabs whose
          //   directive can never clear. Suppressing on idleness alone took away the only
          //   notice the operator got that a tab had jammed.
          //
          // What is left after all this is what the card was always for: a directive with no
          // ritual and no drive watch — the census track-request — that is not going to be
          // answered, either because the tab stopped working or because it has sat far too
          // long.
          od.unackedNotified = true;
          escalate(
            tab.id,
            od.ruleId,
            'directive_unacked',
            `${tabDisplayName(tab.id)} has not acknowledged a directive sent ` +
              `${Math.round((now - od.sentAt) / 60_000)} min ago` +
              (unackedFor >= DIRECTIVE_UNACKED_MS
                ? `, and has not been working for the last ${Math.round(unackedFor / 60_000)} min of that`
                : ` and is still working, which is why this waited`) +
              `: ${JSON.stringify(od.text.slice(0, 160))}. That tab's outstanding slot is ` +
              // Says what actually works. The old text sent the reader to driveTab, which is
              // the one thing that CANNOT work here: it refuses a tab that already owes an
              // answer (`outstanding_directive`). Nothing clears this slot but the tab
              // answering or the tab going away — `ackOutstanding` exists but is wired to no
              // control, so there is no manual release. Better to say so than to send
              // someone round a loop that returns a refusal.
              `held until it answers, so no rule and no driveTab can reach it. If the agent ` +
              `is gone or wedged, reload or close the tab — that is the only thing that ` +
              `releases the slot. If it is alive and simply never acknowledges, it can clear ` +
              `this itself with replyToOverlord kind:'ack'.`,
          );
        }
        // Claude task-store importer — one-way, into maiTerm's own store.
        if (f?.todos?.length) {
          syncMirrorTasks(tab.id, f, now);
          retirePlaceholderIfMirrored(tab.id);
        } else if (f?.tracked) {
          // Tracked but empty — the agent finished its whole list.
          closeOutMirrorRows(tab.id, now);
        }
        // 2) Rule evaluation
        for (const rule of rulesForWorkspace(ws.id)) {
          if (!hasRunnableSequence(rule)) continue;
          if (!conditionFires(rule, tab, now, edges.turnEnded, edges.committed)) continue;
          if (!guardsPassSync(rule, tab.id, now)) continue;
          if (preferencesStore.overlordProposeMode) {
            propose(rule, tab.id);
          } else {
            void runSequence(rule, tab.id, 'rule');
          }
          // Spent, whether it ran or was proposed: a proposal the human sits on must not
          // re-propose itself every 5 seconds for the life of the latch.
          consumeEdge(tab.id, rule.when.event);
          break; // one rule fire per tab per tick — directives serialize anyway
        }
      }
      void tryPrimeOverlordAgent();
      // Escalations that arrived while the agent was busy still owe it a doorbell.
      if (unNudged.size) void wakeOverlordAgent();
      sweepUndeliverableEscalations(now);
      // After sweepClosedTabs, which drops the handoffs whose tab is gone — a closed tab is
      // not a prompt that got answered, and must not be reported as one.
      sweepClosedTabs();
      sweepResolvedPermissionHandoffs();
      sweepStaleProposals(now);
      sweepRetiredTasks(now);
    } finally {
      ticking = false;
    }
  }

  // ── Public API ──────────────────────────────────────────────────────────────

  return {
    get running() { return running; },
    get facts() { return facts; },
    /** Flat board view: every task in this window, tagged with its workspace. */
    get tasks(): TaskRow[] { return allTasks(); },
    get proposals() { return proposals; },
    get escalations() { return escalations; },
    /** Escalations addressed to the HUMAN. Anything that counts or badges "waiting on you"
     *  must read this, not `escalations` — an agent-only row lit the sidebar's urgent badge
     *  with a tooltip saying an item was waiting, while the deck deliberately showed nothing
     *  and offered no way to clear it. */
    get humanEscalations() {
      return escalations.filter((e) => !AGENT_ONLY_ESCALATIONS.has(e.kind));
    },
    get recentLedger() { return recentLedger; },
    /** tabId of any in-flight ritual's target, for board display. */
    get activeRituals() { void liveVersion; return [...rituals.keys()]; },
    get outstandingDirectives() { void liveVersion; return [...outstanding.values()]; },
    /** Live ritual progress per tab — what the deck renders as "step 2/3". */
    get ritualProgress(): { tabId: string; ruleName: string; step: number; steps: number; startedAt: number }[] {
      void liveVersion;
      return [...rituals.values()].map((r) => ({
        tabId: r.tabId, ruleName: r.ruleName, step: r.stepIndex + 1, steps: r.stepCount, startedAt: r.startedAt,
      }));
    },
    /** The outstanding directive on a tab, if any (board badge). */
    outstandingFor(tabId: string): OutstandingDirective | null {
      void liveVersion;
      return outstanding.get(tabId) ?? null;
    },

    /** Start the engine for this window: seed defaults, load persisted board + ledger,
     *  start the ticker. Idempotent. */
    async rehydrate() {
      if (ticker) return;
      await preferencesStore.ready;
      // Seed default rules (same lifecycle as triggers — auto-updates un-modified defaults).
      const seeded = seedDefaultOverlordRules(
        $state.snapshot(preferencesStore.overlordRules) as OverlordRule[],
        preferencesStore.hiddenDefaultOverlordRules,
      );
      if (seeded) await preferencesStore.setOverlordRules(seeded);
      // Board rows live in the tasks store now — hydrate it if nothing else has yet
      // (the engine can start before any panel has mounted).
      if (!tasksStore.loaded) await tasksStore.rehydrate();
      try {
        recentLedger = await commands.getOverlordLedger();
      } catch { /* fresh window */ }
      // Each tick re-publishes the maiLink mirror if anything changed; the mutation paths
      // schedule it sooner. Both funnel through the same changed-check, so this is cheap.
      ticker = setInterval(() => { void tick().then(scheduleMirror, scheduleMirror); }, TICK_MS);
      running = true;
      logInfo('overlord: engine started');
      scheduleMirror();
    },

    destroy() {
      if (ticker) clearInterval(ticker);
      ticker = null;
      running = false;
      for (const run of rituals.values()) run.aborted = true;
      // Tell the phone the engine is off rather than leaving a `running: true` snapshot to age.
      if (mirrorTimer) clearTimeout(mirrorTimer);
      publishMirror();
    },

    // ── Propose-mode (§3) ────────────────────────────────────────────────────
    /** `stale`: the proposal was no longer true and did NOT run — the card is removed, since
     *  a proposal that has stopped applying is not pending. `permission`: the tab is stopped
     *  at a prompt, so the card STAYS — the proposal is still true, it just can't be typed
     *  until the human answers, and running it would hold the tab's ritual slot for the
     *  whole `waitInjectable` cap doing nothing (see `permissionBlocks`). */
    approveProposal(id: string): 'started' | 'stale' | 'permission' {
      const p = proposals.find((x) => x.id === id);
      if (!p) return 'stale';
      const rule = preferencesStore.overlordRules.find((r) => r.id === p.ruleId);
      if (rule && permissionBlocks(rule, p.tabId)) return 'permission';
      proposals = proposals.filter((x) => x.id !== id);
      scheduleMirror();
      if (!rule) return 'stale';
      // Re-check at fire time, not just on the tick that rendered the card. The human can
      // click a card the moment it stops being true, and the whole point is that a
      // proposal is a snapshot — approving one must never act on a stale one.
      if (!proposalStillHolds(p, Date.now())) {
        logInfo(`overlord: refused stale proposal "${p.ruleName}" for ${p.tabName} at approval`);
        return 'stale';
      }
      void runSequence($state.snapshot(rule) as OverlordRule, p.tabId, 'rule');
      return 'started';
    },
    dismissProposal(id: string) {
      proposals = proposals.filter((x) => x.id !== id);
      scheduleMirror();
    },

    // ── Escalations (§9.1) ───────────────────────────────────────────────────
    /** Pull + mark read — the listEscalations MCP surface (S4) and the board both use this. */
    consumeEscalations(): OverlordEscalation[] {
      // Unread only — re-delivering handled escalations makes the agent re-resolve
      // day-old timeouts. Read ones stay on the board until the human dismisses them.
      const out = ($state.snapshot(escalations) as OverlordEscalation[]).filter((e) => !e.read);
      // Drop delivered replies rather than marking them read. Read escalations linger on
      // purpose — they stay on the human's board until dismissed — but a `drive_reply` is
      // filtered off that board, so nobody would ever dismiss one and they would pile up
      // for the life of the window. Handing it to the agent IS its disposal.
      escalations = escalations
        .filter((e) => !(AGENT_ONLY_ESCALATIONS.has(e.kind) && !e.read))
        .map((e) => (e.read ? e : { ...e, read: true }));
      unNudged.clear(); // delivered by the pull itself; no doorbell owed
      return out;
    },
    dismissEscalation(id: string) {
      escalations = escalations.filter((e) => e.id !== id);
      unNudged.delete(id);
      scheduleMirror();
    },
    /** Resolve an 'ack' gate / clear the outstanding directive for a tab (§8). */
    ackOutstanding(tabId: string) {
      const d = outstanding.get(tabId);
      if (d) {
        d.acked = true;
        if (d.ruleId === null) clearOutstanding(tabId);
        else bumpLive();
      }
    },

    // ── Board CRUD (human-owned rows; §11) ───────────────────────────────────
    addTask(title: string, workspaceId: string, tabId?: string | null) {
      tasksStore.add(workspaceId, { title, tab_id: tabId ?? null, origin: 'human' });
    },
    updateTaskState(id: string, status: TaskStatus) {
      const hit = tasksStore.findAnywhere(id);
      if (hit) tasksStore.setStatus(hit.workspaceId, id, status);
    },
    /** Drop a task off the board AND tell whoever was carrying it.
     *
     *  Deleting used to be silent, which made the board lie to the agent: the row vanished
     *  here while the agent still believed in the work, and the next time it re-sent its
     *  list (re-prime, resume, compaction) `findDuplicate` saw nothing and put the task
     *  straight back. The human's decision has to reach the tab or it doesn't stick.
     *
     *  Direct when the tab can take it — a one-line notice, not a directive: it asks for
     *  nothing back, so it must not occupy the tab's outstanding slot and block every
     *  `only_if_no_outstanding` rule behind it. When the tab can't be typed into (no live
     *  REPL, mid-repaint, not mounted), the Overlord agent is told instead and relays it
     *  when the tab comes back — the same act-or-escalate shape as every other card.
     *
     *  NOT a tombstone: an agent that ignores the notice can still re-add the row. Making
     *  that impossible needs a persisted drop list the dedup consults, which is a schema
     *  change; this closes the "nobody ever told it" hole, which was the actual bug.
     *
     *  **The direct notice is NOT gated on `overlordEnabled`** (corrected 2026-09-10). It
     *  used to be, on the reasoning that maiTerm must not type into a terminal for a
     *  supervisor the human switched off — but that reasoning is `startTask`'s, and
     *  `startTask` reaches the opposite conclusion from it: the human clicked delete, on the
     *  tab they were looking at, so this is the human speaking, not the supervisor. Gating
     *  it meant that with Overlord off — which is most installs — deleting a task told the
     *  agent nothing, and `findDuplicate` put the row straight back on its next list
     *  re-send. That is the exact hole this notice exists to close, left open for everyone
     *  who is not running a supervisor. Only the RELAY belongs to Overlord, and it is gated
     *  below, on the same three conditions `startTask` uses. */
    async deleteTask(id: string): Promise<{ removed: boolean; told: 'tab' | 'agent' | 'nobody' }> {
      const hit = tasksStore.findAnywhere(id);
      if (!hit) return { removed: false, told: 'nobody' };
      const { workspaceId, task } = hit;
      const tabId = task.tab_id;
      tasksStore.remove(workspaceId, id);
      handedOff.delete(id);
      bumpLive();
      // Nobody was carrying it — an unassigned row is the board's alone to forget. Nor is
      // there anything to say about work already finished or parked: the agent isn't going
      // to re-add what it has closed out, and clearing out done rows is routine tidying
      // that would otherwise type a line into a tab for every card swept.
      if (!tabId || !isInFlight(task)) {
        return { removed: true, told: 'nobody' };
      }

      // Written for a tab that may never have heard of this task. A human can create a row
      // and delete it inside a minute, and nothing here knows whether the agent has read the
      // board since — `listTasks` is a pull, so delivery is not a fact this side holds. The
      // old wording ("Drop it from your own list too") asserted it did, and an agent that had
      // never seen the row answered that it has no such task and went looking for what it had
      // missed. Same shape as every other bug this file has had: stating as fact something
      // that is only knowable elsewhere. Cover both readings in one notice instead.
      const text =
        `Board update: I removed the task "${task.title}" from the board — it is no longer ` +
        `something I want done. If it is on your list, drop it and don't re-add it. If you ` +
        `have never seen it, there is nothing to do and nothing was missed: it was created ` +
        `and removed between your reads of the board. Either way, don't add it later.`;
      const step: OverlordStep = { kind: 'process', text };
      const sent = await noticeToTab(tabId, text, 'drop');
      if (sent) {
        ledger(tabId, null, 'human', 0, step, 'sent');
        return { removed: true, told: 'tab' };
      }
      ledger(tabId, null, 'human', 0, step, 'blocked_no_repl');
      // The RELAY is the supervisor's, and only it. Same split `startTask` makes, and for
      // the same reason — see the comment on the direct notice above.
      if (!preferencesStore.overlordEnabled || !hasOverlordAgentTab() || isExemptTab(tabId)) {
        return { removed: true, told: 'nobody' };
      }
      escalate(
        tabId,
        null,
        'task_dropped',
        `The human deleted the task "${task.title}" from the board. ${tabDisplayName(tabId)} was ` +
          `carrying it and could not be told directly. Tell it when it is reachable: the task is ` +
          `off the board, it should drop it from its own list and not re-add it. Word that for a ` +
          `tab that may never have heard of the task — a row can be created and deleted between ` +
          `an agent's reads of the board, and nothing here knows whether it was ever delivered. ` +
          `"Drop it if you have it, and don't add it later" is right; "you were asked to do this ` +
          `and now you aren't" sends an agent that never saw it looking for what it missed.`,
      );
      return { removed: true, told: 'agent' };
    },

    /**
     * "Do it" — tell the tab carrying this task to start on it now, and move it to `active`.
     *
     * This is the HUMAN typing, not the supervisor: they clicked the button — or tapped it on
     * the phone, `POST /tasks/{id}/start` (docs/mailink-protocol.md §13.3) — and the text goes
     * to the tab they were already looking at. So unlike `deleteTask`'s notice it is NOT gated
     * on `overlordEnabled`: refusing here would mean the button silently does nothing for
     * everyone with the supervisor switched off, which is most people. Only the escalation
     * FALLBACK belongs to Overlord, and it is skipped when there is no agent to relay through
     * AND when the tab is EXEMPT — an exempt tab's handoff would be consumed off the board by
     * `listEscalations` and then refused by `driveTab`, so it is a promise nothing can keep.
     *
     * **Only a human may reach this.** An agent moving its own row to `active` goes through
     * `tasksStore.update` and sends nothing — otherwise every agent picking up work would type
     * "please pick up this task now" at itself, mid-turn, about the thing it is already doing.
     * That is why the phone has a separate `/start` verb rather than a flag on its status
     * patch: a distinct endpoint cannot be reached by an agent updating its own status.
     *
     * The status moves either way. The human has said what they want done, and that is true
     * whether or not the tab happened to be typeable at that instant — leaving the row in
     * `todo` because a paste couldn't land would lose the decision. `told` reports what
     * actually reached the agent, which is a different question and the caller's to surface.
     */
    async startTask(id: string): Promise<{ started: boolean; told: 'tab' | 'agent' | 'nobody' }> {
      const hit = tasksStore.findAnywhere(id);
      if (!hit) return { started: false, told: 'nobody' };
      const { workspaceId, task } = hit;
      const tabId = task.tab_id;
      if (task.status !== 'active') tasksStore.update(workspaceId, id, { status: 'active' });
      bumpLive();
      // Nobody is carrying it, so there is nobody to tell. Claim it first (the row's
      // hand-back/claim control) and the button means something.
      if (!tabId) return { started: true, told: 'nobody' };

      const text =
        `Board update: please pick up "${task.title}" now — I've moved it to Active. ` +
        (task.detail ? `\n\n${task.detail}\n\n` : '') +
        `Track it with the maiTerm task tools (task id ${task.id}) and keep its status ` +
        `current as you go. If you are mid-way through something else, finish that first ` +
        `and come to this next rather than abandoning it.`;
      const step: OverlordStep = { kind: 'process', text };
      if (await noticeToTab(tabId, text, 'start')) {
        ledger(tabId, null, 'human', 0, step, 'sent');
        return { started: true, told: 'tab' };
      }
      ledger(tabId, null, 'human', 0, step, 'blocked_no_repl');
      // Busy, at a prompt, or unmounted. The supervisor relays it when the tab is reachable
      // — the same act-or-escalate shape every other board action uses. With no supervisor
      // to relay it, say so rather than reporting a delivery that did not happen.
      //
      // An EXEMPT tab has no supervisor either, whatever the preference says: the handoff is
      // agent-only, so it is consumed off the board by `listEscalations` and never seen by a
      // human — and the one tool that could act on it, `driveTab`, refuses an exempt tab. So
      // escalating here would delete the card into an agent that is forbidden to use it, and
      // report a relay that cannot happen. It would also hand the exempt tab's name and the
      // task's detail to the supervisor, which is the exact thing exemption promises not to do.
      if (!preferencesStore.overlordEnabled || !hasOverlordAgentTab() || isExemptTab(tabId)) {
        return { started: true, told: 'nobody' };
      }
      escalate(
        tabId,
        null,
        'task_handoff',
        `The human pressed "Do it" on the task "${task.title}" (id ${task.id}), which is ` +
          `carried by ${tabDisplayName(tabId)}. That tab could not be typed into just then — ` +
          `it was mid-turn, stopped at a prompt, or not mounted — so it has NOT been told. ` +
          `It is already marked Active on the board. Tell it when it is reachable: start on ` +
          `this now, and keep the task's status current.` +
          (task.detail ? `\n\nWhat the task says:\n${task.detail}` : ''),
        task.id,
      );
      return { started: true, told: 'agent' };
    },

    /** Hand a board task to the Overlord agent to carry (the card's "Send").
     *
     *  A handoff is queued as an agent-only escalation rather than typed at the owning tab:
     *  the agent decides what the task needs — drive the tab that owns it, drive a different
     *  one, ask the human, or do it itself — and it already has the doorbell + `listEscalations`
     *  pull for exactly this. Re-sending is allowed (a nudge is sometimes the point); the
     *  receipt just says when it last went.
     *
     *  Refused outright when this window has no agent tab. The queue does not simply wait
     *  any more — `sweepUndeliverableEscalations` throws undeliverable handoffs away after
     *  30 minutes — and a handoff is hidden from the deck, so accepting one here would have
     *  shown a "Sent" receipt for a hand-off that was silently destroyed later with no
     *  surface anywhere that could have told the human. */
    sendTaskToOverlord(id: string): boolean {
      if (!hasOverlordAgentTab()) return false;
      const hit = tasksStore.findAnywhere(id);
      if (!hit) return false;
      const { task } = hit;
      const stream = tasksStore.workstream(hit.workspaceId, task.workstream_id)?.name;
      const where = task.tab_id
        ? `It is assigned to ${tabDisplayName(task.tab_id)} (tab ${task.tab_id}).`
        : 'It is not assigned to any tab — pick one, or carry it yourself.';
      escalate(
        task.tab_id ?? '',
        null,
        'task_handoff',
        `The human handed you a board task to carry: "${task.title}"` +
          (stream ? ` (workstream: ${stream})` : '') +
          `, currently in ${task.status}. ${where}` +
          (task.detail ? `\n\nWhat it says: ${task.detail}` : '') +
          `\n\nTake it from here: move it forward yourself, drive the tab that owns it, or ` +
          `escalate to the human if it needs a decision. Update its status as it moves ` +
          `(task id ${task.id}).`,
        task.id,
      );
      handedOff.set(id, Date.now());
      bumpLive();
      return true;
    },

    /**
     * An AGENT handed a task to another tab (`updateTasks`'s `assign_to`). Raise it; do not
     * type it.
     *
     * **An ordinary agent may not put text into another agent's terminal, and this does not
     * change that.** `driveTab` is Overlord-only and `startTask` is human-only, both because
     * cross-tab injection carries the human's authority — a tab cannot tell an injected line
     * from something its human typed. An agent that could notify a peer directly would have
     * that authority by writing one field of a task update, which is the cheapest possible
     * route to the most privileged act in the app.
     *
     * So the assignment lands on the board (that part is silent and always works) and the
     * NOTICE goes to whoever is entitled to act on it — the supervisor if there is one, and
     * otherwise nobody, reported as such. `told: 'nobody'` is not a failure: the row is
     * assigned and visible on the board and in the target's own panel, where the human can
     * press "Do it". What must never happen is the caller believing a hand-off was delivered
     * when it was not — the `sent ≠ done` rule this file keeps relearning.
     */
    announceHandoff(taskId: string, fromTabId: string, toTabId: string): 'agent' | 'nobody' {
      const hit = tasksStore.findAnywhere(taskId);
      if (!hit) return 'nobody';
      const { task } = hit;
      if (!toTabId || toTabId === fromTabId) return 'nobody';
      // The target is the one the CALLER was told about, and the stored row must agree.
      // Re-deriving it from `task.tab_id` instead let a caller be handed a receipt naming
      // one tab while the escalation named another — the supervisor was then asked to drive
      // whichever tab already owned the row, about a delegation that never happened. Within
      // one call these cannot diverge (the recording happens after the synchronous commit),
      // so a mismatch means another writer moved the row and there is nothing to announce.
      if (task.tab_id !== toTabId) return 'nobody';
      // Same three conditions startTask's fallback uses. An exempt tab in particular: the
      // escalation is agent-only, so it would be consumed off the board by listEscalations
      // and then refused by driveTab — a promise nothing can keep — while handing the exempt
      // tab's name and the task's detail to the supervisor, which is what exemption exists
      // to prevent.
      if (!preferencesStore.overlordEnabled || !hasOverlordAgentTab() || isExemptTab(toTabId)) {
        return 'nobody';
      }
      const stream = tasksStore.workstream(hit.workspaceId, task.workstream_id)?.name;
      escalate(
        toTabId,
        null,
        'task_handoff',
        `${tabDisplayName(fromTabId)} assigned the task "${task.title}"` +
          (stream ? ` (workstream: ${stream})` : '') +
          ` to ${tabDisplayName(toTabId)} (tab ${toTabId}), currently in ${task.status}. ` +
          `That was an AGENT's decision, not the human's, and the target has NOT been told — ` +
          `nothing types into a tab on an agent's say-so.` +
          (task.detail ? `\n\nWhat it says: ${task.detail}` : '') +
          `\n\nDecide whether it should proceed: drive the target tab if the hand-off makes ` +
          `sense, reassign it, or ask the human if it does not. Task id ${task.id}.`,
        task.id,
      );
      handedOff.set(taskId, Date.now());
      bumpLive();
      return 'agent';
    },

    /** When a task was last handed to the agent, for the card's receipt. */
    taskHandoffAt(id: string): number | null {
      void liveVersion;
      return handedOff.get(id) ?? null;
    },

    /** Is there an Overlord agent tab in this window to hand work to? The card's Send
     *  button reads this so it can say why it is disabled instead of accepting a handoff
     *  nothing will ever collect. */
    get hasAgentTab(): boolean {
      return hasOverlordAgentTab();
    },

    get agentReports() { return agentReports; },
    get lastScan() { return lastScan; },
    get scanning() { return scanning; },
    clearScan() { lastScan = null; },

    /** ADOPT pass — populate the board from every currently-running agent tab. Free,
     *  silent, and safe to run as often as you like. Returns what it found. */
    async scanWorkspaces(): Promise<ScanSummary> {
      if (scanning) return lastScan ?? { at: Date.now(), tabsSeen: 0, mirrored: 0, fromStore: 0, adopted: 0, finished: 0, silent: [] };
      scanning = true;
      try {
        const pairs = agentTabs();
        // Refresh facts first so a scan reflects reality now, not the last 5s tick.
        if (pairs.length) {
          try {
            const res = await commands.getOverlordTabFacts(pairs.map((p) => p.tab.id));
            facts = new Map(Object.entries(res));
          } catch (e) {
            logError(`overlord: scan facts poll failed: ${e}`);
          }
        }
        const now = Date.now();
        let mirrored = 0, fromStore = 0, adopted = 0, finished = 0, tabsSeen = 0, changed = false;
        const silent: string[] = [];
        for (const { tab, ws } of pairs) {
          // The Overlord workspace is excluded from every board surface, so a row created
          // for the supervisor's own agent would be invisible — and asking it "what are
          // you working on" is a category error. Rules still evaluate against it (§9.3:
          // the checkpoint rule applies to Overlord too); only the board does not.
          if (ws.overlord) continue;
          const live = claudeStateStore.getState(tab.id);
          if (!live) continue; // only tabs actually running an agent right now
          tabsSeen++;
          const f = facts.get(tab.id);
          // Import whatever the runtime's own store holds first — free, and it is what
          // makes a Claude tab that never learned our tools still land on the board.
          if (f?.todos?.length) {
            syncMirrorTasks(tab.id, f, now);
            if (f.todos_source === 'store') fromStore++;
          } else if (f?.tracked) {
            // Keeps a list and has finished everything on it — close out its imports.
            closeOutMirrorRows(tab.id, now);
          }
          // Classification is against maiTerm's OWN tasks, not the runtime's store. That
          // is what makes "untracked" mean the same thing for Codex and Gemini (neither
          // has a store to read) as it does for Claude, and it counts work an agent
          // recorded through createTasks — which the old test could not see at all.
          // Overlord's placeholders don't count as tracking; they're what a scan creates.
          // Parked rows are excluded: a tab whose only tasks are shelved for next month
          // is not tracking its current work, and asking it to would be right.
          const tracked = tasksForTab(tab.id).filter((t) => t.origin !== 'overlord' && !isParked(t.status));
          if (tracked.length) {
            retirePlaceholderIfMirrored(tab.id);
            if (tracked.every((t) => isRetired(t.status))) finished++;
            else mirrored++;
            changed = true;
            continue;
          }
          // Nothing tracked — stand up (or refresh) one row for the tab itself, titled
          // with the tab name until the tab tells us something better.
          const outcome = adoptRow(tab.id, tab.name, live.state);
          if (outcome !== 'skipped') changed = true;
          if (outcome === 'created') adopted++;
          if (outcome === 'skipped') continue; // finished by hand, or not boardable
          const askedAt = Number(getVariables(tab.id)?.get(TRACK_ASK_VAR) ?? 0);
          if (now - askedAt >= TRACK_ASK_COOLDOWN_MS) silent.push(tab.id);
        }
        lastScan = { at: now, tabsSeen, mirrored, fromStore, adopted, finished, silent };
        logInfo(`overlord: scan — ${tabsSeen} running tabs, ${mirrored} tracking work (${fromStore} imported from a runtime store), ${finished} finished, ${adopted} adopted, ${silent.length} untracked`);
        return lastScan;
      } finally {
        scanning = false;
      }
    },

    /** ASK-TO-TRACK pass — nudge the given tabs to start recording their work, which then
     *  feeds the board on its own via Claude Code's todo store. One short directive each,
     *  through the same mechanical guards as any rule, ledgered as human-origin (you asked
     *  for it). Skips anything busy, guarded, or asked recently. */
    async askTabsToTrack(tabIds: string[]): Promise<{ asked: number; skipped: number }> {
      const step = { kind: 'process' as const, text: TRACK_REQUEST_TEXT };
      let asked = 0, skipped = 0;
      for (const tabId of tabIds) {
        // Never interrogate the supervisor's own agent (its row wouldn't render anyway).
        if (!isBoardableTab(tabId)) { skipped++; continue; }
        const askedAt = Number(getVariables(tabId)?.get(TRACK_ASK_VAR) ?? 0);
        if (Date.now() - askedAt < TRACK_ASK_COOLDOWN_MS) { skipped++; continue; }
        if (outstanding.has(tabId) || rituals.has(tabId)) {
          ledger(tabId, null, 'human', 0, step, 'blocked_guard');
          skipped++; continue;
        }
        if (mappedState(tabId) !== 'idle') { skipped++; continue; }
        if (!(await hasLiveRepl(tabId))) {
          ledger(tabId, null, 'human', 0, step, 'blocked_no_repl');
          skipped++; continue;
        }
        const inst = terminalsStore.get(tabId);
        if (!inst) { skipped++; continue; }
        // Don't type over a repaint (same quiescence rule as every other injection).
        const lastOut = terminalsStore.getLastOutputAt(tabId) ?? 0;
        if (Date.now() - lastOut < 3000) { skipped++; continue; }
        try {
          await bracketedPasteSubmit(inst.ptyId, TRACK_REQUEST_TEXT);
        } catch (e) {
          logError(`overlord: track-request inject failed for ${tabId.slice(0, 8)}: ${e}`);
          skipped++; continue;
        }
        setOutstanding(tabId, {
          id: crypto.randomUUID(),
          ruleId: null,
          tabId,
          stepIndex: 0,
          text: TRACK_REQUEST_TEXT,
          sentAt: Date.now(),
          acked: false,
        });
        ledger(tabId, null, 'human', 0, step, 'sent');
        await setVariable(tabId, TRACK_ASK_VAR, String(Date.now()));
        asked++;
        await sleep(400); // stagger so a wide sweep doesn't hammer every PTY at once
      }
      if (lastScan) lastScan = { ...lastScan, silent: [], asked };
      logInfo(`overlord: track request — asked ${asked}, skipped ${skipped}`);
      return { asked, skipped };
    },
    /** Why this tab has no live agent, if it doesn't. Drives the triage deck's copy AND
     *  which remedy it offers — the two must agree. */
    unreadyKind,

    /** Was this tab classified `stopped` because a re-bind was tried and didn't land,
     *  rather than because the process probe saw nothing? The card says which, since
     *  "the agent exited" is not what the human sees on an SSH tab whose ssh is alive. */
    rebindDidNotTake(tabId: string): boolean {
      void liveVersion;
      return rebindFailed.has(tabId);
    },

    /**
     * Is this tab loaded in the DOM — i.e. can anything here classify or recover it?
     *
     * A TerminalPane mounts only for activated workspaces and tabs, and every dormancy
     * probe and every injection goes through its terminal instance. So a tab in a
     * suspended or never-visited workspace can never be classified: `unreadyKind` stays
     * null and `recoverTab` refuses with `no_terminal`.
     *
     * Which made "unready" the deck's worst card at fleet scale: every agent tab in every
     * unopened workspace produced one that described a problem and offered nothing, and
     * nothing about opening the app was ever going to change that. Those tabs are not
     * unready — they are not loaded, which is what a suspended workspace MEANS. The signal
     * is gated on this instead, and appears with a working remedy the moment the workspace
     * is opened and the pane mounts.
     */
    tabLoaded(tabId: string): boolean {
      return !!terminalsStore.get(tabId);
    },

    /** Exempt from Overlord by its own flag or its workspace's (docs/overlord.md §11). */
    isExemptTab(tabId: string): boolean {
      return isExemptTab(tabId);
    },

    /**
     * ONE vocabulary for what a tab's agent is doing, for every consumer that has to pick an
     * action for it — the deck, the doctrine, and `listWorkspaces`.
     *
     * Deliberately NOT merged with `tabLoaded`. They are orthogonal facts and collapsing them
     * is how the whole area got confusing: an agent can be running and bound (`idle`) in a
     * workspace whose pane is not mounted, in which case it is perfectly healthy and still
     * cannot be typed into. One enum answering both questions would have to lie about one.
     */
    tabAgentState(tabId: string): 'active' | 'idle' | 'permission' | 'unbound' | 'stopped' | 'unknown' {
      const st = mappedState(tabId);
      if (st) return st;
      const kind = unreadyKind(tabId);
      return kind === 'unbound' || kind === 'stopped' ? kind : 'unknown';
    },

    /**
     * The terminal underneath the agent, which is a THIRD fact — not the agent's state and
     * not whether its pane is mounted.
     *
     * A tab can be suspended on its own, inside a perfectly active workspace: `suspendTab`
     * (and `suspendOtherTabs`, which does it to every tab but the active one) kills the PTY,
     * clears `pty_id` and stamps `suspended_at`, leaving the tab in the pane tree with a
     * Resume prompt. That is neither an archived tab nor a suspended workspace, and reporting
     * it as `unknown`/not-loaded — which is all it could look like without this — is exactly
     * how it became indistinguishable from a tab in a workspace nobody has opened.
     */
    tabPtyState(tabId: string): 'live' | 'suspended' | 'none' {
      // `pty_id` is NOT "has a live PTY" — in the frontend mirror it means "has had one".
      // `suspendWorkspace` clears it in Rust and only writes `suspended = true` back to the
      // mirror, and a cancelled session restore leaves it set on purpose. Reading it as live
      // reported every tab in an auto-suspended workspace as a running session.
      //
      // The app's own test is the one to use (`+page.svelte`: had a PTY, has no live
      // instance ⇒ suspended), and a live terminal instance is what "live" has to mean here
      // anyway, since that instance is the thing anything types into.
      if (terminalsStore.get(tabId) || terminalsStore.isSpawning(tabId)) return 'live';
      const tab = workspacesStore._locateTab(tabId)?.tab;
      return tab?.pty_id || tab?.suspended_at ? 'suspended' : 'none';
    },

    /**
     * Wake a single suspended tab (S4 resumeTab) — the answer to `pty: 'suspended'`.
     *
     * Resuming is mount-driven: the PTY respawns when the TerminalPane mounts, which is why
     * this navigates to the tab and then lifts the pane's resume gate, exactly as the human's
     * Resume click does. A tab in a SUSPENDED WORKSPACE is a different case with a different
     * answer, and says so rather than half-working.
     */
    async resumeTabById(tabId: string): Promise<{ ok: boolean; reason?: string; detail?: string }> {
      const loc = workspacesStore._locateTab(tabId);
      if (!loc) return { ok: false, reason: 'not_found', detail: 'No tab with that id in this window.' };
      if (isExemptTab(tabId)) return { ok: false, reason: 'exempt', detail: EXEMPT_DETAIL };
      // Workspace first. Testing `pty_id` first made `workspace_suspended` unreachable in the
      // exact case it was written for: suspendWorkspace leaves a stale `pty_id` in the mirror,
      // so every tab in a parked workspace answered "already live — nothing to resume".
      const ws = workspaceForTab(tabId);
      if (ws?.suspended) {
        return {
          ok: false,
          reason: 'workspace_suspended',
          detail: `That tab is in the suspended workspace "${ws.name}". Resuming the workspace brings back every tab that was live in it — call resumeWorkspace instead of waking this one.`,
        };
      }
      if (terminalsStore.get(tabId) || terminalsStore.isSpawning(tabId)) {
        return { ok: false, reason: 'already_live', detail: 'That tab already has a live terminal — nothing to resume.' };
      }
      try {
        await navigateToTab(tabId);
        resumePane(loc.paneId);
      } catch (e) {
        logError(`overlord: resume failed for tab ${tabId.slice(0, 8)}: ${e}`);
        return { ok: false, reason: 'failed', detail: String(e) };
      }
      logInfo(`overlord: resumed suspended tab ${tabId.slice(0, 8)} on the agent's request`);
      return { ok: true };
    },

    /** Recover a tab that was an agent and isn't responding.
     *
     *  This is the point of a supervisor: the deck used to print "resume it or run
     *  /maiterm init" and leave the human to do it, on every dormant tab, forever. The
     *  remedy is chosen from the process state, not guessed:
     *
     *    unbound — agent alive, no binding → type `/maiterm init`. Cheap and safe.
     *    stopped — nothing running → type the runtime's resume command, relaunching the
     *              agent. Bigger, so it is never automatic: only this explicit call.
     *
     *  Deliberately does NOT go through driveTab, whose guards require a live REPL and an
     *  idle agent — both false here by definition. It keeps the quiescence rule (never
     *  type over a repaint) and ledgers verbatim like every other injection. */
    async recoverTab(
      tabId: string,
    ): Promise<{ sent: boolean; kind?: UnreadyKind; verified?: boolean; reason?: string; detail?: string }> {
      if (isExemptTab(tabId)) return { sent: false, reason: 'exempt', detail: EXEMPT_DETAIL };
      // Terminal first. A tab with no mounted pane has no liveness entry either — the probe
      // only considers tabs it can reach — so asking `unreadyKind` first answered
      // `not_unready`, which reads as "nothing wrong with that tab" for a tab nothing can
      // reach. The description promised the opposite; now it is true.
      const inst = terminalsStore.get(tabId);
      if (!inst) {
        return {
          sent: false,
          reason: 'no_terminal',
          detail: terminalsStore.isSpawning(tabId)
            ? 'That tab is still starting up. Retry in a few seconds.'
            : 'Nothing can be typed into that tab: it has no live terminal. If it is suspended, resumeTab wakes it; if its workspace is suspended, resumeWorkspace does.',
        };
      }
      const kind = unreadyKind(tabId);
      if (!kind) return { sent: false, reason: 'not_unready', detail: 'That tab is not classified as unready — its agent is running and bound, so there is nothing to recover.' };

      let text: string;
      if (kind === 'unbound') {
        text = REBIND_COMMAND;
      } else {
        const runtime = workspacesStore.getTabRuntime(tabId);
        if (!runtime) return { sent: false, reason: 'unknown_runtime' };
        // Interpolates %<runtime>SessionId from the tab's trigger variables, the same way
        // auto-resume does — so this resumes the tab's own session, not a fresh one.
        text = interpolateVariables(tabId, resumeCommandFor(runtime));
        if (text.includes('%')) return { sent: false, reason: 'no_session_id' };
      }

      const step: OverlordStep = { kind: kind === 'unbound' ? 'slash' : 'process', text };
      const lastOut = terminalsStore.getLastOutputAt(tabId) ?? 0;
      if (Date.now() - lastOut < 1500) {
        ledger(tabId, null, 'human', 0, step, 'blocked_guard');
        return { sent: false, kind, reason: 'output_in_flight' };
      }
      try {
        await bracketedPasteSubmit(inst.ptyId, text);
      } catch (e) {
        logError(`overlord: recover inject failed for ${tabId.slice(0, 8)}: ${e}`);
        ledger(tabId, null, 'human', 0, step, 'blocked_no_repl');
        return { sent: false, kind, reason: 'inject_failed' };
      }
      ledger(tabId, null, 'human', 0, step, 'sent');
      logInfo(`overlord: recover ${tabId.slice(0, 8)} (${kind}) — sent ${JSON.stringify(text)}`);
      if (kind === 'unbound') {
        // Watch for the outcome. Without this the deck could only ever re-ask the process
        // probe, which returns the same `unbound` guess forever for an SSH tab whose remote
        // agent is gone — so run-all reported "sent 69, skipped 0" while a dozen tabs took
        // a line of junk at a bash prompt and nothing ever said so.
        rebindWatch.set(tabId, Date.now());
      } else {
        // A resume was just typed; give the previous verdict up so the next probe judges
        // the tab on what happens now.
        rebindFailed.delete(tabId);
      }
      // Clear the classification so the deck stops showing it immediately; the next tick
      // re-probes and will re-raise it if the remedy didn't take.
      liveness.delete(tabId);
      bumpLive();
      // `sent` means the bytes were written, and that is ALL it has ever meant. For a
      // re-bind that is not the same as recovered: a resuming agent can swallow the line
      // and nothing here can tell. Say so at the call site rather than letting the caller
      // read `sent: true` as a result, and name the watch that will correct it — the
      // verdict lands as a `rebind_failed` escalation about 45s from now if it didn't take.
      // A `stopped` recovery needs no such hedge: it relaunches the agent, and a relaunch
      // that fails is visible as the tab staying dormant.
      return kind === 'unbound'
        ? {
            sent: true,
            kind,
            verified: false,
            detail:
              `/maiterm init was typed into that tab. That is NOT confirmation it bound — an agent ` +
              `still coming up, especially a remote one over SSH, swallows it silently. maiTerm ` +
              `watches for ${REBIND_VERIFY_MS / 1000}s and raises a rebind_failed escalation if it ` +
              `did not take. Until then treat the tab as still unbound: do not report it recovered, ` +
              `and re-read its state from listWorkspaces before relying on it.`,
          }
        : { sent: true, kind, verified: false };
    },

    // ── Context pressure: checkpoint ─────────────────────────────────────────

    /** The context level at which something will ACTUALLY happen: the lowest enabled
     *  `context_pct` threshold. The deck reads this instead of its own constant — a
     *  hardcoded 50 against a rule firing at 55 gave a five-point band where the deck
     *  complained and nothing could act, which is advice wearing a supervisor's badge.
     *  Null when no rule is enabled, which the deck says out loud. */
    get checkpointThreshold(): number | null {
      // The LOWEST enabled threshold — the level at which the first rule can fire, which is
      // what `checkpointRuleFor` picks too. Returning the first rule in list order meant a
      // second rule added at a lower threshold fired a checkpoint on a tab the deck was
      // showing no pressure card for: the same deck-says-one-thing, engine-does-another
      // gap this whole change closed, just inverted.
      //
      // Workspace scope is deliberately ignored: this is one window-wide number for the
      // deck's severity ramp, and a scoped rule still narrows what actually fires.
      let lowest: number | null = null;
      for (const r of preferencesStore.overlordRules) {
        if (!r.enabled || r.when.event !== 'context_pct' || !hasRunnableSequence(r)) continue;
        const at = (r.when as { at_or_above: number }).at_or_above;
        if (lowest === null || at < lowest) lowest = at;
      }
      return lowest;
    },

    /** Why a tab under context pressure is or isn't being checkpointed right now. The deck
     *  renders this verbatim: "a checkpoint should run" is not an answer when the whole
     *  point is that Overlord runs it. */
    checkpointState(tabId: string): CheckpointState {
      void liveVersion;
      const run = rituals.get(tabId);
      if (run) return { kind: 'running', step: run.stepIndex + 1, steps: run.stepCount };
      const rule = checkpointRuleFor(tabId);
      if (!rule) return { kind: 'no_rule' };
      if (proposals.some((p) => p.tabId === tabId && p.ruleId === rule.id)) return { kind: 'proposed' };
      // The card's threshold and this tab's rule are not the same number (see `below_rule`).
      // Say the rule's, or the card promises a checkpoint that is not coming.
      const at = (rule.when as { at_or_above?: number }).at_or_above;
      const pct = facts.get(tabId)?.context_pct;
      if (at !== undefined && pct !== undefined && pct < at) return { kind: 'below_rule', at };
      const last = lastFiredAt.get(`${rule.id}|${tabId}`) ?? 0;
      const left = rule.cooldown * 1000 - (Date.now() - last);
      if (left > 0) return { kind: 'cooling', minutes: Math.max(1, Math.ceil(left / 60_000)) };
      const st = mappedState(tabId);
      if (st && st !== 'idle') return { kind: 'busy' };
      return { kind: 'ready' };
    },

    /** Run the checkpoint on this tab now — `fireRule` with the tab's checkpoint rule. */
    async checkpointTab(tabId: string): Promise<{ started: boolean; reason?: string }> {
      const rule = checkpointRuleFor(tabId);
      if (!rule) return { started: false, reason: 'no_rule' };
      return fireRuleNow(rule, tabId);
    },

    /**
     * Rules a human may fire BY HAND at this tab — the Trigger menu on a fleet card and on
     * the composer bar.
     *
     * Wider than `rulesForWorkspace`: a disabled rule is still a defined routine ("don't
     * run this on its own, but let me run it"), and a superseded one is still a sequence
     * somebody wrote. Narrower in one way: scope holds. A rule pinned to a workspace was
     * pinned because its steps belong there, and typing them into another workspace's tab
     * is the one thing the scope field exists to prevent. What a manual fire skips is the
     * `when` clause — that is the whole point of the button.
     *
     * Empty for a tab that has never hosted an agent: nothing here can be typed into bash.
     * The supervisor's own tab is NOT excluded — the ruleset is its harness too (§9.2).
     */
    rulesForTab(tabId: string): OverlordRule[] {
      const found = agentTabs().find((p) => p.tab.id === tabId);
      if (!found) return [];
      const wsId = found.ws.id;
      return preferencesStore.overlordRules
        .filter((r) => hasRunnableSequence(r) && (r.workspaces.length === 0 || r.workspaces.includes(wsId)))
        .sort((a, b) => Number(b.enabled) - Number(a.enabled) || a.name.localeCompare(b.name));
    },

    /** Run a rule's sequence on a tab now, whatever its `when` clause says. See `fireRuleNow`.
     *
     *  The rule is looked up THROUGH `rulesForTab`, not in the raw list, so the pair has to be
     *  one that tab is actually offered: an agent tab, a runnable sequence, and — the part that
     *  matters — scope. `rulesForTab`'s doc calls typing a workspace-pinned rule into another
     *  workspace's tab "the one thing the scope field exists to prevent", but that check lived
     *  only in the menu. Safe while every caller built its buttons from the menu; not safe once
     *  maiLink could name any (ruleId, tabId) pair over the network. One predicate, one place. */
    async fireRule(tabId: string, ruleId: string): Promise<{ started: boolean; reason?: string }> {
      const rule = this.rulesForTab(tabId).find((r) => r.id === ruleId);
      if (!rule) return { started: false, reason: 'no_rule' };
      return fireRuleNow(rule, tabId);
    },

    // ── Spent sessions: archive or close out ─────────────────────────────────

    /**
     * Agent tabs whose tracked work is finished and which have gone quiet.
     *
     * The deck's housekeeping queue: a session that did a big piece of work, completed it,
     * and is now an idle terminal holding a PTY and a slot in the fleet. Requires *tracked*
     * tasks — a tab with nothing on the board has told us nothing, and "no tasks" is not
     * evidence of being finished — and at least one of them actually done, so a tab holding
     * only parked work is never mistaken for a completed one.
     *
     * Never offered for a tab that is mid-turn, awaiting permission, or carrying an
     * outstanding directive: those are live, whatever their task list says.
     */
    get spentTabs(): SpentTab[] {
      void liveVersion;
      const now = Date.now();
      const out: SpentTab[] = [];
      for (const { tab } of agentTabs()) {
        if (retireGuard(tab.id)) continue;

        const tasks = tasksForTab(tab.id);
        if (!tasks.length) continue;
        if (tasks.some((t) => isInFlight(t))) continue;
        const done = tasks.filter((t) => t.status === 'done').length;
        if (!done) continue;

        // Newest evidence of life wins: a turn we recorded, or the last task touched. With
        // neither (0), there is nothing suggesting recent activity, so it qualifies.
        let lastActivity = facts.get(tab.id)?.last_turn_ts ?? 0;
        for (const t of tasks) {
          const ts = Date.parse(t.updated_at);
          if (Number.isFinite(ts) && ts > lastActivity) lastActivity = ts;
        }
        if (lastActivity && now - lastActivity < SPENT_IDLE_MS) continue;

        if (now - keptAt(tab) < SPENT_KEEP_MS) continue;

        out.push({
          tabId: tab.id,
          name: tab.name,
          done,
          parked: tasks.filter((t) => isParked(t.status)).length,
          lastActivity: lastActivity || undefined,
        });
      }
      return out;
    },

    /**
     * Archive a finished session — RECOVERABLE. Keeps the scrollback, cwd and ssh context
     * so the tab can be restored when a bug surfaces in the work it did, which is the whole
     * reason to prefer this over closing.
     *
     * Unfinished rows are released to the project first. An archived tab is out of the
     * window, and work owned by a tab nobody can see is work nobody will do — the same
     * reasoning `releaseTab` already applies when a tab closes. Done rows keep their tab id;
     * that is history, and history should stay attributed.
     */
    async archiveSpentTab(tabId: string): Promise<boolean> {
      if (!isBoardableTab(tabId)) return false;
      try {
        await workspacesStore.archiveTabById(tabId);
      } catch (e) {
        logError(`overlord: archive failed for ${tabId.slice(0, 8)}: ${e}`);
        return false;
      }
      // No release any more: `archive_tab` MOVES this tab's rows onto the archived record,
      // and restoring brings them back still attributed to it. Releasing here would have
      // fought that — clearing tab_id on rows Rust had already taken off the board, so the
      // frontend mirror wrote them back unowned and restore returned a tab whose work had
      // been scattered into the workspace's unclaimed pile.
      liveness.delete(tabId);
      bumpLive();
      logInfo(`overlord: archived spent tab ${tabId.slice(0, 8)}`);
      return true;
    },

    /**
     * Close a finished session — IRREVERSIBLE. Kills the PTY, tears down bridges, keeps no
     * archive entry.
     *
     * Deliberately has no bulk form and no rule: Overlord never does an irreversible thing
     * on its own, which is the same line that keeps `stopped` agents out of bulk recovery
     * and task deletion out of the MCP surface. One tab, one explicit click, behind a
     * confirmation in the UI.
     */
    async closeSpentTab(tabId: string): Promise<boolean> {
      if (!isBoardableTab(tabId)) return false;
      try {
        await workspacesStore.closeTabById(tabId);
      } catch (e) {
        logError(`overlord: close failed for ${tabId.slice(0, 8)}: ${e}`);
        return false;
      }
      liveness.delete(tabId);
      bumpLive();
      logInfo(`overlord: closed spent tab ${tabId.slice(0, 8)}`);
      return true;
    },

    /**
     * Take a tab out of the window on the agent's judgement (S4 archiveTab / closeTab).
     *
     * The human's rule, and the reason both verbs exist: **archive** when there is a chance
     * of coming back to that session for bugs or follow-up work, **close** when it is
     * definitively over or a fresh session would serve just as well. That is a judgement the
     * agent is now trusted to make; what the engine still enforces is that it cannot take
     * away a tab that is working (`retireGuard`).
     *
     * Ledgered either way. An irreversible action taken on the human's behalf has to be at
     * least as auditable as a directive typed on their behalf.
     */
    async retireTab(tabId: string, mode: 'archive' | 'close'): Promise<{ ok: boolean; reason?: string; detail?: string }> {
      // Quiet windows scaled to reversibility: an archive can be undone by restoring it, a
      // close cannot be undone at all, so it asks for five minutes rather than one.
      const refusal = retireGuard(tabId, mode === 'close' ? CLOSE_QUIET_MS : ARCHIVE_QUIET_MS, true);
      const step: OverlordStep = { kind: 'process', text: `[${mode}] ${tabDisplayName(tabId)}` };
      if (refusal) {
        ledger(tabId, null, 'overlord_judgment', 0, step, 'blocked_guard');
        return { ok: false, ...refusal };
      }
      const ok = mode === 'archive' ? await this.archiveSpentTab(tabId) : await this.closeSpentTab(tabId);
      ledger(tabId, null, 'overlord_judgment', 0, step, ok ? 'sent' : 'aborted');
      if (!ok) return { ok: false, reason: 'failed', detail: `maiTerm could not ${mode} that tab; see the log.` };
      return { ok: true };
    },

    /**
     * Delete an ARCHIVED tab for good (S4 deleteArchivedTab) — the one disposition verb the
     * archive had no way to reach.
     *
     * `closeTab` only works on tabs in the pane tree, so an archived session could be created
     * and restored but never discarded, and an archive nobody can prune is a list that only
     * grows. Irreversible, like `closeTab`, but with none of its danger: an archived tab holds
     * no PTY and no process, so there is nothing running to destroy — only the record.
     */
    async deleteArchivedTabById(tabId: string): Promise<{ ok: boolean; reason?: string; detail?: string }> {
      const ws = workspacesStore.workspaces.find(
        (w) => !w.overlord && (w.archived_tabs ?? []).some((t) => t.id === tabId),
      );
      if (!ws) {
        return {
          ok: false,
          reason: 'not_archived',
          detail: 'No archived tab with that id in this window. This deletes ARCHIVED tabs only — a tab still in a pane is closeTab\'s job.',
        };
      }
      // An archived tab is not in any pane, so `isExemptTab` cannot see it; the workspace
      // flag is the only one that can apply, and this is the one irreversible verb.
      if (ws.overlord_exempt) {
        return { ok: false, reason: 'exempt', detail: 'That workspace is exempt from Overlord — the human marked it so. Leave its archive alone.' };
      }
      const name = (ws.archived_tabs ?? []).find((t) => t.id === tabId)?.archived_name ?? tabId.slice(0, 8);
      try {
        await workspacesStore.deleteArchivedTab(ws.id, tabId);
      } catch (e) {
        logError(`overlord: delete archived failed for ${tabId.slice(0, 8)}: ${e}`);
        return { ok: false, reason: 'failed', detail: String(e) };
      }
      ledger(tabId, null, 'overlord_judgment', 0, { kind: 'process', text: `[delete-archived] ${name}` }, 'sent');
      logInfo(`overlord: deleted archived tab ${tabId.slice(0, 8)} from "${ws.name}"`);
      return { ok: true };
    },

    /**
     * Bring a suspended workspace back (S4 resumeWorkspace) — the answer to `loaded: false`.
     *
     * A suspended workspace's tabs are still in its panes with their PTYs killed, so nothing
     * can be typed into or probed there. Resuming respawns exactly the tabs that were live
     * when it was suspended. Without this the agent could SEE those tabs and had no way to
     * make any of them reachable.
     */
    async resumeWorkspaceById(workspaceId: string): Promise<{ ok: boolean; reason?: string; detail?: string }> {
      const ws = workspacesStore.workspaces.find((w) => w.id === workspaceId);
      if (!ws) return { ok: false, reason: 'not_found', detail: 'No workspace with that id in this window.' };
      if (ws.overlord) return { ok: false, reason: 'not_boardable', detail: 'That is the Overlord workspace.' };
      if (ws.overlord_exempt) return { ok: false, reason: 'exempt', detail: 'That workspace is exempt from Overlord — the human marked it so. Leave it alone.' };
      if (!ws.suspended) return { ok: false, reason: 'not_suspended', detail: `"${ws.name}" is not suspended. If its tabs still read loaded:false, its panes simply are not mounted — switch to it.` };
      try {
        await workspacesStore.resumeWorkspace(workspaceId);
      } catch (e) {
        logError(`overlord: resume failed for workspace ${workspaceId.slice(0, 8)}: ${e}`);
        return { ok: false, reason: 'failed', detail: String(e) };
      }
      logInfo(`overlord: resumed workspace "${ws.name}" on the agent's request`);
      return { ok: true };
    },

    /** "I'm keeping this one." Persisted, so the deck stops offering it for a week rather
     *  than re-asking on the next tick. */
    async keepSpentTab(tabId: string) {
      const stamp = String(Date.now());
      const inst = terminalsStore.get(tabId);
      if (inst) {
        // Mounted: the trigger store owns the live map and persists through it.
        await setVariable(tabId, SPENT_KEEP_VAR, stamp);
      } else {
        // Unmounted — `setVariable` would update an in-memory map that is discarded on the
        // next mount and never reaches disk, so the card would return within the week the
        // button promises. Write the tab's own record directly. Only in this branch: doing
        // both would clobber variables a live trigger wrote since we read the tab.
        const loc = workspacesStore._locateTab(tabId);
        if (!loc) return;
        const vars = { ...(loc.tab.trigger_variables ?? {}), [SPENT_KEEP_VAR]: stamp };
        try {
          await commands.setTabTriggerVariables(loc.workspaceId, loc.paneId, tabId, vars);
          loc.tab.trigger_variables = vars;
        } catch (e) {
          logError(`overlord: keep marker failed for ${tabId.slice(0, 8)}: ${e}`);
          return;
        }
      }
      bumpLive();
    },

    /** Bulk archive — the recoverable action only. Snapshotted first, since archiving
     *  mutates the list this reads from. */
    async archiveAllSpent(): Promise<{ archived: number; skipped: number }> {
      const list = [...this.spentTabs];
      let archived = 0, skipped = 0;
      for (const s of list) {
        if (await this.archiveSpentTab(s.tabId)) archived++;
        else skipped++;
        await sleep(200);
      }
      return { archived, skipped };
    },

    // ── Triage: run everything actionable ────────────────────────────────────

    get triageRun() { return triageRun; },

    /** What "run all" would do right now, for the button's own label. The count and the
     *  run read the SAME list, so the label can't promise work the run then skips. */
    get triageActionable(): { rebinds: number; proposals: number; total: number } {
      void liveVersion;
      const j = triageJobs();
      return {
        rebinds: j.rebinds.length,
        proposals: j.proposalIds.length,
        total: j.rebinds.length + j.proposalIds.length,
      };
    },

    cancelTriageRun() { triageCancelled = true; },

    /**
     * Clear the triage deck in one pass, paced for an API rate limit.
     *
     * Only the two actions that are Overlord's own call to make:
     *   1. re-bind every `unbound` agent, and
     *   2. approve every pending proposal.
     *
     * Re-binds go FIRST because a proposal aimed at an unbound tab lands nowhere — fixing
     * the binding first is what makes the directives that follow it worth sending.
     *
     * Deliberately NOT included: restarting `stopped` agents (relaunching a fleet is not a
     * one-click action), and the stale-task and escalation buttons — marking work done,
     * parking it, dropping it or clearing an escalation are judgement calls, and a bulk
     * control that silently makes them would be the worst kind of convenience.
     *
     * The worklist is snapshotted up front, then re-validated at send time: the deck keeps
     * changing underneath a run that takes minutes, and firing a proposal the human
     * dismissed thirty seconds ago would be indistinguishable from ignoring them.
     */
    async runTriage(opts?: { batch?: number; gapMs?: number }): Promise<TriageResult> {
      if (triageRun) return { sent: 0, skipped: 0, bound: 0, silent: 0 };
      const batch = Math.max(1, opts?.batch ?? TRIAGE_BATCH);
      const gapMs = Math.max(0, opts?.gapMs ?? TRIAGE_GAP_MS);

      type Job = { kind: 'rebind'; tabId: string } | { kind: 'proposal'; id: string };
      const { rebinds, proposalIds, superseded } = triageJobs();
      const jobs: Job[] = [
        ...rebinds.map((tabId): Job => ({ kind: 'rebind', tabId })),
        ...proposalIds.map((id): Job => ({ kind: 'proposal', id })),
      ];
      if (!jobs.length) return { sent: 0, skipped: 0, bound: 0, silent: 0 };

      // Drop the proposals the re-bind replaces, rather than leaving stale cards on the
      // deck offering a remedy this run is about to deliver a better way.
      if (superseded.length) {
        const drop = new Set(superseded);
        proposals = proposals.filter((p) => !drop.has(p.id));
        logInfo(`overlord: run-all — ${superseded.length} agent_unready proposal(s) superseded by re-bind`);
      }

      triageCancelled = false;
      let sent = 0, skipped = 0;
      /** Re-bind targets whose injection was delivered — the set we will actually verify.
       *  Entries are removed as they register, so what remains at the end is the silence. */
      const awaitingRebind = new Set<string>();
      let rebindSent = 0;
      const waves = Math.ceil(jobs.length / batch);
      triageRun = {
        total: jobs.length, done: 0, sent: 0, skipped: 0,
        wave: 1, waves, phase: 'running', resumeAt: null, label: null,
      };

      try {
        for (let w = 0; w < waves && !triageCancelled; w++) {
          for (const job of jobs.slice(w * batch, (w + 1) * batch)) {
            if (triageCancelled) break;
            const label = job.kind === 'rebind'
              ? `re-bind ${tabDisplayName(job.tabId)}`
              : (proposals.find((p) => p.id === job.id)?.ruleName ?? 'proposal');
            triageRun = { ...triageRun!, wave: w + 1, phase: 'running', resumeAt: null, label };

            let ok = false;
            if (job.kind === 'rebind') {
              ok = (await this.recoverTab(job.tabId)).sent;
              if (ok) { awaitingRebind.add(job.tabId); rebindSent++; }
            } else {
              // Re-read: the human may have dismissed it, or the rule may have been edited
              // away, since the snapshot. And re-CHECK: run all is exactly where a stale
              // proposal does the most damage, since it fires a whole queue at once without
              // anyone reading the cards one by one.
              // A tab stopped at a prompt is skipped, and its card KEPT, for the same reason
              // "Send it" refuses it: the proposal is still true, it just can't be typed
              // until the human answers, and firing it would hold that tab's ritual slot
              // for the whole waitInjectable cap while counting here as sent.
              const p = proposals.find((x) => x.id === job.id);
              const rule = p && preferencesStore.overlordRules.find((r) => r.id === p.ruleId);
              if (p && rule && !rituals.has(p.tabId) && !permissionBlocks(rule, p.tabId) && proposalStillHolds(p, Date.now())) {
                proposals = proposals.filter((x) => x.id !== p.id);
                void runSequence($state.snapshot(rule) as OverlordRule, p.tabId, 'rule');
                ok = true;
              }
            }
            if (ok) sent++; else skipped++;
            triageRun = { ...triageRun!, done: sent + skipped, sent, skipped };
            await sleep(TRIAGE_STAGGER_MS);
          }
          if (w === waves - 1 || triageCancelled) break;

          const until = Date.now() + gapMs;
          triageRun = { ...triageRun!, phase: 'waiting', resumeAt: until, label: null };
          while (Date.now() < until && !triageCancelled) await sleep(250);

          // The gap only spread the STARTS. Turns run for minutes, so hold here while the
          // fleet is still saturated or the waves stack into exactly the burst we paced to
          // avoid. Capped — a wedged ritual must not strand the rest of the run.
          const holdUntil = Date.now() + TRIAGE_HOLD_CAP_MS;
          while (!triageCancelled && rituals.size >= batch && Date.now() < holdUntil) {
            triageRun = { ...triageRun!, phase: 'holding', resumeAt: null };
            await sleep(1_000);
          }
        }

        // ── Verify ────────────────────────────────────────────────────────────
        //
        // "Sent" is not an outcome. A `/maiterm init` typed at a tab whose agent is gone
        // is delivered perfectly and achieves nothing — and for an SSH tab that is the
        // NORMAL failure, because `ssh_foreground` cannot see the far side (§9.4). A real
        // run over 69 tabs reported "sent 69, skipped 0" while 14 never came back.
        //
        // So the run stays open until every re-bind target has either registered or run out
        // of time. Only re-binds are verifiable this way: a proposal starts a ritual whose
        // completion is a different thing entirely, and is reported as sent.
        if (awaitingRebind.size) {
          const until = Date.now() + REBIND_VERIFY_MS;
          while (!triageCancelled && Date.now() < until) {
            for (const id of [...awaitingRebind]) {
              if (claudeStateStore.getState(id)) awaitingRebind.delete(id);
            }
            if (!awaitingRebind.size) break;
            triageRun = {
              ...triageRun!,
              phase: 'verifying',
              resumeAt: until,
              label: `${awaitingRebind.size} tab${awaitingRebind.size === 1 ? '' : 's'} still coming back`,
            };
            await sleep(1_000);
          }
        }
      } finally {
        triageRun = null;
      }
      // Cancelling stops the watching, not the tabs — a cancelled run has no verdict to
      // report, so it says how many were sent and leaves the deck to classify the rest.
      const silent = triageCancelled ? 0 : awaitingRebind.size;
      const bound = Math.max(0, rebindSent - silent);
      logInfo(
        `overlord: run-all — sent ${sent}, skipped ${skipped}` +
          (triageCancelled ? ' (cancelled)' : `, re-bound ${bound}, no answer from ${silent}`),
      );
      return { sent, skipped, bound, silent };
    },

    /** Recover every unready tab in one pass — the deck's bulk action. Staggered so a
     *  window with dozens of dormant tabs doesn't hammer every PTY at once. */
    async recoverAllUnbound(): Promise<{ sent: number; skipped: number }> {
      let sent = 0, skipped = 0;
      for (const [tabId, entry] of [...liveness]) {
        // Only the safe remedy in bulk. Relaunching agents en masse is not something to
        // trigger from one button click.
        if (entry.kind !== 'unbound') continue;
        const r = await this.recoverTab(tabId);
        if (r.sent) sent++;
        else skipped++;
        await sleep(400);
      }
      return { sent, skipped };
    },

    get pendingRuleChanges() { return pendingRuleChanges; },

    isOverlordAgentTab,

    /** replyToOverlord (§8) — the active return channel. Called from the MCP tool
     *  handler with the CALLING tab's id. */
    handleAgentReply(tabId: string, args: {
      kind: string; state: string; summary: string; task?: string;
      blockers?: string[]; next?: string; needs_human?: boolean;
    }): { received: true; outstanding_directive: string | null } {
      const summary = (args.summary ?? '').slice(0, 280);
      agentReports = new Map(agentReports);
      agentReports.set(tabId, {
        tabId,
        kind: args.kind,
        state: args.state,
        summary,
        task: args.task,
        ts: Date.now(),
      });
      // A census answer (or any status carrying `task`) is what upgrades this tab's
      // placeholder row into a real description of the work.
      if (args.task?.trim()) {
        titleRow(tabId, args.task.trim().slice(0, 200));
      }
      const d = outstanding.get(tabId);
      if (args.kind === 'ack' && d) {
        d.acked = true;
        // A driveTab directive (no rule, no ritual loop watching it) is DONE on ack —
        // leaving it in the map locks the tab against rules and further driveTabs.
        if (d.ruleId === null) clearOutstanding(tabId);
        else bumpLive();
      }
      // Recovery withdraws the condition card. Before the escalate below, so a report that
      // is both a recovery AND a fresh ask ("unblocked, but now I need a decision") clears
      // the old `blocked` and raises the new `agent_report`, rather than the withdrawal
      // eating a card raised microseconds earlier.
      //
      // **An ALLOWLIST, not `!== 'blocked'`.** `state` is declared required with a
      // four-value enum, but this is the hand-rolled JSON-RPC server and nothing enforces it
      // at runtime — the same gap `coerceStatus` exists to cover on the task side, and its
      // docstring says agents do drift from a declared vocabulary. `!== 'blocked'` handed a
      // malformed report retraction power it never had: a tab that reported `blocked`, then
      // reported `state: 'stuck'` while still stuck, had its card deleted and nothing
      // re-raised it — the deck and the phone both cleared while the tab sat blocked. Before
      // this withdrawal existed an off-vocabulary state was inert in BOTH directions; it
      // stays that way.
      if (RECOVERED_STATES.has(args.state)) withdrawBlocked(tabId);
      if (args.needs_human || args.kind === 'escalate' || args.state === 'blocked') {
        const blockers = args.blockers?.length ? ` — blockers: ${args.blockers.join('; ')}` : '';
        // "I am stuck" and "I need you" are different cards. Filing both as `agent_report`
        // left the `blocked` kind with no producer at all and flattened the deck's only
        // distinction between a tab that has hit a wall and one asking for a decision — so
        // an explicit ask wins, and a bare blocked state gets the kind the doctrine names.
        const kind = args.needs_human || args.kind === 'escalate' ? 'agent_report' : 'blocked';
        escalate(tabId, null, kind, `${tabDisplayName(tabId)} reports ${args.state}: ${summary}${blockers}`);
      }
      return { received: true, outstanding_directive: d && !d.acked ? d.text : null };
    },

    /** listEscalations (§9.1) — Overlord-agent-only pull; content stays out of the
     *  wake nudge so the agent's transcript stays lean. */
    listEscalationsFor(callerTabId: string): { error: string } | { escalations: unknown[] } {
      if (!isOverlordAgentTab(callerTabId)) {
        return { error: 'listEscalations is available only to the Overlord agent tab.' };
      }
      const items = this.consumeEscalations().map((e) => ({
        id: e.id,
        ts: new Date(e.ts).toISOString(),
        tab_id: e.tabId,
        tab_name: tabDisplayName(e.tabId),
        workspace: workspaceForTab(e.tabId)?.name ?? null,
        kind: e.kind,
        detail: e.detail,
      }));
      return { escalations: items };
    },

    /** proposeRuleChanges (§10): queue the batch for explicit human approval (a modal
     *  with per-change selection). Resolves when the human decides, or returns
     *  pending:true if they haven't within the MCP window — the modal stays up and the
     *  decision applies asynchronously. */
    async proposeRuleChanges(
      callerTabId: string,
      rationale: string,
      changes: OverlordRuleChange[],
    ): Promise<Record<string, unknown>> {
      if (!isOverlordAgentTab(callerTabId)) {
        return { error: 'proposeRuleChanges is available only to the Overlord agent tab.' };
      }
      if (pendingRuleChanges) {
        return { error: 'A rule-change batch is already awaiting the human. Wait for it to resolve.' };
      }
      if (!changes?.length) return { error: 'No changes given.' };
      // Validate before the human ever sees it. The MCP schema takes `rule` as a bare
      // object, so a create with no `when` or no `sequence` arrives well-formed, renders
      // fine in the approval prompt, and then applies to nothing. Refuse the batch with the
      // specific field named — the agent can fix that; it cannot fix a false "approved".
      const problems = changes
        .map((c, i) => {
          const p = changeProblem(c, $state.snapshot(preferencesStore.overlordRules) as OverlordRule[]);
          return p ? `changes[${i}]: ${p}` : null;
        })
        .filter((p): p is string => p !== null);
      if (problems.length) {
        return {
          error:
            `${problems.length} change${problems.length === 1 ? '' : 's'} cannot be applied, so the batch was not shown to the human. ` +
            `Fix and re-propose — this is not a rejection.\n${problems.join('\n')}`,
        };
      }
      const repitched = changes.filter((c) => rejectedChangeKeys.has(changeKey(c)));
      if (repitched.length === changes.length) {
        return { error: 'Every change in this batch was already rejected by the human. Do not re-propose.' };
      }
      const batch: PendingRuleChangeBatch = {
        id: crypto.randomUUID(),
        tabId: callerTabId,
        rationale: String(rationale ?? '').slice(0, 1000),
        changes,
      };
      pendingRuleChanges = batch;
      dispatch('Overlord', 'Overlord proposes rule changes — review in the approval prompt.', 'info');
      const decision = await new Promise<{
        approved: string[];
        rejected: string[];
        failed?: string[];
        pending?: boolean;
      }>((resolve) => {
        ruleChangeResolver = resolve;
        // Answer inside the MCP response window; the modal stays up past this.
        setTimeout(() => {
          if (ruleChangeResolver === resolve) {
            ruleChangeResolver = null;
            resolve({ approved: [], rejected: [], pending: true });
          }
        }, 100_000);
      });
      if (decision.pending) {
        return {
          pending: true,
          note: 'The human has not decided yet. The approval prompt stays open; check the ruleset later. Do not re-propose.',
        };
      }
      return {
        approved: decision.approved,
        rejected: decision.rejected,
        // Only present when something the human said yes to could not be built. Absent is
        // the normal case; a non-empty list means the ruleset does NOT contain it.
        ...(decision.failed?.length ? { failed: decision.failed } : {}),
      };
    },

    /** The approval modal's answer: apply the selected change indexes, reject the rest. */
    resolveRuleChanges(batchId: string, approvedIdx: number[]) {
      const batch = pendingRuleChanges;
      if (!batch || batch.id !== batchId) return;
      pendingRuleChanges = null;
      const approved: string[] = [];
      const rejected: string[] = [];
      const failed: string[] = [];
      let rules = ($state.snapshot(preferencesStore.overlordRules) as OverlordRule[]);
      batch.changes.forEach((change, i) => {
        const label = describeChange(change);
        if (!approvedIdx.includes(i)) {
          rejectedChangeKeys.add(changeKey(change));
          rejected.push(label);
          return;
        }
        // Look the target up BEFORE applying — a delete removes it from the list — but act
        // on it only once the change has actually landed.
        const target = change.op === 'delete' ? matchRule(rules, change.rule_id) : undefined;
        const next = applyRuleChange(rules, change);
        if (!next) {
          // The human said yes to something that cannot be built. Saying "approved" here is
          // how "SSH tabs must report explicitly" came to be ledgered as approved, reported
          // approved to the agent, and never exist. Propose-time validation should stop this
          // reaching the modal at all; this is the backstop for a ruleset that moved in
          // between (the target rule deleted while the prompt was open).
          const why = changeProblem(change, rules) ?? 'it no longer applies to the current ruleset';
          failed.push(`${label} — ${why}`);
          ledger(batch.tabId, null, 'overlord_judgment', 0,
            { kind: 'process', text: `rule change approved but NOT applied: ${label} — ${why}` }, 'aborted');
          return;
        }
        rules = next;
        // Deleting a seeded default must also hide its default_id, or the next
        // startup re-seeds it right back.
        if (target?.default_id && !preferencesStore.hiddenDefaultOverlordRules.includes(target.default_id)) {
          void preferencesStore.setHiddenDefaultOverlordRules([
            ...preferencesStore.hiddenDefaultOverlordRules,
            target.default_id,
          ]);
        }
        approved.push(label);
        ledger(batch.tabId, null, 'overlord_judgment', 0, { kind: 'process', text: `rule change approved: ${label}` }, 'sent');
      });
      if (approved.length) void preferencesStore.setOverlordRules(rules);
      if (failed.length) {
        dispatch(
          'Overlord',
          `${failed.length} approved rule change${failed.length === 1 ? '' : 's'} could not be applied — see the ledger.`,
          'error',
        );
      }
      ruleChangeResolver?.({ approved, rejected, failed });
      ruleChangeResolver = null;
    },

    /** What is blocking a tab, if anything — the read half of the prompt surface. */
    async tabPrompt(tabId: string) {
      return commands.getTabPrompt(tabId);
    },

    /**
     * Answer a tab's open prompt with the human's authority.
     *
     * Goes through maiLink's hardened responder, not a raw paste: a permission menu takes a
     * single runtime-specific keystroke, and an AskUserQuestion selector is driven by
     * relative navigation that can only be attempted ONCE (a retry starts from an unknown
     * highlight position and can record the operator as having said something they didn't).
     * Bracketed-paste text would answer neither correctly.
     *
     * Ledgered like every other injection — an approval made on the human's behalf has to
     * be as auditable as a directive typed on their behalf.
     */
    async answerPrompt(
      tabId: string,
      promptId: string | null,
      choice: string | null,
      answers: commands.PromptAnswer[] | null,
    ): Promise<{ ok: boolean; reason?: string; detail?: string }> {
      const shown = choice ?? (answers ?? []).map((a) => [...(a.selected ?? []), a.other ?? ''].filter(Boolean).join(', ')).join(' | ');
      const step: OverlordStep = { kind: 'process', text: `[prompt] ${shown}` };
      let res: { ok: boolean; reason?: string; detail?: string };
      try {
        res = await commands.answerTabPrompt(tabId, promptId, choice, answers);
      } catch (e) {
        logError(`overlord: prompt answer failed for ${tabId.slice(0, 8)}: ${e}`);
        ledger(tabId, null, 'overlord_judgment', 0, step, 'blocked_guard');
        return { ok: false, reason: 'inject_failed' };
      }
      ledger(tabId, null, 'overlord_judgment', 0, step, res.ok ? 'sent' : 'blocked_guard');
      logInfo(`overlord: answered ${tabId.slice(0, 8)} prompt — ${JSON.stringify(shown)} (${res.ok ? 'ok' : res.reason})`);
      // The agent just did the thing the handoff asked it to do, so there is nothing left to
      // stand it down from. Without this the doctrine's own success path ends in a false
      // correction — the tab leaves `permission`, and the sweep, which can only see that the
      // queue is empty and the tab is running, tells the agent the human must have answered
      // it. Answering is the only resolution the engine can attribute; every other way out
      // of a prompt is somebody else's doing, which is what the stand-down may then assert.
      // Withdraw rather than forget: asks still queued would otherwise be delivered later,
      // about a gate this agent itself closed.
      if (res.ok) withdrawPermissionHandoff(tabId);
      return res;
    },

    /** Drive a tab on the Overlord agent's behalf (S4 driveTab): same guards, same
     *  ledger, structured refusal. */
    async driveTab(
      tabId: string,
      kind: 'process' | 'slash',
      text: string,
    ): Promise<{ sent: boolean; reason?: string; detail?: string }> {
      if (isExemptTab(tabId)) return { sent: false, reason: 'exempt', detail: EXEMPT_DETAIL };
      const step: OverlordStep = { kind, text };
      if (kind === 'slash') {
        const rt = workspacesStore.getTabRuntime(tabId);
        // Unknown-slash risk is the caller's to manage per §6; only hard-block Gemini
        // (no slash surface at all today).
        if (rt === 'gemini') {
          ledger(tabId, null, 'overlord_judgment', 0, step, 'skipped_runtime');
          return { sent: false, reason: 'runtime_mismatch' };
        }
      }
      // Name what holds the tab. This was the one refusal in this function with no `detail`,
      // and a bare `outstanding_directive` reads as "busy, retry" — so the agent retried a
      // step the engine was already part-way through sending, three times over ninety
      // seconds, and the human saw every directive twice (2026-08-31). The refusal itself
      // was correct; its silence about WHY is what turned one collision into three.
      const run = rituals.get(tabId);
      if (run) {
        ledger(tabId, null, 'overlord_judgment', 0, step, 'blocked_guard');
        const seq = preferencesStore.overlordRules.find((r) => r.id === run.ruleId)?.sequence ?? [];
        const dup = seq.findIndex((s) => s.text === text);
        const where =
          `The rule "${run.ruleName}" is running its own sequence on that tab right now ` +
          `(step ${Math.min(run.stepIndex + 1, run.stepCount)} of ${run.stepCount}). Nothing was typed.`;
        // The blocked text IS one of that sequence's steps: the agent is hand-driving a
        // ritual the engine owns. Waiting and retrying is the wrong remedy — the engine
        // sends this step itself — so say so with a reason code that isn't "wait".
        return dup >= 0
          ? {
              sent: false,
              reason: 'already_running',
              detail:
                `${where} What you tried to send IS step ${dup + 1} of that sequence — the engine ` +
                `sends it itself, so this is already being done. Stand down: do not retry, and do ` +
                `not drive the rest of the sequence by hand either, or the human gets every ` +
                `directive twice. The rules in your doctrine all run on their own.`,
            }
          : {
              sent: false,
              reason: 'outstanding_directive',
              detail: `${where} Wait for that sequence to finish, then retry.`,
            };
      }
      if (outstanding.has(tabId)) {
        ledger(tabId, null, 'overlord_judgment', 0, step, 'blocked_guard');
        return {
          sent: false,
          reason: 'outstanding_directive',
          detail:
            'That tab still owes an answer to an earlier directive; nothing was typed. Its reply ' +
            'is queued for you when its turn ends and you are rung for it, so wait for that ' +
            'rather than retrying.',
        };
      }
      const repl = await replState(tabId);
      if (repl !== 'ready') {
        ledger(tabId, null, 'overlord_judgment', 0, step, 'blocked_no_repl');
        if (repl === 'unbound') {
          // An agent IS running there; it just hasn't told maiTerm which tab it is, so
          // nothing can be routed to or from it. Saying "no live REPL" here is what sent
          // everyone looking for a dead terminal.
          //
          // And then say it with the block already clearing: `recoverTab` types the one
          // line that fixes it, exactly as the deck's Re-bind button does, and refuses to
          // type over live output. Describing this and leaving it is the failure mode this
          // subsystem keeps repeating — the remedy is one call away and the supervisor has
          // no other way to reach an unregistered tab, because driveTab is that way.
          const recent = rebindWatch.get(tabId);
          const fresh = recent !== undefined && Date.now() - recent < REBIND_VERIFY_MS;
          const r = fresh ? { sent: false, reason: 'already_sent' } : await this.recoverTab(tabId);
          return {
            sent: false,
            reason: 'not_registered',
            detail:
              `An agent is running in that tab, but it has not run /maiterm init since it ` +
              `started, so maiTerm cannot route to it — the tab is not dead. ` +
              (r.sent || fresh
                ? `/maiterm init has been sent for you; retry this directive in a few seconds. ` +
                  `If it still refuses, the agent is gone rather than unbound and the tab needs restarting.`
                : `Sending /maiterm init failed (${r.reason}) — use the Re-bind action on the ` +
                  `triage deck, or ask the human to run it in the tab.`),
          };
        }
        return {
          sent: false,
          reason: repl === 'unknown' ? 'not_classified' : 'no_live_repl',
          detail:
            repl === 'no_terminal'
              ? 'That tab has no terminal in this window.'
              : repl === 'unknown'
                ? 'That tab could not be classified — its pane is not mounted, which happens ' +
                  'in a suspended or never-opened workspace. Open that workspace and retry.'
                : 'Nothing is running in that tab — the agent exited. It needs restarting ' +
                  'before it can be driven.',
        };
      }
      const st = mappedState(tabId);
      if (st !== 'idle') {
        ledger(tabId, null, 'overlord_judgment', 0, step, 'blocked_guard');
        // "busy" is wrong for a tab stopped at a permission prompt, and wrong in the way
        // that matters: busy invites a retry, and retrying never clears a gate that is
        // waiting on a person. Name the actual state so the supervisor stops guessing.
        return st === 'permission'
          ? {
              sent: false,
              reason: 'awaiting_permission',
              detail:
                'That tab is stopped at a prompt, so there is nothing to type a directive ' +
                'into — retrying will not clear it. Use getTabPrompt to see what it is ' +
                'asking and answerTabPrompt to answer it (escalate to the human first if ' +
                'the decision is consequential). The tab resumes once it is answered.',
            }
          : { sent: false, reason: 'agent_busy' };
      }
      const inst = terminalsStore.get(tabId);
      if (!inst) return { sent: false, reason: 'no_live_repl' };
      try {
        await bracketedPasteSubmit(inst.ptyId, text);
      } catch {
        return { sent: false, reason: 'no_live_repl' };
      }
      setOutstanding(tabId, {
        id: crypto.randomUUID(),
        ruleId: null,
        tabId,
        stepIndex: 0,
        text,
        sentAt: Date.now(),
        acked: false,
      });
      ledger(tabId, null, 'overlord_judgment', 0, step, 'sent');
      // Watch for the answer. `replyToOverlord` is voluntary and the directive is raw text
      // with no envelope, so an agent that simply answers in its terminal — the normal
      // case — would otherwise never reach the supervisor that asked.
      driveWatch.set(tabId, {
        // Falling back to 0 meant "everything in the tail": a tab with no facts yet (driven
        // before the first tick, or whose transcript appears later) would harvest the whole
        // recent transcript and present it as the answer. `Date.now()` is the right floor for
        // a local tab, whose transcript clock is this machine's; for a remote tab there is no
        // readable transcript either way, and the expiry escalation covers it.
        baseline: facts.get(tabId)?.last_turn_ts ?? Date.now(),
        sentAt: Date.now(),
        text,
        reads: 0,
        readErrors: 0,
      });
      // Fire-and-forget from the engine's perspective; the ack/turn-end clears it.
      setTimeout(() => {
        const d = outstanding.get(tabId);
        if (d && d.text === text && Date.now() - d.sentAt > 15 * 60_000) clearOutstanding(tabId);
      }, 15 * 60_000 + 1000);
      return { sent: true };
    },
  };
}

export const overlordStore = createOverlordStore();
