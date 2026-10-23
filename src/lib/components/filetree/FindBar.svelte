<script lang="ts">
  /**
   * Find (docs/file-tree.md §8): opened by `/` or Cmd/Ctrl+F while the tree has focus.
   *
   * Every keystroke asks Rust (`ftSearch`, fuzzy over the hub's path index) and the tree
   * shows the answer as a pruned tree: the matches, their folders open, the matched letters
   * marked. The input keeps the keyboard: ↑/↓ move the tree's focus over the results, Enter
   * opens the focused file (or opens/closes a folder), Escape closes the bar and puts the
   * tree back exactly as it was: expansion, scroll and selection.
   */
  import { tick } from 'svelte';
  import { fileTreeStore } from '$lib/stores/fileTree.svelte';
  import Icon from '$lib/components/Icon.svelte';
  import IconButton from '$lib/components/ui/IconButton.svelte';
  import { isMac } from '$lib/utils/platform';

  const FIND_LIMIT = 500;
  const mac = isMac();

  let input = $state<HTMLInputElement | null>(null);
  const f = $derived(fileTreeStore.find);

  // Each `/` or Cmd+F (also with the bar already open) puts the keyboard here, text selected.
  // tick, not rAF (stalls in an occluded webview), and never `autofocus` (CLAUDE.md).
  $effect(() => {
    void fileTreeStore.findFocusSeq;
    if (!input) return;
    void tick().then(() => {
      input?.focus({ preventScroll: true });
      input?.select();
    });
  });

  const status = $derived.by(() => {
    if (!f || !f.query.trim()) return '';
    if (f.unavailable) return '';
    if (f.pending || f.indexing) return 'indexing…';
    const n = f.hits.length;
    if (n === 0) return 'no matches';
    return n >= FIND_LIMIT ? `best ${FIND_LIMIT}` : `${n} file${n === 1 ? '' : 's'}`;
  });

  function onKeydown(e: KeyboardEvent) {
    if (e.isComposing) return;
    const mod = mac ? e.metaKey : e.ctrlKey;
    if (mod && !e.shiftKey && !e.altKey && e.key.toLowerCase() === 'f') {
      e.preventDefault();
      input?.select();
      return;
    }
    if (mod || e.altKey) return;
    switch (e.key) {
      case 'ArrowDown':
        e.preventDefault();
        fileTreeStore.findMove(1);
        break;
      case 'ArrowUp':
        e.preventDefault();
        fileTreeStore.findMove(-1);
        break;
      case 'Enter':
        e.preventDefault();
        fileTreeStore.findActivate();
        break;
      case 'Escape':
        e.preventDefault();
        e.stopPropagation();
        fileTreeStore.closeFind();
        break;
    }
  }
</script>

<div class="find" role="search">
  <span class="ic" aria-hidden="true"><Icon name="search" size={12} /></span>
  <input
    bind:this={input}
    type="text"
    value={f?.query ?? ''}
    oninput={(e) => fileTreeStore.setFindQuery(e.currentTarget.value)}
    onkeydown={onKeydown}
    placeholder="Find a file"
    aria-label="Find a file"
    spellcheck="false"
    autocomplete="off"
    autocapitalize="off"
  />
  {#if status}<span class="status" aria-live="polite">{status}</span>{/if}
  <IconButton tooltip="Close (Esc)" size={20} onclick={() => fileTreeStore.closeFind()}>
    <Icon name="close" size={11} />
  </IconButton>
</div>
{#if f?.unavailable && f.query.trim()}
  <div class="unavailable">
    <span>{f.unavailable}</span>
    <button class="link" onclick={() => fileTreeStore.indexShallow()}>Index this folder</button>
  </div>
{/if}

<style>
  .find {
    flex-shrink: 0;
    display: flex;
    align-items: center;
    gap: 6px;
    margin: 4px 8px;
    padding: 0 2px 0 8px;
    height: 26px;
    border: 1px solid var(--bg-light);
    border-radius: 5px;
    background: var(--bg-dark);
  }

  .find:focus-within {
    border-color: color-mix(in srgb, var(--accent) 70%, transparent);
  }

  .ic {
    display: inline-flex;
    color: var(--fg-dim);
  }

  input {
    flex: 1;
    min-width: 0;
    background: none;
    border: none;
    outline: none;
    color: var(--fg);
    font: inherit;
    padding: 0;
  }

  .status {
    flex-shrink: 0;
    color: var(--fg-dim);
    font-size: 0.85em;
  }

  .unavailable {
    flex-shrink: 0;
    display: flex;
    flex-wrap: wrap;
    gap: 4px 8px;
    padding: 0 10px 6px;
    color: var(--fg-dim);
    font-size: 0.88em;
    overflow-wrap: anywhere;
  }

  .link {
    background: none;
    border: none;
    padding: 0;
    color: var(--accent);
    cursor: pointer;
    font: inherit;
  }

  .link:hover {
    text-decoration: underline;
  }
</style>
