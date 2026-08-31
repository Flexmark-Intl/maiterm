<script lang="ts">
  /**
   * The bubble half of {@link Tooltip}, anchored to an element you already have.
   *
   * `Tooltip` wraps its trigger in a `<span>`, which is not renderable inside an `<svg>` —
   * so SVG hover targets (the mesh graph's nodes) drive this directly: track the hovered
   * element yourself, pass it as `anchor`, and pass `null` to hide.
   */
  interface Props {
    text: string;
    /** The element to point at. `null` hides the bubble (nothing is mounted). */
    anchor: Element | null;
  }

  let { text, anchor }: Props = $props();

  /** Placed AND measured — see Tooltip.svelte for why reveal waits on measurement. */
  let placed = $state(false);
  let tipEl = $state<HTMLElement | null>(null);
  let style = $state('');

  $effect(() => {
    // Hidden again: drop `placed` so the next bubble can't flash at the last one's spot
    // before it has been measured.
    if (!anchor || !text) {
      placed = false;
      return;
    }
    if (!tipEl) return;
    const a = anchor.getBoundingClientRect();
    const tip = tipEl.getBoundingClientRect();
    const pad = 8; // viewport edge padding

    // Horizontal: center on anchor, clamp to viewport
    let left = a.left + a.width / 2 - tip.width / 2;
    left = Math.max(pad, Math.min(left, window.innerWidth - tip.width - pad));

    // Vertical: prefer above, fall below if no room
    let top = a.top - tip.height - 6;
    if (top < pad) top = a.bottom + 6;

    style = `left:${left}px;top:${top}px`;
    placed = true;
  });
</script>

<!-- Empty text means "no tooltip here", so callers can pass a conditional string without
     guarding every use site — a bubble with nothing in it is worse than none. -->
{#if anchor && text}
  <span class="tooltip-bubble" class:visible={placed} bind:this={tipEl} {style}>
    {text}
  </span>
{/if}

<style>
  .tooltip-bubble {
    position: fixed;
    width: max-content;
    max-width: min(320px, calc(100vw - 16px));
    background: var(--bg-dark);
    border: 1px solid var(--bg-light);
    color: var(--fg);
    font-size: 0.846rem;
    line-height: 1.45;
    white-space: pre-line;
    padding: 6px 10px;
    border-radius: 5px;
    pointer-events: none;
    opacity: 0;
    transition: opacity 0.12s;
    z-index: 9999;
  }

  .tooltip-bubble.visible {
    opacity: 1;
  }
</style>
