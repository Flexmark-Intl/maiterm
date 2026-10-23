<script lang="ts">
  /**
   * The inline New / Rename input (docs/file-tree.md §10.4).
   *
   * - Focused with `await tick(); input.focus({preventScroll: true})`: never Svelte's
   *   `autofocus` (it doesn't focus when a keyboard opened it) and never rAF (it stalls in an
   *   occluded webview).
   * - Enter commits, Escape cancels, a REAL blur commits (a valid name; an invalid one is
   *   dropped). Switching to another app is not a real blur: the window lost focus, not the
   *   input, so the edit waits.
   * - If the column unmounts mid-edit, nothing is created or renamed: an accidental create
   *   is worse than a lost name. Blur can fire during teardown, so a commit only runs while
   *   the component is alive and its input still in the document.
   * - Every key and press is kept from the tree around it: arrows, Space and letters are
   *   the input's, not the tree's navigation and type-ahead.
   */
  import { onDestroy, onMount, tick } from 'svelte';
  import { nameProblem, type NameRules } from '$lib/filetree/names';

  interface Props {
    initial: string;
    /** The part preselected (a rename's stem). */
    select?: [number, number];
    placeholder?: string;
    rules: NameRules | null;
    /** The store's word after a failed commit. */
    error: string | null;
    pending: boolean;
    oncommit: (value: string) => void;
    oncancel: () => void;
  }

  let { initial, select, placeholder = '', rules, error, pending, oncommit, oncancel }: Props = $props();

  // svelte-ignore state_referenced_locally
  let value = $state(initial);
  let input = $state<HTMLInputElement | null>(null);
  let alive = true;
  /** Edited since the store's last error: the live check speaks, not the stale failure. */
  let typed = $state(false);

  const live = $derived(rules && value !== '' ? nameProblem(value, rules) : null);
  const message = $derived(typed ? live : (error ?? live));

  onMount(() => {
    void tick().then(() => {
      if (!alive || !input) return;
      input.focus({ preventScroll: true });
      if (select) input.setSelectionRange(select[0], select[1]);
      else input.select();
    });
  });

  onDestroy(() => {
    alive = false;
  });

  // A failed commit hands the keyboard back so the name can be fixed.
  $effect(() => {
    if (error && !pending && input && alive) {
      typed = false;
      void tick().then(() => input?.focus({ preventScroll: true }));
    }
  });

  function onKeydown(e: KeyboardEvent) {
    e.stopPropagation();
    if (e.isComposing) return;
    if (e.key === 'Enter') {
      e.preventDefault();
      if (!pending) oncommit(value);
    } else if (e.key === 'Escape') {
      e.preventDefault();
      oncancel();
    }
  }

  function onBlur() {
    // Decided after the event: teardown and window switches blur too, and neither commits.
    queueMicrotask(() => {
      if (!alive || !input?.isConnected || pending) return;
      if (!document.hasFocus()) return;
      if (document.activeElement === input) return;
      if (value.trim() === '' || (rules && nameProblem(value, rules))) oncancel();
      else oncommit(value);
    });
  }

  const stop = (e: Event) => e.stopPropagation();
</script>

<!-- Only stops presses reaching the row; the input inside takes the keyboard. -->
<!-- svelte-ignore a11y_no_static_element_interactions -->
<!-- svelte-ignore a11y_click_events_have_key_events -->
<div class="inline-name" onpointerdown={stop} onmousedown={stop} onclick={stop} ondblclick={stop} oncontextmenu={stop}>
  <input
    bind:this={input}
    bind:value
    class:bad={!!message}
    {placeholder}
    disabled={pending}
    spellcheck="false"
    autocomplete="off"
    aria-label={placeholder || 'Name'}
    aria-invalid={!!message}
    oninput={() => { typed = true; }}
    onkeydown={onKeydown}
    onblur={onBlur}
  />
  {#if message}<div class="msg" role="alert">{message}</div>{/if}
</div>

<style>
  .inline-name {
    position: relative;
    flex: 1;
    min-width: 0;
    height: 100%;
    display: flex;
    align-items: center;
  }

  input {
    width: 100%;
    min-width: 0;
    height: calc(100% - 2px);
    box-sizing: border-box;
    padding: 0 4px;
    border: 1px solid var(--accent);
    border-radius: 3px;
    background: var(--bg-dark);
    color: var(--fg);
    font: inherit;
    outline: none;
  }

  input.bad {
    border-color: var(--red);
  }

  /* Under the input, over the rows below it: the row height is fixed. Wraps rather than
     clipping, since the column can be 200 px wide. */
  .msg {
    position: absolute;
    top: 100%;
    left: 0;
    right: 0;
    z-index: 5;
    padding: 3px 6px;
    border: 1px solid var(--red);
    border-radius: 0 0 3px 3px;
    background: var(--bg-dark);
    color: var(--fg);
    font-size: 0.9em;
    line-height: 1.35;
    overflow-wrap: anywhere;
  }
</style>
