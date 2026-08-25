<script lang="ts">
  import type { Snippet } from 'svelte';

  interface Props {
    text: string;
    /** Let the wrapper carry the trigger's layout instead of shrink-wrapping it.
     *  The wrapper is the flex/grid item once it's in the way, so a trigger that filled
     *  its row — an ellipsizing label, a full-width button — collapses to its content
     *  width without this. */
    block?: boolean;
    children: Snippet;
  }

  let { text, block = false, children }: Props = $props();

  let visible = $state(false);
  /** Placed AND measured. The bubble is mounted unpositioned for one beat so it can be
   *  measured, and revealing it before then would flash it at the wrong place. */
  let placed = $state(false);
  let wrapperEl = $state<HTMLElement | null>(null);
  let tipEl = $state<HTMLElement | null>(null);
  let style = $state('');

  function show() {
    visible = true;
  }

  function hide() {
    visible = false;
    placed = false;
  }

  /**
   * The bubble exists only while it is shown.
   *
   * It used to be mounted permanently at opacity 0, one per trigger, which is fine for a
   * toolbar and not fine for the Overlord board: a few hundred cards with half a dozen
   * tooltips each is a few thousand fixed-position spans nobody ever looks at.
   *
   * Positioning therefore can't be a one-shot rAF from `show()` — the element doesn't
   * exist yet. This effect runs when `tipEl` binds, measures at its natural size, clamps
   * to the viewport, and only then reveals.
   */
  $effect(() => {
    if (!visible || !tipEl || !wrapperEl) return;
    const anchor = wrapperEl.getBoundingClientRect();
    const tip = tipEl.getBoundingClientRect();
    const pad = 8; // viewport edge padding

    // Horizontal: center on anchor, clamp to viewport
    let left = anchor.left + anchor.width / 2 - tip.width / 2;
    left = Math.max(pad, Math.min(left, window.innerWidth - tip.width - pad));

    // Vertical: prefer above, fall below if no room
    let top = anchor.top - tip.height - 6;
    if (top < pad) top = anchor.bottom + 6;

    style = `left:${left}px;top:${top}px`;
    placed = true;
  });
</script>

<!-- svelte-ignore a11y_no_static_element_interactions -->
<span
  class="tooltip-wrapper"
  class:block
  bind:this={wrapperEl}
  onmouseenter={show}
  onmouseleave={hide}
>
  {@render children()}
</span>

<!-- Empty text means "no tooltip here", so callers can pass a conditional string without
     guarding every use site — a bubble with nothing in it is worse than none. -->
{#if visible && text}
  <span class="tooltip-bubble" class:visible={placed} bind:this={tipEl} {style}>
    {text}
  </span>
{/if}

<style>
  .tooltip-wrapper {
    display: inline-flex;
    align-items: center;
  }

  /* Shrinks with its container rather than to its content, so an ellipsizing trigger
     still ellipsizes and a full-width button stays full width. */
  .tooltip-wrapper.block {
    display: flex;
    width: 100%;
    min-width: 0;
  }

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
