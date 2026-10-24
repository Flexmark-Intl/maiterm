<script lang="ts">
  /** Run on ▸ Other branch… (docs/hive.md §6.9): every branch Rust listed, newest commit
   *  first, filtered as you type. A name that isn't listed can still be typed: Rust checks it
   *  (the branch must exist, and its name must be shell-inert) when the copy resolves, and the
   *  refusal comes back as a toast. Follows ServiceModal's backdrop/panel vocabulary and its
   *  explicit rAF focus (Svelte's `autofocus` is not focus). */
  import type { CopyTarget } from '$lib/tauri/commands';

  interface Props {
    serviceName: string;
    /** The `kind: 'branch'` rows of `stack_copy_targets`. */
    targets: CopyTarget[];
    onpick: (target: string) => void;
    oncancel: () => void;
  }

  let { serviceName, targets, onpick, oncancel }: Props = $props();

  let query = $state('');
  let inputEl = $state<HTMLInputElement | null>(null);

  $effect(() => {
    const id = requestAnimationFrame(() => inputEl?.focus());
    return () => cancelAnimationFrame(id);
  });

  const shown = $derived.by(() => {
    const q = query.trim().toLowerCase();
    return q ? targets.filter((t) => t.label.toLowerCase().includes(q)) : targets;
  });
  const typed = $derived(query.trim());
  const exact = $derived(targets.some((t) => t.label === typed));

  function pickTyped() {
    if (shown.length === 1 && !exact) onpick(shown[0].target);
    else if (typed) onpick(exact ? targets.find((t) => t.label === typed)!.target : `branch:${typed}`);
  }

  function handleKeydown(e: KeyboardEvent) {
    if (e.key === 'Escape') {
      e.stopPropagation();
      oncancel();
    }
  }
</script>

<!-- svelte-ignore a11y_no_noninteractive_tabindex -->
<div
  class="backdrop"
  onclick={(e) => { if (e.target === e.currentTarget) oncancel(); }}
  onkeydown={handleKeydown}
  role="dialog"
  aria-modal="true"
  aria-label="Run {serviceName} on a branch"
  tabindex="-1"
>
  <div class="panel">
    <div class="header">
      <div class="title">Run {serviceName} on…</div>
      <div class="subtitle">A branch with no checkout gets one of its own, so your folders are never switched.</div>
    </div>
    <input
      class="text-input mono"
      bind:this={inputEl}
      bind:value={query}
      placeholder="branch name"
      spellcheck="false"
      onkeydown={(e) => { if (e.key === 'Enter') { e.preventDefault(); pickTyped(); } }}
    />
    <div class="list">
      {#each shown as t (t.target)}
        <button type="button" class="item mono" onclick={() => onpick(t.target)}>{t.label}</button>
      {:else}
        <p class="empty">{typed ? `Enter runs it on ${typed}, if that branch exists.` : 'No branches.'}</p>
      {/each}
    </div>
  </div>
</div>

<style>
  .backdrop {
    position: fixed;
    inset: 0;
    background: rgba(0, 0, 0, 0.4);
    display: flex;
    justify-content: center;
    padding-top: 12vh;
    z-index: 1000;
  }
  .panel {
    align-self: flex-start;
    background: var(--bg-medium);
    border: 1px solid var(--bg-light);
    border-radius: 8px;
    box-shadow: 0 8px 32px rgba(0, 0, 0, 0.5);
    display: flex;
    flex-direction: column;
    gap: 10px;
    max-height: 70vh;
    max-width: calc(100vw - 32px);
    padding: 12px 14px;
    width: 420px;
  }
  .title { color: var(--fg); font-size: 1rem; font-weight: 600; overflow-wrap: anywhere; }
  .subtitle { color: var(--fg-dim); font-size: 0.8rem; line-height: 1.4; margin-top: 3px; }
  .text-input {
    background: var(--bg-dark);
    border: 1px solid var(--bg-light);
    border-radius: 5px;
    color: var(--fg);
    font-size: 0.85rem;
    padding: 7px 9px;
    width: 100%;
  }
  .text-input:focus { border-color: var(--accent); outline: none; }
  .mono { font-family: var(--font-mono, ui-monospace, monospace); font-size: 0.8rem; }
  .list { display: flex; flex-direction: column; overflow-y: auto; min-height: 0; }
  .item {
    background: none;
    border: none;
    border-radius: 4px;
    color: var(--fg);
    cursor: pointer;
    overflow-wrap: anywhere;
    padding: 5px 8px;
    text-align: left;
  }
  .item:hover, .item:focus-visible { background: var(--bg-light); outline: none; }
  .empty { color: var(--fg-dim); font-size: 0.8rem; margin: 4px 8px; }
</style>
