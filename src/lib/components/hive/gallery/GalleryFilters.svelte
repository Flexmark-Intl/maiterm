<!--
  The gallery's filter row (docs/hive.md §10.4), under the kind tabs and search: Made by, Task,
  Status, Starred, Tags, Collection and the trash as compact controls on one wrapping row, then
  the filters in force as removable chips with Clear all. Every choice narrows the query Rust
  runs (`filterToQuery`); the tags offered are those on the assets listed now, most used first.
-->
<script lang="ts">
  import Icon from '$lib/components/Icon.svelte';
  import Tooltip from '$lib/components/Tooltip.svelte';
  import type { GalleryAssetRow, GalleryCollection, HiveWorkerCard } from '$lib/tauri/types';
  import { activeFilterChips, clearedQuery, STATUS_FILTERS, type GalleryQuery } from '$lib/hive/gallery';
  import GalleryFilterMenu from './GalleryFilterMenu.svelte';

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

  const byBadge = $derived([...workers].sort((a, b) => a.badge - b.badge));
  const madeByOptions = $derived([
    { key: '', label: 'Anyone' },
    { key: 'human', label: 'You' },
    { key: 'coordinator', label: 'Queen' },
    ...byBadge.map((w) => ({ key: `task:${w.task_id}`, label: `#${w.badge} ${w.title}` })),
  ]);
  const taskOptions = $derived([{ key: '', label: 'Any task' }, ...byBadge.map((w) => ({ key: w.task_id, label: w.title }))]);
  const statusOptions = STATUS_FILTERS.map((s) => ({ key: s.key, label: s.label }));
  const tagOptions = $derived.by(() => {
    const n = new Map<string, number>();
    for (const r of rows) for (const t of r.tags) n.set(t, (n.get(t) ?? 0) + 1);
    const list = [...n].sort((a, b) => b[1] - a[1] || a[0].localeCompare(b[0])).map(([key, count]) => ({ key, label: key, count }));
    // A tag picked stays offered, so it can be unpicked, even once nothing listed carries it.
    if (query.tag && !list.some((t) => t.key === query.tag)) list.unshift({ key: query.tag, label: query.tag, count: 0 });
    return [{ key: '', label: 'Any tag' }, ...list];
  });
  const collectionOptions = $derived([{ key: '', label: 'Any collection' }, ...collections.map((c) => ({ key: c.id, label: c.name, count: c.asset_ids.length }))]);

  const chips = $derived(
    activeFilterChips(query, {
      worker: (t) => {
        const w = workers.find((x) => x.task_id === t);
        return w ? `#${w.badge} ${w.title}` : undefined;
      },
      task: (t) => workers.find((x) => x.task_id === t)?.title,
      collection: (c) => collections.find((x) => x.id === c)?.name,
    }),
  );
  const noFocus = (e: MouseEvent) => e.preventDefault();
</script>

