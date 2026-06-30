<script lang="ts">
  import { workspacesStore, navigateToTab } from '$lib/stores/workspaces.svelte';
  import { agentMeshStore } from '$lib/stores/agentMesh.svelte';
  import { computeGraph, topicHue } from '$lib/stores/meshGraph';
  import type { MeshMember } from '$lib/stores/meshRouting';
  import StatusDot from '$lib/components/ui/StatusDot.svelte';

  interface Props {
    open: boolean;
    onclose: () => void;
  }
  let { open, onclose }: Props = $props();

  // The cockpit operates on the workspace you're looking at.
  const ws = $derived(workspacesStore.activeWorkspace);
  const isMesh = $derived(!!ws?.bridge_all);

  // A 1s tick (only while open) keeps the live pulse / state fresh without depending on
  // every upstream store being individually reactive.
  let tick = $state(0);
  $effect(() => {
    if (!open) return;
    const id = setInterval(() => { tick++; }, 1000);
    return () => clearInterval(id);
  });

  let busy = $state(false);

  // ── Derived cockpit data (re-reads on mesh changes + the tick) ──────────────
  const board = $derived.by(() => {
    void agentMeshStore.version; void tick;
    return ws ? agentMeshStore.statusBoard(ws.id) : [];
  });
  const topics = $derived.by(() => {
    void agentMeshStore.version; void tick;
    return ws ? agentMeshStore.topicsForWorkspace(ws.id) : [];
  });
  const paused = $derived.by(() => {
    void agentMeshStore.version; void tick;
    return ws ? agentMeshStore.pausedTopics(ws.id) : [];
  });
  const pausedIds = $derived(new Set(paused.map((p) => p.id)));
  const roleOf = $derived((tabId: string) => board.find((b) => b.tabId === tabId)?.role ?? tabId.slice(0, 6));

  // ── Conversation graph geometry ─────────────────────────────────────────────
  // Node labels are word-wrapped via <foreignObject>; LBL_* size the label box and GH leaves
  // vertical room above the top node / below the bottom node so 2-line labels don't clip.
  const GW = 260, GH = 240;
  const LBL_W = 104, LBL_H = 26, LBL_GAP = 12;
  const graph = $derived.by(() => {
    void agentMeshStore.version; void tick;
    if (!ws || !isMesh) return { nodes: [], edges: [] };
    const members: MeshMember[] = board.map((b) => ({ tabId: b.tabId, role: b.role, cwd: b.cwd, purpose: b.purpose, live: b.live }));
    const active = new Set(board.filter((b) => b.claudeState === 'active').map((b) => b.tabId));
    return computeGraph(members, topics, agentMeshStore.getEdges(), active, Date.now(), { cx: GW / 2, cy: GH / 2, radius: Math.min(78, 30 + members.length * 8) }, pausedIds);
  });

  function reasonLabel(r: string): string {
    return r === 'soft' ? 'soft cap' : r === 'hard' ? 'hard ceiling' : 'time limit';
  }

  function enableMesh() {
    if (!ws) return;
    // Open the pre-flight setup modal (readiness check + fixes) instead of enabling blindly.
    window.dispatchEvent(new CustomEvent('open-mesh-setup', { detail: ws.id }));
    onclose();
  }
  async function disableMesh() {
    if (!ws) return;
    busy = true;
    try { await agentMeshStore.setMeshEnabled(ws.id, false); } finally { busy = false; }
  }
  function setPurpose(tabId: string, e: Event) {
    agentMeshStore.setPurpose(tabId, (e.currentTarget as HTMLInputElement).value);
  }
  function completeTopic(id: string) {
    agentMeshStore.completeTopic(null, id, true);
  }
  function resumeTopic(id: string) {
    agentMeshStore.resumeTopic(id);
  }
  async function openTab(tabId: string) {
    await navigateToTab(tabId);
    onclose();
  }
  const stageActive = $derived.by(() => { void agentMeshStore.version; return ws ? agentMeshStore.isStageView(ws.id) : false; });
  function toggleStage() {
    if (!ws) return;
    agentMeshStore.toggleStageView(ws.id);
    onclose(); // get out of the way so the stage layout is visible
  }
  function recheck() {
    if (!ws) return;
    // Re-open the readiness modal for this already-enabled mesh (wake/init agents that dropped,
    // e.g. after an app restart). The modal detects bridge_all and runs in re-check mode.
    window.dispatchEvent(new CustomEvent('open-mesh-setup', { detail: ws.id }));
    onclose();
  }

  function handleKeydown(e: KeyboardEvent) {
    if (e.key === 'Escape') { e.stopPropagation(); onclose(); }
  }
  function handleBackdrop(e: MouseEvent) {
    if (e.target === e.currentTarget) onclose();
  }
</script>

