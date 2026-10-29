<script lang="ts">
  import type { Snippet } from 'svelte';
  import type { Tab } from '$lib/tauri/types';
  import Icon from '$lib/components/Icon.svelte';
  import Tooltip from '$lib/components/Tooltip.svelte';
  import { isImageFile, isPdfFile } from '$lib/utils/languageDetect';

  interface TabMenuItem {
    tab: Tab;
    label: string;
    meta?: string | null;
  }

  interface Props {
    items: TabMenuItem[];
    /** Anchor position. Provide `left` to open right-aligned to the anchor, or `right` to open left-aligned. */
    position: { top: number; left?: number; right?: number };
    /** Called when an item's name is clicked. */
    onActivate: (tab: Tab) => void;
    /** Trailing action buttons rendered per row. */
    actions: Snippet<[Tab]>;
    /** Show a search box that filters rows by name. */
    searchable?: boolean;
  }

  let { items, position, onActivate, actions, searchable = false }: Props = $props();

  let query = $state('');
  const filtered = $derived(
    query.trim()
      ? items.filter(i => i.label.toLowerCase().includes(query.trim().toLowerCase()))
      : items
  );

  // Group terminals and viewers (editors/diffs) into sections, matching the tab bar.
  const shellItems = $derived(filtered.filter(i => i.tab.tab_type === 'terminal' || !i.tab.tab_type));
  const viewerItems = $derived(filtered.filter(i => i.tab.tab_type === 'editor' || i.tab.tab_type === 'diff'));
  const showHeaders = $derived(shellItems.length > 0 && viewerItems.length > 0);
  // Rows in the order they are drawn, for arrow-key navigation from the search box.
  const ordered = $derived([...shellItems, ...viewerItems]);

  let highlighted = $state(0);
  let menuEl = $state<HTMLElement | null>(null);

  // A new query starts over at the first match, so typing then Enter opens it.
  $effect(() => {
    void query;
    highlighted = 0;
  });

  // A row closed or archived from inside the menu can shrink the list under the highlight.
  $effect(() => {
    if (highlighted >= ordered.length) highlighted = Math.max(0, ordered.length - 1);
  });

  function moveHighlight(delta: number) {
    if (ordered.length === 0) return;
    highlighted = (highlighted + delta + ordered.length) % ordered.length;
    requestAnimationFrame(() => {
      menuEl?.querySelector('.tab-menu-item.highlighted')?.scrollIntoView({ block: 'nearest' });
    });
  }

  function handleSearchKeydown(e: KeyboardEvent) {
    if (e.key === 'ArrowDown') {
      e.preventDefault();
      moveHighlight(1);
    } else if (e.key === 'ArrowUp') {
      e.preventDefault();
      moveHighlight(-1);
    } else if (e.key === 'Enter') {
      const item = ordered[highlighted];
      if (!item) return;
      e.preventDefault();
      onActivate(item.tab);
    }
  }

  const posStyle = $derived(
    `top:${position.top}px;` +
      (position.right != null ? `right:${position.right}px;` : `left:${position.left ?? 0}px;`)
  );

  function autofocus(node: HTMLInputElement) {
    node.focus();
  }
</script>

