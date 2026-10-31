<script lang="ts">
  /** The current goal's commits that aren't landed yet (docs/hive.md §11.5): the headline, one row
   *  per branch, why it matters, and the one thing to do about it, Ask the Queen to land them. The
   *  same block on the layer over the workers and on the New goal sheet. */
  import { hiveAskQueenToLand } from '$lib/tauri/commands';
  import type { HivePendingBranch } from '$lib/tauri/types';
  import { pendingHeadline, pendingRow } from '$lib/hive/newGoal';
  import { error as logError } from '@tauri-apps/plugin-log';

  interface Props {
    workspaceId: string;
    unlanded: HivePendingBranch[];
  }
  let { workspaceId, unlanded }: Props = $props();

  let asking = $state<'idle' | 'sending' | 'sent' | 'failed'>('idle');
  let askError = $state('');

  async function ask() {
    if (asking === 'sending') return;
    asking = 'sending';
    askError = '';
    try {
      await hiveAskQueenToLand(workspaceId);
      asking = 'sent';
    } catch (e) {
      asking = 'failed';
      askError = String(e).replace(/^Error: /, '');
      logError(`hive: ask-to-land for ${workspaceId.slice(0, 8)} failed: ${e}`);
    }
  }
</script>

<div class="pending" role="group" aria-label="Commits not landed">
  <div class="head"><span class="dot" aria-hidden="true"></span>{pendingHeadline(unlanded)}</div>
  <ul>
    {#each unlanded as b (b.folder + '\u0000' + b.branch)}
      {@const r = pendingRow(b)}
      <li>
        <span class="who">{r.worker}</span>
        <span class="where"><code>{r.place}</code></span>
        <span class="ahead">{r.ahead}</span>
      </li>
    {/each}
  </ul>
  <p class="why">A new goal needs them landed first, so no finished work is left behind.</p>
  <div class="act">
    <button type="button" class="ask" disabled={asking === 'sending'} onclick={() => void ask()}>
      {asking === 'sending' ? 'Asking…' : asking === 'sent' ? 'Ask the Queen again' : 'Ask the Queen to land them'}
    </button>
    {#if asking === 'sent'}
      <span class="sent" role="status">Sent: the Queen is landing them. This updates as they land.</span>
    {:else if asking === 'failed'}
      <span class="failed" role="alert">Not sent: {askError}</span>
    {/if}
  </div>
</div>

<style>
  .pending { display: flex; flex-direction: column; gap: 8px; min-width: 0; }
  .head { display: flex; align-items: center; gap: 8px; color: var(--fg); font-size: 14px; font-weight: 600; overflow-wrap: anywhere; }
  .dot { flex: none; width: 8px; height: 8px; border-radius: 50%; background: var(--yellow); }
  ul { margin: 0; padding: 0; list-style: none; display: flex; flex-direction: column; gap: 6px; }
  li { display: flex; flex-wrap: wrap; align-items: baseline; gap: 2px 10px; padding: 6px 8px; border: 1px solid var(--bg-light); border-radius: 5px; background: color-mix(in srgb, var(--bg-dark) 70%, transparent); font-size: 12px; min-width: 0; }
  .who { color: var(--fg); font-weight: 500; overflow-wrap: anywhere; }
  .where { color: var(--fg-dim); overflow-wrap: anywhere; min-width: 0; }
  .ahead { margin-left: auto; color: var(--yellow); font-variant-numeric: tabular-nums; }
  code { font-family: var(--font-mono, ui-monospace, monospace); font-size: 0.92em; }
  .why { margin: 0; color: var(--fg-dim); font-size: 12px; line-height: 1.4; overflow-wrap: anywhere; }
  .act { display: flex; flex-wrap: wrap; align-items: center; gap: 6px 10px; }
  .ask { height: 26px; padding: 0 12px; border: 1px solid var(--accent); border-radius: 5px; background: var(--accent); color: var(--bg-dark); font: inherit; font-size: 12px; font-weight: 600; cursor: pointer; }
  .ask:hover:not(:disabled) { filter: brightness(1.08); }
  .ask:focus-visible { outline: 2px solid var(--accent); outline-offset: 2px; }
  .ask:disabled { cursor: default; opacity: 0.55; }
  .sent { color: var(--green); font-size: 12px; overflow-wrap: anywhere; }
  .failed { color: var(--red); font-size: 12px; overflow-wrap: anywhere; }
</style>
