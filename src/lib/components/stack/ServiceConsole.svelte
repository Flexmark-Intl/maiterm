<script lang="ts">
  /** The service console drawer (docs/stack.md §7).
   *
   *  A service's tab is never in the tab strip and never a pane's active tab, so this is
   *  the only way to watch or type into one. It is a DRAWER, not a dock: it floats over
   *  the terminal area, absolutely positioned inside `.main-content`, so opening it
   *  changes neither the width nor the height of the tab underneath. That matters here
   *  beyond tidiness — a width change makes Claude Code re-render its whole transcript
   *  into scrollback (root CLAUDE.md), which is exactly what a side dock would cause on
   *  every open and close.
   *
   *  The terminal is not re-parented between panes: the flat TerminalPane in `+page`
   *  portals its container into the `data-terminal-slot` below, the same mechanism the
   *  mesh stage uses. Closing the drawer detaches it again; the PTY is untouched either
   *  way, which is the whole point — hiding a service must never stop it.
   */
  import { tick } from 'svelte';
  import { stackStore } from '$lib/stores/stack.svelte';
  import { preferencesStore } from '$lib/stores/preferences.svelte';
  import { error as logError } from '@tauri-apps/plugin-log';
  import StatusDot from '$lib/components/ui/StatusDot.svelte';
  import Tooltip from '$lib/components/Tooltip.svelte';

  interface Props {
    workspaceId: string;
  }

  let { workspaceId }: Props = $props();

  const services = $derived(stackStore.services(workspaceId));
  const openId = $derived(stackStore.consoleServiceId(workspaceId));
  const service = $derived(services.find((s) => s.id === openId) ?? null);
  const status = $derived(service ? stackStore.status(service.id) : 'stopped');
  const live = $derived(status === 'running' || status === 'ready' || status === 'starting');
  const tabId = $derived(stackStore.consoleTabId(workspaceId));
  const note = $derived(service ? stackStore.runtime(service.id).note : null);

  function dotColor(s: string): 'green' | 'yellow' | 'red' | 'dim' {
    switch (s) {
      case 'ready':
      case 'running': return 'green';
      case 'starting': return 'yellow';
      case 'crashed': return 'red';
      default: return 'dim';
    }
  }

  function run(label: string, p: Promise<unknown>) {
    p.catch((e) => logError(`stack console: ${label}: ${e}`));
  }

  // Re-home the terminal whenever the shown tab changes. Same contract as MeshStageView:
  // the portal matches on `data-terminal-slot`, and this event tells the pane to look again.
  $effect(() => {
    const id = tabId;
    if (!id) return;
    tick().then(() => {
      window.dispatchEvent(new CustomEvent('terminal-slot-ready', { detail: { tabId: id } }));
    });
  });

  // Escape closes. Capture phase would steal it from the terminal's own handlers; the
  // drawer is a peer of the tab underneath, not a modal, so it listens on the bubble.
  $effect(() => {
    if (!openId) return;
    const onKey = (e: KeyboardEvent) => {
      if (e.key !== 'Escape') return;
      // Not when the service's own terminal has the keyboard: Escape is a byte a shell or
      // TUI wants (leaving insert mode, dismissing a completion), and swallowing it to
      // close the drawer would make the console unusable for the thing it is for. The × ,
      // the sidebar row and a click on the work behind still dismiss it.
      const target = e.target as Element | null;
      if (target?.closest?.('.console-slot')) return;
      e.preventDefault();
      stackStore.closeConsole(workspaceId);
    };
    window.addEventListener('keydown', onKey);
    return () => window.removeEventListener('keydown', onKey);
  });

  /** Clicking the work behind the drawer dismisses it — it is an overlay, and reaching for
   *  the tab underneath IS the intent to put the service away. Scoped to the content area
   *  so the sidebar stays usable: clicking another service there swaps the drawer's
   *  contents rather than closing it. Capture phase, and never `preventDefault` — the
   *  click must still land, so the terminal takes focus on the same press. */
  $effect(() => {
    if (!openId) return;
    const onPointerDown = (e: PointerEvent) => {
      const target = e.target as HTMLElement | null;
      if (!target || !(target instanceof Element)) return;
      if (target.closest('.service-console')) return;
      if (!target.closest('.main-content')) return;
      stackStore.closeConsole(workspaceId);
    };
    window.addEventListener('pointerdown', onPointerDown, true);
    return () => window.removeEventListener('pointerdown', onPointerDown, true);
  });

  // ── Drag-resize from the top edge (TasksPanel.svelte does the same on its left edge) ──
  let dragging = false;
  let dragStartY = 0;
  let dragStartHeight = 0;
  let drawerEl = $state<HTMLElement | null>(null);

  function handleResizePointerDown(e: PointerEvent) {
    e.preventDefault();
    dragging = true;
    dragStartY = e.clientY;
    dragStartHeight = preferencesStore.stackConsoleHeight;
    (e.target as HTMLElement).setPointerCapture(e.pointerId);
  }

  function handleResizePointerMove(e: PointerEvent) {
    if (!dragging) return;
    const delta = dragStartY - e.clientY;
    const areaHeight = drawerEl?.parentElement?.clientHeight ?? window.innerHeight;
    const maxHeight = Math.floor(areaHeight * 0.9);
    void preferencesStore.setStackConsoleHeight(Math.max(120, Math.min(maxHeight, dragStartHeight + delta)));
  }

  function handleResizePointerUp() {
    dragging = false;
  }

  // Deliberately NOT focused on open. A peek should not take the keyboard away from the
  // tab you are working in — and with focus in the service's terminal, Escape belongs to
  // that terminal, so the drawer would lose its keyboard dismiss. Click into it to type.
