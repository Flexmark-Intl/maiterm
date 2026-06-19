<script lang="ts">
  import { tick } from 'svelte';
  import { agentMeshStore } from '$lib/stores/agentMesh.svelte';
  import { claudeStateStore } from '$lib/stores/agentState.svelte';
  import StatusDot from '$lib/components/ui/StatusDot.svelte';

  interface Props { workspaceId: string; }
  let { workspaceId }: Props = $props();

  // Members of this mesh workspace (reactive on mesh + agent-state changes).
  const members = $derived.by(() => { void agentMeshStore.version; return agentMeshStore.rosterForWorkspace(workspaceId); });
  const slots = $derived.by(() => { void agentMeshStore.version; return agentMeshStore.stageSlots(workspaceId); });
  const filmstrip = $derived(members.filter((m) => m.tabId !== slots.left && m.tabId !== slots.right));

  const roleOf = $derived((tabId: string | null) => members.find((m) => m.tabId === tabId)?.role ?? '');

  // After the slot divs render (or recompose), tell the flat TerminalPanes to (re)attach.
  // The portal matches data-terminal-slot={tabId}; firing terminal-slot-ready re-homes a
  // terminal whenever its slot appears or moves (stage ⇄ filmstrip).
  $effect(() => {
    const ids = [slots.left, slots.right, ...filmstrip.map((m) => m.tabId)].filter(Boolean) as string[];
    tick().then(() => {
      for (const id of ids) window.dispatchEvent(new CustomEvent('terminal-slot-ready', { detail: { tabId: id } }));
    });
  });

  // Measure the stage panel so filmstrip tiles render the terminal at the SAME pixel size as
  // the stage and just CSS-scale it down. A transform doesn't change clientWidth, so a scaled
  // tile still fits to the stage column count — promoting/demoting never reflows the terminal.
  let rowEl = $state<HTMLDivElement | undefined>();
  let rowW = $state(0);
  let rowH = $state(0);
  $effect(() => {
    const el = rowEl;
    if (!el) return;
    const measure = () => { rowW = el.clientWidth; rowH = el.clientHeight; };
    measure();
    const ro = new ResizeObserver(measure);
    ro.observe(el);
    return () => ro.disconnect();
  });
  const PANEL_LABEL_H = 28; // the slot is the panel height minus its label bar
  const panelW = $derived(Math.max(240, Math.floor((rowW - 1) / 2)));
  const panelH = $derived(Math.max(160, rowH - PANEL_LABEL_H));
  // Scale to the filmstrip HEIGHT (panels are tall, so a height-based thumbnail fits the strip
  // row; a width-based one would overflow vertically). Width follows from the panel aspect.
  const TILE_H = 116;
  const tileScale = $derived(TILE_H / panelH);
  const tileW = $derived(Math.max(56, Math.round(panelW * tileScale)));

  function promote(tabId: string, e: MouseEvent) {
    agentMeshStore.promoteToStage(workspaceId, tabId, e.shiftKey ? 'right' : 'left');
  }
  function exit() {
    agentMeshStore.toggleStageView(workspaceId);
  }
  function dotColor(tabId: string | null): 'accent' | 'green' | 'dim' {
    if (!tabId) return 'dim';
    const cs = claudeStateStore.getState(tabId);
    return cs?.state === 'active' ? 'accent' : cs ? 'green' : 'dim';
  }
</script>

