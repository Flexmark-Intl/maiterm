<script lang="ts">
  /** A stop, restart or move the human asked for on a copy a queued run depends on (docs/hive.md
   *  §6.8). The human is never queued without being told: Run when free waits in Rust for the run
   *  to end, Stop anyway interrupts it (never a receipt) and goes ahead now. Inline beside what
   *  was clicked, since `confirm()` doesn't exist in a webview. */
  import { stackStore, type LeaseAsk } from '$lib/stores/stack.svelte';
  import { toastStore } from '$lib/stores/toasts.svelte';
  import { error as logError } from '@tauri-apps/plugin-log';

  interface Props {
    ask: LeaseAsk;
    onclose: () => void;
  }

  let { ask, onclose }: Props = $props();
  let busy = $state(false);

  const verb = $derived(ask.action === 'retarget' ? 'move' : ask.action);

  async function choose(anyway: boolean) {
    if (busy) return;
    busy = true;
    try {
      if (anyway) {
        await stackStore.stopAnyway(ask.workspaceId, ask.serviceId, ask.target, ask.action, ask.to);
      } else {
        const held = await stackStore.runWhenFree(ask.workspaceId, ask.serviceId, ask.target, ask.action, ask.to);
        toastStore.addToast(`The ${verb} waits for the run`, `It happens when ${held.job} ends.`, 'info');
      }
      onclose();
    } catch (e) {
      busy = false;
      logError(`stack: ${verb} ${ask.serviceId} (${anyway ? 'anyway' : 'when free'}): ${e}`);
      toastStore.addToast(`Couldn't ${verb} it`, String(e).replace(/^Error: /, ''), 'error');
    }
  }
</script>

<div class="lease-ask" role="group" aria-label="A queued run is using this service">
  <p class="msg">{ask.message}</p>
  <div class="choices">
    <button type="button" class="choice" disabled={busy} onclick={() => choose(false)}>Run when free</button>
    <button type="button" class="choice danger" disabled={busy} onclick={() => choose(true)}>Stop anyway</button>
    <button type="button" class="choice quiet" disabled={busy} onclick={onclose}>Cancel</button>
  </div>
</div>

<style>
  .lease-ask {
    background: var(--bg-medium);
    border: 1px solid var(--bg-light);
    border-radius: 6px;
    box-shadow: 0 4px 14px rgba(0, 0, 0, 0.3);
    color: var(--fg);
    font-size: 0.75rem;
    padding: 8px 10px;
  }

  .msg {
    margin: 0 0 6px;
    overflow-wrap: anywhere;
  }

  .choices {
    display: flex;
    flex-wrap: wrap;
    gap: 6px;
  }

  .choice {
    background: var(--bg-light);
    border: none;
    border-radius: 4px;
    color: var(--fg);
    cursor: pointer;
    font-family: inherit;
    font-size: 0.72rem;
    padding: 3px 8px;
  }
  .choice:hover:not(:disabled) { background: var(--accent); color: var(--bg-dark); }
  .choice.danger:hover:not(:disabled) { background: var(--error, #f7768e); }
  .choice.quiet { background: none; color: var(--fg-dim); }
  .choice:disabled { cursor: default; opacity: 0.6; }
</style>
