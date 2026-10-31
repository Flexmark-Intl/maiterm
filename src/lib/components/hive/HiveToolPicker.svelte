<!--
  The detected tools (hiveDetectTools: MCP servers, skills, programs, ComfyUI) grouped by kind,
  with a filter box and a per-group all/none toggle. Shared by the Make this a Hive wizard's Tools
  step and Preferences → Hive (docs/hive.md §3). Grouping and filtering are `lib/hive/defaults.ts`.
-->
<script lang="ts">
  import type { HiveDetectedTool } from '$lib/tauri/types';
  import { groupTools, toggleGroup } from '$lib/hive/defaults';

  interface Props {
    tools: HiveDetectedTool[];
    picked: ReadonlySet<string>;
    onchange: (next: Set<string>) => void;
  }
  let { tools, picked, onchange }: Props = $props();

  let query = $state('');
  const groups = $derived(groupTools(tools, query));
  /** The filter box earns its place only once the list is long. */
  const showFilter = $derived(tools.length > 6);

  function flip(id: string) {
    const next = new Set(picked);
    if (next.has(id)) next.delete(id);
    else next.add(id);
    onchange(next);
  }
</script>

<div class="picker">
  {#if showFilter}
    <input class="filter" type="search" placeholder="Filter tools" aria-label="Filter tools" bind:value={query} />
  {/if}
  {#if !groups.length}
    <p class="empty">No tool matches “{query.trim()}”.</p>
  {/if}
  {#each groups as g (g.kind)}
    {@const all = g.tools.every((t) => picked.has(t.id))}
    {@const count = g.tools.filter((t) => picked.has(t.id)).length}
    <section class="group">
      <div class="group-head">
        <span class="group-label">{g.label}</span>
        <span class="group-count">{count} of {g.tools.length}</span>
        <button type="button" class="link" onclick={() => onchange(toggleGroup(picked, g))}>
          {all ? 'Clear all' : 'Select all'}
        </button>
      </div>
      <div class="rows">
        {#each g.tools as t (t.id)}
          <label class="row">
            <input type="checkbox" checked={picked.has(t.id)} onchange={() => flip(t.id)} />
            <span class="name">{t.label}</span>
            {#if t.host}<span class="host">{t.host}</span>{/if}
          </label>
        {/each}
      </div>
    </section>
  {/each}
</div>

<style>
  .picker { display: flex; flex-direction: column; gap: 10px; min-width: 0; }
  .filter {
    background: var(--bg-dark);
    border: 1px solid var(--bg-light);
    border-radius: 6px;
    color: var(--fg);
    font: inherit;
    font-size: 0.923rem;
    padding: 6px 8px;
    min-width: 0;
  }
  .filter:focus { outline: none; border-color: var(--accent); }
  .empty { margin: 0; font-size: 0.846rem; color: var(--fg-dim); overflow-wrap: anywhere; }
  .group { display: flex; flex-direction: column; gap: 4px; min-width: 0; }
  .group-head {
    display: flex;
    align-items: baseline;
    gap: 8px;
    flex-wrap: wrap;
    padding-bottom: 3px;
    border-bottom: 1px solid var(--bg-light);
  }
  .group-label { font-size: 0.846rem; font-weight: 600; color: var(--fg); }
  .group-count { font-size: 0.808rem; color: var(--fg-dim); font-variant-numeric: tabular-nums; }
  .link { margin-left: auto; background: none; border: none; padding: 0; color: var(--accent); cursor: pointer; font: inherit; font-size: 0.846rem; }
  .link:hover { text-decoration: underline; }
  .link:focus-visible { outline: 2px solid var(--accent); outline-offset: 2px; border-radius: 3px; }
  .rows { display: grid; grid-template-columns: repeat(auto-fill, minmax(180px, 1fr)); gap: 2px 12px; }
  .row { display: flex; align-items: baseline; gap: 6px; font-size: 0.923rem; color: var(--fg); cursor: pointer; min-width: 0; padding: 2px 0; flex-wrap: wrap; }
  .row input { accent-color: var(--accent); align-self: center; }
  .name { overflow-wrap: anywhere; min-width: 0; }
  .host { font-size: 0.808rem; color: var(--fg-dim); overflow-wrap: anywhere; }
</style>
