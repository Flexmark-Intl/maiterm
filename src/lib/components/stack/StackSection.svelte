<script lang="ts">
  /** The Stack section under a workspace row in the sidebar (docs/stack.md §7): one line
   *  per service — status dot, name, port, uptime — click to show it in the console
   *  drawer (click again to hide), right-click for the verbs. The rows wrap rather than clip: real service names and ports share prefixes
   *  and the dock is narrow (CLAUDE.md on `white-space: nowrap` in the side docks). */
  import type { Service, Workspace } from '$lib/tauri/types';
  import { stackStore, type ServiceInput } from '$lib/stores/stack.svelte';
  import { workspacesStore } from '$lib/stores/workspaces.svelte';
  import { error as logError } from '@tauri-apps/plugin-log';
  import StatusDot from '$lib/components/ui/StatusDot.svelte';
  import ContextMenu from '$lib/components/ContextMenu.svelte';
  import ServiceModal from './ServiceModal.svelte';
  import ImportServicesModal from './ImportServicesModal.svelte';
  import type { StackSuggestion } from '$lib/tauri/commands';
  import { untrack } from 'svelte';

  interface Props {
    workspace: Workspace;
    expanded: boolean;
    ontoggle: () => void;
    /** An add/import modal closed (saved or cancelled) — the sidebar uses it to drop the
     *  section it mounted for an empty stack if nothing was added. */
    onsettled?: () => void;
  }

  let { workspace, expanded, ontoggle, onsettled }: Props = $props();

  const services = $derived(workspace.stack ?? []);

  let menu = $state<{ x: number; y: number; service: Service } | null>(null);
  let editing = $state<Service | null>(null);
  let adding = $state(false);
  let importing = $state(false);

  /** Re-render uptime once a second while anything is running. */
  let now = $state(Date.now());
  $effect(() => {
    const anyLive = services.some((s) => { const st = stackStore.status(s.id); return st === 'running' || st === 'ready' || st === 'starting'; });
    if (!anyLive) return;
    const id = setInterval(() => { now = Date.now(); }, 1000);
    return () => clearInterval(id);
  });

  function uptime(since: number | null): string {
    if (!since) return '';
    const s = Math.max(0, Math.floor((now - since) / 1000));
    if (s < 60) return `${s}s`;
    const m = Math.floor(s / 60);
    if (m < 60) return `${m}m`;
    const h = Math.floor(m / 60);
    return `${h}h${m % 60 ? ` ${m % 60}m` : ''}`;
  }

  function dotColor(status: string): 'green' | 'yellow' | 'red' | 'dim' {
    switch (status) {
      case 'ready': return 'green';
      case 'running': return 'green';
      case 'starting': return 'yellow';
      case 'crashed': return 'red';
      default: return 'dim';
    }
  }

  /** The cwd a new service defaults to: the workspace's active tab's directory. */
  function defaultCwd(): string {
    const pane = workspace.panes.find((p) => p.id === workspace.active_pane_id) ?? workspace.panes[0];
    const tab = pane?.tabs.find((t) => t.id === pane.active_tab_id) ?? pane?.tabs[0];
    return tab?.last_cwd ?? tab?.restore_cwd ?? services[0]?.cwd ?? '';
  }

  function run(label: string, p: Promise<unknown>) {
    p.catch((e) => logError(`stack: ${label}: ${e}`));
  }

  /** Show a service in the console drawer (docs/stack.md §7). Clicking the row that is
   *  already showing hides it again — the drawer is a peek, not a place you have to
   *  navigate out of. A stopped service opens the drawer too, on its Start button, rather
   *  than starting behind the human's back. */
  function openService(service: Service) {
    if (workspace.id !== workspacesStore.activeWorkspaceId) {
      void workspacesStore.setActiveWorkspace(workspace.id);
    }
    stackStore.toggleConsole(workspace.id, service.id);
  }

  function menuItems(service: Service) {
    const st = stackStore.status(service.id);
    const live = st === 'running' || st === 'ready' || st === 'starting';
    return [
      { label: live ? 'Restart' : 'Start', action: () => run('start', live ? stackStore.restart(workspace.id, service.id) : stackStore.start(workspace.id, service.id)) },
      { label: 'Stop', disabled: !live, action: () => run('stop', stackStore.stop(workspace.id, service.id)) },
      { label: stackStore.consoleServiceId(workspace.id) === service.id ? 'Hide console' : 'Show console', action: () => openService(service) },
      { label: '', separator: true, action: () => {} },
      { label: 'Edit…', action: () => { editing = service; } },
      { label: 'Remove', disabled: live, action: () => run('remove', stackStore.removeService(workspace.id, service.id)) },
    ];
  }

  async function submit(input: ServiceInput) {
    const target = untrack(() => editing);
    try {
      if (target) await stackStore.updateService(workspace.id, target.id, input);
      else await stackStore.createService(workspace.id, input);
    } catch (e) {
      logError(`stack: save service: ${e}`);
    }
    editing = null;
    adding = false;
    onsettled?.();
  }

  function cancelModal() {
    editing = null;
    adding = false;
    importing = false;
    onsettled?.();
  }

  export function openAdd() { adding = true; }
  export function openImport() { importing = true; }

  async function importRows(rows: StackSuggestion[]) {
    importing = false;
    for (const r of rows) {
      try {
        await stackStore.createService(workspace.id, { name: r.name, command: r.command, cwd: r.cwd, auto_start: r.recommended, origin: 'suggested' });
      } catch (e) {
        logError(`stack: import ${r.name}: ${e}`);
      }
    }
    onsettled?.();
  }
