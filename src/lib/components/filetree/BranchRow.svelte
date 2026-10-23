<script lang="ts">
  /**
   * The branch row (docs/file-tree.md §3.2): `⎇ main ↑2 ↓0 · 7 changed`, the short commit on a
   * detached HEAD. Drawn only inside a repository with git decorations on. A click on the
   * name copies it; a click on "N changed" turns the Changed lens on or off, which shows the
   * changed files as a pruned tree and leaves the real tree's expansion alone.
   */
  import { writeText as clipboardWriteText } from '@tauri-apps/plugin-clipboard-manager';
  import { fileTreeStore } from '$lib/stores/fileTree.svelte';
  import { toastStore } from '$lib/stores/toasts.svelte';
  import Tooltip from '$lib/components/Tooltip.svelte';

  const b = $derived(fileTreeStore.branch);
  const detached = $derived(!!b && b.head === '(detached)');
  const label = $derived(!b ? '' : detached ? b.oid.slice(0, 7) : b.head);
  const count = $derived(fileTreeStore.gitCount);
  const changedOn = $derived(fileTreeStore.lens === 'changed');

  async function copyName() {
    if (!label) return;
    try {
      await clipboardWriteText(label);
      toastStore.addToast(detached ? 'Commit copied' : 'Branch copied', label, 'success');
    } catch (e) {
      toastStore.addToast('Copy failed', String(e), 'error');
    }
  }

  const nameTip = $derived.by(() => {
    if (!b) return '';
    const what = detached ? `Detached at ${b.oid}` : b.oid === '(initial)' ? `${b.head}, no commits yet` : `${b.head} at ${b.oid.slice(0, 7)}`;
    const up = b.upstream ? `\ntracking ${b.upstream} · ${b.ahead} ahead, ${b.behind} behind` : '';
    return `${what}${up}\nclick to copy`;
  });
</script>

{#if b}
  <div class="branch">
    <Tooltip text={nameTip}>
      <button class="name" onclick={copyName}>
        <span class="glyph" aria-hidden="true">⎇</span>
        <span class="label" class:mono={detached}>{label}</span>
      </button>
    </Tooltip>
    {#if b.upstream && (b.ahead || b.behind)}
      <span class="ab" aria-label="{b.ahead} ahead, {b.behind} behind">↑{b.ahead} ↓{b.behind}</span>
    {/if}
    {#if count > 0 || changedOn}
      <span class="dot" aria-hidden="true">·</span>
      <Tooltip text={changedOn ? 'Show the whole tree again' : 'Show only the changed files'}>
        <button class="changed" class:on={changedOn} aria-pressed={changedOn} onclick={() => fileTreeStore.toggleChanged()}>
          {count.toLocaleString()} changed
        </button>
      </Tooltip>
    {/if}
  </div>
{/if}

<style>
  .branch {
    flex-shrink: 0;
    display: flex;
    align-items: center;
    flex-wrap: wrap;
    gap: 0 6px;
    min-height: 20px;
    padding: 0 10px;
    font-size: 0.85em;
    color: var(--fg-dim);
    border-bottom: 1px solid color-mix(in srgb, var(--bg-light) 50%, transparent);
  }

  button {
    background: none;
    border: none;
    padding: 0;
    color: inherit;
    font: inherit;
    cursor: pointer;
  }

  .name {
    display: inline-flex;
    align-items: center;
    gap: 4px;
    min-width: 0;
    color: var(--fg);
  }

  /* A long branch name wraps rather than clipping to the same prefix as its neighbour. */
  .label {
    overflow-wrap: anywhere;
    text-align: left;
  }

  .label.mono {
    font-family: var(--font-mono, ui-monospace, monospace);
  }

  .glyph {
    color: var(--fg-dim);
  }

  .name:hover .label,
  .changed:hover {
    text-decoration: underline;
  }

  .changed.on {
    color: var(--accent);
  }
</style>
