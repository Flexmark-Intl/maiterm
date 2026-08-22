<script lang="ts">
  import { onMount, onDestroy } from 'svelte';
  import { workspacesStore, navigateToTab } from '$lib/stores/workspaces.svelte';
  import { claudeStateStore } from '$lib/stores/agentState.svelte';
  import { overlordStore } from '$lib/stores/overlord.svelte';
  import { preferencesStore } from '$lib/stores/preferences.svelte';
  import type { OverlordTask, OverlordTaskState, Workspace, Tab } from '$lib/tauri/types';
  import { fmtAge, outcomeLabel, outcomeTone } from '$lib/overlord/format';
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
  const LANES: OverlordTaskState[] = ['backlog', 'active', 'blocked', 'review', 'done'];
  const PRESSURE_PCT = 50;
  const STALE_DAYS = 3;

  type View = 'deck' | 'fleet' | 'board' | 'ledger';
  let view = $state<View>('deck');
  let newTaskTitles = $state<Record<string, string>>({});
  let collapsed = $state<Set<string>>(new Set());

  try {
    const saved = localStorage.getItem('overlord-collapsed');
    if (saved) collapsed = new Set(JSON.parse(saved) as string[]);
  } catch { /* first run / storage blocked */ }

  function toggleCollapse(id: string) {
    const next = new Set(collapsed);
    if (next.has(id)) next.delete(id); else next.add(id);
    collapsed = next;
    try { localStorage.setItem('overlord-collapsed', JSON.stringify([...next])); } catch { /* ignore */ }
  }

  // ── Fleet derivation ────────────────────────────────────────────────────────
  const boardWorkspaces = $derived(workspacesStore.workspaces.filter((w) => !w.overlord));

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
    awaiting: boolean;
    report: string | null;
  }

  const fleet = $derived.by<FleetUnit[]>(() => {
    void now;
    const rituals = overlordStore.ritualProgress;
    const units: FleetUnit[] = [];
    for (const ws of boardWorkspaces) {
      for (const pane of ws.panes) {
        for (const tab of pane.tabs) {
          if ((tab.tab_type ?? 'terminal') !== 'terminal' || !tab.runtime) continue;
          const f = overlordStore.facts.get(tab.id);
          const live = claudeStateStore.getState(tab.id);
          const todos = f?.todos ?? [];
          const active = todos.find((t) => t.status === 'in_progress');
          const r = rituals.find((x) => x.tabId === tab.id);
          units.push({
            tab,
            ws,
            state: live ? live.state : 'dormant',
            pct: f?.context_pct ?? null,
            lastTurn: f?.last_turn_ts,
            todosDone: todos.filter((t) => t.status === 'completed').length,
            todosTotal: todos.length,
            topTodo: active?.content ?? todos.find((t) => t.status === 'pending')?.content ?? null,
            ritual: r ? { ruleName: r.ruleName, step: r.step, steps: r.steps } : null,
            awaiting: !!overlordStore.outstandingFor(tab.id),
            report: overlordStore.agentReports.get(tab.id)?.summary ?? null,
          });
        }
      }
    }
    // Most-in-need first: busy rituals, then permission, then pressure, then staleness.
    const rank = (u: FleetUnit) =>
      (u.ritual ? 0 : u.state === 'permission' ? 1 : (u.pct ?? 0) >= PRESSURE_PCT ? 2 : u.state === 'dormant' ? 4 : 3);
    return units.sort((a, b) => rank(a) - rank(b) || (b.pct ?? 0) - (a.pct ?? 0));
  });

  // ── Triage signals — one severity-ordered queue ─────────────────────────────
  type Signal =
    | { sev: number; id: string; type: 'proposal'; p: (typeof overlordStore.proposals)[number] }
    | { sev: number; id: string; type: 'escalation'; e: (typeof overlordStore.escalations)[number] }
    | { sev: number; id: string; type: 'permission' | 'pressure' | 'unready'; u: FleetUnit }
    | { sev: number; id: string; type: 'stale'; t: OverlordTask };

  const signals = $derived.by<Signal[]>(() => {
    const out: Signal[] = [];
    for (const e of overlordStore.escalations) out.push({ sev: 0, id: e.id, type: 'escalation', e });
    for (const p of overlordStore.proposals) out.push({ sev: 1, id: p.id, type: 'proposal', p });
    for (const u of fleet) {
      if (u.state === 'permission') out.push({ sev: 2, id: `perm-${u.tab.id}`, type: 'permission', u });
      else if ((u.pct ?? 0) >= PRESSURE_PCT) out.push({ sev: 3, id: `ctx-${u.tab.id}`, type: 'pressure', u });
      else if (u.state === 'dormant') out.push({ sev: 5, id: `dead-${u.tab.id}`, type: 'unready', u });
    }
    for (const t of overlordStore.tasks) {
      if (t.state !== 'done' && now - Date.parse(t.updated_at) > STALE_DAYS * 86_400_000) {
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
  const needsYou = $derived(signals.length);
  const engineOn = $derived(preferencesStore.overlordEnabled);
  const scan = $derived(overlordStore.lastScan);
  let asking = $state(false);

  async function runScan() { await overlordStore.scanWorkspaces(); }

  async function runCensus() {
    if (!scan?.silent.length || asking) return;
    asking = true;
    try { await overlordStore.askCensus(scan.silent); } finally { asking = false; }
  }

  // ── Board helpers ───────────────────────────────────────────────────────────
  function tasksFor(wsId: string, state: OverlordTaskState) {
    return overlordStore.tasks.filter((t) => t.workspace_id === wsId && t.state === state);
  }
  function taskCount(wsId: string) {
    return overlordStore.tasks.filter((t) => t.workspace_id === wsId && t.state !== 'done').length;
  }
  function moveTask(task: OverlordTask, dir: 1 | -1) {
    const i = LANES.indexOf(task.state);
    const next = LANES[Math.min(LANES.length - 1, Math.max(0, i + dir))];
    if (next !== task.state) overlordStore.updateTaskState(task.id, next);
  }
  function addTask(wsId: string) {
    const title = (newTaskTitles[wsId] ?? '').trim();
    if (!title) return;
    overlordStore.addTask(title, wsId);
    newTaskTitles = { ...newTaskTitles, [wsId]: '' };
  }

  function tabName(id: string): string {
    for (const ws of workspacesStore.workspaces) {
      for (const pane of ws.panes) {
        const t = pane.tabs.find((t) => t.id === id);
        if (t) return t.name;
      }
    }
    return id.slice(0, 8);
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
        {#each [['deck', 'Triage', needsYou], ['fleet', 'Fleet', fleet.length], ['board', 'Board', overlordStore.tasks.filter(t => t.state !== 'done').length], ['ledger', 'Ledger', 0]] as [id, label, count] (id)}
          <button class="segment" class:on={view === id} onclick={() => (view = id as View)}>
            {label}
            {#if (count as number) > 0}<span class="segment-count">{count}</span>{/if}
          </button>
        {/each}
      </nav>

      <button class="ov-btn scan-btn" onclick={runScan} disabled={overlordStore.scanning}
              title="Read every running agent tab and populate the board from it. Safe to repeat — nothing is typed into any tab.">
        {overlordStore.scanning ? 'Scanning…' : 'Scan tabs'}
      </button>

      <div class="mode-toggle" title="Rules land as proposals you approve, instead of typing into tabs on their own">
        <span class="ov-label">Propose first</span>
        <button
          class="toggle"
          class:active={preferencesStore.overlordProposeMode}
          onclick={() => preferencesStore.setOverlordProposeMode(!preferencesStore.overlordProposeMode)}
          aria-pressed={preferencesStore.overlordProposeMode}
          aria-label="Toggle propose mode"
        ><span class="toggle-knob"></span></button>
      </div>
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
  <div class="body">

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
      {#if scan}
        <article class="signal ov-panel scan-card ov-in" style:--tone="var(--ov-cool)">
          <div class="signal-rail"></div>
          <div class="signal-body">
            <div class="signal-head">
              <span class="ov-chip ov-chip-tone">scan</span>
              <span class="signal-title">
                {scan.tabsSeen} running tab{scan.tabsSeen === 1 ? '' : 's'} ·
                {scan.mirrored} todo list{scan.mirrored === 1 ? '' : 's'} mirrored
                {#if scan.adopted > 0}· {scan.adopted} added{/if}
              </span>
              <span class="signal-age ov-mono">{fmtAge(scan.at)}</span>
            </div>
            {#if scan.silent.length}
              <p class="signal-text">
                {scan.silent.length} tab{scan.silent.length === 1 ? ' is' : 's are'} running with no todo list,
                so the board only knows {scan.silent.length === 1 ? 'its' : 'their'} tab name. Overlord can ask
                {scan.silent.length === 1 ? 'it' : 'them'} what {scan.silent.length === 1 ? "it's" : "they're"}
                working on — one short question each, answered back into the board.
              </p>
              <div class="signal-actions">
                <button class="ov-btn ov-btn-primary" onclick={runCensus} disabled={asking}>
                  {asking ? 'Asking…' : `Ask ${scan.silent.length}`}
                </button>
                <button class="ov-btn" onclick={() => overlordStore.clearScan()}>Dismiss</button>
              </div>
            {:else if scan.asked !== undefined}
              <p class="signal-text">
                {#if scan.asked === 0}
                  Nothing was asked — every candidate was busy, guarded, or asked recently.
                {:else}
                  Asked {scan.asked} tab{scan.asked === 1 ? '' : 's'}. Answers land on the board
                  as each one replies; nothing else will be sent.
                {/if}
              </p>
              <div class="signal-actions">
                <button class="ov-btn" onclick={() => overlordStore.clearScan()}>Dismiss</button>
              </div>
            {:else}
              <p class="signal-text">Every running tab is represented on the board.</p>
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

      {#each signals as s, i (s.id)}
        <article class="signal ov-panel ov-in" style:--i={i}
                 style:--tone={s.sev === 0 ? 'var(--ov-critical)'
                   : s.sev === 1 ? 'var(--ov-live)'
                   : s.sev === 2 ? 'var(--ov-warn)'
                   : s.sev === 3 ? 'var(--ov-pressure)'
                   : 'var(--ov-ink-dim)'}>
          <div class="signal-rail"></div>
          <div class="signal-body">

            {#if s.type === 'escalation'}
              <div class="signal-head">
                <span class="ov-chip ov-chip-tone">escalation</span>
                <button class="ov-chip ov-chip-tab" onclick={() => navigateToTab(s.e.tabId)}>{tabName(s.e.tabId)}</button>
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
                <button class="ov-btn ov-btn-primary" onclick={() => overlordStore.approveProposal(s.p.id)}>Send it</button>
                <button class="ov-btn" onclick={() => overlordStore.dismissProposal(s.p.id)}>Not now</button>
              </div>

            {:else if s.type === 'permission'}
              <div class="signal-head">
                <span class="ov-chip ov-chip-tone">permission</span>
                <button class="ov-chip ov-chip-tab" onclick={() => navigateToTab(s.u.tab.id)}>{s.u.tab.name}</button>
                <span class="ov-chip">{s.u.ws.name}</span>
              </div>
              <p class="signal-text">Waiting on your approval — the agent is stopped until you answer.</p>

            {:else if s.type === 'pressure'}
              <div class="signal-head">
                <span class="ov-chip ov-chip-tone">{s.u.pct}% context</span>
                <button class="ov-chip ov-chip-tab" onclick={() => navigateToTab(s.u.tab.id)}>{s.u.tab.name}</button>
                <span class="ov-chip">{s.u.ws.name}</span>
                <span class="signal-age ov-mono">{fmtAge(s.u.lastTurn)}</span>
              </div>
              <p class="signal-text">
                Approaching compaction.
                {#if s.u.ritual}Checkpoint running — step {s.u.ritual.step} of {s.u.ritual.steps}.
                {:else}A checkpoint should run before it hits the wall.{/if}
              </p>

            {:else if s.type === 'stale'}
              <div class="signal-head">
                <span class="ov-chip ov-chip-tone">stale {fmtAge(s.t.updated_at)}</span>
                <span class="signal-title">{s.t.title}</span>
                {#if s.t.tab_id}
                  <button class="ov-chip ov-chip-tab" onclick={() => navigateToTab(s.t.tab_id!)}>{tabName(s.t.tab_id)}</button>
                {/if}
              </div>
              <div class="signal-actions">
                <button class="ov-btn" onclick={() => overlordStore.updateTaskState(s.t.id, 'done')}>Mark done</button>
                <button class="ov-btn ov-btn-danger" onclick={() => overlordStore.deleteTask(s.t.id)}>Drop</button>
              </div>

            {:else}
              <div class="signal-head">
                <span class="ov-chip ov-chip-tone">not running</span>
                <button class="ov-chip ov-chip-tab" onclick={() => navigateToTab(s.u.tab.id)}>{s.u.tab.name}</button>
                <span class="ov-chip">{s.u.ws.name}</span>
              </div>
              <p class="signal-text">This tab was an agent but nothing is running in it now — resume it or run <code>/maiterm init</code>.</p>
            {/if}

          </div>
        </article>
      {/each}
    {/if}

    <!-- ── Fleet ───────────────────────────────────────────────────────── -->
    {#if view === 'fleet'}
      {#if fleet.length === 0}
        <div class="allclear ov-in"><div class="allclear-rule"></div><span class="ov-label">no agent tabs in this window</span><div class="allclear-rule"></div></div>
      {/if}
      <div class="fleet">
        {#each fleet as u, i (u.tab.id)}
          <button class="unit ov-panel ov-in" class:unit-busy={!!u.ritual} style:--i={i}
                  onclick={() => navigateToTab(u.tab.id)}>
            <div class="unit-head">
              <span class="ov-dot" class:ov-dot-live={u.state === 'active' || u.state === 'permission'}
                    style:--tone={stateTone(u.state)}></span>
              <span class="unit-name">{u.tab.name}</span>
              <span class="ov-mono unit-age">{fmtAge(u.lastTurn)}</span>
            </div>
            <div class="unit-ws ov-label">{u.ws.name}</div>

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
          </button>
        {/each}
      </div>
    {/if}

    <!-- ── Board ───────────────────────────────────────────────────────── -->
    {#if view === 'board'}
      {#if overlordStore.tasks.length === 0}
        <div class="board-empty ov-panel ov-bracket ov-in">
          <p class="ov-label-lead">The board is empty</p>
          <p class="board-empty-copy">
            Scan your running agent tabs to populate it — todo lists are mirrored where they
            exist, and every other running tab gets a row you can fill in. Nothing is typed
            into any tab, and it's safe to run again any time.
          </p>
          <button class="ov-btn ov-btn-primary" onclick={runScan} disabled={overlordStore.scanning}>
            {overlordStore.scanning ? 'Scanning…' : 'Scan running tabs'}
          </button>
        </div>
      {/if}
      {#each boardWorkspaces as ws, i (ws.id)}
        {@const open = !collapsed.has(ws.id)}
        <section class="group ov-in" style:--i={i} class:dimmed={ws.suspended}>
          <button class="group-head" onclick={() => toggleCollapse(ws.id)}>
            <span class="group-caret" class:open>▸</span>
            <span class="ov-label-lead group-name">{ws.name}</span>
            {#if taskCount(ws.id) > 0}<span class="ov-chip">{taskCount(ws.id)} open</span>{/if}
            {#if ws.suspended}<span class="ov-chip">suspended</span>{/if}
            <span class="group-rule"></span>
          </button>

          {#if open}
            <div class="lanes">
              {#each LANES as lane (lane)}
                {@const cards = tasksFor(ws.id, lane)}
                <div class="lane">
                  <div class="lane-head">
                    <span class="ov-label">{lane}</span>
                    <span class="ov-mono lane-count">{cards.length}</span>
                  </div>
                  {#each cards as t (t.id)}
                    <div class="card" class:from-agent={t.origin === 'agent'} class:from-overlord={t.origin === 'overlord'}>
                      <div class="card-title">{t.title}</div>
                      <div class="card-foot">
                        {#if t.tab_id}
                          <button class="ov-chip ov-chip-tab" onclick={() => navigateToTab(t.tab_id!)}>{tabName(t.tab_id)}</button>
                        {/if}
                        <span class="ov-mono card-age">{fmtAge(t.updated_at)}</span>
                        <span class="card-ctl">
                          <button class="tick" title="Back" disabled={lane === 'backlog'} onclick={() => moveTask(t, -1)}>‹</button>
                          <button class="tick" title="Forward" disabled={lane === 'done'} onclick={() => moveTask(t, 1)}>›</button>
                          <button class="tick tick-del" title="Delete" onclick={() => overlordStore.deleteTask(t.id)}>×</button>
                        </span>
                      </div>
                    </div>
                  {/each}
                </div>
              {/each}
            </div>
            <input
              class="ov-input add-task"
              type="text"
              placeholder="Add a task to {ws.name}…"
              bind:value={newTaskTitles[ws.id]}
              onkeydown={(e) => e.key === 'Enter' && addTask(ws.id)}
            />
          {/if}
        </section>
      {/each}
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
            <button class="ov-chip ov-chip-tab" onclick={() => navigateToTab(e.tab_id)}>{tabName(e.tab_id)}</button>
            <span class="ov-mono entry-text" title={e.text}>{e.text}</span>
          </div>
        {/each}
      </div>
    {/if}
  </div>
</div>

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

  .scan-card { border-color: color-mix(in srgb, var(--ov-cool) 35%, var(--ov-hair)); }

  .board-empty { text-align: center; padding: 30px 24px; margin-bottom: 20px; }
  .board-empty-copy {
    color: var(--ov-ink-dim);
    font-size: 0.92rem;
    line-height: 1.6;
    max-width: 54ch;
    margin: 9px auto 14px;
  }

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
  .body {
    flex: 1;
    overflow-y: auto;
    padding: 16px var(--ov-gutter) 40px;
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
  .signal-text { color: var(--ov-ink-mid); font-size: 0.9rem; line-height: 1.5; }
  .signal-text code {
    font-family: var(--ov-mono);
    font-size: 0.82rem;
    color: var(--ov-live);
  }
  .signal-note { color: var(--ov-ink-dim); font-size: 0.8rem; margin-top: 5px; }
  .signal-actions { display: flex; gap: 6px; margin-top: 9px; }

  /* ── Fleet ────────────────────────────────────────────────────────────── */
  .fleet {
    display: grid;
    grid-template-columns: repeat(auto-fill, minmax(232px, 1fr));
    gap: 10px;
  }

  .unit {
    text-align: left;
    padding: 11px 12px 12px;
    display: flex;
    flex-direction: column;
    gap: 8px;
    cursor: pointer;
    transition: border-color 0.16s ease, transform 0.16s ease, background 0.16s ease;
  }
  .unit:hover {
    border-color: color-mix(in srgb, var(--ov-live) 55%, transparent);
    background: var(--ov-panel-lift);
    transform: translateY(-2px);
  }
  .unit-busy { border-color: color-mix(in srgb, var(--ov-live) 45%, transparent); }

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

  /* ── Board ────────────────────────────────────────────────────────────── */
  .group { margin-bottom: 20px; }
  .group.dimmed { opacity: 0.5; }

  .group-head {
    display: flex;
    align-items: center;
    gap: 9px;
    width: 100%;
    padding: 5px 0 9px;
    text-align: left;
  }
  .group-caret {
    color: var(--ov-ink-dim);
    font-size: 0.7rem;
    transition: transform 0.16s ease;
  }
  .group-caret.open { transform: rotate(90deg); }
  .group-name { font-size: 1.05rem; }
  .group-head:hover .group-name { color: var(--ov-live); }
  .group-rule { flex: 1; height: 1px; background: var(--ov-hair); }

  .lanes {
    display: grid;
    grid-template-columns: repeat(5, minmax(0, 1fr));
    gap: 8px;
  }

  .lane { min-width: 0; }
  .lane-head {
    display: flex;
    align-items: baseline;
    justify-content: space-between;
    padding-bottom: 5px;
    margin-bottom: 7px;
    border-bottom: 1px solid var(--ov-hair);
  }
  .lane-count { font-size: 0.75rem; color: var(--ov-ink-dim); }

  .card {
    background: var(--ov-panel);
    border: 1px solid var(--ov-hair);
    border-left: 2px solid var(--ov-hair-strong);
    border-radius: 2px;
    padding: 7px 9px;
    margin-bottom: 6px;
    transition: border-color 0.14s ease, background 0.14s ease;
  }
  .card:hover { background: var(--ov-panel-lift); }
  .card.from-agent { border-left-color: var(--ov-live); }
  .card.from-overlord { border-left-color: var(--ov-note); }

  .card-title { font-size: 0.86rem; line-height: 1.4; word-break: break-word; }
  .card-foot { display: flex; align-items: center; gap: 6px; margin-top: 6px; }
  .card-age { font-size: 0.7rem; color: var(--ov-ink-dim); margin-left: auto; }
  .card-ctl { display: flex; gap: 1px; }

  .tick {
    color: var(--ov-ink-dim);
    font-size: 0.95rem;
    line-height: 1;
    padding: 0 3px;
    border-radius: 2px;
    transition: color 0.12s ease;
  }
  .tick:hover { color: var(--ov-live); }
  .tick:disabled { opacity: 0.2; cursor: default; }
  .tick:disabled:hover { color: var(--ov-ink-dim); }
  .tick-del:hover { color: var(--ov-critical); }

  .add-task { width: 100%; margin-top: 9px; }

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
  .entry-text {
    color: var(--ov-ink-dim);
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    flex: 1;
    min-width: 0;
  }
</style>
