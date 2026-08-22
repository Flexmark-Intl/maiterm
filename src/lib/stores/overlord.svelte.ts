import * as commands from '$lib/tauri/commands';
import type { OverlordTabFacts } from '$lib/tauri/commands';
import type {
  OverlordGuards,
  OverlordLedgerEntry,
  OverlordLedgerOutcome,
  OverlordRule,
  OverlordStep,
  OverlordTask,
  OverlordTaskState,
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
  tabId: string;
  aborted: boolean;
}

function createOverlordStore() {
  // ── Reactive surfaces (board + gauges + queues) ─────────────────────────────
  let facts = $state<Map<string, OverlordTabFacts>>(new Map());
  let tasks = $state<OverlordTask[]>([]);
  let proposals = $state<OverlordProposal[]>([]);
  let escalations = $state<OverlordEscalation[]>([]);
  let recentLedger = $state<OverlordLedgerEntry[]>([]);
  let running = $state(false);

  // ── Engine bookkeeping (non-reactive) ───────────────────────────────────────
  const prevAgentState = new Map<string, AgentState | undefined>();
  const prevCommitTs = new Map<string, number | undefined>();
  const permissionSince = new Map<string, number>();
  const lastFiredAt = new Map<string, number>(); // `${ruleId}|${tabId}` → ms
  const fireLog = new Map<string, number[]>(); // `${ruleId}|${tabId}` → recent fire ts
  const outstanding = new Map<string, OutstandingDirective>(); // tabId → directive
  const rituals = new Map<string, RitualRun>(); // tabId → active ritual
  let ticker: ReturnType<typeof setInterval> | null = null;
  let ticking = false;
  let tasksDirty = false;

  const sleep = (ms: number) => new Promise<void>((r) => setTimeout(r, ms));

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
    // agent_state (default idle-only)
    const allowed = g.agent_state ?? ['idle'];
    const st = mappedState(tabId);
    if (!st || !allowed.includes(st)) return false;
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
      const st = mappedState(run.tabId);
      const lastOut = terminalsStore.getLastOutputAt(run.tabId) ?? 0;
      if (st && allowed.includes(st) && Date.now() - lastOut >= quiet) return true;
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
    void wakeOverlordAgent();
  }

  /** One-line doorbell into the Overlord agent's PTY (§9.1) — content stays behind
   *  the listEscalations pull, keeping the agent's transcript lean. No live agent →
   *  the queue simply waits (the engine runs regardless; §2 agent lifecycle). */
  async function wakeOverlordAgent() {
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
        } catch (e) {
          logError(`overlord: wake nudge failed: ${e}`);
        }
        return;
      }
    }
  }

  /** Run a rule's sequence against a tab. All ledger writes for the run happen here. */
  async function runSequence(rule: OverlordRule, tabId: string, origin: OverlordLedgerEntry['origin']) {
    if (rituals.has(tabId)) return;
    const run: RitualRun = { runId: crypto.randomUUID(), ruleId: rule.id, tabId, aborted: false };
    rituals.set(tabId, run);
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
        const preInject = Date.now();
        if (humanTypedSince(tabId, preInject - (rule.guards.min_quiet_ms ?? 3000))) {
          // Human is at the keyboard in this tab right now — their tab, their turn (§7).
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
        const directive: OutstandingDirective = {
          id: crypto.randomUUID(),
          ruleId: rule.id,
          tabId,
          stepIndex: i,
          text: step.text,
          sentAt: Date.now(),
          acked: false,
        };
        if (step.await) outstanding.set(tabId, directive);
        ledger(tabId, rule.id, origin, i, step, 'sent');
        const res = await awaitGate(run, step, directive);
        if (outstanding.get(tabId)?.id === directive.id) outstanding.delete(tabId);
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
        return tasks.some(
          (t) =>
            t.tab_id === tab.id &&
            t.state !== 'done' &&
            now - Date.parse(t.updated_at) >= w.days * 86_400_000,
        );
      case 'agent_unready':
        return !claudeStateStore.getState(tab.id) && !!terminalsStore.get(tab.id);
      case 'no_todo_list': {
        if (!f || f.todos) return false;
        if ((f.context_used ?? 0) < NO_TODO_MIN_CONTEXT_TOKENS) return false;
        return f.last_turn_ts !== undefined && now - f.last_turn_ts < NO_TODO_RECENT_TURN_MS;
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

  // ── TodoWrite mirror → board (§11) ─────────────────────────────────────────

  function todoState(status: string): OverlordTaskState {
    if (status === 'completed') return 'done';
    if (status === 'in_progress') return 'active';
    return 'backlog';
  }

  function syncMirrorTasks(tabId: string, f: OverlordTabFacts, now: number): boolean {
    if (!f.todos) return false;
    const ws = workspaceForTab(tabId);
    if (!ws) return false;
    let changed = false;
    for (const item of f.todos) {
      if (!item?.content) continue;
      const existing = tasks.find((t) => t.origin === 'agent' && t.tab_id === tabId && t.title === item.content);
      const state = todoState(item.status);
      if (existing) {
        if (existing.state !== state) {
          existing.state = state;
          existing.updated_at = new Date(now).toISOString();
          changed = true;
        }
      } else {
        tasks.push({
          id: crypto.randomUUID(),
          title: item.content,
          workspace_id: ws.id,
          tab_id: tabId,
          state,
          origin: 'agent',
          created_at: new Date(now).toISOString(),
          updated_at: new Date(now).toISOString(),
        });
        changed = true;
      }
    }
    return changed;
  }

  function sweepDoneTasks(now: number): boolean {
    const before = tasks.length;
    tasks = tasks.filter((t) => t.state !== 'done' || now - Date.parse(t.updated_at) < TASK_DONE_RETENTION_MS);
    return tasks.length !== before;
  }

  function persistTasks() {
    commands.setOverlordTasks($state.snapshot(tasks) as OverlordTask[]).catch((e) =>
      logError(`overlord: task persist failed: ${e}`),
    );
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
      let boardChanged = false;
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
        // TodoWrite mirror
        if (f && syncMirrorTasks(tab.id, f, now)) boardChanged = true;
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
      if (sweepDoneTasks(now)) boardChanged = true;
      if (boardChanged || tasksDirty) {
        tasksDirty = false;
        tasks = [...tasks]; // Map/array reactivity: new array so $derived consumers re-read
        persistTasks();
      }
    } finally {
      ticking = false;
    }
  }

  // ── Public API ──────────────────────────────────────────────────────────────

  return {
    get running() { return running; },
    get facts() { return facts; },
    get tasks() { return tasks; },
    get proposals() { return proposals; },
    get escalations() { return escalations; },
    get recentLedger() { return recentLedger; },
    /** tabId of any in-flight ritual's target, for board display. */
    get activeRituals() { return [...rituals.keys()]; },
    get outstandingDirectives() { return [...outstanding.values()]; },

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
      try {
        const win = await commands.getWindowData();
        tasks = win.overlord_tasks ?? [];
      } catch (e) {
        logError(`overlord: board load failed: ${e}`);
      }
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
      const out = $state.snapshot(escalations) as OverlordEscalation[];
      escalations = escalations.map((e) => ({ ...e, read: true }));
      return out;
    },
    dismissEscalation(id: string) {
      escalations = escalations.filter((e) => e.id !== id);
    },
    /** An agent reported blocked / needs_human via replyToOverlord (S4 wiring). */
    reportFromAgent(tabId: string, detail: string) {
      escalate(tabId, null, 'agent_report', detail);
    },
    /** Resolve an 'ack' gate / clear the outstanding directive for a tab (§8). */
    ackOutstanding(tabId: string) {
      const d = outstanding.get(tabId);
      if (d) d.acked = true;
    },

    // ── Board CRUD (human-owned rows; §11) ───────────────────────────────────
    addTask(title: string, workspaceId: string, tabId?: string | null) {
      const now = new Date().toISOString();
      tasks = [
        ...tasks,
        {
          id: crypto.randomUUID(),
          title,
          workspace_id: workspaceId,
          tab_id: tabId ?? null,
          state: 'backlog',
          origin: 'human',
          created_at: now,
          updated_at: now,
        },
      ];
      persistTasks();
    },
    updateTaskState(id: string, state: OverlordTaskState) {
      tasks = tasks.map((t) => (t.id === id ? { ...t, state, updated_at: new Date().toISOString() } : t));
      persistTasks();
    },
    deleteTask(id: string) {
      tasks = tasks.filter((t) => t.id !== id);
      persistTasks();
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
      outstanding.set(tabId, {
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
        if (d && d.text === text && Date.now() - d.sentAt > 15 * 60_000) outstanding.delete(tabId);
      }, 15 * 60_000 + 1000);
      return { sent: true };
    },
  };
}

export const overlordStore = createOverlordStore();
