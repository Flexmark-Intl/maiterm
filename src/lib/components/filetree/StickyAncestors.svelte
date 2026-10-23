<script lang="ts">
  /**
   * Sticky ancestors (docs/file-tree.md §12.3): the folders above the first visible row whose
   * own rows have scrolled away, pinned over the top of the tree. Computed from the rows
   * alone — no IPC. A click scrolls to the folder and focuses it.
   */
  import type { Row } from '$lib/filetree/model';
  import { iconFor } from '$lib/filetree/icons';
  import TooltipBubble from '$lib/components/TooltipBubble.svelte';

  interface Props {
    rows: Row[];
    indices: number[];
    rowH: number;
    /** Where a drag would land: shown here too, since the folder's own row is under us. */
    dropTarget?: string | null;
    onpick: (key: string) => void;
  }

  let { rows, indices, rowH, dropTarget = null, onpick }: Props = $props();

  // A clipped name gets its full path in a bubble (the nowrap-needs-a-tooltip rule).
  let tip = $state<{ el: Element; text: string } | null>(null);
  function hover(e: MouseEvent, key: string) {
    const name = (e.currentTarget as HTMLElement).querySelector('.name');
    tip = name && name.scrollWidth > name.clientWidth ? { el: name, text: key } : null;
  }
</script>

<TooltipBubble text={tip?.text ?? ''} anchor={tip?.el ?? null} />

{#if indices.length > 0}
  <div class="sticky" style="height:{indices.length * rowH}px">
    {#each indices as i, n (rows[i].key)}
      {@const r = rows[i]}
      {@const ic = iconFor(r.name, r.kind, true)}
      <button
        class="sticky-row"
        class:drop-target={r.key === dropTarget}
        style="top:{n * rowH}px; height:{rowH}px; padding-left:{8 + r.depth * 12}px"
        tabindex="-1"
        onmousedown={(e) => e.preventDefault()}
        onmouseenter={(e) => hover(e, r.key)}
        onmouseleave={() => { tip = null; }}
        onclick={() => onpick(r.key)}
      >
        <span class="chev"></span>
        <svg class="ic" style="color: var(--ft-ic-{ic.color})" width="16" height="16" aria-hidden="true"><use href="#{ic.id}" /></svg>
        <span class="name">{r.name}</span>
      </button>
    {/each}
  </div>
{/if}

<style>
  .sticky {
    position: relative;
    background: var(--bg-medium);
    border-bottom: 1px solid var(--bg-light);
    box-shadow: 0 2px 4px rgba(0, 0, 0, 0.2);
  }

  .sticky-row {
    position: absolute;
    left: 0;
    right: 0;
    display: flex;
    align-items: center;
    gap: 4px;
    border: none;
    background: var(--bg-medium);
    color: var(--fg);
    font: inherit;
    text-align: left;
    cursor: pointer;
    padding-right: 8px;
  }

  .sticky-row:hover {
    background: var(--bg-light);
  }

  /* The tree's own drop-target look, over the opaque header background. */
  .sticky-row.drop-target {
    background: color-mix(in srgb, var(--accent) 28%, var(--bg-medium));
    box-shadow: inset 0 0 0 1px var(--accent);
  }

  .chev {
    width: 12px;
    flex-shrink: 0;
  }

  .ic {
    flex-shrink: 0;
  }

  /* Ellipsized; a clipped name shows its full path in the hover bubble. */
  .name {
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
</style>
