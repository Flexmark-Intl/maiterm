<script lang="ts">
  /**
   * The Lens bar (docs/file-tree.md §9.4): All · Changed · Turn · Session. Every lens but All
   * draws a pruned tree, and leaving one puts the real tree back exactly as it was. Turn and
   * Session follow the chosen agent chip, or every agent in the root when none is chosen.
   * Shown once git or an agent gives it something to show. Lenses switch by click (the `1`–`4`
   * keys are vim mode's only).
   */
  import { fileTreeStore, type Lens } from '$lib/stores/fileTree.svelte';
  import Tooltip from '$lib/components/Tooltip.svelte';

  const counts = $derived(fileTreeStore.lensCounts);
  const filterName = $derived(fileTreeStore.agentFilter ? fileTreeStore.tabName(fileTreeStore.agentFilter) : null);

  const lenses = $derived.by((): { id: Lens; label: string; count: number | null; tip: string; disabled: boolean }[] => {
    const who = filterName ? `“${filterName}”` : 'the agents in this folder';
    return [
      { id: 'all', label: 'All', count: null, tip: 'The whole folder', disabled: false },
      {
        id: 'changed',
        label: 'Changed',
        count: fileTreeStore.gitOn ? fileTreeStore.gitCount : null,
        tip: fileTreeStore.gitOn ? 'Files git reports as changed' : 'Git decorations are off (Preferences → Files)',
        disabled: !fileTreeStore.gitOn,
      },
      { id: 'turn', label: 'Turn', count: counts.turn, tip: `What ${who} changed this turn`, disabled: false },
      { id: 'session', label: 'Session', count: counts.session, tip: `What ${who} changed this session`, disabled: false },
    ];
  });
</script>

<div class="lens-bar" role="radiogroup" aria-label="Lens">
  {#each lenses as l (l.id)}
    <Tooltip text={l.tip}>
      <button
        class="lens"
        class:on={fileTreeStore.lens === l.id}
        role="radio"
        aria-checked={fileTreeStore.lens === l.id}
        disabled={l.disabled}
        onclick={() => { fileTreeStore.humanTouched(); fileTreeStore.setLens(l.id); }}
      >
        {l.label}{#if l.count}<span class="count">{l.count.toLocaleString()}</span>{/if}
      </button>
    </Tooltip>
  {/each}
  {#if filterName}
    <Tooltip text={`Turn and Session show only ${filterName}: click to show every agent`}>
      <button class="filter" onclick={() => fileTreeStore.chooseAgent(null)}>{filterName} ×</button>
    </Tooltip>
  {/if}
</div>

<style>
  .lens-bar {
    flex-shrink: 0;
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: 2px;
    padding: 3px 6px;
    border-bottom: 1px solid var(--bg-light);
    font-size: 0.85em;
  }

  .lens {
    padding: 1px 7px;
    border-radius: 4px;
    color: var(--fg-dim);
  }

  .lens:hover:not(:disabled) {
    color: var(--fg);
    background: var(--bg-light);
  }

  .lens.on {
    color: var(--fg);
    background: color-mix(in srgb, var(--accent) 22%, transparent);
  }

  .lens:disabled {
    opacity: 0.5;
    cursor: default;
  }

  .count {
    margin-left: 4px;
    font-variant-numeric: tabular-nums;
    color: var(--fg-dim);
  }

  .filter {
    margin-left: auto;
    max-width: 50%;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    padding: 1px 6px;
    border-radius: 4px;
    color: var(--fg-dim);
  }

  .filter:hover {
    color: var(--fg);
    background: var(--bg-light);
  }
</style>
