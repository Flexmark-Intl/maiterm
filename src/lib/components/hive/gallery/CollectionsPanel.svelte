<!--
  The gallery's collections (docs/hive.md §10.4, step 8e), in the gallery's area: the list, and
  the chosen one's members in order. Rename, delete (the grouping only: its assets stay), a new
  one, drag members to reorder (pointer events, never `draggable`: no HTML5 drop reaches a Tauri
  webview, OverlordBoardView), set the cover, remove a member, and show it in the grid.
-->
<script lang="ts">
  import { untrack } from 'svelte';
  import { error as logError } from '@tauri-apps/plugin-log';
  import Icon from '$lib/components/Icon.svelte';
  import Tooltip from '$lib/components/Tooltip.svelte';
  import { galleryCollect, galleryDeleteCollection, galleryList, galleryRenameCollection } from '$lib/tauri/commands';
  import type { GalleryAssetRow, GalleryCollection } from '$lib/tauri/types';
  import { galleryUrl, picture } from '$lib/hive/gallery';
  import { DRAG_THRESHOLD } from '$lib/hive/galleryCurate';

  interface Props {
    workspaceId: string;
    hiveId: string;
    collections: GalleryCollection[];
    /** Moves on every gallery change: the members are read again. */
    version: number;
    onback: () => void;
    onshow: (collectionId: string) => void;
    onchanged: () => void;
    /** Export… of the whole collection, in its order. */
    onexport: (collectionId: string) => void;
  }
  let { workspaceId, hiveId, collections, version, onback, onshow, onchanged, onexport }: Props = $props();

  let chosen = $state<string | null>(untrack(() => collections[0]?.id ?? null));
  const current = $derived(collections.find((c) => c.id === chosen) ?? null);
  $effect(() => {
    if (!current && collections.length) chosen = collections[0].id;
  });

  // The members as the gallery lists them (out of the trash), in the collection's order.
  let members = $state<GalleryAssetRow[]>([]);
  $effect(() => {
    const c = current;
    const ws = workspaceId;
    void version;
    if (!c) {
      members = [];
      return;
    }
    let alive = true;
    galleryList(ws, { collection: c.id })
      .then((rows) => {
        if (!alive) return;
        const by = new Map(rows.map((r) => [r.id, r]));
        members = c.asset_ids.map((id) => by.get(id)).filter((r): r is GalleryAssetRow => !!r);
      })
      .catch((e) => logError(`hive: collection ${c.id} not read: ${e}`));
    return () => { alive = false; };
  });

  let note = $state('');
  async function run(what: string, f: () => Promise<unknown>) {
    note = '';
    try {
      await f();
      onchanged();
    } catch (e) {
      note = String(e).replace(/^Error: /, '');
      logError(`hive: ${what} refused: ${e}`);
    }
  }

  let renaming = $state(false);
  let nameDraft = $state('');
  let renameEl = $state<HTMLInputElement | null>(null);
  function startRename() {
    if (!current) return;
    nameDraft = current.name;
    renaming = true;
    // Explicitly, not `autofocus`: the gallery itself may hold focus, and autofocus then won't.
    requestAnimationFrame(() => renameEl?.select());
  }
  function saveRename() {
    const c = current;
    const name = nameDraft.trim();
    renaming = false;
    if (!c || !name || name === c.name) return;
    void run('rename', () => galleryRenameCollection(workspaceId, c.id, name));
  }

  let confirmDelete = $state(false);
  $effect(() => {
    void chosen;
    confirmDelete = false;
    renaming = false;
  });
  function remove() {
    const c = current;
    confirmDelete = false;
    if (c) void run('delete collection', () => galleryDeleteCollection(workspaceId, c.id));
  }

  let newName = $state('');
  function create() {
    const name = newName.trim();
    if (!name) return;
    newName = '';
    void run('new collection', async () => {
      const c = await galleryCollect(workspaceId, { collection: name, create: true });
      chosen = c.id;
    });
  }

  const setCover = (id: string) => current && void run('cover', () => galleryCollect(workspaceId, { collection: current!.id, cover: current!.cover === id ? '' : id }));
  const drop = (id: string) => current && void run('remove member', () => galleryCollect(workspaceId, { collection: current!.id, remove: [id] }));

  // ── Reorder by pointer drag ──
  let pending: { id: string; x: number; y: number } | null = null;
  let dragging = $state<string | null>(null);
  let over = $state<number | null>(null);
  function onDown(e: PointerEvent, id: string) {
    if (e.button !== 0 || (e.target as HTMLElement).closest('button')) return;
    pending = { id, x: e.clientX, y: e.clientY };
  }
  function onMove(e: PointerEvent) {
    if (!(e.buttons & 1)) {
      pending = null;
      return;
    }
    if (pending && !dragging) {
      if (Math.abs(e.clientX - pending.x) < DRAG_THRESHOLD && Math.abs(e.clientY - pending.y) < DRAG_THRESHOLD) return;
      dragging = pending.id;
      pending = null;
      (e.currentTarget as HTMLElement).setPointerCapture(e.pointerId);
    }
    if (!dragging) return;
    const el = document.elementFromPoint(e.clientX, e.clientY)?.closest<HTMLElement>('[data-member-index]');
    over = el ? Number(el.dataset.memberIndex) : null;
  }
  function onUp(e: PointerEvent) {
    pending = null;
    const id = dragging;
    const to = over;
    dragging = null;
    over = null;
    if (!id || to === null || !current) return;
    (e.currentTarget as HTMLElement).releasePointerCapture?.(e.pointerId);
    const order = members.map((m) => m.id).filter((m) => m !== id);
    const from = members.findIndex((m) => m.id === id);
    if (from === to) return;
    order.splice(to, 0, id);
    const c = current;
    void run('reorder', () => galleryCollect(workspaceId, { collection: c.id, order }));
  }
  function onKeyMove(e: KeyboardEvent, i: number) {
    if (!current || !(e.altKey && (e.key === 'ArrowLeft' || e.key === 'ArrowRight'))) return;
    e.preventDefault();
    const j = i + (e.key === 'ArrowLeft' ? -1 : 1);
    if (j < 0 || j >= members.length) return;
    const order = members.map((m) => m.id);
    [order[i], order[j]] = [order[j], order[i]];
    const c = current;
    void run('reorder', () => galleryCollect(workspaceId, { collection: c.id, order }));
  }
