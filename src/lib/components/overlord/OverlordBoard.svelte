<script lang="ts">
  import { onMount, onDestroy } from 'svelte';
  import { workspacesStore, navigateToTab, tabDisplayName } from '$lib/stores/workspaces.svelte';
  import { claudeStateStore } from '$lib/stores/agentState.svelte';
  import { overlordStore, type SpentTab } from '$lib/stores/overlord.svelte';
  import { preferencesStore } from '$lib/stores/preferences.svelte';
  import type { Workspace, Tab } from '$lib/tauri/types';
  import { isInFlight, type TaskRow } from '$lib/tasks/model';
  import { tasksStore } from '$lib/stores/tasks.svelte';
  import { escalationLabel, fireRefusal, fmtAge, outcomeLabel, outcomeTone } from '$lib/overlord/format';
  import Tooltip from '$lib/components/Tooltip.svelte';
  import ContextMenu from '$lib/components/ContextMenu.svelte';
  import OverlordBoardView from './OverlordBoardView.svelte';
  import '$lib/overlord/deck.css';

  /**
   * The Overlord deck (docs/overlord.md §11).
   *
   * Ordered by what the human actually needs: TRIAGE first (the one queue worth opening
   * every morning), then the FLEET — one instrument per supervised agent, which is the
   * direct answer to "what am I losing track of" — then the board, then the ledger.
   * Kanban is for planning; the queue and the fleet are for the actual pain.
   */

  interface Props {
    workspaceId: string;
    paneId: string;
    tabId: string;
    visible: boolean;
  }

  let { workspaceId, paneId, tabId, visible }: Props = $props();

  function focusPane() {
    if (workspacesStore.activeWorkspace?.active_pane_id !== paneId) {
      workspacesStore.setActivePane(workspaceId, paneId);
    }
  }

  // ── Portal plumbing (same contract as DiffPane/EditorPane) ──────────────────
  let containerRef: HTMLDivElement;

  function attachToSlot() {
    const slot = document.querySelector(`[data-terminal-slot="${tabId}"]`) as HTMLElement;
    if (slot && containerRef && containerRef.parentElement !== slot) slot.appendChild(containerRef);
  }

  function handleSlotReady(e: Event) {
    if ((e as CustomEvent).detail?.tabId === tabId) attachToSlot();
  }

  onMount(() => {
    attachToSlot();
    window.addEventListener('terminal-slot-ready', handleSlotReady);
  });

  onDestroy(() => {
    window.removeEventListener('terminal-slot-ready', handleSlotReady);
    clearInterval(clock);
    containerRef?.remove();
  });

  // Ages are relative; re-tick so "4m" doesn't freeze on a board left open.
  let now = $state(Date.now());
  const clock = setInterval(() => { now = Date.now(); }, 20_000);

  // ── Constants ───────────────────────────────────────────────────────────────
  /** Only used when NO context_pct rule is enabled — then nothing can act at any level,
   *  and the deck says so rather than pretending a checkpoint is coming. */
  const PRESSURE_FALLBACK_PCT = 60;
  const STALE_DAYS = 3;

  /** Raise pressure at the level where a checkpoint will actually fire, not at a constant
   *  of our own. The old hardcoded 50 sat five points below the default rule's 55, so
   *  every tab in that band got a card saying "a checkpoint should run" while no rule
   *  could possibly run one. */
  const PRESSURE_PCT = $derived(overlordStore.checkpointThreshold ?? PRESSURE_FALLBACK_PCT);

  type View = 'deck' | 'fleet' | 'board' | 'ledger';
  let view = $state<View>('deck');

  // ── Fleet derivation ────────────────────────────────────────────────────────
  const boardWorkspaces = $derived(workspacesStore.workspaces.filter((w) => !w.overlord && !w.overlord_exempt));

  interface FleetUnit {
    tab: Tab;
    ws: Workspace;
    state: 'permission' | 'active' | 'idle' | 'dormant';
    pct: number | null;
    lastTurn: number | undefined;
    todosDone: number;
    todosTotal: number;
    topTodo: string | null;
    ritual: { ruleName: string; step: number; steps: number } | null;
    /** Its TerminalPane is mounted, so it can be probed, classified and typed into. */
    loaded: boolean;
    awaiting: boolean;
    report: string | null;
  }

  function unitFor(tab: Tab, ws: Workspace): FleetUnit {
    const f = overlordStore.facts.get(tab.id);
    const live = claudeStateStore.getState(tab.id);
    const todos = f?.todos ?? [];
    const active = todos.find((t) => t.status === 'in_progress');
    const r = overlordStore.ritualProgress.find((x) => x.tabId === tab.id);
    return {
      tab,
      ws,
      state: live ? live.state : 'dormant',
      pct: f?.context_pct ?? null,
      lastTurn: f?.last_turn_ts,
      todosDone: todos.filter((t) => t.status === 'completed').length,
      todosTotal: todos.length,
      topTodo: active?.content ?? todos.find((t) => t.status === 'pending')?.content ?? null,
      ritual: r ? { ruleName: r.ruleName, step: r.step, steps: r.steps } : null,
      loaded: overlordStore.tabLoaded(tab.id),
      awaiting: !!overlordStore.outstandingFor(tab.id),
      report: overlordStore.agentReports.get(tab.id)?.summary ?? null,
    };
  }

  /** `context` — most-in-need first: busy rituals, then permission, then pressure, then
   *  staleness, highest context within each. `activity` — most recent real turn first. */
  type FleetSort = 'context' | 'activity';
  let fleetSort = $state<FleetSort>('context');

  /** Agent tabs whose whole workspace is suspended. Parked, not dormant: every PTY in it is
   *  killed by design, so a card for one would say "not loaded" about a tab nobody expects
   *  to be running. They are counted so the fleet says where they went, not shown. */
  const parkedCount = $derived.by(() => {
    let n = 0;
    for (const ws of boardWorkspaces) {
      if (!ws.suspended) continue;
      for (const pane of ws.panes) {
        for (const tab of pane.tabs) {
          if ((tab.tab_type ?? 'terminal') === 'terminal' && tab.runtime && !tab.overlord_exempt) n++;
        }
      }
    }
    return n;
  });

  /** Agent tabs the human exempted, by tab flag or workspace flag. Counted in the bar, never
   *  shown: a card for one would offer actions every one of which is refused. */
  const exemptCount = $derived.by(() => {
    let n = 0;
    for (const ws of workspacesStore.workspaces) {
      if (ws.overlord) continue;
      for (const pane of ws.panes) {
        for (const tab of pane.tabs) {
          if ((tab.tab_type ?? 'terminal') === 'terminal' && tab.runtime && (ws.overlord_exempt || tab.overlord_exempt)) n++;
        }
      }
    }
    return n;
  });

  const fleet = $derived.by<FleetUnit[]>(() => {
    void now;
    const units: FleetUnit[] = [];
    for (const ws of boardWorkspaces) {
      if (ws.suspended) continue;
      for (const pane of ws.panes) {
        for (const tab of pane.tabs) {
          if ((tab.tab_type ?? 'terminal') !== 'terminal' || !tab.runtime || tab.overlord_exempt) continue;
          units.push(unitFor(tab, ws));
        }
      }
    }
    if (fleetSort === 'activity') {
      return units.sort((a, b) => (b.lastTurn ?? 0) - (a.lastTurn ?? 0) || (b.pct ?? 0) - (a.pct ?? 0));
    }
    const rank = (u: FleetUnit) =>
      (u.ritual ? 0 : u.state === 'permission' ? 1 : (u.pct ?? 0) >= PRESSURE_PCT ? 2 : u.state === 'dormant' ? 4 : 3);
    return units.sort((a, b) => rank(a) - rank(b) || (b.pct ?? 0) - (a.pct ?? 0));
  });

  /**
   * The Overlord agent's own tab, when it is stopped at a prompt.
   *
   * Its workspace is deliberately absent from `boardWorkspaces` — it is the supervisor, not
   * a supervised tab, and every other card would be wrong or dangerous applied to it: a
   * re-bind it can't accept (`isBoardableTab` refuses the tools), an Archive/Close button
   * that would put away the supervisor, a checkpoint for a session nothing else supervises.
   *
   * But being blocked is the one state where that exclusion hurt. The agent's only sanctioned
   * way to reach its human is AskUserQuestion, which stops it dead; supervision for the whole
   * window stops with it; and the board the human opens to find out why showed an empty deck.
   * So: this one signal, above everything else on the queue, with the same remedy any
   * permission card offers — open the tab and answer it.
   */
  const supervisorBlocked = $derived.by<FleetUnit | null>(() => {
    void now;
    const ws = workspacesStore.workspaces.find((w) => w.overlord);
    if (!ws) return null;
    for (const pane of ws.panes) {
      for (const tab of pane.tabs) {
        if ((tab.tab_type ?? 'terminal') !== 'terminal' || !tab.runtime) continue;
        if (claudeStateStore.getState(tab.id)?.state !== 'permission') continue;
        return unitFor(tab, ws);
      }
    }
    return null;
  });

  // ── Triage signals — one severity-ordered queue ─────────────────────────────
  type Signal =
    | { sev: number; id: string; type: 'proposal'; p: (typeof overlordStore.proposals)[number] }
    | { sev: number; id: string; type: 'escalation'; e: (typeof overlordStore.escalations)[number] }
    | { sev: number; id: string; type: 'permission' | 'pressure' | 'unready'; u: FleetUnit; supervisor?: boolean }
    | { sev: number; id: string; type: 'stale'; t: TaskRow }
    | { sev: number; id: string; type: 'spent'; s: SpentTab };

  const spent = $derived(overlordStore.spentTabs);

  const signals = $derived.by<Signal[]>(() => {
    const out: Signal[] = [];
    // Ahead of the escalations, because a blocked supervisor is why there are no new ones:
    // nothing else on this deck can advance until it is answered.
    if (supervisorBlocked) {
      out.push({ sev: -1, id: `perm-sup-${supervisorBlocked.tab.id}`, type: 'permission', u: supervisorBlocked, supervisor: true });
    }
    // A finished session that has also gone dormant is not offered a re-bind: its agent
    // EXITED having done everything asked of it, so packing it away is the useful move.
    //
    // This is only safe because `spentTabs` admits a stateless tab solely when it is
    // classified `stopped`. An `unbound` tab still has a live agent process, and hiding
    // its re-bind here while the deck offers Archive/Close would hand the human a button
    // that kills a running agent. Do not widen that predicate without revisiting this.
    const spentIds = new Set(spent.map((s) => s.tabId));
    // Some escalations are addressed to the Overlord AGENT, not to the human — a tab
    // answering a question the agent asked it, routing information for an agent that is
    // blocked waiting, a task the human handed over. Showing those would turn every
    // ordinary Overlord↔tab exchange into a red card on the triage queue.
    //
    // Which kinds those are is the STORE's list (`AGENT_ONLY_ESCALATIONS`), read through
    // `humanEscalations`. This used to name them inline, so every kind added to the set
    // afterwards leaked onto the deck — and the sidebar badge, which already read
    // `humanEscalations`, disagreed with the deck about what was waiting.
    for (const e of overlordStore.humanEscalations) {
      out.push({ sev: 0, id: e.id, type: 'escalation', e });
    }
    for (const p of overlordStore.proposals) out.push({ sev: 1, id: p.id, type: 'proposal', p });
    for (const u of fleet) {
      if (u.state === 'permission') out.push({ sev: 2, id: `perm-${u.tab.id}`, type: 'permission', u });
      else if ((u.pct ?? 0) >= PRESSURE_PCT) out.push({ sev: 3, id: `ctx-${u.tab.id}`, type: 'pressure', u });
      // `loaded` gates this, not just dormancy. A tab in a suspended or never-opened
      // workspace has no mounted pane, so nothing can probe it, classify it or type into
      // it — it would raise a permanent card that describes a problem and offers nothing,
      // one per agent tab in the window. It isn't unready; it isn't loaded, which is what
      // a suspended workspace means. Opening the workspace mounts the pane and the card
      // appears with a remedy that works.
      else if (u.state === 'dormant' && u.loaded && !spentIds.has(u.tab.id)) {
        out.push({ sev: 5, id: `dead-${u.tab.id}`, type: 'unready', u });
      }
    }
    // Last: housekeeping, not a problem. Nothing here is going wrong.
    for (const s of spent) out.push({ sev: 6, id: `spent-${s.tabId}`, type: 'spent', s });
    for (const t of overlordStore.tasks) {
      // Parked tasks are never stale. A backlog item is meant to sit untouched — raising
      // it here would turn the parking lot into a queue of things demanding attention.
      if (isInFlight(t) && now - Date.parse(t.updated_at) > STALE_DAYS * 86_400_000) {
        out.push({ sev: 4, id: `stale-${t.id}`, type: 'stale', t });
      }
    }
    return out.sort((a, b) => a.sev - b.sev);
  });

  // ── Telemetry ───────────────────────────────────────────────────────────────
  const peakPct = $derived(fleet.reduce((m, u) => Math.max(m, u.pct ?? 0), 0));
  const inFlight = $derived(overlordStore.ritualProgress.length);
  const sentToday = $derived.by(() => {
    void now;
    const cutoff = now - 86_400_000;
    return overlordStore.recentLedger.filter(
      (e) => e.outcome === 'sent' && Date.parse(e.ts) > cutoff,
    ).length;
  });
  /** Excludes the put-away group. A finished session offered for archiving needs nothing
   *  from anyone — counting it made "needs you: 64" mostly a count of work that went
   *  well, which is the readout people glance at to decide whether to open this tab. */
  const needsYou = $derived(signals.filter((s) => s.sev < 6).length);
  const engineOn = $derived(preferencesStore.overlordEnabled);
  const scan = $derived(overlordStore.lastScan);
  let asking = $state(false);

  async function runScan() { await overlordStore.scanWorkspaces(); }

  async function runTrackRequest() {
    if (!scan?.silent.length || asking) return;
    asking = true;
    try { await overlordStore.askTabsToTrack(scan.silent); } finally { asking = false; }
  }

  function workstreamName(t: TaskRow): string | null {
    return tasksStore.workstream(t.workspace_id, t.workstream_id)?.name ?? null;
  }

  // ── Recovery ───────────────────────────────────────────────────────────────
  let recovering = $state<string | null>(null);
  let recoveringAll = $state(false);
  let recoverNote = $state<string | null>(null);

  async function recover(tabId: string) {
    recovering = tabId;
    try {
      const r = await overlordStore.recoverTab(tabId);
      // Say what happened rather than failing silently — a button that does nothing
      // visible is the problem this whole change exists to fix.
      recoverNote = r.sent
        ? null
        : r.reason === 'output_in_flight'
          ? 'That tab is mid-output — try again in a moment.'
          : r.reason === 'no_session_id'
            ? "No saved session for that tab, so there's nothing to resume."
            : `Couldn't recover that tab (${r.reason}).`;
    } finally {
      recovering = null;
    }
  }

  const unboundCount = $derived(
    fleet.filter((u) => u.state === 'dormant' && overlordStore.unreadyKind(u.tab.id) === 'unbound').length,
  );

  async function recoverAll() {
    recoveringAll = true;
    try {
      const r = await overlordStore.recoverAllUnbound();
      // Says what it KNOWS. This claimed "Re-bound N" the instant the last paste went out,
      // which is delivery, not outcome — and a re-bind typed at a tab whose agent is gone
      // is delivered perfectly. Unlike Run all, this button doesn't hold open to verify
      // (it's usually a tab or two, and a 45s spinner would be worse); the cards say.
      recoverNote = r.sent
        ? `Sent /maiterm init to ${r.sent} tab${r.sent === 1 ? '' : 's'} — any that don't answer come back as needing a restart.`
        : 'Nothing could be re-bound.';
    } finally {
      recoveringAll = false;
    }
  }

  // ── Run all ────────────────────────────────────────────────────────────────
  const runnable = $derived(overlordStore.triageActionable);
  const runProgress = $derived(overlordStore.triageRun);

  /** Seconds left in the pacing gap. Re-derived off `now`… which ticks every 20s, far too
   *  slow for a 5s countdown — so the run drives its own 1s clock while it is live. */
  let runTick = $state(0);
  $effect(() => {
    if (!runProgress) return;
    const t = setInterval(() => { runTick = Date.now(); }, 500);
    return () => clearInterval(t);
  });
  const resumeIn = $derived.by(() => {
    void runTick;
    const at = runProgress?.resumeAt;
    return at ? Math.max(0, Math.ceil((at - Date.now()) / 1000)) : 0;
  });

  // ── Checkpoint ─────────────────────────────────────────────────────────────
  let checkpointing = $state<string | null>(null);

  async function checkpoint(tabId: string) {
    checkpointing = tabId;
    try {
      const r = await overlordStore.checkpointTab(tabId);
      recoverNote = r.started ? null : fireRefusal(r.reason, 'a checkpoint');
    } finally {
      checkpointing = null;
    }
  }

  /** A proposal refused for a permission prompt stays on the deck; say why the click did
   *  nothing, or the button reads as broken. */
  function approve(id: string) {
    const r = overlordStore.approveProposal(id);
    // Cleared on success like every other path here: the refusal from the first click
    // must not outlive the retry that worked, or that send reads as a second refusal.
    recoverNote = r === 'permission' ? fireRefusal('tab_permission', 'that') : null;
  }

  // ── Fleet: manual trigger ──────────────────────────────────────────────────
  /** Which card's Trigger menu is open, and where. */
  let triggerMenu = $state<{ x: number; y: number; tabId: string; anchor: HTMLElement } | null>(null);
  /** A refusal, shown on the card that was clicked rather than in the deck's note slot:
   *  the fleet is a grid, and a message at the top of it doesn't say which card it means. */
  let unitNotes = $state<Record<string, string>>({});

  /** Toggles. The menu leaves a mousedown on its anchor alone, so this is the only
   *  handler that runs for a second press on the same button. */
  function openTrigger(e: MouseEvent, tabId: string) {
    if (triggerMenu?.tabId === tabId) { triggerMenu = null; return; }
    const anchor = e.currentTarget as HTMLElement;
    const r = anchor.getBoundingClientRect();
    triggerMenu = { x: r.left, y: r.bottom + 4, tabId, anchor };
  }

  function triggerItems(tabId: string) {
    return overlordStore.rulesForTab(tabId).map((rule) => ({
      label: rule.name,
      // A disabled rule is still offered — "don't run this on its own, but let me run it" —
      // and says so, since firing it is the one time its switch position matters.
      shortcut: rule.enabled ? undefined : 'off',
      action: () => void fire(tabId, rule.id),
    }));
  }

  function clearNote(tabId: string) {
    if (!(tabId in unitNotes)) return;
    const next = { ...unitNotes };
    delete next[tabId];
    unitNotes = next;
  }

  async function fire(tabId: string, ruleId: string) {
    clearNote(tabId);
    const r = await overlordStore.fireRule(tabId, ruleId);
    if (r.started) return; // the card's ritual strip is the feedback
    unitNotes = { ...unitNotes, [tabId]: fireRefusal(r.reason, 'that rule') };
    setTimeout(() => clearNote(tabId), 8000);
  }

  // ── Archive / close ────────────────────────────────────────────────────────
  let tabBusy = $state<string | null>(null);
  /** Inline confirmation — `confirm()` does nothing in a Tauri webview, and closing a
   *  session is the one action here with no way back. */
  let closingTab = $state<string | null>(null);
  let archivingAll = $state(false);

  async function archiveTab(tabId: string) {
    tabBusy = tabId;
    try {
      const ok = await overlordStore.archiveSpentTab(tabId);
      if (!ok) recoverNote = "Couldn't archive that tab.";
    } finally { tabBusy = null; }
  }

  async function closeTab(tabId: string) {
    tabBusy = tabId;
    try {
      const ok = await overlordStore.closeSpentTab(tabId);
      if (!ok) recoverNote = "Couldn't close that tab.";
    } finally {
      tabBusy = null;
      closingTab = null;
    }
  }

  async function archiveAll() {
    archivingAll = true;
    try {
      const r = await overlordStore.archiveAllSpent();
      recoverNote = `Archived ${r.archived} session${r.archived === 1 ? '' : 's'}${r.skipped ? `, skipped ${r.skipped}` : ''}.`;
    } finally { archivingAll = false; }
  }

  /** Reports the OUTCOME, not the delivery. "Sent 69" was true and useless: a re-bind typed
   *  at a tab whose agent is gone is delivered perfectly and achieves nothing, which is the
   *  normal failure for SSH tabs. The tabs that never answered are named as needing a
   *  restart, because that is the remedy their cards now offer. */
  async function runAll() {
    const r = await overlordStore.runTriage();
    if (r.sent === 0 && r.skipped === 0) { recoverNote = null; return; }
    const parts = [`Run all: sent ${r.sent}`];
    if (r.skipped) parts.push(`skipped ${r.skipped}`);
    if (r.bound) parts.push(`re-bound ${r.bound}`);
    recoverNote = r.silent
      ? `${parts.join(', ')} — ${r.silent} never answered and ${r.silent === 1 ? 'needs' : 'need'} a restart.`
      : `${parts.join(', ')}.`;
  }

  /** Context ring geometry — r=13 → circumference 81.68. */
  const RING_C = 81.68;
  function ringDash(pct: number | null): string {
    const v = Math.max(0, Math.min(100, pct ?? 0));
    return `${(v / 100) * RING_C} ${RING_C}`;
  }
  function pctTone(pct: number | null): string {
    if (pct === null) return 'var(--ov-ink-dim)';
    if (pct >= 75) return 'var(--ov-critical)';
    if (pct >= PRESSURE_PCT) return 'var(--ov-pressure)';
    return 'var(--ov-ok)';
  }
  function stateTone(s: FleetUnit['state']): string {
    return s === 'permission' ? 'var(--ov-warn)'
      : s === 'active' ? 'var(--ov-live)'
      : s === 'idle' ? 'var(--ov-ok)'
      : 'var(--ov-ink-dim)';
  }
