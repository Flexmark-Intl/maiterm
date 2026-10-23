<script lang="ts">
  /**
   * The agent strip (docs/file-tree.md §3.2 item 3): one chip per agent tab working in the
   * root, in that tab's colour (the same one its rails and dots use), with its state and the
   * files it changed this turn. Choosing a chip scopes the Turn and Session lenses to it; with
   * a chip chosen, "Follow Agent" in the ⋯ menu follows its writes (shown here as an eye).
   */
  import { fileTreeStore } from '$lib/stores/fileTree.svelte';
  import Tooltip from '$lib/components/Tooltip.svelte';
  import Icon from '$lib/components/Icon.svelte';

  const chips = $derived(fileTreeStore.agentChips);

  function stateWord(s: string | null): string {
    switch (s) {
      case 'active': return 'working';
      case 'permission': return 'waiting for you';
      case 'idle': return 'idle';
      default: return 'no session';
    }
  }

  function tip(c: (typeof chips)[number]): string {
    const chosen = fileTreeStore.agentFilter === c.tabId;
    const changed = `${c.changed} file${c.changed === 1 ? '' : 's'} changed this turn`;
    const follow = fileTreeStore.followAgent === c.tabId ? '\nFiles follows its writes (⋯ menu to stop)' : '';
    return `${c.name} · ${stateWord(c.state)} · ${changed}\n${chosen ? 'Click to show every agent in Turn and Session' : 'Click to scope Turn and Session to this tab'}${follow}`;
  }
</script>

{#if chips.length}
  <div class="strip" role="group" aria-label="Agents in this folder">
    {#each chips as c (c.tabId)}
      <Tooltip text={tip(c)}>
        <button
          class="chip"
          class:chosen={fileTreeStore.agentFilter === c.tabId}
          style="--c: var(--ft-agent-{c.colour})"
          aria-pressed={fileTreeStore.agentFilter === c.tabId}
          onclick={() => { fileTreeStore.humanTouched(); fileTreeStore.chooseAgent(c.tabId); }}
        >
          <span class="state s-{c.state ?? 'none'}" aria-hidden="true"></span>
          <span class="name">{c.name}</span>
          {#if c.changed}<span class="count">{c.changed}</span>{/if}
          {#if fileTreeStore.followAgent === c.tabId}<span class="eye" aria-label="followed"><Icon name="eye" size={11} /></span>{/if}
        </button>
      </Tooltip>
    {/each}
  </div>
{/if}

<style>
  .strip {
    flex-shrink: 0;
    display: flex;
    flex-wrap: wrap;
    gap: 4px;
    padding: 4px 6px;
    border-bottom: 1px solid var(--bg-light);
  }

  .chip {
    display: inline-flex;
    align-items: center;
    gap: 5px;
    max-width: 100%;
    padding: 1px 7px 1px 5px;
    border-radius: 10px;
    border: 1px solid color-mix(in srgb, var(--c) 55%, transparent);
    background: color-mix(in srgb, var(--c) 10%, transparent);
    color: var(--fg);
    font-size: 0.85em;
  }

  .chip:hover {
    background: color-mix(in srgb, var(--c) 20%, transparent);
  }

  .chip.chosen {
    background: color-mix(in srgb, var(--c) 32%, transparent);
    border-color: var(--c);
  }

  /* The agent's colour; the state rides on its fill: working solid, waiting ringed, idle hollow. */
  .state {
    width: 7px;
    height: 7px;
    border-radius: 50%;
    flex-shrink: 0;
    box-sizing: border-box;
    border: 1.5px solid var(--c);
  }

  .s-active {
    background: var(--c);
  }

  .s-permission {
    background: var(--yellow);
    border-color: var(--yellow);
  }

  /* Names ellipsize; the chip's tooltip carries the whole name. */
  .name {
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  .count {
    font-variant-numeric: tabular-nums;
    color: var(--fg-dim);
  }

  .eye {
    display: inline-flex;
    color: var(--c);
  }
</style>