<div class="filters" role="group" aria-label="Filters">
  <GalleryFilterMenu label="Made by" options={madeByOptions} value={query.madeBy} active={!!query.madeBy} onpick={(k) => onchange({ madeBy: k })} />
  <GalleryFilterMenu label="Task" options={taskOptions} value={query.task} active={!!query.task} empty="No tasks yet." onpick={(k) => onchange({ task: k })} />
  <GalleryFilterMenu label="Status" options={statusOptions} value={query.status} active={!!query.status} onpick={(k) => onchange({ status: k })} />
  <button type="button" class="tog" class:on={query.starred} aria-pressed={query.starred} onmousedown={noFocus} onclick={() => onchange({ starred: !query.starred })}>
    <span class="star" aria-hidden="true">★</span> Starred
  </button>
  <GalleryFilterMenu label="Tags" options={tagOptions} value={query.tag} active={!!query.tag} empty="No tags yet." onpick={(k) => onchange({ tag: k })} />
  <GalleryFilterMenu
    label="Collection"
    options={collectionOptions}
    value={query.collection}
    active={!!query.collection}
    empty="None yet: you or the queen make them."
    footer={[{ label: 'Manage collections…', action: onmanage }]}
    onpick={(k) => onchange({ collection: k })}
  />
  <Tooltip text="Show what’s in the trash alongside the rest">
    <button type="button" class="tog" class:on={query.trash} aria-pressed={query.trash} onmousedown={noFocus} onclick={() => onchange({ trash: !query.trash })}>
      <Icon name="trash" size={10} /> Trash{#if trashed}<em>{trashed}</em>{/if}
    </button>
  </Tooltip>
  {#if query.trash && trashed}
    {#if confirmEmpty}
      <span class="confirm" role="group" aria-label="Empty the trash">
        Delete all {trashed} file{trashed === 1 ? '' : 's'} in the trash for good? Their records stay, so lineage holds.
        <button type="button" class="danger" onclick={() => { confirmEmpty = false; onemptytrash(); }}>Empty the trash</button>
        <button type="button" class="plain" onclick={() => (confirmEmpty = false)}>Keep</button>
      </span>
    {:else}
      <button type="button" class="plain" onclick={() => (confirmEmpty = true)}>Empty the trash…</button>
    {/if}
  {/if}

  {#if chips.length}
    <span class="sep" aria-hidden="true"></span>
    {#each chips as c (c.key)}
      <span class="chip">
        <span class="cl">{c.label}</span>
        <button type="button" class="x" aria-label="Remove the filter {c.label}" onmousedown={noFocus} onclick={() => onchange(c.clear)}><Icon name="close" size={9} /></button>
      </span>
    {/each}
    <button type="button" class="plain" onmousedown={noFocus} onclick={() => onchange(clearedQuery(query))}>Clear all</button>
  {/if}
</div>

<style>
  .filters { display: flex; flex-wrap: wrap; align-items: center; gap: 5px 6px; padding: 0 12px 8px; font-size: 11.5px; }
  .tog { display: inline-flex; align-items: center; gap: 5px; height: 24px; box-sizing: border-box; background: var(--bg-medium); border: 1px solid var(--bg-light); color: var(--fg-dim); border-radius: 5px; padding: 0 8px; font: inherit; font-size: 11.5px; cursor: pointer; white-space: nowrap; }
  .tog:hover { color: var(--fg); }
  .tog.on { color: var(--fg); border-color: color-mix(in srgb, var(--accent) 70%, var(--bg-light)); background: color-mix(in srgb, var(--accent) 12%, var(--bg-medium)); }
  .tog.on .star { color: var(--yellow); }
  .tog em { font-style: normal; color: var(--fg-dim); font-variant-numeric: tabular-nums; }
  .sep { width: 1px; height: 16px; background: var(--bg-light); margin: 0 4px; }
  .chip { display: inline-flex; align-items: center; gap: 2px; height: 22px; box-sizing: border-box; padding: 0 2px 0 8px; border-radius: 11px; background: color-mix(in srgb, var(--accent) 16%, var(--bg-dark)); color: var(--fg); max-width: 260px; }
  .cl { overflow: hidden; text-overflow: ellipsis; white-space: nowrap; min-width: 0; }
  .x { flex: none; display: inline-flex; align-items: center; justify-content: center; width: 18px; height: 18px; border-radius: 50%; border: none; background: none; color: var(--fg-dim); cursor: pointer; padding: 0; }
  .x:hover { background: color-mix(in srgb, var(--fg) 14%, transparent); color: var(--fg); }
  .plain { background: none; border: none; padding: 0 4px; color: var(--accent); font: inherit; font-size: 11.5px; cursor: pointer; }
  .confirm { display: inline-flex; flex-wrap: wrap; align-items: center; gap: 6px; color: var(--fg-dim); }
  .danger { background: none; border: 1px solid var(--red); color: var(--red); border-radius: 5px; padding: 1px 8px; font: inherit; font-size: 11.5px; cursor: pointer; }
  button:focus-visible { outline: 1px solid var(--accent); outline-offset: 1px; }
</style>
