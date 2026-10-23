<script lang="ts">
  /**
   * The root switcher (docs/file-tree.md §3.2): the folders of every terminal tab in the
   * workspace (deduped, with the tabs and their agent dots), the pin, recent roots, and
   * "Open Folder…". Choosing any of them pins it for the workspace (§4.2); unpinning goes
   * back to following the active tab.
   */
  import { onMount, tick } from 'svelte';
  import { open as dialogOpen } from '@tauri-apps/plugin-dialog';
  import { fileTreeStore } from '$lib/stores/fileTree.svelte';
  import { claudeStateStore } from '$lib/stores/agentState.svelte';
  import StatusDot from '$lib/components/ui/StatusDot.svelte';
  import Tooltip from '$lib/components/Tooltip.svelte';
  import { lastSegment, tildePath } from '$lib/filetree/paths';
  import type { TreePlace } from '$lib/tauri/types';

  interface Props {
    anchor: HTMLElement | null;
    onclose: () => void;
  }

  let { anchor, onclose }: Props = $props();

  let el = $state<HTMLDivElement | null>(null);
  let tabRoots = $state<{ place: TreePlace; tabs: { id: string; name: string }[] }[]>([]);
  /** Some tab hasn't answered yet: the list grows as they do. */
  let looking = $state(true);
  const current = $derived(fileTreeStore.place?.place ?? null);
  const pinned = $derived(fileTreeStore.pinned);
  const recent = $derived(
    fileTreeStore.recentPlaces.filter((p) => p !== current && p !== pinned && !tabRoots.some((r) => r.place.place === p)),
  );

  const pos = $derived.by(() => {
    const r = anchor?.getBoundingClientRect();
    return r ? { left: r.left, top: r.bottom + 2 } : { left: 8, top: 40 };
  });

  onMount(() => {
    let open = true;
    void fileTreeStore.tabRoots((r) => { if (open) tabRoots = r; }).finally(() => { looking = false; });
    // Focus explicitly: Svelte's autofocus doesn't move focus off a keyboard-opened button.
    void tick().then(() => el?.querySelector<HTMLButtonElement>('button.item')?.focus({ preventScroll: true }));
    const onDown = (e: MouseEvent) => {
      const t = e.target as Node;
      if (el?.contains(t) || anchor?.contains(t)) return;
      onclose();
    };
    window.addEventListener('mousedown', onDown, true);
    return () => {
      open = false;
      window.removeEventListener('mousedown', onDown, true);
    };
  });

  function dotColor(tabId: string): 'accent' | 'green' | 'yellow' | null {
    const s = claudeStateStore.getState(tabId)?.state;
    if (s === 'active') return 'accent';
    if (s === 'permission') return 'yellow';
    if (s === 'idle') return 'green';
    return null;
  }

  async function choose(p: string) {
    onclose();
    await fileTreeStore.pin(p);
  }

  async function openFolder() {
    onclose();
    const picked = await dialogOpen({ directory: true, multiple: false, title: 'Open Folder in Files' }).catch(() => null);
    if (typeof picked === 'string' && picked) await fileTreeStore.pin(picked);
  }

  function onKeydown(e: KeyboardEvent) {
    const items = [...(el?.querySelectorAll<HTMLButtonElement>('button.item') ?? [])];
    const i = items.indexOf(document.activeElement as HTMLButtonElement);
    if (e.key === 'Escape') {
      e.preventDefault();
      e.stopPropagation();
      onclose();
      anchor?.focus();
    } else if (e.key === 'ArrowDown') {
      e.preventDefault();
      items[(i + 1) % items.length]?.focus();
    } else if (e.key === 'ArrowUp') {
      e.preventDefault();
      items[(i - 1 + items.length) % items.length]?.focus();
    }
  }
</script>