</script>

<!-- svelte-ignore a11y_no_static_element_interactions -->
<div class="deck ov-grain" class:hidden={!visible} bind:this={containerRef} onmousedowncapture={focusPane}>

  <!-- ══ Command bar ═══════════════════════════════════════════════════════ -->
  <header class="command">
    <div class="command-row">
      <div class="crest">
        <span class="crest-mark">♔</span>
        <span class="ov-label-lead">Overlord</span>
        <span class="ov-dot" class:ov-dot-live={engineOn && inFlight > 0}
              style:--tone={engineOn ? 'var(--ov-ok)' : 'var(--ov-ink-dim)'}></span>
        <span class="ov-label crest-mode">
          {#if !engineOn}standby{:else if preferencesStore.overlordProposeMode}propose{:else}autonomous{/if}
        </span>
      </div>

      <nav class="segments">
        {#each [['deck', 'Triage', needsYou], ['fleet', 'Fleet', fleet.length], ['board', 'Board', overlordStore.tasks.filter(isInFlight).length], ['ledger', 'Ledger', 0]] as [id, label, count] (id)}
          <button class="segment" class:on={view === id} onclick={() => (view = id as View)}>
            {label}
            {#if (count as number) > 0}<span class="segment-count">{count}</span>{/if}
          </button>
        {/each}
      </nav>

      {#if unboundCount > 1}
        <Tooltip text="Send /maiterm init to every tab whose agent is running but unbound. Nothing is sent to tabs that aren't running an agent.">
          <button class="ov-btn scan-btn" onclick={recoverAll} disabled={recoveringAll}>
            {recoveringAll ? 'Re-binding…' : `Re-bind ${unboundCount}`}
          </button>
        </Tooltip>
      {/if}
      <Tooltip text="Read every running agent tab and populate the board from it. Safe to repeat — nothing is typed into any tab.">
        <button class="ov-btn scan-btn" onclick={runScan} disabled={overlordStore.scanning}>
          {overlordStore.scanning ? 'Scanning…' : 'Scan tabs'}
        </button>
      </Tooltip>

      <Tooltip text="Rules land as proposals you approve, instead of typing into tabs on their own">
        <div class="mode-toggle">
          <span class="ov-label">Propose first</span>
          <button
            class="toggle"
            class:active={preferencesStore.overlordProposeMode}
            onclick={() => preferencesStore.setOverlordProposeMode(!preferencesStore.overlordProposeMode)}
            aria-pressed={preferencesStore.overlordProposeMode}
            aria-label="Toggle propose mode"
          ><span class="toggle-knob"></span></button>
        </div>
      </Tooltip>
    </div>

    {#if inFlight > 0}<div class="sweep"><span></span></div>{/if}
    <div class="ov-tickrule"></div>

    <div class="telemetry">
      <div class="readout">
        <span class="ov-mono readout-value">{fleet.length}</span>
        <span class="ov-label">watched</span>
      </div>
      <div class="readout">
        <span class="ov-mono readout-value" style:color={pctTone(peakPct || null)}>{peakPct || '—'}<i>%</i></span>
        <span class="ov-label">peak context</span>
        <div class="readout-bar"><span style:width="{peakPct}%" style:background={pctTone(peakPct || null)}></span></div>
      </div>
      <div class="readout">
        <span class="ov-mono readout-value" style:color={inFlight ? 'var(--ov-live)' : undefined}>{inFlight}</span>
        <span class="ov-label">in flight</span>
      </div>
      <div class="readout">
        <span class="ov-mono readout-value" style:color={needsYou ? 'var(--ov-warn)' : undefined}>{needsYou}</span>
        <span class="ov-label">needs you</span>
      </div>
      <div class="readout">
        <span class="ov-mono readout-value">{sentToday}</span>
        <span class="ov-label">sent · 24h</span>
      </div>
    </div>
  </header>

  <!-- ══ Body ══════════════════════════════════════════════════════════════ -->
  <!-- Every view but the board is a vertical list that scrolls as one. The board is a
       fixed frame with its own scrollers inside it, so it takes the height instead. -->
  <div class="body" class:body-fill={view === 'board'}>

    {#if !engineOn}
      <div class="standby ov-panel ov-bracket ov-in">
        <span class="standby-mark">♔</span>
        <p class="ov-label-lead">Overlord is on standby</p>
        <p class="standby-copy">
          The engine is switched off, so nothing is watched and nothing is typed.
          Turn it on in <strong>Preferences → Overlord</strong>; it starts in propose mode,
          where every directive waits for your click.
        </p>
      </div>
    {/if}

    <!-- ── Triage ──────────────────────────────────────────────────────── -->
    {#if view === 'deck'}
      <!-- Run all: the two actions Overlord already decided on, paced for a rate limit.
           Stale tasks and escalations are deliberately absent — those are judgement. -->
      {#if runProgress}
        <div class="runbar ov-panel running ov-in">
          <div class="runbar-line">
            <span class="ov-label runbar-lead">
              {#if runProgress.phase === 'running'}sending
              {:else if runProgress.phase === 'waiting'}pacing
              {:else if runProgress.phase === 'verifying'}verifying
              {:else}holding{/if}
            </span>
            <span class="ov-mono runbar-count">{runProgress.done}/{runProgress.total}</span>
            <span class="runbar-what">
              {#if runProgress.phase === 'running'}
                {runProgress.label ?? ''}
              {:else if runProgress.phase === 'waiting'}
                wave {runProgress.wave} of {runProgress.waves} done · next in {resumeIn}s
              {:else if runProgress.phase === 'verifying'}
                <!-- Everything is sent; this is watching whether it took. A re-bind that
                     lands on a tab whose agent is gone looks identical to one that works. -->
                {runProgress.label ?? ''} · giving up in {resumeIn}s
              {:else}
                waiting for the fleet to drain before the next wave
              {/if}
            </span>
            <button class="ov-btn ov-btn-danger runbar-stop" onclick={() => overlordStore.cancelTriageRun()}>Stop</button>
          </div>
          <div class="runbar-track">
            <span style:width="{Math.round((runProgress.done / Math.max(1, runProgress.total)) * 100)}%"></span>
          </div>
        </div>
      {:else if runnable.total > 0}
        <div class="runbar ov-panel ov-in">
          <div class="runbar-line">
            <button class="ov-btn ov-btn-primary" onclick={runAll}>Run all {runnable.total}</button>
            <span class="runbar-what">
              {#if runnable.rebinds}re-bind {runnable.rebinds} agent{runnable.rebinds === 1 ? '' : 's'}{/if}
              {#if runnable.rebinds && runnable.proposals}, then {/if}
              {#if runnable.proposals}send {runnable.proposals} proposed directive{runnable.proposals === 1 ? '' : 's'}{/if}
              · 10 at a time, 1s apart, 5s between waves
            </span>
          </div>
        </div>
      {/if}

      <!-- Separate from "run all" on purpose: that button talks to agents, this one takes
           tabs out of the window. One control doing both would be a nasty surprise. -->
      {#if spent.length > 1}
        <div class="runbar ov-panel ov-in">
          <div class="runbar-line">
            <button class="ov-btn" onclick={archiveAll} disabled={archivingAll}>
              {archivingAll ? 'Archiving…' : `Archive all ${spent.length}`}
            </button>
            <span class="runbar-what">
              {spent.length} finished sessions · restorable later, nothing is closed for good
            </span>
          </div>
        </div>
      {/if}

      {#if recoverNote}
        <!-- A recovery that couldn't run has to say so. A button that silently does
             nothing is the exact failure this whole section was built to remove. -->
        <div class="deck-note ov-in">
          <span>{recoverNote}</span>
          <button class="ov-btn" onclick={() => (recoverNote = null)}>Dismiss</button>
        </div>
      {/if}
      {#if scan}
        <article class="signal ov-panel scan-card ov-in" style:--tone="var(--ov-cool)">
          <div class="signal-rail"></div>
          <div class="signal-body">
            <div class="signal-head">
              <span class="ov-chip ov-chip-tone">scan</span>
              <span class="signal-title">
                {scan.tabsSeen} running tab{scan.tabsSeen === 1 ? '' : 's'} ·
                {scan.mirrored} tracking work
                {#if scan.finished > 0}· {scan.finished} all-done{/if}
                {#if scan.adopted > 0}· {scan.adopted} added{/if}
              </span>
              <span class="signal-age ov-mono">{fmtAge(scan.at)}</span>
            </div>
            {#if scan.silent.length}
              <p class="signal-text">
                {scan.silent.length} tab{scan.silent.length === 1 ? ' is' : 's are'} working with no task list,
                so the board only knows {scan.silent.length === 1 ? 'its' : 'their'} tab name. Overlord can ask
                {scan.silent.length === 1 ? 'it' : 'them'} to start keeping one — a single nudge each, after
                which {scan.silent.length === 1 ? 'its' : 'their'} tasks flow onto the board on their own.
              </p>
              <div class="signal-actions">
                <button class="ov-btn ov-btn-primary" onclick={runTrackRequest} disabled={asking}>
                  {asking ? 'Asking…' : `Ask ${scan.silent.length} to track`}
                </button>
                <button class="ov-btn" onclick={() => overlordStore.clearScan()}>Dismiss</button>
              </div>
            {:else if scan.asked !== undefined}
              <p class="signal-text">
                {#if scan.asked === 0}
                  Nothing was asked — every candidate was busy, guarded, or asked recently.
                {:else}
                  Asked {scan.asked} tab{scan.asked === 1 ? '' : 's'} to start tracking. Their tasks
                  appear here as each one writes its list; nothing else will be sent.
                {/if}
              </p>
              <div class="signal-actions">
                <button class="ov-btn" onclick={() => overlordStore.clearScan()}>Dismiss</button>
              </div>
            {:else}
              <p class="signal-text">Every running tab is tracked and on the board.</p>
              <div class="signal-actions">
                <button class="ov-btn" onclick={() => overlordStore.clearScan()}>Dismiss</button>
              </div>
            {/if}
          </div>
        </article>
      {/if}

      {#if signals.length === 0}
        <div class="allclear ov-in">
          <div class="allclear-rule"></div>
          <span class="ov-label">all clear · {fleet.length} agents nominal</span>
          <div class="allclear-rule"></div>
        </div>
      {/if}

      <!-- The queue TILES once there is room for it. A triage card is card-shaped work —
           a chip row, a sentence or two, a couple of buttons — and in a 1800px window a
           one-column stack turned every one of them into a banner with 1500px of dead
           space and a line length nobody can read back.
           Every card is one tile, including proposals: giving those the wider measure for
           their verbatim block meant consecutive ones each claimed two of three columns
           and grid placement left the third empty — a re-bind sweep raises five at once,
           so the common case was a column-wide hole down the page. The block is pre-wrap
           and breaks words, so it reads at a tile's width. -->
      <div class="queue">
      {#each signals as s, i (s.id)}
        {#if s.sev === 6 && (i === 0 || signals[i - 1].sev !== 6)}
          <!-- The one grouping the severity ramp already implies: everything above is a
               problem, everything below is a finished session waiting to be put away.
               In a stack that read as "further down the list"; tiled, a green card next
               to a red one needs the line drawn. -->
          <div class="queue-break">
            <span class="ov-label">put away · nothing here is wrong</span>
            <div class="allclear-rule"></div>
          </div>
        {/if}
        <article class="signal ov-panel ov-in" style:--i={i}
                 style:--tone={s.sev <= 0 ? 'var(--ov-critical)'
                   : s.sev === 1 ? 'var(--ov-live)'
                   : s.sev === 2 ? 'var(--ov-warn)'
                   : s.sev === 3 ? 'var(--ov-pressure)'
                   : s.sev === 6 ? 'var(--ov-ok)'
                   : 'var(--ov-ink-dim)'}>
          <div class="signal-rail"></div>
          <div class="signal-body">

            {#if s.type === 'escalation'}
              <div class="signal-head">
                <span class="ov-chip ov-chip-tone">{escalationLabel(s.e.kind)}</span>
                <button class="ov-chip ov-chip-tab" onclick={() => navigateToTab(s.e.tabId)}>{tabDisplayName(s.e.tabId)}</button>
                <span class="signal-age ov-mono">{fmtAge(s.e.ts)}</span>
              </div>
              <p class="signal-text">{s.e.detail}</p>
              <div class="signal-actions">
                <button class="ov-btn" onclick={() => overlordStore.dismissEscalation(s.e.id)}>Clear</button>
              </div>

            {:else if s.type === 'proposal'}
              <div class="signal-head">
                <span class="ov-chip ov-chip-tone">proposed</span>
                <span class="signal-title">{s.p.ruleName}</span>
                <button class="ov-chip ov-chip-tab" onclick={() => navigateToTab(s.p.tabId)}>{s.p.tabName}</button>
                <span class="ov-chip">{s.p.workspaceName}</span>
                <span class="signal-age ov-mono">{fmtAge(s.p.createdAt)}</span>
              </div>
              <div class="ov-verbatim">{s.p.preview}</div>
              {#if s.p.stepCount > 1}
                <p class="signal-note">+ {s.p.stepCount - 1} more step{s.p.stepCount > 2 ? 's' : ''} once this one lands.</p>
              {/if}
              <div class="signal-actions">
                <button class="ov-btn ov-btn-primary" onclick={() => approve(s.p.id)}>Send it</button>
                <button class="ov-btn" onclick={() => overlordStore.dismissProposal(s.p.id)}>Not now</button>
              </div>

            {:else if s.type === 'permission'}
              <div class="signal-head">
                <span class="ov-chip ov-chip-tone">{s.supervisor ? 'overlord blocked' : 'permission'}</span>
                <button class="ov-chip ov-chip-tab" onclick={() => navigateToTab(s.u.tab.id)}>{s.u.tab.name}</button>
                <span class="ov-chip">{s.u.ws.name}</span>
              </div>
              <p class="signal-text">
                {#if s.supervisor}
                  Overlord itself is stopped at a prompt, waiting on you. The engine keeps
                  running its rules, but nothing is exercising judgment while it sits here —
                  escalations go unanswered and no tab gets driven — so this comes first.
                {:else}
                  Waiting on your approval — the agent is stopped until you answer. This is the
                  one signal Overlord cannot clear for you: answering a permission prompt on
                  your behalf is exactly what that prompt exists to prevent.
                {/if}
              </p>
              <div class="signal-actions">
                <button class="ov-btn ov-btn-primary" onclick={() => navigateToTab(s.u.tab.id)}>Open tab</button>
              </div>

            {:else if s.type === 'pressure'}
              {@const cp = overlordStore.checkpointState(s.u.tab.id)}
              <div class="signal-head">
                <span class="ov-chip ov-chip-tone">{s.u.pct}% context</span>
                <button class="ov-chip ov-chip-tab" onclick={() => navigateToTab(s.u.tab.id)}>{s.u.tab.name}</button>
                <span class="ov-chip">{s.u.ws.name}</span>
                <span class="signal-age ov-mono">{fmtAge(s.u.lastTurn)}</span>
              </div>
              <!-- Says what is HAPPENING, not what ought to. "A checkpoint should run"
                   is not an answer when running it is Overlord's entire job. -->
              <p class="signal-text">
                Approaching compaction.
                {#if cp.kind === 'running'}
                  Checkpoint running — step {cp.step} of {cp.steps}.
                {:else if cp.kind === 'proposed'}
                  A checkpoint is waiting for your approval in the queue above.
                {:else if cp.kind === 'no_rule'}
                  No checkpoint rule is enabled for this tab, so nothing will run on its own.
                {:else if cp.kind === 'cooling'}
                  A checkpoint ran recently, so the rule holds off for another {cp.minutes}m.
                {:else if cp.kind === 'busy'}
                  The agent is mid-turn — the checkpoint runs as soon as it finishes.
                {:else if cp.kind === 'below_rule'}
                  This tab's own rule doesn't fire until {cp.at}% — nothing runs on its own
                  before then. Checkpoint now if you'd rather not wait.
                {:else}
                  The checkpoint runs on the next tick.
                {/if}
              </p>
              {#if cp.kind !== 'running' && cp.kind !== 'proposed'}
                <div class="signal-actions">
                  {#if cp.kind === 'no_rule'}
                    <span class="signal-note">Enable one in Preferences → Overlord.</span>
                  {:else}
                    <Tooltip text="Run the checkpoint now, ignoring the rule's cooldown. If the agent is mid-turn it waits for the turn to end rather than typing over it.">
                      <button class="ov-btn ov-btn-primary" disabled={checkpointing === s.u.tab.id}
                              onclick={() => checkpoint(s.u.tab.id)}>
                        {checkpointing === s.u.tab.id ? 'Starting…' : 'Checkpoint now'}
                      </button>
                    </Tooltip>
                  {/if}
                </div>
              {/if}

            {:else if s.type === 'spent'}
              <div class="signal-head">
                <span class="ov-chip ov-chip-tone">finished</span>
                <button class="ov-chip ov-chip-tab" onclick={() => navigateToTab(s.s.tabId)}>{s.s.name}</button>
                <span class="ov-chip">{s.s.done} done</span>
                {#if s.s.parked}<span class="ov-chip">{s.s.parked} parked</span>{/if}
                <span class="signal-age ov-mono">{fmtAge(s.s.lastActivity)}</span>
              </div>
              {#if closingTab === s.s.tabId}
                <p class="signal-text">
                  Close <strong>{s.s.name}</strong> for good? The session ends and its
                  transcript is not kept — there is no way back. Archive instead if there is
                  any chance you'll need to look at this work again.
                </p>
                <div class="signal-actions">
                  <button class="ov-btn ov-btn-danger" disabled={tabBusy === s.s.tabId}
                          onclick={() => closeTab(s.s.tabId)}>
                    {tabBusy === s.s.tabId ? 'Closing…' : 'Close for good'}
                  </button>
                  <button class="ov-btn" onclick={() => (closingTab = null)}>Cancel</button>
                </div>
              {:else}
                <p class="signal-text">
                  Every tracked task on this tab is done and it has been quiet since.
                  <strong>Archive</strong> keeps the scrollback and the working directory, so
                  the session can be restored if a bug turns up in what it built.
                  {#if s.s.parked}
                    Its {s.s.parked} parked task{s.s.parked === 1 ? '' : 's'} return{s.s.parked === 1 ? 's' : ''} to the project.
                  {/if}
                </p>
                <div class="signal-actions">
                  <button class="ov-btn ov-btn-primary" disabled={tabBusy === s.s.tabId}
                          onclick={() => archiveTab(s.s.tabId)}>
                    {tabBusy === s.s.tabId ? 'Archiving…' : 'Archive'}
                  </button>
                  <button class="ov-btn ov-btn-danger" onclick={() => (closingTab = s.s.tabId)}>Close</button>
                  <Tooltip text="Stop offering this session for a week">
                    <button class="ov-btn" onclick={() => overlordStore.keepSpentTab(s.s.tabId)}>Keep</button>
                  </Tooltip>
                </div>
              {/if}

            {:else if s.type === 'stale'}
              <div class="signal-head">
                <span class="ov-chip ov-chip-tone">stale {fmtAge(s.t.updated_at)}</span>
                <span class="signal-title">{s.t.title}</span>
                {#if workstreamName(s.t)}
                  <span class="ov-chip">{workstreamName(s.t)}</span>
                {/if}
                {#if s.t.tab_id}
                  <button class="ov-chip ov-chip-tab" onclick={() => navigateToTab(s.t.tab_id!)}>{tabDisplayName(s.t.tab_id)}</button>
                {/if}
              </div>
              {#if s.t.detail}
                <p class="signal-text">{s.t.detail}</p>
              {/if}
              <div class="signal-actions">
                <button class="ov-btn" onclick={() => overlordStore.updateTaskState(s.t.id, 'done')}>Mark done</button>
                <!-- Parking is usually the honest answer for something untouched for days:
                     it stops the nagging without pretending the work happened. -->
                <button class="ov-btn" onclick={() => overlordStore.updateTaskState(s.t.id, 'backlog')}>Park</button>
                <button class="ov-btn ov-btn-danger" onclick={() => overlordStore.deleteTask(s.t.id)}>Drop</button>
              </div>

            {:else}
              <div class="signal-head">
                <span class="ov-chip ov-chip-tone">
                  {overlordStore.unreadyKind(s.u.tab.id) === 'unbound' ? 'not responding' : 'not running'}
                </span>
                <button class="ov-chip ov-chip-tab" onclick={() => navigateToTab(s.u.tab.id)}>{s.u.tab.name}</button>
                <span class="ov-chip">{s.u.ws.name}</span>
              </div>
              {#if overlordStore.unreadyKind(s.u.tab.id) === 'unbound'}
                <p class="signal-text">
                  Its agent is running but isn't bound to maiTerm, so tools and replies don't
                  reach it. Re-binding is one command.
                </p>
                <div class="signal-actions">
                  <button class="ov-btn ov-btn-primary" disabled={recovering === s.u.tab.id}
                          onclick={() => recover(s.u.tab.id)}>
                    {recovering === s.u.tab.id ? 'Re-binding…' : 'Re-bind'}
                  </button>
                </div>
              {:else if overlordStore.unreadyKind(s.u.tab.id) === 'stopped'}
                <p class="signal-text">
                  {#if overlordStore.rebindDidNotTake(s.u.tab.id)}
                    Re-binding was tried here and nothing came back, so the agent is gone
                    rather than unbound — on an SSH tab that is invisible from this side
                    until it's attempted. Restarting resumes its own session, not a fresh one.
                  {:else}
                    Nothing is running in this tab — the agent exited. Restarting resumes its
                    own session, not a fresh one.
                  {/if}
                </p>
                <div class="signal-actions">
                  <button class="ov-btn" disabled={recovering === s.u.tab.id}
                          onclick={() => recover(s.u.tab.id)}>
                    {recovering === s.u.tab.id ? 'Restarting…' : 'Restart agent'}
                  </button>
                </div>
              {:else}
                <p class="signal-text">Checking what's running in this tab…</p>
              {/if}
            {/if}

          </div>
        </article>
      {/each}
      </div>
    {/if}

    <!-- ── Fleet ───────────────────────────────────────────────────────── -->
    {#if view === 'fleet'}
      {#if fleet.length === 0}
        <div class="allclear ov-in"><div class="allclear-rule"></div><span class="ov-label">{parkedCount ? 'every agent tab is in a suspended workspace' : 'no agent tabs in this window'}</span><div class="allclear-rule"></div></div>
      {/if}
      {#if fleet.length > 0 || parkedCount > 0 || exemptCount > 0}
        <div class="fleet-bar ov-in">
          <span class="ov-label">sort</span>
          <div class="fleet-sort" role="group" aria-label="Sort the fleet">
            <button class="fleet-sort-btn" class:on={fleetSort === 'context'} onclick={() => (fleetSort = 'context')}>peak context</button>
            <button class="fleet-sort-btn" class:on={fleetSort === 'activity'} onclick={() => (fleetSort = 'activity')}>latest activity</button>
          </div>
          {#if parkedCount > 0 || exemptCount > 0}
            <span class="ov-label fleet-parked">
              {[
                parkedCount > 0 ? `${parkedCount} in suspended workspaces` : '',
                exemptCount > 0 ? `${exemptCount} exempt` : '',
              ].filter(Boolean).join(' · ')} — not shown
            </span>
          {/if}
        </div>
      {/if}
      <div class="fleet">
        {#each fleet as u, i (u.tab.id)}
          <div class="unit ov-panel ov-in" class:unit-busy={!!u.ritual} style:--i={i}>
            <div class="unit-head">
              <span class="ov-dot" class:ov-dot-live={u.state === 'active' || u.state === 'permission'}
                    style:--tone={stateTone(u.state)}></span>
              <span class="unit-name">{u.tab.name}</span>
              <span class="ov-mono unit-age">{fmtAge(u.lastTurn)}</span>
            </div>
            <div class="unit-ws ov-label">{u.ws.name}</div>
            {#if u.state === 'dormant' && !u.loaded}
              <!-- Says why the gauges are empty. Nothing can read this tab until its pane
                   mounts, and reading "dormant" with no numbers looks like a dead agent. -->
              <div class="unit-note">not loaded — open its workspace to check on it</div>
            {/if}

            <div class="unit-gauge">
              <svg viewBox="0 0 32 32" class="ring" aria-hidden="true">
                <circle cx="16" cy="16" r="13" class="ring-track" />
                <circle cx="16" cy="16" r="13" class="ring-fill"
                        style:stroke={pctTone(u.pct)} stroke-dasharray={ringDash(u.pct)} />
              </svg>
              <div class="unit-gauge-read">
                <span class="ov-mono unit-pct" style:color={pctTone(u.pct)}>{u.pct ?? '—'}<i>%</i></span>
                <span class="ov-label">context</span>
              </div>
              {#if u.todosTotal > 0}
                <div class="unit-todos">
                  <span class="ov-mono">{u.todosDone}/{u.todosTotal}</span>
                  <span class="ov-label">todos</span>
                </div>
              {/if}
            </div>

            {#if u.ritual}
              <div class="unit-ritual">
                <div class="unit-ritual-head">
                  <span class="ov-label" style:color="var(--ov-live)">{u.ritual.ruleName}</span>
                  <span class="ov-mono">{u.ritual.step}/{u.ritual.steps}</span>
                </div>
                <div class="steps">
                  {#each Array(u.ritual.steps) as _, si (si)}
                    <span class="step" class:done={si < u.ritual.step - 1} class:now={si === u.ritual.step - 1}></span>
                  {/each}
                </div>
              </div>
            {:else if u.topTodo}
              <p class="unit-todo">{u.topTodo}</p>
            {:else if u.report}
              <p class="unit-todo unit-report">“{u.report}”</p>
            {/if}

            {#if u.awaiting && !u.ritual}
              <span class="ov-chip ov-chip-tone unit-flag" style:--tone="var(--ov-warn)">awaiting reply</span>
            {/if}

            {#if unitNotes[u.tab.id]}
              <div class="unit-note unit-refusal">{unitNotes[u.tab.id]}</div>
            {/if}

            <div class="unit-foot">
              <button class="ov-btn" onclick={() => navigateToTab(u.tab.id)}>View</button>
              {#if overlordStore.rulesForTab(u.tab.id).length > 0}
                <button class="ov-btn unit-trigger" class:on={triggerMenu?.tabId === u.tab.id}
                        onclick={(e) => openTrigger(e, u.tab.id)} aria-haspopup="menu">
                  Trigger <span class="unit-caret">▾</span>
                </button>
              {:else}
                <Tooltip text="No Overlord rule with a sequence applies to this tab.">
                  <button class="ov-btn unit-trigger" disabled>Trigger <span class="unit-caret">▾</span></button>
                </Tooltip>
              {/if}
            </div>
          </div>
        {/each}
      </div>
    {/if}

    <!-- ── Board ───────────────────────────────────────────────────────── -->
    <!-- Indexed by workstream, not by workspace→tab. The component owns its own
         scrolling, so the deck body stops scrolling while it is on screen. -->
    {#if view === 'board'}
      <OverlordBoardView {now} />
    {/if}

    <!-- ── Ledger ──────────────────────────────────────────────────────── -->
    {#if view === 'ledger'}
      <p class="ledger-intro">
        Every byte Overlord has typed into a tab, verbatim. Directives are indistinguishable
        from you typing — this is the only record of which were yours.
      </p>
      {#if overlordStore.recentLedger.length === 0}
        <div class="allclear"><div class="allclear-rule"></div><span class="ov-label">nothing typed yet</span><div class="allclear-rule"></div></div>
      {/if}
      <div class="ledger">
        {#each [...overlordStore.recentLedger].reverse().slice(0, 120) as e, i (e.id)}
          <div class="entry ov-in" style:--i={Math.min(i, 12)} style:--tone={
            outcomeTone(e.outcome) === 'good' ? 'var(--ov-ok)'
            : outcomeTone(e.outcome) === 'warn' ? 'var(--ov-warn)'
            : outcomeTone(e.outcome) === 'bad' ? 'var(--ov-critical)'
            : 'var(--ov-ink-dim)'}>
            <span class="entry-tick"></span>
            <span class="ov-mono entry-time">{new Date(e.ts).toLocaleTimeString([], { hour: '2-digit', minute: '2-digit' })}</span>
            <span class="ov-label entry-outcome">{outcomeLabel(e.outcome)}</span>
            <button class="ov-chip ov-chip-tab" onclick={() => navigateToTab(e.tab_id)}>{tabDisplayName(e.tab_id)}</button>
            <!-- The row clips the directive; the bubble is the only way to read the rest. -->
            <Tooltip text={e.text} block>
              <span class="ov-mono entry-text">{e.text}</span>
            </Tooltip>
          </div>
        {/each}
      </div>
    {/if}
  </div>
</div>

{#if triggerMenu}
  <ContextMenu items={triggerItems(triggerMenu.tabId)} x={triggerMenu.x} y={triggerMenu.y} anchor={triggerMenu.anchor} onclose={() => (triggerMenu = null)} />
{/if}

<style>
  /* ── Shell ────────────────────────────────────────────────────────────── */
  .deck {
    position: relative;
    display: flex;
    flex-direction: column;
    flex: 1;
    min-height: 0;
    min-width: 0;
    overflow: hidden;
    color: var(--ov-ink);
    background:
      radial-gradient(120% 60% at 50% -10%,
        color-mix(in srgb, var(--accent) 9%, transparent) 0%, transparent 65%),
      var(--ov-deck);
  }

  .deck.hidden {
    position: absolute;
    inset: 0;
    opacity: 0;
    pointer-events: none;
    z-index: -1;
  }

  /* ── Command bar ──────────────────────────────────────────────────────── */
  .command {
    position: relative;
    flex-shrink: 0;
    background:
      repeating-linear-gradient(0deg,
        color-mix(in srgb, var(--fg) 3%, transparent) 0 1px,
        transparent 1px 3px),
      color-mix(in srgb, var(--bg-dark) 55%, transparent);
    border-bottom: 1px solid var(--ov-hair);
    backdrop-filter: blur(6px);
  }

  .command-row {
    display: flex;
    align-items: center;
    gap: 16px;
    padding: 12px var(--ov-gutter) 10px;
  }

  .crest { display: flex; align-items: center; gap: 9px; }
  .crest-mark {
    font-size: 1.15rem;
    line-height: 1;
    color: var(--ov-live);
    text-shadow: 0 0 14px color-mix(in srgb, var(--ov-live) 55%, transparent);
  }
  .crest-mode { letter-spacing: 0.14em; }

  .segments {
    display: flex;
    gap: 2px;
    margin-left: auto;
    padding: 2px;
    border: 1px solid var(--ov-hair);
    border-radius: 2px;
    background: color-mix(in srgb, var(--bg-dark) 55%, transparent);
  }

  .segment {
    display: flex;
    align-items: center;
    gap: 6px;
    font-family: var(--ov-face);
    font-size: 0.78rem;
    font-weight: 600;
    letter-spacing: 0.06em;
    text-transform: uppercase;
    color: var(--ov-ink-dim);
    padding: 4px 12px;
    border-radius: 1px;
    transition: color 0.15s ease, background 0.15s ease;
  }
  .segment:hover { color: var(--ov-ink); }
  .segment.on {
    color: var(--ov-ink);
    background: color-mix(in srgb, var(--ov-live) 18%, transparent);
    box-shadow: inset 0 -2px 0 var(--ov-live);
  }
  .segment-count {
    font-family: var(--ov-mono);
    font-size: 0.7rem;
    letter-spacing: 0;
    color: var(--ov-live);
  }

  .scan-btn { flex-shrink: 0; }

  /* A tooltip's wrapper becomes the flex item, so `flex-shrink: 0` has to live on the
     wrapper or the buttons it holds start wrapping their labels as the bar fills up. */
  .command-row :global(.tooltip-wrapper) { flex-shrink: 0; }

  .mode-toggle { display: flex; align-items: center; gap: 8px; flex-shrink: 0; }

  /* Same pill as Preferences — a switch has to look like the app's switches. */
  .toggle {
    position: relative;
    width: 34px;
    height: 19px;
    background: var(--bg-light);
    border-radius: 10px;
    border: none;
    cursor: pointer;
    flex-shrink: 0;
    transition: background-color 0.2s;
  }
  .toggle.active { background: var(--accent); }
  .toggle-knob {
    position: absolute;
    top: 2px; left: 2px;
    width: 15px; height: 15px;
    background: white;
    border-radius: 50%;
    transition: transform 0.2s;
  }
  .toggle.active .toggle-knob { transform: translateX(15px); }


  /* Radar sweep — present only while a ritual is actually running. */
  .sweep {
    position: absolute;
    left: 0; right: 0; bottom: 7px;
    height: 1px;
    overflow: hidden;
  }
  .sweep span {
    display: block;
    width: 45%;
    height: 100%;
    background: linear-gradient(90deg, transparent,
      color-mix(in srgb, var(--ov-live) 85%, transparent), transparent);
    animation: ovSweep 2.6s cubic-bezier(0.5, 0, 0.5, 1) infinite;
  }

  /* ── Telemetry strip ──────────────────────────────────────────────────── */
  .telemetry {
    display: flex;
    gap: 30px;
    padding: 11px var(--ov-gutter) 13px;
  }

  .readout { display: flex; flex-direction: column; gap: 4px; min-width: 62px; }
  .readout-value {
    font-size: 1.5rem;
    font-weight: 500;
    line-height: 0.9;
    letter-spacing: -0.02em;
    color: var(--ov-ink);
  }
  .readout-value i { font-size: 0.8rem; font-style: normal; opacity: 0.5; margin-left: 1px; }
  .readout-bar {
    height: 2px;
    background: var(--ov-hair);
    border-radius: 1px;
    overflow: hidden;
  }
  .readout-bar span { display: block; height: 100%; transition: width 0.5s cubic-bezier(0.2, 0.7, 0.3, 1); }

  /* ── Body ─────────────────────────────────────────────────────────────── */
  /* A container, not a media query: the deck is a tab, so it can be half of a split in a
     huge window or the whole of a small one — the window's width says nothing about how
     much room these cards actually have. Queried by `.queue` below; `.body` itself is
     never restyled by it, because an element can't match its own container. */
  .body {
    flex: 1;
    overflow-y: auto;
    padding: 16px var(--ov-gutter) 40px;
    container-type: inline-size;
  }

  .body.body-fill {
    display: flex;
    flex-direction: column;
    min-height: 0;
    overflow: hidden;
    padding-bottom: 16px;
  }

  .standby {
    text-align: center;
    padding: 28px 24px;
    margin-bottom: 18px;
  }
  .standby-mark { font-size: 1.7rem; color: var(--ov-ink-dim); display: block; margin-bottom: 8px; }
  .standby-copy {
    color: var(--ov-ink-dim);
    font-size: 0.92rem;
    line-height: 1.6;
    max-width: 46ch;
    margin: 8px auto 0;
  }
  .standby-copy strong { color: var(--ov-ink-mid); font-weight: 600; }

  .allclear {
    display: flex;
    align-items: center;
    gap: 14px;
    padding: 26px 0;
  }
  .allclear-rule { flex: 1; height: 1px; background: linear-gradient(90deg, transparent, var(--ov-hair), transparent); }

  /* ── Signals ──────────────────────────────────────────────────────────── */
  /* One column until there is room for two. `align-items: start` matters: a stretched
     card would grow its severity rail to the height of the tallest card in its row and
     hold that much empty body, which reads as a card with something missing. */
  .queue {
    display: grid;
    grid-template-columns: minmax(0, 1fr);
    align-items: start;
    gap: 8px;
  }
  .queue .signal { margin-bottom: 0; }

  .queue-break {
    grid-column: 1 / -1;
    display: flex;
    align-items: center;
    gap: 12px;
    padding: 14px 0 4px;
  }

  @container (min-width: 980px) { .queue { grid-template-columns: repeat(2, minmax(0, 1fr)); } }
  @container (min-width: 1460px) { .queue { grid-template-columns: repeat(3, minmax(0, 1fr)); } }
  @container (min-width: 1960px) { .queue { grid-template-columns: repeat(4, minmax(0, 1fr)); } }

  .signal {
    display: flex;
    margin-bottom: 8px;
    overflow: hidden;
    transition: border-color 0.16s ease, transform 0.16s ease;
  }
  .signal:hover {
    border-color: color-mix(in srgb, var(--tone) 45%, var(--ov-hair));
    transform: translateX(2px);
  }
  .signal-rail {
    width: 3px;
    flex-shrink: 0;
    background: var(--tone);
    opacity: 0.85;
  }
  .signal-body { flex: 1; min-width: 0; padding: 10px 13px; }

  .signal-head {
    display: flex;
    align-items: center;
    gap: 7px;
    flex-wrap: wrap;
    margin-bottom: 7px;
  }
  .signal-title { font-weight: 600; font-size: 0.95rem; }
  .signal-age { font-size: 0.75rem; color: var(--ov-ink-dim); margin-left: auto; }
  /* Caps the line, not the card. Matters for the two things that still span the full
     width — the scan headline and a wide proposal — where an uncapped paragraph became a
     single 200-character line. */
  .signal-text { color: var(--ov-ink-mid); font-size: 0.9rem; line-height: 1.5; max-width: 82ch; }

  /* ── Run all ──────────────────────────────────────────────────────────── */
  .runbar { margin-bottom: 10px; padding: 9px 12px; }
  .runbar.running { border-color: color-mix(in srgb, var(--ov-live) 45%, var(--ov-hair)); }
  .runbar-line { display: flex; align-items: center; gap: 10px; }
  .runbar-lead { color: var(--ov-live); flex-shrink: 0; }
  .runbar-count { font-size: 0.85rem; color: var(--ov-ink); flex-shrink: 0; }
  .runbar-what {
    color: var(--ov-ink-dim);
    font-size: 0.82rem;
    line-height: 1.4;
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .runbar-stop { margin-left: auto; flex-shrink: 0; }
  .runbar-track {
    height: 2px;
    margin-top: 8px;
    border-radius: 1px;
    background: var(--ov-hair);
    overflow: hidden;
  }
  .runbar-track span {
    display: block;
    height: 100%;
    background: var(--ov-live);
    transition: width 0.3s cubic-bezier(0.2, 0.7, 0.3, 1);
  }

  .deck-note {
    align-items: center;
    background: var(--ov-panel);
    border: 1px solid var(--ov-hair);
    border-left: 2px solid var(--ov-warn);
    border-radius: 2px;
    color: var(--ov-ink-mid);
    display: flex;
    font-size: 0.88rem;
    gap: 10px;
    justify-content: space-between;
    margin-bottom: 12px;
    padding: 8px 12px;
  }
  .signal-note { color: var(--ov-ink-dim); font-size: 0.8rem; margin-top: 5px; }
  .signal-actions { display: flex; gap: 6px; margin-top: 9px; }

  /* ── Fleet ────────────────────────────────────────────────────────────── */
  .fleet {
    display: grid;
    grid-template-columns: repeat(auto-fill, minmax(232px, 1fr));
    gap: 10px;
  }

  .fleet-bar {
    display: flex;
    align-items: center;
    gap: 10px;
    margin-bottom: 12px;
    flex-wrap: wrap;
  }
  .fleet-sort {
    display: inline-flex;
    border: 1px solid var(--ov-hair);
    border-radius: 6px;
    overflow: hidden;
  }
  .fleet-sort-btn {
    padding: 3px 10px;
    font-size: 0.74rem;
    color: var(--ov-ink-dim);
    background: transparent;
    border: 0;
    cursor: pointer;
  }
  .fleet-sort-btn + .fleet-sort-btn { border-left: 1px solid var(--ov-hair); }
  .fleet-sort-btn:hover { color: var(--ov-ink); }
  .fleet-sort-btn.on {
    color: var(--ov-ink);
    background: color-mix(in srgb, var(--accent) 16%, transparent);
  }
  .fleet-parked { margin-left: auto; opacity: 0.8; }

  .unit {
    text-align: left;
    padding: 11px 12px 10px;
    display: flex;
    flex-direction: column;
    gap: 8px;
    transition: border-color 0.16s ease, background 0.16s ease;
  }
  .unit:hover {
    border-color: color-mix(in srgb, var(--ov-live) 45%, transparent);
    background: var(--ov-panel-lift);
  }
  .unit-busy { border-color: color-mix(in srgb, var(--ov-live) 45%, transparent); }

  .unit-foot {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 6px;
    margin-top: auto;
    padding-top: 8px;
    border-top: 1px solid color-mix(in srgb, var(--ov-hair) 60%, transparent);
  }
  .unit-trigger.on { border-color: var(--accent); color: var(--ov-ink); }
  .unit-caret { font-size: 0.7em; opacity: 0.7; margin-left: 2px; }
  .unit-refusal { color: var(--ov-warn); }

  .unit-head { display: flex; align-items: center; gap: 7px; }
  .unit-name {
    font-weight: 600;
    font-size: 0.92rem;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .unit-age { font-size: 0.73rem; color: var(--ov-ink-dim); margin-left: auto; }
  .unit-ws { opacity: 0.75; margin-top: -4px; }
  .unit-note { color: var(--ov-ink-dim); font-size: 0.74rem; line-height: 1.4; }

  .unit-gauge { display: flex; align-items: center; gap: 11px; }
  .ring { width: 34px; height: 34px; transform: rotate(-90deg); flex-shrink: 0; }
  .ring-track { fill: none; stroke: var(--ov-hair); stroke-width: 2.5; }
  .ring-fill {
    fill: none;
    stroke-width: 2.5;
    stroke-linecap: round;
    transition: stroke-dasharray 0.6s cubic-bezier(0.2, 0.7, 0.3, 1);
  }
  .unit-gauge-read { display: flex; flex-direction: column; gap: 2px; }
  .unit-pct { font-size: 1.15rem; line-height: 1; font-weight: 500; }
  .unit-pct i { font-size: 0.68rem; font-style: normal; opacity: 0.55; }
  .unit-todos {
    display: flex;
    flex-direction: column;
    gap: 2px;
    margin-left: auto;
    text-align: right;
    font-size: 0.88rem;
    color: var(--ov-ink-mid);
  }

  .unit-todo {
    font-size: 0.82rem;
    color: var(--ov-ink-dim);
    line-height: 1.45;
    display: -webkit-box;
    -webkit-line-clamp: 2;
    line-clamp: 2;
    -webkit-box-orient: vertical;
    overflow: hidden;
  }
  .unit-report { font-style: italic; }

  .unit-ritual { display: flex; flex-direction: column; gap: 5px; }
  .unit-ritual-head {
    display: flex;
    align-items: baseline;
    justify-content: space-between;
    gap: 8px;
    font-size: 0.78rem;
    color: var(--ov-ink-dim);
  }
  .steps { display: flex; gap: 3px; }
  .step {
    flex: 1;
    height: 3px;
    border-radius: 1px;
    background: var(--ov-hair);
  }
  .step.done { background: color-mix(in srgb, var(--ov-live) 55%, transparent); }
  .step.now {
    background: var(--ov-live);
    animation: ovBreathe 1.7s ease-in-out infinite;
  }

  .unit-flag { align-self: flex-start; }

  /* ── Ledger ───────────────────────────────────────────────────────────── */
  .ledger-intro {
    color: var(--ov-ink-dim);
    font-size: 0.88rem;
    line-height: 1.6;
    max-width: 68ch;
    margin-bottom: 14px;
  }

  .ledger { display: flex; flex-direction: column; }

  .entry {
    display: flex;
    align-items: center;
    gap: 9px;
    padding: 5px 0;
    border-bottom: 1px solid color-mix(in srgb, var(--ov-hair) 45%, transparent);
    font-size: 0.8rem;
  }
  .entry:hover { background: color-mix(in srgb, var(--fg) 3%, transparent); }
  .entry-tick {
    width: 2px;
    align-self: stretch;
    background: var(--tone);
    opacity: 0.8;
    flex-shrink: 0;
  }
  .entry-time { color: var(--ov-ink-dim); flex-shrink: 0; }
  .entry-outcome { color: var(--tone); min-width: 88px; flex-shrink: 0; }
  /* Sits inside the tooltip wrapper, which is the flex item the row actually measures. */
  .entry :global(.tooltip-wrapper.block) { flex: 1; }
  .entry-text {
    color: var(--ov-ink-dim);
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    flex: 1;
    min-width: 0;
  }
</style>
