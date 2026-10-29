<!--
  The gallery's filter rail (docs/hive.md §10.4): Made by, Task, Status, Starred, Tags and
  Collections, and the trash. Every choice narrows the query Rust runs (`filterToQuery`); the
  tags offered are those on the assets listed now, most used first.
-->
<script lang="ts">
  import type { GalleryAssetRow, GalleryCollection, HiveWorkerCard } from '$lib/tauri/types';
  import type { GalleryQuery } from '$lib/hive/gallery';

  interface Props {
    query: GalleryQuery;
    workers: HiveWorkerCard[];
    rows: GalleryAssetRow[];
    collections: GalleryCollection[];
    /** The whole trash, whatever the filters: emptying it takes all of it. */
    trashed: number;
    onchange: (patch: Partial<GalleryQuery>) => void;
    /** Opens the collections panel. */
    onmanage: () => void;
    /** Empties the trash, once the human confirmed here. */
    onemptytrash: () => void;
  }
  let { query, workers, rows, collections, trashed, onchange, onmanage, onemptytrash }: Props = $props();

  let confirmEmpty = $state(false);
  $effect(() => {
    if (!query.trash) confirmEmpty = false;
  });

  const STATUSES = [
    { key: '', label: 'Any' },
    { key: 'picked', label: 'Picked' },
    { key: 'final', label: 'Final' },
    { key: 'rejected', label: 'Rejected' },
    { key: 'none', label: 'No status' },
  ];
  const TAGS_SHOWN = 30;
  const tags = $derived.by(() => {
    const n = new Map<string, number>();
    for (const r of rows) for (const t of r.tags) n.set(t, (n.get(t) ?? 0) + 1);
    const list = [...n].sort((a, b) => b[1] - a[1] || a[0].localeCompare(b[0])).slice(0, TAGS_SHOWN);
    // A tag picked stays offered, so it can be unpicked, even once nothing listed carries it.
    if (query.tag && !list.some(([t]) => t === query.tag)) list.unshift([query.tag, 0]);
    return list;
  });
  const byBadge = $derived([...workers].sort((a, b) => a.badge - b.badge));
</script>

<aside class="rail" aria-label="Filters">
  <section>
    <h4>Made by</h4>
    <select value={query.madeBy} onchange={(e) => onchange({ madeBy: (e.currentTarget as HTMLSelectElement).value })} aria-label="Made by">
      <option value="">Anyone</option>
      <option value="human">You</option>
      <option value="coordinator">Queen</option>
      {#each byBadge as w (w.task_id)}
        <option value="task:{w.task_id}">#{w.badge} {w.title}</option>
      {/each}
    </select>
  </section>
  <section>
    <h4>Task</h4>
    <select value={query.task} onchange={(e) => onchange({ task: (e.currentTarget as HTMLSelectElement).value })} aria-label="Task">
      <option value="">Any task</option>
      {#each byBadge as w (w.task_id)}
        <option value={w.task_id}>{w.title}</option>
      {/each}
    </select>
  </section>
  <section>
    <h4>Status</h4>
    <div class="chips" role="radiogroup" aria-label="Status">
      {#each STATUSES as s (s.key)}
        <button type="button" role="radio" aria-checked={query.status === s.key} class:on={query.status === s.key} onclick={() => onchange({ status: s.key })}>{s.label}</button>
      {/each}
    </div>
  </section>
  <section>
    <label class="check"><input type="checkbox" checked={query.starred} onchange={(e) => onchange({ starred: (e.currentTarget as HTMLInputElement).checked })} /> Starred only</label>
  </section>
  <section>
    <h4>Tags</h4>
    {#if tags.length}
      <div class="chips">
        {#each tags as [t, n] (t)}
          <button type="button" aria-pressed={query.tag === t} class:on={query.tag === t} onclick={() => onchange({ tag: query.tag === t ? '' : t })}>{t}{#if n}<em>{n}</em>{/if}</button>
        {/each}
      </div>
    {:else}
      <p class="none">No tags yet.</p>
    {/if}
  </section>
  <section>
    <h4>Collections</h4>
    {#if collections.length}
      <div class="list">
        {#each collections as c (c.id)}
          <button type="button" aria-pressed={query.collection === c.id} class:on={query.collection === c.id} onclick={() => onchange({ collection: query.collection === c.id ? '' : c.id })}>
            <span>{c.name}</span><em>{c.asset_ids.length}</em>
          </button>
        {/each}
      </div>
    {:else}
      <p class="none">None yet: you or the queen make them.</p>
    {/if}
    <button type="button" class="manage" onclick={onmanage}>Manage collections…</button>
  </section>
  <section>
    <label class="check"><input type="checkbox" checked={query.trash} onchange={(e) => onchange({ trash: (e.currentTarget as HTMLInputElement).checked })} /> Show the trash</label>
    {#if query.trash && trashed}
      {#if confirmEmpty}
        <p class="none">Delete all {trashed} file{trashed === 1 ? '' : 's'} in the trash for good, whatever the filters show? Their records stay, so lineage holds.</p>
        <div class="chips">
          <button type="button" class="danger" onclick={() => { confirmEmpty = false; onemptytrash(); }}>Empty the trash</button>
          <button type="button" onclick={() => (confirmEmpty = false)}>Keep</button>
        </div>
      {:else}
        <button type="button" class="manage" onclick={() => (confirmEmpty = true)}>Empty the trash ({trashed})…</button>
      {/if}
    {/if}
  </section>
</aside>

<style>
  .rail { flex: none; width: 176px; overflow-y: auto; padding: 10px 12px; border-right: 1px solid var(--bg-light); display: flex; flex-direction: column; gap: 12px; font-size: 11.5px; }
  h4 { margin: 0 0 4px; font-size: 10px; font-weight: 500; letter-spacing: 0.08em; text-transform: uppercase; color: var(--fg-dim); }
  select { width: 100%; background: var(--bg-medium); color: var(--fg); border: 1px solid var(--bg-light); border-radius: 5px; padding: 3px 4px; font: inherit; font-size: 11.5px; }
  .chips { display: flex; flex-wrap: wrap; gap: 4px; }
  .chips button, .list button { background: var(--bg-medium); border: 1px solid var(--bg-light); color: var(--fg-dim); border-radius: 10px; padding: 1px 7px; font: inherit; font-size: 11px; cursor: pointer; overflow-wrap: anywhere; text-align: left; }
  .list { display: flex; flex-direction: column; gap: 3px; }
  .list button { border-radius: 5px; display: flex; justify-content: space-between; gap: 6px; }
  .chips button.on, .list button.on { color: var(--fg); border-color: var(--accent); }
  .chips button:hover, .list button:hover { color: var(--fg); }
  em { font-style: normal; opacity: 0.6; margin-left: 4px; font-variant-numeric: tabular-nums; }
  .check { display: flex; align-items: center; gap: 6px; color: var(--fg); cursor: pointer; }
  .none { margin: 0; color: var(--fg-dim); font-size: 11px; }
  .manage { margin-top: 6px; background: none; border: none; padding: 0; color: var(--accent); font: inherit; font-size: 11px; cursor: pointer; text-align: left; }
  .chips button.danger { color: var(--red); border-color: var(--red); }
  button:focus-visible, select:focus-visible { outline: 1px solid var(--accent); outline-offset: 1px; }
</style>