</script>

{#if service}
  <section
    class="service-console"
    bind:this={drawerEl}
    style:height="{preferencesStore.stackConsoleHeight}px"
    aria-label="Service console"
  >
    <!-- svelte-ignore a11y_no_static_element_interactions -->
    <div
      class="resize-handle"
      onpointerdown={handleResizePointerDown}
      onpointermove={handleResizePointerMove}
      onpointerup={handleResizePointerUp}
    ></div>

    <header class="console-header">
      <div class="pills">
        {#each services as s (s.id)}
          {@const st = stackStore.status(s.id)}
          <button
            class="pill"
            class:on={s.id === service.id}
            onclick={() => stackStore.viewConsole(workspaceId, s.id)}
          >
            <StatusDot color={dotColor(st)} pulse={st === 'starting'} />
            <span class="pill-name">{s.name}</span>
          </button>
        {/each}
      </div>

      <div class="controls">
        {#if service.port || service.url}
          <!-- Last REPORTED, not observed: nothing sniffs sockets (docs/stack.md §9), so a
               service that moved ports this run still shows the old one until an agent or
               the ready trigger says otherwise. Say "last reported" rather than implying
               this is where it is listening now. -->
          <Tooltip text="Last reported endpoint">
            <span class="endpoint">{service.url ?? `:${service.port}`}</span>
          </Tooltip>
        {/if}
        {#if live}
          <Tooltip text="Restart {service.name}">
            <button class="ctl" onclick={() => run('restart', stackStore.restart(workspaceId, service.id))}>Restart</button>
          </Tooltip>
          <Tooltip text="Stop {service.name}">
            <button class="ctl" onclick={() => run('stop', stackStore.stop(workspaceId, service.id))}>Stop</button>
          </Tooltip>
        {:else}
          <Tooltip text="Start {service.name}">
            <button class="ctl primary" onclick={() => run('start', stackStore.start(workspaceId, service.id))}>Start</button>
          </Tooltip>
        {/if}
        <Tooltip text="Hide the console (Esc)">
          <button class="ctl close" onclick={() => stackStore.closeConsole(workspaceId)} aria-label="Hide the console">&times;</button>
        </Tooltip>
      </div>
    </header>

    {#if tabId}
      <!-- The portaled terminal fills this and takes its own focus on click. -->
      <div class="console-slot" data-terminal-slot={tabId}></div>
    {:else}
      <div class="console-empty">
        <p>{service.name} is {status}{note ? ` — ${note}` : ''}.</p>
        <button class="ctl primary" onclick={() => run('start', stackStore.start(workspaceId, service.id))}>Start it</button>
      </div>
    {/if}
  </section>
{/if}

<style>
  /* Absolute, not fixed: the drawer covers the terminal area only, so the sidebar stays
     live and you can click straight from one service to the next while it is open. */
  .service-console {
    position: absolute;
    left: 0;
    right: 0;
    bottom: 0;
    z-index: 40;
    display: flex;
    flex-direction: column;
    min-height: 120px;
    background: var(--bg-medium);
    border-top: 1px solid var(--bg-light);
    box-shadow: 0 -8px 24px rgba(0, 0, 0, 0.35);
    overflow: hidden;
  }

  .resize-handle {
    position: absolute;
    top: -3px;
    left: 0;
    right: 0;
    height: 6px;
    cursor: row-resize;
    z-index: 10;
  }
  .resize-handle:hover,
  .resize-handle:active {
    background: var(--accent);
    opacity: 0.3;
  }

  .console-header {
    display: flex;
    align-items: center;
    gap: 8px;
    padding: 4px 8px;
    background: var(--bg-dark);
    border-bottom: 1px solid var(--bg-light);
    flex-shrink: 0;
  }

  .pills {
    display: flex;
    align-items: center;
    gap: 4px;
    flex-wrap: wrap;
    min-width: 0;
  }

  .pill {
    display: flex;
    align-items: center;
    gap: 5px;
    padding: 3px 8px;
    border: 1px solid transparent;
    border-radius: 4px;
    background: transparent;
    color: var(--fg-dim);
    font-size: 11px;
    font-family: inherit;
    cursor: pointer;
    /* Real service names overflow a narrow window; let them wrap rather than clip two
       services down to the same visible string (root CLAUDE.md). */
    overflow-wrap: anywhere;
    text-align: left;
  }
  .pill:hover { background: var(--bg-medium); color: var(--fg); }
  .pill.on {
    background: var(--bg-medium);
    border-color: var(--bg-light);
    color: var(--fg);
  }

  .controls {
    display: flex;
    align-items: center;
    gap: 6px;
    margin-left: auto;
    flex-shrink: 0;
  }

  .endpoint {
    font-family: var(--font-mono, monospace);
    font-size: 11px;
    color: var(--fg-dim);
  }

  .ctl {
    padding: 3px 8px;
    border: 1px solid var(--bg-light);
    border-radius: 4px;
    background: transparent;
    color: var(--fg-dim);
    font-size: 11px;
    font-family: inherit;
    cursor: pointer;
  }
  .ctl:hover { color: var(--fg); border-color: var(--accent); }
  .ctl.primary { color: var(--accent); border-color: var(--accent); }
  .ctl.close {
    border-color: transparent;
    font-size: 15px;
    line-height: 1;
    padding: 1px 6px;
  }

  /* display:flex is load-bearing: the portaled TerminalPane container is `flex:1`, so the
     slot MUST be a flex container or the terminal hugs its initial fit and never grows
     (same note as .stage-slot in MeshStageView and .terminal-slot in SplitPane). */
  .console-slot {
    flex: 1;
    min-height: 0;
    min-width: 0;
    display: flex;
    position: relative;
    overflow: hidden;
  }

  .console-empty {
    flex: 1;
    display: flex;
    flex-direction: column;
    align-items: center;
    justify-content: center;
    gap: 10px;
    color: var(--fg-dim);
    font-size: 12px;
    padding: 0 24px;
    text-align: center;
  }
</style>