{#if open}
  <!-- svelte-ignore a11y_no_noninteractive_tabindex -->
  <div class="mesh-backdrop" onclick={handleBackdrop} onkeydown={handleKeydown} role="dialog" aria-modal="true" tabindex="-1">
    <aside class="cockpit">
      <header class="cockpit-header">
        <span class="mesh-badge">MESH</span>
        <h2>{ws?.name ?? 'Workspace'}</h2>
        {#if isMesh}<span class="count">{board.length} agent{board.length === 1 ? '' : 's'}</span>{/if}
        <button class="close-btn" onclick={onclose} title="Close (Esc)" aria-label="Close">×</button>
      </header>

      {#if !ws}
        <div class="empty">No active workspace.</div>
      {:else if !isMesh}
        <div class="enable-cta">
          <p class="lead">Turn <strong>{ws.name}</strong> into a <strong>Mesh Workspace</strong>.</p>
          <p class="hint">
            Every agent tab here becomes reachable by every other over topic-scoped threads —
            a moderated roundtable you steer from this cockpit. Name each agent tab descriptively
            (that name is its address) and give it a one-line purpose below once enabled.
          </p>
          <button class="primary" disabled={busy} onclick={enableMesh}>Enable Mesh</button>
        </div>
      {:else}
        {#if paused.length > 0}
          <div class="paused-banner">
            {#each paused as p (p.id)}
              <div class="paused-row">
                <span class="pause-icon">⏸</span>
                <span class="pause-label">"{p.label}"</span>
                <span class="pause-meta">paused · {reasonLabel(p.reason)} · {p.turn}/{p.cap} turns</span>
                {#if p.reason !== 'hard'}
                  <button class="mini" onclick={() => resumeTopic(p.id)}>Resume</button>
                {/if}
                <button class="mini ghost" onclick={() => completeTopic(p.id)}>Complete</button>
              </div>
            {/each}
          </div>
        {/if}

        <!-- Conversation graph -->
        <section class="graph-section">
          {#if board.length === 0}
            <div class="empty small">No named agents yet. Name each agent tab — that name is how peers address it.</div>
          {:else}
            <svg viewBox="0 0 {GW} {GH}" class="graph" role="img" aria-label="Conversation graph">
              {#each graph.edges as e (e.topicId + e.from + e.to)}
                <line
                  x1={e.x1} y1={e.y1} x2={e.x2} y2={e.y2}
                  class="edge" class:recent={e.recent} class:paused={e.paused}
                  stroke="hsl({e.hue} 70% 62%)"
                  stroke-width={Math.min(5, 1 + e.turns * 0.45)}
                  stroke-dasharray={e.paused ? '3 3' : undefined}
                />
                {#if e.recent}
                  <circle class="flow" r="2.4" fill="hsl({e.hue} 80% 70%)">
                    <animate attributeName="cx" from={e.x1} to={e.x2} dur="1.1s" repeatCount="indefinite" />
                    <animate attributeName="cy" from={e.y1} to={e.y2} dur="1.1s" repeatCount="indefinite" />
                  </circle>
                {/if}
              {/each}
              {#each graph.nodes as n (n.tabId)}
                {@const below = n.y >= GH / 2}
                <g class="node" class:active={n.active} class:offline={!n.live} onclick={() => openTab(n.tabId)} onkeydown={(e) => { if (e.key === 'Enter') openTab(n.tabId); }} role="button" tabindex="-1">
                  <circle cx={n.x} cy={n.y} r="9" />
                  {#if n.active}<circle class="halo" cx={n.x} cy={n.y} r="9" />{/if}
                  <foreignObject x={n.x - LBL_W / 2} y={below ? n.y + LBL_GAP : n.y - LBL_GAP - LBL_H} width={LBL_W} height={LBL_H}>
                    <div xmlns="http://www.w3.org/1999/xhtml" class="node-label" class:below class:above={!below}><span>{n.role}</span></div>
                  </foreignObject>
                </g>
              {/each}
            </svg>
          {/if}
        </section>

        <!-- Topics -->
        {#if topics.length > 0}
          <section class="panel">
            <h3>Topics</h3>
            {#each topics as t (t.id)}
              {@const isPaused = pausedIds.has(t.id)}
              <div class="topic" class:complete={t.state === 'complete'} class:paused={isPaused}>
                <span class="swatch" style="background: hsl({topicHue(t.id)} 70% 62%)"></span>
                <span class="t-label" title={t.label}>{t.label}</span>
                <span class="t-meta">{roleOf(t.owner_tab_id)} · {t.turn} turn{t.turn === 1 ? '' : 's'}</span>
                {#if t.state === 'complete'}
                  <span class="t-state done">done</span>
                {:else}
                  {#if isPaused}<button class="mini" onclick={() => resumeTopic(t.id)}>Resume</button>{/if}
                  <button class="mini ghost" onclick={() => completeTopic(t.id)}>Complete</button>
                {/if}
              </div>
            {/each}
          </section>
        {/if}

        <!-- Status board -->
        <section class="panel">
          <h3>Status board</h3>
          {#if board.length === 0}
            <div class="empty small">Agents post their status here as they work.</div>
          {/if}
          {#each board as a (a.tabId)}
            <div class="agent-card" class:needs={a.needsInput}>
              <div class="agent-head">
                <StatusDot color={a.claudeState === 'active' ? 'accent' : a.live ? 'green' : 'dim'} pulse={a.claudeState === 'active'} />
                <button class="role-link" onclick={() => openTab(a.tabId)} title="Open this agent's tab">{a.role}</button>
                <span class="spacer"></span>
                {#if a.needsInput}<button class="needs-you" onclick={() => openTab(a.tabId)} title="This agent is asking you — open its tab to answer">needs you</button>{/if}
                {#if a.cwd}<span class="cwd" title={a.cwd}>{a.cwd.split('/').pop()}</span>{/if}
              </div>
              <input
                class="purpose-input"
                placeholder="optional steer — a boundary or scope the name doesn't say…"
                title="Optional. The agent declares its own scope on join; use this only to constrain or correct it (e.g. 'auth flow only, not the whole API')."
                value={a.purpose ?? ''}
                onchange={(e) => setPurpose(a.tabId, e)}
              />
            </div>
          {/each}
        </section>

        <footer class="cockpit-footer">
          <button class="mini" onclick={toggleStage}>{stageActive ? 'Exit stage view' : 'Stage view'}</button>
          <button class="mini ghost" onclick={recheck}>Re-check</button>
          <button class="mini ghost danger" disabled={busy} onclick={disableMesh}>Disable Mesh</button>
        </footer>
      {/if}
    </aside>
  </div>
{/if}

<style>
  .mesh-backdrop {
    position: fixed;
    inset: 0;
    background: rgba(0, 0, 0, 0.35);
    z-index: 1000;
    display: flex;
    justify-content: flex-end;
  }
  .cockpit {
    width: 420px;
    max-width: 92vw;
    height: 100%;
    background: var(--bg-medium);
    border-left: 1px solid var(--bg-light);
    box-shadow: -8px 0 28px rgba(0, 0, 0, 0.4);
    display: flex;
    flex-direction: column;
    overflow-y: auto;
    animation: slide-in 0.16s ease-out;
  }
  @keyframes slide-in { from { transform: translateX(28px); opacity: 0.6; } to { transform: translateX(0); opacity: 1; } }

  .cockpit-header {
    display: flex;
    align-items: center;
    gap: 8px;
    padding: 12px 14px;
    border-bottom: 1px solid var(--bg-light);
    position: sticky;
    top: 0;
    background: var(--bg-medium);
    z-index: 2;
  }
  .cockpit-header h2 { font-size: 14px; margin: 0; font-weight: 600; color: var(--fg); white-space: nowrap; overflow: hidden; text-overflow: ellipsis; }
  .mesh-badge {
    font-size: 9px; font-weight: 700; letter-spacing: 0.08em;
    color: var(--bg-dark); background: var(--accent);
    padding: 2px 5px; border-radius: 3px;
  }
  .count { font-size: 11px; color: var(--fg-dim); margin-left: auto; }
  .close-btn { background: none; border: none; color: var(--fg-dim); font-size: 20px; line-height: 1; cursor: pointer; padding: 0 2px; }
  .close-btn:hover { color: var(--fg); }

  .empty { padding: 24px 16px; color: var(--fg-dim); font-size: 12px; text-align: center; }
  .empty.small { padding: 12px; font-size: 11px; }

  .enable-cta { padding: 20px 16px; display: flex; flex-direction: column; gap: 12px; }
  .enable-cta .lead { font-size: 14px; color: var(--fg); margin: 0; }
  .enable-cta .hint { font-size: 12px; color: var(--fg-dim); line-height: 1.5; margin: 0; }
  .primary {
    align-self: flex-start;
    background: var(--accent); color: var(--bg-dark);
    border: none; border-radius: 5px; padding: 7px 16px;
    font-size: 12px; font-weight: 600; cursor: pointer;
  }
  .primary:hover { background: var(--accent-hover); }
  .primary:disabled { opacity: 0.5; cursor: default; }

  .paused-banner {
    margin: 10px 12px 0;
    background: color-mix(in srgb, var(--yellow) 12%, transparent);
    border: 1px solid color-mix(in srgb, var(--yellow) 40%, transparent);
    border-radius: 6px;
    padding: 6px 8px;
    display: flex; flex-direction: column; gap: 6px;
  }
  .paused-row { display: flex; align-items: center; gap: 6px; font-size: 11px; }
  .pause-icon { color: var(--yellow); }
  .pause-label { font-weight: 600; color: var(--fg); }
  .pause-meta { color: var(--fg-dim); margin-right: auto; }

  .graph-section { padding: 8px 12px 4px; }
  .graph { width: 100%; height: auto; display: block; }
  .edge { opacity: 0.5; transition: opacity 0.3s; }
  .edge.recent { opacity: 0.95; }
  .edge.paused { opacity: 0.35; }
  .flow { opacity: 0.9; }
  .node circle { fill: var(--bg-light); stroke: var(--fg-dim); stroke-width: 1.5; cursor: pointer; transition: fill 0.2s; }
  .node.active circle { fill: var(--accent); stroke: var(--accent); }
  .node.offline circle { fill: var(--bg-dark); stroke: var(--bg-light); }
  .node .halo { fill: none; stroke: var(--accent); stroke-width: 1.5; opacity: 0.5; animation: halo 1.4s ease-out infinite; }
  @keyframes halo { from { r: 9px; opacity: 0.55; } to { r: 18px; opacity: 0; } }
  /* foreignObject label: word-wrap + clamp to 2 lines so long agent names don't run into
     neighbors or clip. Anchored to the node edge — above for top-half nodes, below otherwise. */
  .node-label {
    width: 100%; height: 100%; box-sizing: border-box;
    display: flex; justify-content: center;
    text-align: center; line-height: 1.12;
    color: var(--fg); font-size: 9px;
    overflow: hidden; pointer-events: none;
  }
  .node-label.below { align-items: flex-start; }
  .node-label.above { align-items: flex-end; }
  .node-label span {
    display: -webkit-box; -webkit-box-orient: vertical; -webkit-line-clamp: 2;
    overflow: hidden; overflow-wrap: anywhere;
  }
  .node.offline .node-label { color: var(--fg-dim); }

  .panel { padding: 10px 12px; border-top: 1px solid var(--bg-light); }
  .panel h3 { font-size: 11px; text-transform: uppercase; letter-spacing: 0.06em; color: var(--fg-dim); margin: 0 0 8px; }

  .topic { display: flex; align-items: center; gap: 7px; padding: 4px 0; font-size: 12px; }
  .topic.complete { opacity: 0.5; }
  .topic.paused .t-label { color: var(--yellow); }
  .swatch { width: 9px; height: 9px; border-radius: 2px; flex-shrink: 0; }
  .t-label { color: var(--fg); white-space: nowrap; overflow: hidden; text-overflow: ellipsis; max-width: 130px; }
  .t-meta { color: var(--fg-dim); font-size: 11px; margin-left: auto; white-space: nowrap; }
  .t-state.done { color: var(--green); font-size: 10px; text-transform: uppercase; }

  .agent-card { padding: 8px; margin-bottom: 8px; background: var(--bg-dark); border-radius: 6px; border: 1px solid transparent; }
  .agent-card.needs { border-color: color-mix(in srgb, var(--yellow) 45%, transparent); }
  .agent-head { display: flex; align-items: center; gap: 6px; }
  .role-link { background: none; border: none; color: var(--fg); font-size: 12px; font-weight: 600; cursor: pointer; padding: 0; }
  .role-link:hover { color: var(--accent); }
  .spacer { flex: 1; }
  .cwd { font-size: 10px; color: var(--fg-dim); font-family: monospace; }
  .purpose-input {
    width: 100%; margin-top: 6px; box-sizing: border-box;
    background: var(--bg-medium); border: 1px solid var(--bg-light); border-radius: 4px;
    color: var(--fg); font-size: 11px; padding: 4px 6px;
  }
  .purpose-input:focus { outline: none; border-color: var(--accent); }
  .needs-you {
    background: color-mix(in srgb, var(--yellow) 18%, transparent);
    border: 1px solid color-mix(in srgb, var(--yellow) 45%, transparent); border-radius: 3px;
    color: var(--yellow); font-size: 9px; font-weight: 700; letter-spacing: 0.05em;
    text-transform: uppercase; padding: 1px 5px; cursor: pointer;
  }
  .needs-you:hover { background: color-mix(in srgb, var(--yellow) 28%, transparent); }

  .cockpit-footer { margin-top: auto; padding: 10px 12px; border-top: 1px solid var(--bg-light); }

  .mini {
    background: var(--accent); color: var(--bg-dark);
    border: none; border-radius: 4px; padding: 3px 9px;
    font-size: 11px; font-weight: 600; cursor: pointer;
  }
  .mini:hover { background: var(--accent-hover); }
  .mini.ghost { background: none; color: var(--fg-dim); border: 1px solid var(--bg-light); }
  .mini.ghost:hover { color: var(--fg); border-color: var(--fg-dim); }
  .mini.ghost.danger:hover { color: var(--red); border-color: var(--red); }
</style>
