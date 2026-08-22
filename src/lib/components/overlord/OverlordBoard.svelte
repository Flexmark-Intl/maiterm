<script lang="ts">
  import { onMount, onDestroy } from 'svelte';
  import { workspacesStore, navigateToTab } from '$lib/stores/workspaces.svelte';
  import { claudeStateStore } from '$lib/stores/agentState.svelte';
  import { overlordStore } from '$lib/stores/overlord.svelte';
  import { preferencesStore } from '$lib/stores/preferences.svelte';
  import type { OverlordTask, OverlordTaskState, Workspace, Tab } from '$lib/tauri/types';
  import Button from '$lib/components/ui/Button.svelte';

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

  let containerRef: HTMLDivElement;

  function attachToSlot() {
    const slot = document.querySelector(`[data-terminal-slot="${tabId}"]`) as HTMLElement;
    if (slot && containerRef && containerRef.parentElement !== slot) {
      slot.appendChild(containerRef);
    }
  }

  function handleSlotReady(e: Event) {
    const detail = (e as CustomEvent).detail;
    if (detail?.tabId === tabId) attachToSlot();
  }

  onMount(() => {
    attachToSlot();
    window.addEventListener('terminal-slot-ready', handleSlotReady);
  });

  onDestroy(() => {
    window.removeEventListener('terminal-slot-ready', handleSlotReady);
    containerRef?.remove();
  });

  const TASK_STATES: OverlordTaskState[] = ['backlog', 'active', 'blocked', 'review', 'done'];
  const CONTEXT_ATTENTION_PCT = 50;
  const STALE_DAYS = 3;

  let showLedger = $state(false);
  let newTaskTitles = $state<Record<string, string>>({});

  /** Supervised (non-overlord) workspaces, in sidebar order. */
  const boardWorkspaces = $derived(workspacesStore.workspaces.filter(w => !w.overlord));

  function agentTabsOf(ws: Workspace): Tab[] {
    const out: Tab[] = [];
    for (const pane of ws.panes) {
      for (const t of pane.tabs) {
        if ((t.tab_type ?? 'terminal') === 'terminal' && t.runtime) out.push(t);
      }
    }
    return out;
  }

  // ── Attention queue (docs/overlord.md §11 — the primary view) ───────────────

  const nearCompaction = $derived.by(() => {
    const out: { tab: Tab; ws: Workspace; pct: number }[] = [];
    for (const ws of boardWorkspaces) {
      for (const tab of agentTabsOf(ws)) {
        const pct = overlordStore.facts.get(tab.id)?.context_pct;
        if (pct !== undefined && pct >= CONTEXT_ATTENTION_PCT) out.push({ tab, ws, pct });
      }
    }
    return out.sort((a, b) => b.pct - a.pct);
  });

  const permissionTabs = $derived.by(() => {
    const out: { tab: Tab; ws: Workspace }[] = [];
    for (const ws of boardWorkspaces) {
      for (const tab of agentTabsOf(ws)) {
        if (claudeStateStore.getState(tab.id)?.state === 'permission') out.push({ tab, ws });
      }
    }
    return out;
  });

  const unreadyAgents = $derived.by(() => {
    const out: { tab: Tab; ws: Workspace }[] = [];
    for (const ws of boardWorkspaces) {
      if (ws.suspended) continue;
      for (const tab of agentTabsOf(ws)) {
        if (!claudeStateStore.getState(tab.id)) out.push({ tab, ws });
      }
    }
    return out;
  });

  const staleTasks = $derived(
    overlordStore.tasks.filter(
      t => t.state !== 'done' && Date.now() - Date.parse(t.updated_at) > STALE_DAYS * 86_400_000,
    ),
  );

  const attentionCount = $derived(
    overlordStore.proposals.length +
    overlordStore.escalations.length +
    nearCompaction.length +
    permissionTabs.length +
    staleTasks.length,
  );

  function tasksFor(wsId: string, state: OverlordTaskState): OverlordTask[] {
    return overlordStore.tasks.filter(t => t.workspace_id === wsId && t.state === state);
  }

  function taskCount(wsId: string): number {
    return overlordStore.tasks.filter(t => t.workspace_id === wsId).length;
  }

  function cycleState(task: OverlordTask, dir: 1 | -1) {
    const idx = TASK_STATES.indexOf(task.state);
    const next = TASK_STATES[Math.min(TASK_STATES.length - 1, Math.max(0, idx + dir))];
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
        const t = pane.tabs.find(t => t.id === id);
        if (t) return t.name;
      }
    }
    return id.slice(0, 8);
  }

  function fmtAge(iso: string): string {
    const ms = Date.now() - Date.parse(iso);
    const d = Math.floor(ms / 86_400_000);
    if (d > 0) return `${d}d`;
    const h = Math.floor(ms / 3_600_000);
    if (h > 0) return `${h}h`;
    return `${Math.max(1, Math.floor(ms / 60_000))}m`;
  }