</script>

<div class="stack-section">
  <button type="button" class="stack-head" onclick={ontoggle} aria-expanded={expanded}>
    <span class="chev" class:open={expanded}>›</span>
    <span class="stack-title">Stack</span>
    <span class="stack-count">{services.length}</span>
  </button>

  {#if expanded}
    <div class="stack-rows">
      {#each services as service (service.id)}
        {@const r = stackStore.runtime(service.id)}
        <div
          class="svc"
          class:crashed={r.status === 'crashed'}
          role="button"
          tabindex="0"
          onclick={(e) => { e.stopPropagation(); openService(service); }}
          onkeydown={(e) => { if (e.key === 'Enter') openService(service); }}
          oncontextmenu={(e) => { e.preventDefault(); e.stopPropagation(); menu = { x: e.clientX, y: e.clientY, service }; }}
        >
          <StatusDot color={dotColor(r.status)} pulse={r.status === 'starting'} tooltip={r.note ?? r.status} />
          <span class="svc-name">{service.name}</span>
          {#if service.port}<span class="svc-port">:{service.port}</span>{/if}
          <span class="svc-meta">
            {#if r.status === 'crashed'}crashed{:else if r.status === 'starting'}starting{:else if r.status === 'stopped'}{r.note ? 'held' : ''}{:else}{uptime(r.since)}{/if}
          </span>
        </div>
      {/each}
      <button type="button" class="svc add" onclick={(e) => { e.stopPropagation(); adding = true; }}>
        <span class="plus">+</span><span class="svc-name">Add service…</span>
      </button>
      {#if services.length === 0}
        <button type="button" class="svc add" onclick={(e) => { e.stopPropagation(); importing = true; }}>
          <span class="plus">↓</span><span class="svc-name">Import from project…</span>
        </button>
      {/if}
    </div>
  {/if}
</div>

{#if importing}
  <ImportServicesModal cwd={defaultCwd()} existingNames={services.map((s) => s.name)} onsubmit={importRows} oncancel={cancelModal} />
{/if}

{#if menu}
  <ContextMenu items={menuItems(menu.service)} x={menu.x} y={menu.y} onclose={() => (menu = null)} />
{/if}

{#if editing || adding}
  <ServiceModal service={editing} defaultCwd={defaultCwd()} onsubmit={submit} oncancel={cancelModal} />
{/if}

<style>
  .stack-section {
    padding: 0 8px 4px 24px;
  }

  .stack-head {
    align-items: center;
    background: none;
    border: none;
    color: var(--fg-dim);
    cursor: pointer;
    display: flex;
    font-family: inherit;
    font-size: 0.7rem;
    font-weight: 600;
    gap: 5px;
    letter-spacing: 0.06em;
    padding: 3px 4px;
    text-transform: uppercase;
    width: 100%;
  }
  .stack-head:hover { color: var(--fg); }

  .chev {
    display: inline-block;
    transition: transform 0.12s;
    width: 8px;
  }
  .chev.open { transform: rotate(90deg); }

  .stack-count {
    background: var(--bg-light);
    border-radius: 8px;
    font-size: 0.65rem;
    padding: 0 5px;
  }

  .stack-rows { display: flex; flex-direction: column; }

  .svc {
    align-items: center;
    background: none;
    border: none;
    border-radius: 4px;
    color: var(--fg);
    cursor: pointer;
    display: flex;
    font-family: inherit;
    font-size: 0.85rem;
    gap: 7px;
    padding: 3px 6px;
    text-align: left;
    width: 100%;
  }
  .svc:hover { background: var(--bg-light); }
  .svc.crashed .svc-name { color: var(--red); }

  .svc-name {
    flex: 1;
    min-width: 0;
    overflow-wrap: anywhere;
  }
  .svc-port {
    color: var(--fg-dim);
    font-family: var(--font-mono, ui-monospace, monospace);
    font-size: 0.75rem;
  }
  .svc-meta {
    color: var(--fg-dim);
    flex-shrink: 0;
    font-size: 0.7rem;
  }

  .svc.add { color: var(--fg-dim); }
  .svc.add:hover { color: var(--fg); }
  .plus { width: 6px; text-align: center; }
</style>
