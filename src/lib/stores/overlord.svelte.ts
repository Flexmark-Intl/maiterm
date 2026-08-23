import * as commands from '$lib/tauri/commands';
import type { OverlordTabFacts } from '$lib/tauri/commands';
import type {
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
import { workspacesStore } from '$lib/stores/workspaces.svelte';
import { terminalsStore } from '$lib/stores/terminals.svelte';
import { claudeStateStore } from '$lib/stores/agentState.svelte';
import { preferencesStore } from '$lib/stores/preferences.svelte';
import { bracketedPasteSubmit } from '$lib/utils/agentPrompt';
import { dispatch } from '$lib/stores/notificationDispatch';
import { seedDefaultOverlordRules } from '$lib/overlord/defaults';
import { getVariables, interpolateVariables, setVariable } from '$lib/stores/triggers.svelte';
import { getResumeCommand } from '$lib/agents/resume';
import { tasksStore } from '$lib/stores/tasks.svelte';
import { findImportedDuplicate, isInFlight, isParked, makeTask, normalizeTitle, statusFromAgent, type TaskRow } from '$lib/tasks/model';
import { error as logError, info as logInfo } from '@tauri-apps/plugin-log';

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
/** no_todo_list only nags sessions with real work in them. */
const NO_TODO_MIN_CONTEXT_TOKENS = 30_000;
const NO_TODO_RECENT_TURN_MS = 30 * 60_000;
/** Done board rows are swept after this long (same hygiene as completed mesh topics). */
const TASK_DONE_RETENTION_MS = 48 * 60 * 60 * 1000;
const GATE_POLL_MS = 1_000;
/** turn_end fallback (see awaitGate): how long after injection, and how long the PTY must
 *  have been silent, before an idle tab counts as having finished a sub-poll turn. */
const TURN_FALLBACK_MIN_MS = 4_000;
const TURN_FALLBACK_QUIET_MS = 3_000;

export interface OutstandingDirective {
  id: string;
  ruleId: string | null;
  tabId: string;
  stepIndex: number;
  text: string;
  sentAt: number;
  /** 'ack' gates resolve via replyToOverlord; others resolve inside the ritual loop. */
  acked: boolean;
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
  kind: 'step_timeout' | 'blocked' | 'directive_unacked' | 'permission_stuck' | 'agent_report';
  detail: string;
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

/** Guards an agent-created rule gets, whatever it asked for — the field-tier rule (§10):
 *  guards are human-only, unreachable from the MCP surface. */
const AGENT_RULE_GUARDS = {
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

/** Apply one approved change. Enforces the field tiers mechanically: guards are never
 *  taken from the agent (created rules get AGENT_RULE_GUARDS; update patches have any
 *  guards stripped), and applied edits set user_modified so seeding won't overwrite. */
function applyRuleChange(rules: OverlordRule[], c: OverlordRuleChange): OverlordRule[] {
  switch (c.op) {
    case 'create': {
      if (!c.rule?.name || !c.rule.when || !c.rule.sequence?.length) return rules;
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
        // Guards are chosen here, never taken from the agent (§10 field tiers). The one
        // condition-dependent choice: an `agent_unready` rule targets tabs with NO live
        // agent, so requiring a live REPL would make it permanently unfireable.
        guards: {
          ...AGENT_RULE_GUARDS,
          ...(c.rule.when.event === 'agent_unready' ? { require_live_repl: false } : {}),
        },
        sequence: c.rule.sequence,
        supersedes: c.rule.supersedes,
      };
      return [rule, ...rules];
    }
    case 'update': {
      const target = matchRule(rules, c.rule_id);
      if (!target || !c.patch) return rules;
      const { guards: _guards, id: _id, default_id: _did, ...patch } = c.patch;
      return rules.map((r) => (r.id === target.id ? { ...r, ...patch, user_modified: true } : r));
    }
    case 'rescope': {
      const target = matchRule(rules, c.rule_id);
      if (!target) return rules;
      return rules.map((r) => (r.id === target.id ? { ...r, workspaces: c.workspaces ?? [], user_modified: true } : r));
    }
    case 'enable':
    case 'disable': {
      const target = matchRule(rules, c.rule_id);
      if (!target) return rules;
      return rules.map((r) => (r.id === target.id ? { ...r, enabled: c.op === 'enable' } : r));
    }
    case 'delete': {
      const target = matchRule(rules, c.rule_id);
      if (!target) return rules;
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
  // Resolver for the MCP proposeRuleChanges round trip (the modal answers it).
  let ruleChangeResolver: ((res: { approved: string[]; rejected: string[]; pending?: boolean }) => void) | null = null;
  // Rituals and outstanding directives live in plain Maps (engine-internal, mutated from
  // async loops). This counter is the reactivity bridge for the board — same bump()
  // pattern as agentMesh. Every mutation of those maps calls bumpLive().
  let liveVersion = $state(0);
  // What Overlord already pitched and the human rejected — refuse re-pitches this session.
  const rejectedChangeKeys = new Set<string>();

  // ── Engine bookkeeping (non-reactive) ───────────────────────────────────────
  const prevAgentState = new Map<string, AgentState | undefined>();
  const prevCommitTs = new Map<string, number | undefined>();
  const permissionSince = new Map<string, number>();
  const lastFiredAt = new Map<string, number>(); // `${ruleId}|${tabId}` → ms
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

  const rituals = new Map<string, RitualRun>(); // tabId → active ritual
  let ticker: ReturnType<typeof setInterval> | null = null;
  let ticking = false;

  const sleep = (ms: number) => new Promise<void>((r) => setTimeout(r, ms));
  function bumpLive() { liveVersion++; }

  function setOutstanding(tabId: string, d: OutstandingDirective) { outstanding.set(tabId, d); bumpLive(); }
  function clearOutstanding(tabId: string) { if (outstanding.delete(tabId)) bumpLive(); }

  // ── Tab / workspace helpers ─────────────────────────────────────────────────

  function agentTabs(): { tab: Tab; ws: Workspace }[] {
    const out: { tab: Tab; ws: Workspace }[] = [];
    for (const ws of workspacesStore.workspaces) {
      for (const pane of ws.panes) {
        for (const tab of pane.tabs) {
          if ((tab.tab_type ?? 'terminal') !== 'terminal') continue;
          if (!tab.runtime) continue; // never hosted an agent → not supervised
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

  function tabName(tabId: string): string {
    for (const ws of workspacesStore.workspaces) {
      for (const pane of ws.panes) {
        const t = pane.tabs.find((t) => t.id === tabId);
        if (t) return t.name;
      }
    }
    return tabId.slice(0, 8);
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

  /** The require_live_repl hard precondition (§3): a live agent session AND a live
   *  agent/ssh process in the tty — an absent agent means the directive lands in bash. */
  async function hasLiveRepl(tabId: string): Promise<boolean> {
    if (!claudeStateStore.getState(tabId)) return false;
    const inst = terminalsStore.get(tabId);
    if (!inst) return false;
    try {
      const live = await commands.getAgentLiveness(inst.ptyId);
      return live.agent_running || live.ssh_foreground;
    } catch {
      return false;
    }
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

  function escalate(
    tabId: string,
    ruleId: string | null,
    kind: OverlordEscalation['kind'],
    detail: string,
  ) {
    escalations = [
      ...escalations,
      {
        id: crypto.randomUUID(),
        ts: Date.now(),
        tabId,
        workspaceId: workspaceForTab(tabId)?.id ?? '',
        ruleId,
        kind,
        detail,
        read: false,
      },
    ];
    unNudged.add(escalations[escalations.length - 1].id);
    void wakeOverlordAgent();
  }

  /** One-line doorbell into the Overlord agent's PTY (§9.1) — content stays behind
   *  the listEscalations pull, keeping the agent's transcript lean. No live agent →
   *  the queue simply waits (the engine runs regardless; §2 agent lifecycle). */
  async function wakeOverlordAgent() {
    if (unNudged.size === 0) return;
    // The human may have cleared the queue while the agent was busy — an escalation
    // dismissed from the board must not still ring "0 escalations pending" later.
    for (const id of [...unNudged]) {
      if (!escalations.some((e) => e.id === id && !e.read)) unNudged.delete(id);
    }
    if (unNudged.size === 0) return;
    const ws = overlordWorkspace();
    if (!ws) return;
    for (const pane of ws.panes) {
      for (const tab of pane.tabs) {
        if (!tab.runtime) continue;
        if (mappedState(tab.id) !== 'idle') continue;
        if (!(await hasLiveRepl(tab.id))) continue;
        const inst = terminalsStore.get(tab.id);
        if (!inst) return;
        const n = escalations.filter((e) => !e.read).length;
        try {
          await bracketedPasteSubmit(inst.ptyId, `${n} Overlord escalation${n === 1 ? '' : 's'} pending — call listEscalations.`);
          unNudged.clear();
        } catch (e) {
          logError(`overlord: wake nudge failed: ${e}`);
        }
        return;
      }
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
      `  - Use listWorkspaces to see the tabs; every injection you make is recorded verbatim in the ledger.\n` +
      `  - When you find yourself hand-issuing the same directive repeatedly, propose a rule with proposeRuleChanges (batched; the human approves each change). Never re-propose a rejected change.\n` +
      `  - Reaching your human: AskUserQuestion ONLY — never print questions to the terminal or write status notes.\n\n` +
      `Standing doctrine (the active ruleset — improvise with these same thresholds and phrasings when asked to check on tabs by hand):\n` +
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
        if (getVariables(tab.id)?.get(OVERLORD_PRIMED_VAR) === '1') continue;
        if (!(await hasLiveRepl(tab.id))) { primedAgents.delete(tab.id); continue; }
        const inst = terminalsStore.get(tab.id);
        if (!inst) { primedAgents.delete(tab.id); continue; }
        try {
          await bracketedPasteSubmit(inst.ptyId, buildDoctrine());
          await setVariable(tab.id, OVERLORD_PRIMED_VAR, '1');
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
            dispatch('Overlord', `"${rule.name}" stalled on ${tabName(tabId)} — step ${i + 1} timed out.`, 'error', { tabId });
          } else if (behavior === 'escalate_to_overlord') {
            escalate(tabId, rule.id, 'step_timeout', `"${rule.name}" step ${i + 1} (${step.text.slice(0, 80)}) timed out on ${tabName(tabId)}.`);
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
        tabName: tabName(tabId),
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

  /** Age out finished rows so the board doesn't accumulate history. Human-authored tasks
   *  are exempt: someone typed those, and silently deleting them two days later is a
   *  surprise. Everything machine-authored is swept — imported mirrors, agent-created
   *  tasks, and Overlord's own placeholders alike. */
  function sweepDoneTasks(now: number): boolean {
    let changed = false;
    for (const ws of workspacesStore.workspaces) {
      const swept = tasksStore.mutate(ws.id, (list) => {
        const next = list.filter(
          (t) =>
            t.status !== 'done' ||
            t.origin === 'human' ||
            now - Date.parse(t.updated_at) < TASK_DONE_RETENTION_MS,
        );
        // (Parked rows are never 'done', so the sweep cannot reach them — a shelved idea
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
    return !!ws && !ws.overlord;
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
      // A finished row stays finished; a scan must not resurrect it.
      if (existing.status === 'done') return 'skipped';
      // Bump recency so a tab that is demonstrably alive never ages into "stale".
      tasksStore.update(ws.id, existing.id, {});
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
      if (existing.status === 'done' || existing.title === title) return false;
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
    if (!ws || !existing || existing.status === 'done') return false;
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
    for (const id of [...liveness.keys()]) if (!live.has(id)) liveness.delete(id);
    if (!candidates.length) return;
    try {
      const res = await commands.getAgentLivenessBatch(candidates.map((c) => c.ptyId));
      for (const { tab, ptyId } of candidates) {
        const l = res[ptyId];
        if (!l) continue;
        // ssh_foreground stands in for a remote agent we cannot see in the local process
        // tree — an SSH tab with a live session is treated as unbound, which is the case
        // that actually happens after a restart.
        liveness.set(tab.id, { kind: l.agent_running || l.ssh_foreground ? 'unbound' : 'stopped', at: now });
      }
    } catch (e) {
      logError(`overlord: liveness probe failed: ${e}`);
    }
  }

  function unreadyKind(tabId: string): UnreadyKind | null {
    return liveness.get(tabId)?.kind ?? null;
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
        // A driveTab directive with no ritual watching it is spent once the target's
        // turn demonstrably ran (or it was acked) — otherwise it locks the tab.
        const od = outstanding.get(tab.id);
        if (od && od.ruleId === null && (od.acked || (f?.last_turn_ts !== undefined && f.last_turn_ts > od.sentAt))) {
          clearOutstanding(tab.id);
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
          if (!rule.sequence.length) continue;
          if (!conditionFires(rule, tab, now, turnEnded, committed)) continue;
          if (!guardsPassSync(rule, tab.id, now)) continue;
          if (preferencesStore.overlordProposeMode) {
            propose(rule, tab.id);
          } else {
            void runSequence(rule, tab.id, 'rule');
          }
          break; // one rule fire per tab per tick — directives serialize anyway
        }
      }
      void tryPrimeOverlordAgent();
      // Escalations that arrived while the agent was busy still owe it a doorbell.
      if (unNudged.size) void wakeOverlordAgent();
      sweepDoneTasks(now);
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
      ticker = setInterval(() => void tick(), TICK_MS);
      running = true;
      logInfo('overlord: engine started');
    },

    destroy() {
      if (ticker) clearInterval(ticker);
      ticker = null;
      running = false;
      for (const run of rituals.values()) run.aborted = true;
    },

    // ── Propose-mode (§3) ────────────────────────────────────────────────────
    approveProposal(id: string) {
      const p = proposals.find((x) => x.id === id);
      if (!p) return;
      proposals = proposals.filter((x) => x.id !== id);
      const rule = preferencesStore.overlordRules.find((r) => r.id === p.ruleId);
      if (!rule) return;
      void runSequence($state.snapshot(rule) as OverlordRule, p.tabId, 'rule');
    },
    dismissProposal(id: string) {
      proposals = proposals.filter((x) => x.id !== id);
    },

    // ── Escalations (§9.1) ───────────────────────────────────────────────────
    /** Pull + mark read — the listEscalations MCP surface (S4) and the board both use this. */
    consumeEscalations(): OverlordEscalation[] {
      // Unread only — re-delivering handled escalations makes the agent re-resolve
      // day-old timeouts. Read ones stay on the board until the human dismisses them.
      const out = ($state.snapshot(escalations) as OverlordEscalation[]).filter((e) => !e.read);
      escalations = escalations.map((e) => (e.read ? e : { ...e, read: true }));
      unNudged.clear(); // delivered by the pull itself; no doorbell owed
      return out;
    },
    dismissEscalation(id: string) {
      escalations = escalations.filter((e) => e.id !== id);
      unNudged.delete(id);
    },
    /** An agent reported blocked / needs_human via replyToOverlord (S4 wiring). */
    reportFromAgent(tabId: string, detail: string) {
      escalate(tabId, null, 'agent_report', detail);
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
    deleteTask(id: string) {
      const hit = tasksStore.findAnywhere(id);
      if (hit) tasksStore.remove(hit.workspaceId, id);
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
            if (tracked.every((t) => t.status === 'done')) finished++;
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
    async recoverTab(tabId: string): Promise<{ sent: boolean; kind?: UnreadyKind; reason?: string }> {
      const kind = unreadyKind(tabId);
      if (!kind) return { sent: false, reason: 'not_unready' };
      const inst = terminalsStore.get(tabId);
      if (!inst) return { sent: false, reason: 'no_terminal' };

      let text: string;
      if (kind === 'unbound') {
        text = '/maiterm init';
      } else {
        const runtime = workspacesStore.getTabRuntime(tabId);
        if (!runtime) return { sent: false, reason: 'unknown_runtime' };
        // Interpolates %<runtime>SessionId from the tab's trigger variables, the same way
        // auto-resume does — so this resumes the tab's own session, not a fresh one.
        text = interpolateVariables(tabId, getResumeCommand(runtime));
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
      // Clear the classification so the deck stops showing it immediately; the next tick
      // re-probes and will re-raise it if the remedy didn't take.
      liveness.delete(tabId);
      bumpLive();
      return { sent: true, kind };
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
      if (args.needs_human || args.kind === 'escalate' || args.state === 'blocked') {
        const blockers = args.blockers?.length ? ` — blockers: ${args.blockers.join('; ')}` : '';
        escalate(tabId, null, 'agent_report', `${tabName(tabId)} reports ${args.state}: ${summary}${blockers}`);
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
        tab_name: tabName(e.tabId),
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
      const decision = await new Promise<{ approved: string[]; rejected: string[]; pending?: boolean }>((resolve) => {
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
      return { approved: decision.approved, rejected: decision.rejected };
    },

    /** The approval modal's answer: apply the selected change indexes, reject the rest. */
    resolveRuleChanges(batchId: string, approvedIdx: number[]) {
      const batch = pendingRuleChanges;
      if (!batch || batch.id !== batchId) return;
      pendingRuleChanges = null;
      const approved: string[] = [];
      const rejected: string[] = [];
      let rules = ($state.snapshot(preferencesStore.overlordRules) as OverlordRule[]);
      batch.changes.forEach((change, i) => {
        const label = describeChange(change);
        if (!approvedIdx.includes(i)) {
          rejectedChangeKeys.add(changeKey(change));
          rejected.push(label);
          return;
        }
        // Deleting a seeded default must also hide its default_id, or the next
        // startup re-seeds it right back.
        if (change.op === 'delete') {
          const target = matchRule(rules, change.rule_id);
          if (target?.default_id && !preferencesStore.hiddenDefaultOverlordRules.includes(target.default_id)) {
            void preferencesStore.setHiddenDefaultOverlordRules([
              ...preferencesStore.hiddenDefaultOverlordRules,
              target.default_id,
            ]);
          }
        }
        rules = applyRuleChange(rules, change);
        approved.push(label);
        ledger(batch.tabId, null, 'overlord_judgment', 0, { kind: 'process', text: `rule change approved: ${label}` }, 'sent');
      });
      if (approved.length) void preferencesStore.setOverlordRules(rules);
      ruleChangeResolver?.({ approved, rejected });
      ruleChangeResolver = null;
    },

    /** Drive a tab on the Overlord agent's behalf (S4 driveTab): same guards, same
     *  ledger, structured refusal. */
    async driveTab(tabId: string, kind: 'process' | 'slash', text: string): Promise<{ sent: boolean; reason?: string }> {
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
      if (outstanding.has(tabId) || rituals.has(tabId)) {
        ledger(tabId, null, 'overlord_judgment', 0, step, 'blocked_guard');
        return { sent: false, reason: 'outstanding_directive' };
      }
      if (!(await hasLiveRepl(tabId))) {
        ledger(tabId, null, 'overlord_judgment', 0, step, 'blocked_no_repl');
        return { sent: false, reason: 'no_live_repl' };
      }
      const st = mappedState(tabId);
      if (st !== 'idle') {
        ledger(tabId, null, 'overlord_judgment', 0, step, 'blocked_guard');
        return { sent: false, reason: 'agent_busy' };
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
