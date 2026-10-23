<script lang="ts">
  /**
   * The footer's question (docs/file-tree.md §10.4): permanent delete, a trash that can't,
   * an agent at work on the item, a tab working in the folder, unsaved edits, a name clash.
   * `confirm()` is dead in the webview, so this is where the human answers. The buttons are
   * real buttons (Tab moves between them, Enter or Space presses one) and Escape cancels.
   * The first button takes the keyboard, except a destructive one: Cancel does, so a stray
   * Enter never deletes.
   */
  import { onMount, tick } from 'svelte';
  import type { ConfirmState, ConfirmAction } from '$lib/stores/fileTree.svelte';

  interface Props {
    question: ConfirmState;
    onrun: (a: ConfirmAction) => void;
    oncancel: () => void;
  }

  let { question, onrun, oncancel }: Props = $props();

  let first = $state<HTMLButtonElement | null>(null);
  let cancelBtn = $state<HTMLButtonElement | null>(null);

  onMount(() => {
    void tick().then(() => {
      const danger = question.actions[0]?.tone === 'danger';
      (danger || !first ? cancelBtn : first)?.focus({ preventScroll: true });
    });
  });

  function onKeydown(e: KeyboardEvent) {
    // Nothing here is the tree's: arrows and letters stay put.
    e.stopPropagation();
    if (e.key === 'Escape') {
      e.preventDefault();
      oncancel();
    }
  }
</script>

<!-- svelte-ignore a11y_no_noninteractive_element_interactions -->
<div class="strip" role="alertdialog" aria-label="Confirm" tabindex="-1" onkeydown={onKeydown}>
  <div class="text">{question.text}</div>
  <div class="buttons">
    {#each question.actions as a, i (a.label)}
      {#if i === 0}
        <button bind:this={first} class={a.tone ?? ''} onclick={() => onrun(a)}>{a.label}</button>
      {:else}
        <button class={a.tone ?? ''} onclick={() => onrun(a)}>{a.label}</button>
      {/if}
    {/each}
    <button bind:this={cancelBtn} onclick={oncancel}>{question.actions.length ? 'Cancel' : 'OK'}</button>
  </div>
</div>

<style>
  .strip {
    padding: 6px 8px;
    border-top: 1px solid var(--bg-light);
    background: color-mix(in srgb, var(--yellow) 8%, var(--bg-medium));
    outline: none;
  }

  .text {
    color: var(--fg);
    font-size: 0.9em;
    line-height: 1.4;
    overflow-wrap: anywhere;
    margin-bottom: 6px;
  }

  .buttons {
    display: flex;
    flex-wrap: wrap;
    gap: 6px;
  }

  button {
    background: var(--bg-light);
    color: var(--fg);
    border: 1px solid transparent;
    border-radius: 4px;
    padding: 2px 10px;
    font: inherit;
    font-size: 0.9em;
    cursor: pointer;
  }

  button:hover {
    background: color-mix(in srgb, var(--bg-light) 70%, var(--fg) 15%);
  }

  button:focus-visible {
    outline: 2px solid var(--accent);
    outline-offset: 1px;
  }

  button.danger {
    background: color-mix(in srgb, var(--red) 75%, var(--bg-dark));
    color: var(--bg-dark);
    font-weight: 600;
  }

  button.primary {
    background: color-mix(in srgb, var(--accent) 80%, var(--bg-dark));
    color: var(--bg-dark);
    font-weight: 600;
  }
</style>