<!-- svelte-ignore a11y_no_static_element_interactions -->
<div class="switcher" role="menu" tabindex="-1" bind:this={el} style="left:{pos.left}px; top:{pos.top}px" onkeydown={onKeydown}>
  <div class="section">Tabs in this workspace</div>
  {#if tabRoots.length === 0}
    <div class="empty">{looking ? 'Looking…' : 'No terminal tab has a folder yet.'}</div>
  {:else}
    {#each tabRoots as r (r.place.place)}
      <Tooltip text={fileTreeStore.displayRoot(r.place)} block>
        <button class="item" class:current={r.place.place === current} role="menuitem" onclick={() => choose(r.place.place)}>
          <span class="label">{lastSegment(r.place.root) || r.place.root}{#if r.place.host_label} <span class="host">{r.place.host_label}</span>{/if}</span>
          <span class="tabs">
            {#each r.tabs as t (t.id)}
              {@const c = dotColor(t.id)}
              <span class="tab">{#if c}<StatusDot color={c} />{/if}{t.name}</span>
            {/each}
          </span>
        </button>
      </Tooltip>
    {/each}
    {#if looking}<div class="empty">Looking…</div>{/if}
  {/if}

  {#if pinned}
    <div class="section">Pinned</div>
    <Tooltip text={tildePath(pinned, fileTreeStore.home)} block>
      <button class="item" class:current={pinned === current} role="menuitem" onclick={() => choose(pinned)}>
        <span class="label">{lastSegment(pinned)}</span>
      </button>
    </Tooltip>
    <button class="item subtle" role="menuitem" onclick={() => { onclose(); void fileTreeStore.unpin(); }}>
      <span class="label">Unpin: follow the active tab</span>
    </button>
  {/if}

  {#if recent.length > 0}
    <div class="section">Recent</div>
    {#each recent as p (p)}
      <Tooltip text={tildePath(p, fileTreeStore.home)} block>
        <button class="item" role="menuitem" onclick={() => choose(p)}>
          <span class="label">{lastSegment(p)}</span>
        </button>
      </Tooltip>
    {/each}
  {/if}

  <div class="divider"></div>
  <button class="item" role="menuitem" onclick={openFolder}>
    <span class="label">Open Folder…</span>
  </button>
</div>

<style>
  .switcher {
    position: fixed;
    z-index: 1000;
    min-width: 220px;
    max-width: min(360px, calc(100vw - 16px));
    max-height: 60vh;
    overflow-y: auto;
    background: var(--bg-medium);
    border: 1px solid var(--bg-light);
    border-radius: 6px;
    padding: 4px;
    box-shadow: 0 6px 24px rgba(0, 0, 0, 0.35);
    outline: none;
  }

  .section {
    padding: 6px 8px 2px;
    font-size: 0.8em;
    text-transform: uppercase;
    letter-spacing: 0.04em;
    color: var(--fg-dim);
  }

  .empty {
    padding: 4px 8px 6px;
    color: var(--fg-dim);
    font-size: 0.9em;
  }

  .item {
    display: flex;
    flex-direction: column;
    align-items: flex-start;
    gap: 2px;
    width: 100%;
    text-align: left;
    background: none;
    border: none;
    border-radius: 4px;
    padding: 5px 8px;
    color: var(--fg);
    font: inherit;
    cursor: pointer;
  }

  .item:hover,
  .item:focus-visible {
    background: var(--bg-light);
    outline: none;
  }

  .item.current .label {
    color: var(--accent);
  }

  .item.subtle .label {
    color: var(--fg-dim);
  }

  .label {
    font-weight: 600;
    overflow-wrap: anywhere;
  }

  /* The ssh host of a remote root, as the header's chip shows it. */
  .host {
    font-weight: 400;
    font-size: 0.85em;
    padding: 0 5px;
    border-radius: 8px;
    background: var(--bg-light);
    color: var(--fg-dim);
  }

  .tabs {
    display: flex;
    flex-wrap: wrap;
    gap: 2px 10px;
    font-size: 0.85em;
    color: var(--fg-dim);
  }

  .tab {
    display: inline-flex;
    align-items: center;
    gap: 4px;
    overflow-wrap: anywhere;
  }

  .divider {
    height: 1px;
    background: var(--bg-light);
    margin: 4px 2px;
  }
</style>
