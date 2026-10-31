<!--
  The detected tools (hiveDetectTools: MCP servers, skills, programs, ComfyUI) grouped by kind,
  with a filter box and a per-group all/none toggle. Shared by the Make this a Hive wizard's Tools
  step and Preferences → Hive (docs/hive.md §3). A skill pack (gstack's 50-odd skills) is one row
  whose checkbox picks or clears every member, with its members behind a disclosure arrow; what
  is picked is still each member's id. Grouping and filtering are `lib/hive/defaults.ts`.
-->
<script lang="ts">
  import type { HiveDetectedTool } from '$lib/tauri/types';
  import { groupTools, toggleGroup, togglePack } from '$lib/hive/defaults';

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
  /** Packs the human opened. A filter matching a member opens its pack on its own. */
  let opened = $state(new Set<string>());

  function flip(id: string) {
    const next = new Set(picked);
    if (next.has(id)) next.delete(id);
    else next.add(id);
    onchange(next);
  }
  function flipOpen(name: string) {
    const next = new Set(opened);
    if (next.has(name)) next.delete(name);
    else next.add(name);
    opened = next;
  }
  /** A checkbox's mixed state is a DOM property, not an attribute. */
  function indeterminate(el: HTMLInputElement, on: boolean) {
    el.indeterminate = on;
    return { update(v: boolean) { el.indeterminate = v; } };
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
        {#each g.entries as e (e.type === 'tool' ? e.tool.id : `pack:${e.name}`)}
          {#if e.type === 'tool'}
            <label class="row">
              <input type="checkbox" checked={picked.has(e.tool.id)} onchange={() => flip(e.tool.id)} />
              <span class="name">{e.tool.label}</span>
              {#if e.tool.host}<span class="host">{e.tool.host}</span>{/if}
            </label>
          {:else}
            {@const n = e.members.filter((t) => picked.has(t.id)).length}
            {@const isOpen = e.open || opened.has(e.name)}
            <div class="pack">
              <div class="pack-head">
                <button type="button" class="arrow" class:open={isOpen} aria-expanded={isOpen} aria-label="{isOpen ? 'Hide' : 'Show'} the skills in {e.name}" onclick={() => flipOpen(e.name)}>
                  <svg viewBox="0 0 12 12" width="10" height="10" aria-hidden="true"><path d="M4 2.5l3.5 3.5L4 9.5" fill="none" stroke="currentColor" stroke-width="1.6" stroke-linecap="round" stroke-linejoin="round" /></svg>
                </button>
                <label class="row">
                  <input type="checkbox" checked={n === e.members.length} use:indeterminate={n > 0 && n < e.members.length} onchange={() => onchange(togglePack(picked, e.members))} />
                  <span class="name"><b>{e.name}</b> — {e.members.length} skills</span>
                  {#if n > 0 && n < e.members.length}<span class="host">{n} picked</span>{/if}
                </label>
              </div>
              {#if isOpen}
                <div class="members">
                  {#each e.shown as t (t.id)}
                    <label class="row">
                      <input type="checkbox" checked={picked.has(t.id)} onchange={() => flip(t.id)} />
                      <span class="name">{t.label}</span>
                    </label>
                  {/each}
                </div>
              {/if}
            </div>
          {/if}
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
  .name b { font-weight: 600; }
  .host { font-size: 0.808rem; color: var(--fg-dim); overflow-wrap: anywhere; }
  /* A pack spans the whole row: its members list is wide. */
  .pack { grid-column: 1 / -1; display: flex; flex-direction: column; gap: 2px; }
  .pack-head { display: flex; align-items: center; gap: 2px; }
  .arrow { display: grid; place-items: center; width: 18px; height: 18px; padding: 0; background: none; border: none; border-radius: 3px; color: var(--fg-dim); cursor: pointer; transition: transform 0.15s; }
  .arrow:hover { color: var(--fg); background: var(--bg-light); }
  .arrow:focus-visible { outline: 2px solid var(--accent); outline-offset: 1px; }
  .arrow.open { transform: rotate(90deg); }
  .members { display: grid; grid-template-columns: repeat(auto-fill, minmax(170px, 1fr)); gap: 2px 12px; margin-left: 20px; padding-left: 10px; border-left: 1px solid var(--bg-light); }
  @media (prefers-reduced-motion: reduce) {
    .arrow { transition: none; }
  }
</style>