</script>

<div class="panel">
  <div class="bar">
    <button type="button" class="back" onclick={onback}><Icon name="chevron" size={11} /> Grid</button>
    <b>Collections</b>
  </div>
  <div class="body">
    <aside class="list" aria-label="Collections">
      {#each collections as c (c.id)}
        <button type="button" class:on={c.id === chosen} aria-pressed={c.id === chosen} onclick={() => (chosen = c.id)}>
          <span>{c.name}</span><em>{c.asset_ids.length}</em>
        </button>
      {/each}
      <form class="new" onsubmit={(e) => { e.preventDefault(); create(); }}>
        <input type="text" placeholder="New collection" bind:value={newName} maxlength="100" aria-label="New collection name" />
        <button type="submit" disabled={!newName.trim()}>Add</button>
      </form>
    </aside>
    <section class="members">
      {#if !current}
        <p class="hint">No collections yet. Add one here, or select assets in the grid and add them to a new one.</p>
      {:else}
        <div class="head">
          {#if renaming}
            <input class="rename" type="text" bind:this={renameEl} bind:value={nameDraft} maxlength="100" aria-label="Collection name"
              onkeydown={(e) => { if (e.key === 'Enter') saveRename(); else if (e.key === 'Escape') { e.stopPropagation(); renaming = false; } }}
              onblur={saveRename} />
          {:else}
            <b class="name">{current.name}</b>
            <button type="button" class="act" onclick={startRename}>Rename</button>
          {/if}
          <button type="button" class="act" onclick={() => onshow(current!.id)}>Show in grid</button>
          <Tooltip text="Copy its assets, in order and numbered, into a folder you pick">
            <button type="button" class="act" onclick={() => onexport(current!.id)} disabled={!members.length}>Export…</button>
          </Tooltip>
          {#if confirmDelete}
            <span class="confirm">Delete “{current.name}”? Its assets stay in the gallery.
              <button type="button" class="act danger" onclick={remove}>Delete</button>
              <button type="button" class="act" onclick={() => (confirmDelete = false)}>Keep</button>
            </span>
          {:else}
            <button type="button" class="act" onclick={() => (confirmDelete = true)}>Delete…</button>
          {/if}
        </div>
        {#if note}<p class="hint err">{note}</p>{/if}
        {#if members.length}
          <p class="hint">Drag to reorder (or Alt+←/→ on a focused member). The order is the order delivery writes.</p>
          <div class="strip" role="list">
            {#each members as m, i (m.id)}
              {@const pic = picture(m)}
              <!-- svelte-ignore a11y_no_noninteractive_tabindex, a11y_no_noninteractive_element_interactions -->
              <div
                class="member"
                class:dragging={dragging === m.id}
                class:over={over === i && dragging !== m.id}
                role="listitem"
                tabindex="0"
                data-member-index={i}
                aria-label="{i + 1}. {m.title}"
                onpointerdown={(e) => onDown(e, m.id)}
                onpointermove={onMove}
                onpointerup={onUp}
                onpointercancel={() => { pending = null; dragging = null; over = null; }}
                onkeydown={(e) => onKeyMove(e, i)}
              >
                <span class="n">{i + 1}</span>
                <div class="pic">
                  {#if pic === 'thumb'}<img src={galleryUrl(hiveId, m.id, 'thumb')} alt="" draggable="false" />
                  {:else if pic === 'wave'}<div class="wave" style="--wave: url('{galleryUrl(hiveId, m.id, 'wave')}')"></div>
                  {:else}<Icon name="file" size={18} />{/if}
                </div>
                <span class="mt">{m.title}</span>
                <div class="mact">
                  <Tooltip text={current.cover === m.id ? 'The cover: click to clear' : 'Make it the cover'}>
                    <button type="button" class:on={current.cover === m.id} aria-pressed={current.cover === m.id} onclick={() => setCover(m.id)}>Cover</button>
                  </Tooltip>
                  <Tooltip text="Take it out of the collection (it stays in the gallery)">
                    <button type="button" onclick={() => drop(m.id)}>Remove</button>
                  </Tooltip>
                </div>
              </div>
            {/each}
          </div>
        {:else}
          <p class="hint">Nothing in it out of the trash. Select assets in the grid and use Add to collection.</p>
        {/if}
      {/if}
    </section>
  </div>
</div>

<style>
  .panel { flex: 1; min-width: 0; min-height: 0; display: flex; flex-direction: column; }
  .bar { display: flex; align-items: center; gap: 12px; padding: 8px 12px; border-bottom: 1px solid var(--bg-light); font-size: 12.5px; }
  .back { display: inline-flex; align-items: center; gap: 4px; background: none; border: none; color: var(--accent); font: inherit; font-size: 12px; cursor: pointer; padding: 0; }
  .back :global(svg) { transform: rotate(90deg); }
  .body { flex: 1; min-height: 0; display: flex; }
  .list { flex: none; width: 200px; overflow-y: auto; padding: 10px; border-right: 1px solid var(--bg-light); display: flex; flex-direction: column; gap: 3px; font-size: 11.5px; }
  .list > button { display: flex; justify-content: space-between; gap: 6px; background: var(--bg-medium); border: 1px solid var(--bg-light); color: var(--fg-dim); border-radius: 5px; padding: 3px 7px; font: inherit; cursor: pointer; text-align: left; overflow-wrap: anywhere; }
  .list > button.on { color: var(--fg); border-color: var(--accent); }
  em { font-style: normal; opacity: 0.6; font-variant-numeric: tabular-nums; }
  .new { display: flex; gap: 4px; margin-top: 8px; }
  .new input { flex: 1; min-width: 0; background: var(--bg-medium); border: 1px solid var(--bg-light); color: var(--fg); border-radius: 5px; padding: 2px 6px; font: inherit; font-size: 11.5px; }
  .new button { background: var(--bg-medium); border: 1px solid var(--bg-light); color: var(--fg); border-radius: 5px; padding: 2px 8px; font: inherit; font-size: 11px; cursor: pointer; }
  .new button:disabled { opacity: 0.45; cursor: default; }
  .members { flex: 1; min-width: 0; overflow-y: auto; padding: 12px; display: flex; flex-direction: column; gap: 8px; font-size: 12px; }
  .head { display: flex; flex-wrap: wrap; align-items: center; gap: 8px; }
  .name { font-size: 13px; overflow-wrap: anywhere; }
  .rename { background: var(--bg-medium); border: 1px solid var(--accent); color: var(--fg); border-radius: 5px; padding: 2px 6px; font: inherit; }
  .act { background: var(--bg-medium); border: 1px solid var(--bg-light); color: var(--fg); border-radius: 6px; padding: 2px 9px; font: inherit; font-size: 11.5px; cursor: pointer; }
  .act.danger { border-color: var(--red); color: var(--red); }
  .confirm { display: inline-flex; flex-wrap: wrap; align-items: center; gap: 6px; color: var(--fg-dim); }
  .strip { display: flex; flex-wrap: wrap; gap: 8px; }
  .member { width: 132px; display: flex; flex-direction: column; gap: 4px; padding: 6px; border: 1px solid var(--bg-light); border-radius: 6px; background: var(--bg-medium); cursor: grab; position: relative; touch-action: none; }
  .member.dragging { opacity: 0.5; cursor: grabbing; }
  .member.over { border-color: var(--accent); box-shadow: -3px 0 0 0 var(--accent); }
  .member:focus-visible { outline: 1px solid var(--accent); }
  .n { position: absolute; top: 8px; left: 8px; font-size: 10px; padding: 0 4px; border-radius: 3px; background: color-mix(in srgb, var(--bg-dark) 78%, transparent); }
  .pic { height: 88px; display: flex; align-items: center; justify-content: center; overflow: hidden; border-radius: 4px; background: var(--bg-dark); color: var(--fg-dim); }
  .pic img { width: 100%; height: 100%; object-fit: cover; }
  .wave { width: 90%; height: 60%; background: var(--accent); -webkit-mask: var(--wave) center / 100% 100% no-repeat; mask: var(--wave) center / 100% 100% no-repeat; }
  .mt { font-size: 11px; overflow-wrap: anywhere; }
  .mact { display: flex; gap: 4px; }
  .mact button { background: var(--bg-dark); border: 1px solid var(--bg-light); color: var(--fg-dim); border-radius: 4px; padding: 0 6px; font: inherit; font-size: 10.5px; cursor: pointer; }
  .mact button.on { color: var(--fg); border-color: var(--accent); }
  .hint { margin: 0; color: var(--fg-dim); font-size: 11.5px; }
  .hint.err { color: var(--red); }
  button:focus-visible, input:focus-visible { outline: 1px solid var(--accent); outline-offset: 1px; }
</style>
