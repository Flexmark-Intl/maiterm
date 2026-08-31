<script lang="ts">
  import type { Snippet } from 'svelte';
  import TooltipBubble from '$lib/components/TooltipBubble.svelte';

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
  let wrapperEl = $state<HTMLElement | null>(null);

  function show() {
    visible = true;
  }

  function hide() {
    visible = false;
  }

  /**
   * The bubble exists only while it is shown.
   *
   * It used to be mounted permanently at opacity 0, one per trigger, which is fine for a
   * toolbar and not fine for the Overlord board: a few hundred cards with half a dozen
   * tooltips each is a few thousand fixed-position spans nobody ever looks at. Passing a
   * null anchor mounts nothing; TooltipBubble measures itself once mounted and only then
   * reveals, so it never flashes at the wrong place.
   */
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

<TooltipBubble {text} anchor={visible ? wrapperEl : null} />

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
</style>
