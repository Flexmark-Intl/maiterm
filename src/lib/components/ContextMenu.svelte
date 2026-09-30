<script lang="ts" module>
  export interface MenuItem {
    label: string;
    shortcut?: string;
    action: () => void;
    disabled?: boolean;
    separator?: boolean;
    /** Opens a flyout on hover instead of acting; `action` is not called. An empty list
     *  shows the item disabled. */
    submenu?: MenuItem[];
  }
</script>

<script lang="ts">
  import { onMount } from 'svelte';
  import ContextMenu from './ContextMenu.svelte';

  interface Props {
    items: MenuItem[];
    x: number;
    y: number;
    onclose: () => void;
    /** The button that opened the menu, when there is one. A mousedown on it is left to
     *  that button's own click handler (which toggles), instead of closing here first and
     *  then having the click reopen it — the sequence that made a dropdown un-closable
     *  from its own button. */
    anchor?: HTMLElement | null;
    /** Set on a submenu flyout: the x to open at when there is no room to the right (the
     *  parent menu's left edge). A flyout also leaves dismissal to the root menu, whose
     *  DOM it sits inside — so a click in the parent is not "outside" and closes nothing. */
    flipX?: number;
  }

  let { items, x, y, onclose, anchor = null, flipX }: Props = $props();
  const nested = $derived(flipX !== undefined);

  let menuEl = $state<HTMLDivElement | null>(null);
  let openSub = $state<{ index: number; x: number; y: number; flipX: number } | null>(null);

  function showSub(item: MenuItem, index: number, el: HTMLElement) {
    if (!item.submenu || item.submenu.length === 0 || item.disabled) {
      openSub = null;
      return;
    }
    if (openSub?.index === index) return;
    const r = el.getBoundingClientRect();
    const m = menuEl?.getBoundingClientRect() ?? r;
    openSub = { index, x: m.right - 2, y: r.top - 5, flipX: m.left + 2 };
  }

  const MARGIN = 8; // keep this gap from the window edges

  // Keep the menu fully inside the window. Prefer opening down-right of the
  // cursor, flip when it would overflow, then clamp. When the menu is taller
  // than the viewport (more items than fit), cap its height — `overflow-y:auto`
  // turns it into a scrollable list instead of clipping off-screen.
  const layout = $derived.by(() => {
    const vw = window.innerWidth;
    const vh = window.innerHeight;
    const maxHeight = Math.max(0, vh - MARGIN * 2);
    if (!menuEl) return { left: x, top: y, maxHeight };

    const rect = menuEl.getBoundingClientRect();
    const menuW = rect.width;

    // Horizontal: flip left if it overflows the right edge, then clamp. A flyout flips to
    // the far side of its parent rather than of the cursor.
    let left = x + menuW > vw - MARGIN ? (flipX ?? x) - menuW : x;
    left = Math.max(MARGIN, Math.min(left, vw - menuW - MARGIN));

    // Vertical: place below the cursor if it fits, else above, else pin to the
    // top and let the capped height scroll.
    const cappedH = Math.min(rect.height, maxHeight);
    const spaceBelow = vh - MARGIN - y;
    const spaceAbove = y - MARGIN;
    let top: number;
    if (cappedH <= spaceBelow) top = y;
    else if (cappedH <= spaceAbove) top = y - cappedH;
    else top = MARGIN;
    top = Math.max(MARGIN, Math.min(top, vh - cappedH - MARGIN));

    return { left, top, maxHeight };
  });

  function handleItemClick(item: MenuItem, index: number, el: HTMLElement) {
    if (item.disabled) return;
    if (item.submenu) {
      showSub(item, index, el);
      return;
    }
    item.action();
    onclose();
  }

  // Window-level listeners for robust dismissal (avoids pointer-events
  // inheritance issues when rendered inside pointer-events:none containers)
  onMount(() => {
    if (nested) return;
    function onMousedown(e: MouseEvent) {
      const t = e.target as Node;
      if (anchor?.contains(t)) return;
      if (menuEl && !menuEl.contains(t)) {
        onclose();
      }
    }
    function onKeydown(e: KeyboardEvent) {
      if (e.key === 'Escape') {
        e.stopPropagation();
        onclose();
      }
    }
    function onContextmenu(e: MouseEvent) {
      if (menuEl && !menuEl.contains(e.target as Node)) {
        e.preventDefault();
        onclose();
      }
    }
    window.addEventListener('mousedown', onMousedown, true);
    window.addEventListener('keydown', onKeydown, true);
    window.addEventListener('contextmenu', onContextmenu, true);
    return () => {
      window.removeEventListener('mousedown', onMousedown, true);
      window.removeEventListener('keydown', onKeydown, true);
      window.removeEventListener('contextmenu', onContextmenu, true);
    };
  });
</script>

<div
  class="context-menu"
  bind:this={menuEl}
  style="left: {layout.left}px; top: {layout.top}px; max-height: {layout.maxHeight}px"
  role="menu"
  tabindex="-1"
>
    {#each items as item, i}
      {#if item.separator}
        <div class="separator"></div>
      {:else}
        {@const emptySub = !!item.submenu && item.submenu.length === 0}
        <button
          class="menu-item"
          class:disabled={item.disabled || emptySub}
          class:open={openSub?.index === i}
          onclick={(e) => handleItemClick(item, i, e.currentTarget)}
          onmouseenter={(e) => showSub(item, i, e.currentTarget)}
          role="menuitem"
          aria-haspopup={item.submenu ? 'menu' : undefined}
          aria-expanded={item.submenu ? openSub?.index === i : undefined}
          disabled={item.disabled || emptySub}
        >
          <span class="menu-label">{item.label}</span>
          {#if item.shortcut}
            <span class="menu-shortcut">{item.shortcut}</span>
          {/if}
          {#if item.submenu}
            <span class="menu-chevron" aria-hidden="true">›</span>
          {/if}
        </button>
      {/if}
    {/each}
    {#if openSub && items[openSub.index]?.submenu}
      {#key openSub.index}
        <ContextMenu
          items={items[openSub.index].submenu ?? []}
          x={openSub.x}
          y={openSub.y}
          flipX={openSub.flipX}
          {onclose}
        />
      {/key}
    {/if}
  </div>

<style>
  .context-menu {
    position: fixed;
    z-index: 1000;
    pointer-events: auto;
    background: var(--bg-medium);
    border: 1px solid var(--bg-light);
    border-radius: 6px;
    padding: 4px;
    min-width: 180px;
    box-shadow: 0 4px 16px rgba(0, 0, 0, 0.4);
    overflow-y: auto;
  }

  .menu-item {
    display: flex;
    align-items: center;
    width: 100%;
    padding: 6px 12px;
    border-radius: 4px;
    font-size: 1rem;
    color: var(--fg);
    text-align: left;
    cursor: pointer;
  }

  .menu-item:hover:not(:disabled) {
    background: var(--bg-light);
  }

  .menu-item:disabled {
    color: var(--fg-dim);
    cursor: default;
  }

  .menu-label {
    flex: 1;
  }

  .menu-item.open:not(:disabled) {
    background: var(--bg-light);
  }

  .menu-chevron {
    margin-left: 16px;
    color: var(--fg-dim);
  }

  .menu-shortcut {
    margin-left: 24px;
    color: var(--fg-dim);
    font-size: 0.923rem;
  }

  .separator {
    height: 1px;
    background: var(--bg-light);
    margin: 4px 8px;
  }
</style>