</script>

<!-- svelte-ignore a11y_no_static_element_interactions -->
<div class="overlord-board" class:hidden={!visible} bind:this={containerRef} onmousedowncapture={focusPane}>
  <div class="board-toolbar">
    <span class="board-title">♔ Overlord</span>
    <span class="board-sub">{boardWorkspaces.length} workspaces · {overlordStore.tasks.length} tasks</span>
    <label class="propose-toggle" title="Rules land as proposals you approve, instead of firing autonomously">
      <input
        type="checkbox"
        checked={preferencesStore.overlordProposeMode}
        onchange={(e) => preferencesStore.setOverlordProposeMode((e.target as HTMLInputElement).checked)}
      />
      Propose mode
    </label>
    <button class="ledger-toggle" onclick={() => (showLedger = !showLedger)}>
      Ledger {showLedger ? '▾' : '▸'}
    </button>
  </div>

  <div class="board-scroll">
    <!-- ── Attention queue ─────────────────────────────────────────────── -->
    <section class="attention">
      <h2>Attention {#if attentionCount}<span class="count">{attentionCount}</span>{/if}</h2>
      {#if attentionCount === 0}
        <div class="empty">Nothing needs you right now.</div>
      {/if}

      {#each overlordStore.proposals as p (p.id)}
        <div class="attn-item proposal">
          <div class="attn-body">
            <span class="attn-kind">PROPOSED</span>
            <span class="attn-title">{p.ruleName}</span>
            <button class="tab-chip" onclick={() => navigateToTab(p.tabId)}>{p.tabName}</button>
            <span class="attn-detail">“{p.preview}”{p.stepCount > 1 ? ` (+${p.stepCount - 1} more steps)` : ''}</span>
          </div>
          <div class="attn-actions">
            <Button variant="primary" onclick={() => overlordStore.approveProposal(p.id)} style="padding:2px 10px;font-size:0.846rem">Send</Button>
            <Button variant="secondary" onclick={() => overlordStore.dismissProposal(p.id)} style="padding:2px 10px;font-size:0.846rem">Dismiss</Button>
          </div>
        </div>
      {/each}

      {#each overlordStore.escalations as e (e.id)}
        <div class="attn-item escalation">
          <div class="attn-body">
            <span class="attn-kind esc">ESCALATION</span>
            <button class="tab-chip" onclick={() => navigateToTab(e.tabId)}>{tabName(e.tabId)}</button>
            <span class="attn-detail">{e.detail}</span>
          </div>
          <div class="attn-actions">
            <Button variant="secondary" onclick={() => overlordStore.dismissEscalation(e.id)} style="padding:2px 10px;font-size:0.846rem">Dismiss</Button>
          </div>
        </div>
      {/each}

      {#each permissionTabs as { tab, ws } (tab.id)}
        <div class="attn-item">
          <div class="attn-body">
            <span class="attn-kind perm">PERMISSION</span>
            <button class="tab-chip" onclick={() => navigateToTab(tab.id)}>{tab.name}</button>
            <span class="attn-detail">waiting on an approval in {ws.name}</span>
          </div>
        </div>
      {/each}

      {#each nearCompaction as { tab, ws, pct } (tab.id)}
        <div class="attn-item">
          <div class="attn-body">
            <span class="attn-kind ctx">CONTEXT {pct}%</span>
            <button class="tab-chip" onclick={() => navigateToTab(tab.id)}>{tab.name}</button>
            <span class="attn-detail">{ws.name} — approaching compaction</span>
          </div>
        </div>
      {/each}

      {#each staleTasks as t (t.id)}
        <div class="attn-item">
          <div class="attn-body">
            <span class="attn-kind stale">STALE {fmtAge(t.updated_at)}</span>
            <span class="attn-title">{t.title}</span>
            {#if t.tab_id}<button class="tab-chip" onclick={() => navigateToTab(t.tab_id!)}>{tabName(t.tab_id)}</button>{/if}
          </div>
          <div class="attn-actions">
            <Button variant="secondary" onclick={() => overlordStore.updateTaskState(t.id, 'done')} style="padding:2px 10px;font-size:0.846rem">Done</Button>
          </div>
        </div>
      {/each}

      {#if unreadyAgents.length}
        <div class="attn-item">
          <div class="attn-body">
            <span class="attn-kind unready">UNREADY</span>
            <span class="attn-detail">
              {#each unreadyAgents as { tab } (tab.id)}
                <button class="tab-chip" onclick={() => navigateToTab(tab.id)}>{tab.name}</button>
              {/each}
              — agent not running (needs resume / init)
            </span>
          </div>
        </div>
      {/if}
    </section>

    {#if showLedger}
      <section class="ledger">
        <h2>Ledger</h2>
        {#if overlordStore.recentLedger.length === 0}
          <div class="empty">No injections yet.</div>
        {/if}
        {#each [...overlordStore.recentLedger].reverse().slice(0, 50) as e (e.id)}
          <div class="ledger-row">
            <span class="ledger-ts">{new Date(e.ts).toLocaleTimeString()}</span>
            <span class="ledger-outcome" data-outcome={e.outcome}>{e.outcome}</span>
            <button class="tab-chip" onclick={() => navigateToTab(e.tab_id)}>{tabName(e.tab_id)}</button>
            <span class="ledger-text" title={e.text}>{e.text}</span>
          </div>
        {/each}
      </section>
    {/if}

    <!-- ── Kanban, grouped by workspace ────────────────────────────────── -->
    {#each boardWorkspaces as ws (ws.id)}
      <section class="ws-group" class:suspended={ws.suspended}>
        <h2>
          {ws.name}
          <span class="count">{taskCount(ws.id)}</span>
          {#if ws.suspended}<span class="suspended-tag">suspended</span>{/if}
        </h2>
        <div class="kanban">
          {#each TASK_STATES as state (state)}
            <div class="col">
              <div class="col-head">{state}</div>
              {#each tasksFor(ws.id, state) as t (t.id)}
                <div class="card" class:origin-agent={t.origin === 'agent'} class:origin-overlord={t.origin === 'overlord'}>
                  <div class="card-title">{t.title}</div>
                  <div class="card-meta">
                    {#if t.tab_id}<button class="tab-chip" onclick={() => navigateToTab(t.tab_id!)}>{tabName(t.tab_id)}</button>{/if}
                    <span class="card-age">{fmtAge(t.updated_at)}</span>
                    <span class="card-actions">
                      {#if state !== 'backlog'}<button class="mini" title="Move left" onclick={() => cycleState(t, -1)}>‹</button>{/if}
                      {#if state !== 'done'}<button class="mini" title="Move right" onclick={() => cycleState(t, 1)}>›</button>{/if}
                      <button class="mini del" title="Delete" onclick={() => overlordStore.deleteTask(t.id)}>×</button>
                    </span>
                  </div>
                </div>
              {/each}
            </div>
          {/each}
        </div>
        <div class="add-task">
          <input
            type="text"
            placeholder="Add a task…"
            bind:value={newTaskTitles[ws.id]}
            onkeydown={(e) => e.key === 'Enter' && addTask(ws.id)}
          />
        </div>
      </section>
    {/each}
  </div>
</div>

<style>
  .overlord-board {
    display: flex;
    flex-direction: column;
    flex: 1;
    min-height: 0;
    min-width: 0;
    background: var(--bg-dark);
    overflow: hidden;
    color: var(--fg);
  }

  .overlord-board.hidden {
    position: absolute;
    top: 0; left: 0; right: 0; bottom: 0;
    opacity: 0;
    pointer-events: none;
    z-index: -1;
  }

  .board-toolbar {
    display: flex;
    align-items: center;
    gap: 12px;
    padding: 8px 16px;
    background: var(--bg-medium);
    border-bottom: 1px solid var(--bg-light);
    flex-shrink: 0;
  }

  .board-title { font-weight: 700; font-size: 1rem; }
  .board-sub { color: var(--fg-dim); font-size: 0.846rem; flex: 1; }

  .propose-toggle {
    display: flex;
    align-items: center;
    gap: 6px;
    font-size: 0.846rem;
    color: var(--fg-dim);
    cursor: pointer;
  }

  .ledger-toggle {
    background: none;
    border: 1px solid var(--bg-light);
    border-radius: 4px;
    color: var(--fg-dim);
    padding: 2px 10px;
    font-size: 0.846rem;
    cursor: pointer;
  }
  .ledger-toggle:hover { color: var(--fg); }

  .board-scroll {
    flex: 1;
    overflow-y: auto;
    padding: 12px 16px 24px;
  }

  section { margin-bottom: 20px; }

  h2 {
    font-size: 0.846rem;
    font-weight: 600;
    letter-spacing: 0.5px;
    text-transform: uppercase;
    color: var(--fg-dim);
    margin: 0 0 8px;
    display: flex;
    align-items: center;
    gap: 8px;
  }

  .count {
    background: var(--bg-light);
    border-radius: 8px;
    padding: 0 6px;
    font-size: 0.769rem;
  }

  .suspended-tag { color: var(--fg-dim); font-weight: 400; text-transform: none; }
  .ws-group.suspended { opacity: 0.55; }

  .empty { color: var(--fg-dim); font-size: 0.923rem; padding: 4px 0; }

  .attn-item {
    display: flex;
    align-items: center;
    gap: 8px;
    padding: 6px 10px;
    border: 1px solid var(--bg-light);
    border-radius: 6px;
    margin-bottom: 6px;
    background: var(--bg-medium);
  }
  .attn-item.proposal { border-color: var(--accent); }
  .attn-body { display: flex; align-items: center; gap: 8px; flex: 1; min-width: 0; flex-wrap: wrap; }
  .attn-actions { display: flex; gap: 6px; flex-shrink: 0; }

  .attn-kind {
    font-size: 0.692rem;
    font-weight: 700;
    letter-spacing: 0.5px;
    padding: 1px 6px;
    border-radius: 4px;
    background: var(--accent);
    color: var(--bg-dark);
    flex-shrink: 0;
  }
  .attn-kind.esc { background: #f7768e; }
  .attn-kind.perm { background: #e0af68; }
  .attn-kind.ctx { background: #ff9e64; }
  .attn-kind.stale { background: var(--bg-light); color: var(--fg); }
  .attn-kind.unready { background: var(--bg-light); color: var(--fg-dim); }

  .attn-title { font-weight: 600; font-size: 0.923rem; }
  .attn-detail { color: var(--fg-dim); font-size: 0.846rem; overflow: hidden; text-overflow: ellipsis; }

  .tab-chip {
    background: var(--bg-dark);
    border: 1px solid var(--bg-light);
    border-radius: 10px;
    color: var(--accent);
    font-size: 0.769rem;
    padding: 1px 8px;
    cursor: pointer;
    white-space: nowrap;
  }
  .tab-chip:hover { border-color: var(--accent); }

  .kanban {
    display: grid;
    grid-template-columns: repeat(5, minmax(140px, 1fr));
    gap: 8px;
  }

  .col-head {
    font-size: 0.769rem;
    text-transform: capitalize;
    color: var(--fg-dim);
    padding-bottom: 4px;
    border-bottom: 1px solid var(--bg-light);
    margin-bottom: 6px;
  }

  .card {
    background: var(--bg-medium);
    border: 1px solid var(--bg-light);
    border-radius: 6px;
    padding: 6px 8px;
    margin-bottom: 6px;
  }
  .card.origin-agent { border-left: 3px solid var(--accent); }
  .card.origin-overlord { border-left: 3px solid #e0af68; }

  .card-title { font-size: 0.846rem; word-break: break-word; }
  .card-meta { display: flex; align-items: center; gap: 6px; margin-top: 4px; }
  .card-age { color: var(--fg-dim); font-size: 0.692rem; flex: 1; text-align: right; }
  .card-actions { display: flex; gap: 2px; }

  .mini {
    background: none;
    border: none;
    color: var(--fg-dim);
    cursor: pointer;
    font-size: 0.923rem;
    padding: 0 3px;
    line-height: 1;
  }
  .mini:hover { color: var(--fg); }
  .mini.del:hover { color: #f7768e; }

  .add-task input {
    width: 100%;
    margin-top: 4px;
    background: var(--bg-medium);
    border: 1px solid var(--bg-light);
    border-radius: 4px;
    color: var(--fg);
    padding: 4px 8px;
    font-size: 0.846rem;
  }
  .add-task input:focus { outline: none; border-color: var(--accent); }

  .ledger-row {
    display: flex;
    align-items: center;
    gap: 8px;
    font-size: 0.769rem;
    padding: 3px 0;
    border-bottom: 1px solid var(--bg-medium);
  }
  .ledger-ts { color: var(--fg-dim); flex-shrink: 0; }
  .ledger-outcome {
    flex-shrink: 0;
    font-weight: 600;
    color: var(--fg-dim);
  }
  .ledger-outcome[data-outcome='sent'], .ledger-outcome[data-outcome='acked'] { color: #9ece6a; }
  .ledger-outcome[data-outcome='timed_out'], .ledger-outcome[data-outcome='aborted'] { color: #e0af68; }
  .ledger-outcome[data-outcome='blocked_no_repl'], .ledger-outcome[data-outcome='blocked_guard'] { color: #f7768e; }
  .ledger-text {
    color: var(--fg-dim);
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    font-family: Menlo, monospace;
  }
</style>