<div class="stage-view">
  <div class="stage-row" bind:this={rowEl}>
    {#each (['left', 'right'] as const) as side}
      {@const tabId = slots[side]}
      <div class="stage-panel">
        <div class="panel-label">
          <StatusDot color={dotColor(tabId)} pulse={dotColor(tabId) === 'accent'} />
          <span>{tabId ? roleOf(tabId) : `${side} — click a tile below`}</span>
          <span class="side-tag">{side}</span>
        </div>
        <div class="stage-slot" data-terminal-slot={tabId ?? `__mesh_empty_${side}`}>
          {#if !tabId}<div class="slot-empty">Click a filmstrip tile to stage it{side === 'right' ? ' (Shift+click for the right panel)' : ''}.</div>{/if}
        </div>
      </div>
    {/each}
  </div>

  <div class="filmstrip">
    <button class="exit-btn" onclick={exit} title="Switch back to the normal tab/split layout">⊟ Tab view</button>
    {#if filmstrip.length === 0}
      <div class="strip-empty">{members.length === 0 ? 'No named agents in this mesh yet — name an agent tab to add it.' : 'All agents are on stage.'}</div>
    {/if}
    {#each filmstrip as m (m.tabId)}
      <button
        class="tile"
        style="width: {tileW}px; height: {TILE_H}px;"
        onclick={(e) => promote(m.tabId, e)}
        title="Click → left panel · Shift+click → right panel"
      >
        <!-- Inner is the full stage panel size; the terminal fits to it (clientWidth ignores
             the scale), then we visually shrink it to the tile with transform: scale. -->
        <div class="tile-term" data-terminal-slot={m.tabId} style="width: {panelW}px; height: {panelH}px; transform: scale({tileScale});"></div>
        <div class="tile-overlay">
          <StatusDot color={dotColor(m.tabId)} pulse={dotColor(m.tabId) === 'accent'} />
          <span class="tile-role">{m.role}</span>
        </div>
      </button>
    {/each}
  </div>
</div>

<style>
  /* width:100% + height:100% to fill the flex-row .main-content, exactly like SplitContainer.
     Without width:100% it collapses to content width in the flex row. */
  .stage-view { display: flex; flex-direction: column; width: 100%; height: 100%; min-height: 0; background: var(--bg-dark); overflow: hidden; }
  .stage-row { flex: 1; display: flex; min-height: 0; min-width: 0; gap: 1px; overflow: hidden; }
  .stage-panel { flex: 1; min-width: 0; display: flex; flex-direction: column; min-height: 0; background: var(--bg-dark); overflow: hidden; }
  .panel-label {
    display: flex; align-items: center; gap: 6px;
    padding: 4px 10px; font-size: 11px; color: var(--fg);
    background: var(--bg-medium); border-bottom: 1px solid var(--bg-light);
  }
  .panel-label .side-tag { margin-left: auto; font-size: 9px; text-transform: uppercase; letter-spacing: 0.06em; color: var(--fg-dim); }
  /* display:flex is load-bearing: the portaled TerminalPane container is `flex:1`, so the
     slot MUST be a flex container for it to fill the panel height (matches .terminal-slot in
     SplitPane). Without it the terminal hugs its initial 2-row fit and never grows. */
  .stage-slot { flex: 1; min-height: 0; min-width: 0; position: relative; display: flex; overflow: hidden; }
  .slot-empty {
    position: absolute; inset: 0; display: flex; align-items: center; justify-content: center;
    color: var(--fg-dim); font-size: 12px; text-align: center; padding: 0 24px;
  }

  .filmstrip {
    display: flex; align-items: center; gap: 8px;
    height: 150px; padding: 8px 10px; overflow-x: auto; overflow-y: hidden;
    background: var(--bg-medium); border-top: 1px solid var(--bg-light);
  }
  .exit-btn {
    flex-shrink: 0; align-self: center;
    background: var(--bg-dark); border: 1px solid var(--bg-light); border-radius: 4px;
    color: var(--fg); font-size: 11px; font-weight: 500; padding: 6px 10px; cursor: pointer;
    white-space: nowrap;
  }
  .exit-btn:hover { color: var(--accent); border-color: var(--accent); }
  .strip-empty { color: var(--fg-dim); font-size: 12px; padding-left: 8px; }

  .tile {
    flex-shrink: 0; position: relative;
    padding: 0;
    border: 1px solid var(--bg-light); border-radius: 5px; overflow: hidden;
    background: var(--bg-dark); cursor: pointer;
  }
  .tile:hover { border-color: var(--accent); }
  /* The portaled terminal renders live at the full stage-panel size, then transform:scale
     shrinks it to a thumbnail (set inline). display:flex lets the flex:1 terminal container
     fill it. Its job is "what's it doing / what color is the dot," not legibility (§7.4). */
  .tile-term {
    position: absolute; top: 0; left: 0;
    transform-origin: top left;
    display: flex;
    overflow: hidden;
    pointer-events: none;
  }
  .tile-overlay {
    position: absolute; left: 0; right: 0; bottom: 0;
    display: flex; align-items: center; gap: 5px;
    padding: 3px 6px; font-size: 10px; color: var(--fg);
    background: linear-gradient(transparent, rgba(0,0,0,0.75));
  }
  .tile-role { white-space: nowrap; overflow: hidden; text-overflow: ellipsis; }
</style>