<!-- svelte-ignore a11y_no_static_element_interactions -->
<div class="tab-menu" style={posStyle} bind:this={menuEl} onwheel={(e) => e.stopPropagation()}>
  {#if searchable}
    <div class="tab-menu-search">
      <Icon name="search" size={12} />
      <input
        type="text"
        placeholder="Search tabs…"
        bind:value={query}
        onkeydown={handleSearchKeydown}
        use:autofocus
        spellcheck="false"
        autocomplete="off"
      />
    </div>
  {/if}
  {#if filtered.length === 0}
    <div class="tab-menu-empty">No matching tabs</div>
  {/if}
  {#if showHeaders && shellItems.length > 0}
    <div class="tab-menu-header">Shells</div>
  {/if}
  {#each shellItems as item, i (item.tab.id)}
    {@render row(item, i)}
  {/each}
  {#if showHeaders && viewerItems.length > 0}
    <div class="tab-menu-header">Viewers</div>
  {/if}
  {#each viewerItems as item, i (item.tab.id)}
    {@render row(item, shellItems.length + i)}
  {/each}
</div>

{#snippet row(item: TabMenuItem, index: number)}
  <!-- svelte-ignore a11y_no_static_element_interactions -->
  <div
    class="tab-menu-item"
    class:highlighted={searchable && index === highlighted}
    onmouseenter={() => (highlighted = index)}
  >
    {#if item.tab.tab_type === 'diff'}
      <Tooltip text="Diff"><span class="tab-menu-icon"><Icon name="diff" size={12} /></span></Tooltip>
    {:else if item.tab.tab_type === 'editor'}
      {#if item.tab.editor_file && isPdfFile(item.tab.editor_file.file_path)}
        <Tooltip text="PDF"><span class="tab-menu-icon"><Icon name="pdf" size={12} /></span></Tooltip>
      {:else if item.tab.editor_file && isImageFile(item.tab.editor_file.file_path)}
        <Tooltip text="Image"><span class="tab-menu-icon"><Icon name="image" size={12} /></span></Tooltip>
      {:else}
        <Tooltip text="Editor"><span class="tab-menu-icon"><Icon name="file" size={12} /></span></Tooltip>
      {/if}
    {:else}
      <Tooltip text="Terminal"><span class="tab-menu-icon">&gt;_</span></Tooltip>
    {/if}
    <button class="tab-menu-name" onclick={() => onActivate(item.tab)}>
      <span class="tab-menu-label">{item.label}</span>
      {#if item.meta}
        <span class="tab-menu-meta">{item.meta}</span>
      {/if}
    </button>
    {@render actions(item.tab)}
  </div>
{/snippet}

<style>
  .tab-menu {
    position: fixed;
    z-index: 1000;
    min-width: 260px;
    max-width: 420px;
    max-height: 360px;
    overflow-y: auto;
    background: var(--bg-medium);
    border: 1px solid var(--bg-light);
    border-radius: 6px;
    box-shadow: 0 4px 12px rgba(0, 0, 0, 0.4);
    padding: 4px;
  }

  .tab-menu-search {
    display: flex;
    align-items: center;
    gap: 6px;
    padding: 4px 6px 6px;
    color: var(--fg-dim);
    position: sticky;
    top: -4px;
    background: var(--bg-medium);
    z-index: 1;
  }

  .tab-menu-search :global(svg) {
    flex-shrink: 0;
    opacity: 0.7;
  }

  .tab-menu-search input {
    flex: 1;
    min-width: 0;
    background: var(--bg-dark);
    border: 1px solid var(--bg-light);
    border-radius: 4px;
    color: var(--fg);
    font-size: 0.846rem;
    padding: 4px 8px;
    outline: none;
  }

  .tab-menu-search input:focus {
    border-color: var(--accent);
  }

  .tab-menu-empty {
    font-size: 0.846rem;
    color: var(--fg-dim);
    padding: 8px 6px;
    text-align: center;
  }

  .tab-menu-item {
    display: flex;
    align-items: center;
    gap: 4px;
    padding: 4px 6px;
    border-radius: 4px;
    transition: background 0.1s;
  }

  .tab-menu-item:hover,
  .tab-menu-item.highlighted {
    background: var(--bg-light);
  }

  .tab-menu-name {
    flex: 1;
    display: flex;
    align-items: baseline;
    gap: 6px;
    overflow: hidden;
    font-size: 0.923rem;
    color: var(--fg);
    text-align: left;
    padding: 2px 0;
    background: none;
    border: none;
    cursor: pointer;
    min-width: 0;
  }

  .tab-menu-label {
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    max-width: 260px;
  }

  .tab-menu-meta {
    flex-shrink: 0;
    margin-left: auto;
    font-size: 0.769rem;
    color: var(--fg-dim);
    white-space: nowrap;
  }

  .tab-menu-header {
    font-size: 0.769rem;
    font-weight: 600;
    text-transform: uppercase;
    letter-spacing: 0.05em;
    color: var(--fg-dim);
    padding: 6px 6px 2px;
  }

  .tab-menu-header:not(:first-child) {
    margin-top: 4px;
    border-top: 1px solid var(--bg-light);
    padding-top: 8px;
  }

  .tab-menu-icon {
    flex-shrink: 0;
    width: 14px;
    height: 14px;
    display: flex;
    align-items: center;
    justify-content: center;
    font-size: 0.769rem;
    opacity: 0.6;
    color: var(--fg-dim);
  }

  .tab-menu-icon :global(svg) {
    width: 12px;
    height: 12px;
  }
</style>
