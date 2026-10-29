<script lang="ts">
  /** Hive settings → Autonomy (docs/hive.md §7.4): each checkpoint Coordinator or Ask me, what
   *  never moves to the coordinator, the prompt timeout and the daily digest. Rust checks and saves
   *  it (`hive_set_autonomy`), tells the coordinator, and moves a prompt it holds under a checkpoint
   *  now Ask me to the human; the window's Hive record follows. RunQueueModal's vocabulary, and the
   *  same explicit rAF focus. */
  import type { Hive } from '$lib/tauri/types';
  import { hiveSetAutonomy } from '$lib/tauri/commands';
  import { workspacesStore } from '$lib/stores/workspaces.svelte';
  import { CHECKPOINT_ROWS, formFor, lockedReason, NEVER_DELEGATED, TIMEOUT_MAX_MINUTES, TIMEOUT_MIN_MINUTES, toSave, validateForm, type AutonomyForm, type CheckpointValue } from '$lib/hive/autonomy';
  import Tooltip from '$lib/components/Tooltip.svelte';
  import { modLabel } from '$lib/utils/platform';
  import { error as logError } from '@tauri-apps/plugin-log';

  interface Props {
    workspaceId: string;
    hive: Hive;
    onclose: () => void;
  }

  let { workspaceId, hive, onclose }: Props = $props();

  // A snapshot taken when it opens: a Hive record changed underneath doesn't yank the fields.
  const seed = (() => formFor(hive))();
  let values = $state<Record<string, CheckpointValue>>(seed.values);
  let minutes = $state(seed.minutes);
  let digest = $state(seed.digest);
  let saving = $state(false);
  let refusal = $state<string | null>(null);
  let panelEl = $state<HTMLDivElement | null>(null);

  $effect(() => {
    const id = requestAnimationFrame(() => panelEl?.querySelector<HTMLElement>('input:not(:disabled), button')?.focus());
    return () => cancelAnimationFrame(id);
  });

  const form = $derived<AutonomyForm>({ values, minutes: Number(minutes), digest });
  const problem = $derived(validateForm(hive, form));
  const canSave = $derived(!saving && problem === null);

  async function save() {
    if (!canSave) return;
    saving = true;
    refusal = null;
    try {
      const { checkpoints, autonomy } = toSave(hive, form);
      const saved = await hiveSetAutonomy(workspaceId, checkpoints, autonomy);
      workspacesStore.applyHiveRecord(workspaceId, saved);
      onclose();
    } catch (e) {
      saving = false;
      refusal = String(e).replace(/^Error: /, '');
      logError(`hive: autonomy not saved: ${e}`);
    }
  }

  function handleKeydown(e: KeyboardEvent) {
    if (e.key === 'Escape') {
      e.stopPropagation();
      onclose();
      return;
    }
    if (e.key === 'Enter' && (e.metaKey || e.ctrlKey)) {
      e.preventDefault();
      void save();
    }
  }

  function handleBackdropClick(e: MouseEvent) {
    if (e.target === e.currentTarget) onclose();
  }
</script>

<!-- svelte-ignore a11y_no_noninteractive_tabindex -->
<div class="backdrop" onclick={handleBackdropClick} onkeydown={handleKeydown} role="dialog" aria-modal="true" aria-label="Autonomy" tabindex="-1">
  <div class="panel" bind:this={panelEl}>
    <div class="header">
      <div class="title">Autonomy</div>
      <div class="subtitle">
        The coordinator decides everything it can; you're asked where a checkpoint says Ask me. Every decision is logged, and the
        coordinator is told when you change these.
      </div>
    </div>

    <div class="body">
      <div class="rows" role="group" aria-label="Checkpoints">
        {#each CHECKPOINT_ROWS as row (row.key)}
          {@const locked = lockedReason(hive, row.key)}
          <div class="row">
            <div class="what">
              <span class="name">{row.label}</span>
              <span class="note">{row.note}</span>
            </div>
            <div class="seg" role="radiogroup" aria-label={row.label}>
              {#each [['coordinator', 'Queen'], ['ask', 'Ask me']] as [v, text] (v)}
                {#if locked && v === 'coordinator'}
                  <Tooltip text={locked}>
                    <button type="button" role="radio" aria-checked="false" disabled>{text}</button>
                  </Tooltip>
                {:else}
                  <button
                    type="button"
                    role="radio"
                    aria-checked={values[row.key] === v}
                    class:on={values[row.key] === v}
                    onclick={() => (values = { ...values, [row.key]: v as CheckpointValue })}
                  >{text}</button>
                {/if}
              {/each}
            </div>
          </div>
        {/each}
      </div>

      <div class="never">
        <span class="label">Always yours, whatever the settings</span>
        <ul>
          {#each NEVER_DELEGATED as n (n)}<li>{n}</li>{/each}
        </ul>
      </div>

      <div class="line">
        <label class="field">
          <span class="label">Prompt timeout <span class="opt">minutes, {TIMEOUT_MIN_MINUTES}–{TIMEOUT_MAX_MINUTES}: a prompt the coordinator hasn't answered by then is yours too, and you're told once</span></span>
          <input class="text-input num" type="number" min={TIMEOUT_MIN_MINUTES} max={TIMEOUT_MAX_MINUTES} step="1" bind:value={minutes} />
        </label>
        <label class="check">
          <input type="checkbox" bind:checked={digest} />
          <span>Daily digest <span class="opt">what the queen decided alone, once a day in its chat</span></span>
        </label>
      </div>
      {#if problem}<p class="error">{problem}</p>{/if}
      {#if refusal}<p class="error" role="alert">{refusal}</p>{/if}
    </div>

    <div class="footer">
      <span class="hint">{modLabel}+Enter to save</span>
      <div class="actions">
        <button type="button" class="btn" onclick={onclose}>Cancel</button>
        <button type="button" class="btn primary" disabled={!canSave} onclick={() => void save()}>Save</button>
      </div>
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
    padding-top: 10vh;
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
    max-height: 80vh;
    max-width: calc(100vw - 32px);
    width: 640px;
  }

  .header { border-bottom: 1px solid var(--bg-light); padding: 12px 14px 10px; }
  .title { color: var(--fg); font-size: 1rem; font-weight: 600; }
  .subtitle { color: var(--fg-dim); font-size: 0.8rem; line-height: 1.4; margin-top: 3px; }

  .body { display: flex; flex-direction: column; gap: 14px; overflow-y: auto; padding: 14px; }
  .rows { display: flex; flex-direction: column; gap: 8px; }
  .row { align-items: center; border: 1px solid var(--bg-light); border-radius: 6px; display: flex; flex-wrap: wrap; gap: 8px 12px; justify-content: space-between; padding: 8px 10px; }
  .what { display: flex; flex: 1 1 260px; flex-direction: column; gap: 2px; min-width: 0; }
  .name { color: var(--fg); font-size: 0.85rem; font-weight: 600; overflow-wrap: anywhere; }
  .note { color: var(--fg-dim); font-size: 0.75rem; line-height: 1.35; overflow-wrap: anywhere; }

  .seg { border: 1px solid var(--bg-light); border-radius: 5px; display: flex; flex: none; overflow: hidden; }
  .seg button {
    background: var(--bg-dark);
    border: none;
    color: var(--fg-dim);
    cursor: pointer;
    font-family: inherit;
    font-size: 0.78rem;
    padding: 5px 10px;
  }
  .seg button.on { background: var(--accent); color: var(--bg-dark); font-weight: 600; }
  .seg button:focus-visible { outline: 2px solid var(--accent); outline-offset: -2px; }
  .seg button:disabled { cursor: default; opacity: 0.45; }

  .never { background: var(--bg-dark); border-radius: 5px; padding: 8px 10px; }
  .never ul { color: var(--fg); font-size: 0.78rem; line-height: 1.45; margin: 4px 0 0; padding-left: 18px; }

  .line { display: flex; flex-direction: column; gap: 10px; }
  .field { display: flex; flex-direction: column; gap: 5px; min-width: 0; }
  .label { color: var(--fg-dim); font-size: 0.72rem; font-weight: 600; letter-spacing: 0.06em; text-transform: uppercase; }
  .opt { font-weight: 400; letter-spacing: 0; opacity: 0.7; overflow-wrap: anywhere; text-transform: none; }
  .check { align-items: baseline; color: var(--fg); display: flex; font-size: 0.85rem; gap: 7px; }

  .text-input {
    background: var(--bg-dark);
    border: 1px solid var(--bg-light);
    border-radius: 5px;
    color: var(--fg);
    font-family: inherit;
    font-size: 0.85rem;
    padding: 7px 9px;
  }
  .text-input.num { width: 90px; }
  .text-input:focus { border-color: var(--accent); outline: none; }

  .error { color: var(--red); font-size: 0.72rem; margin: 0; overflow-wrap: anywhere; }

  .footer { align-items: center; border-top: 1px solid var(--bg-light); display: flex; gap: 10px; justify-content: space-between; padding: 10px 14px; }
  .hint { color: var(--fg-dim); font-size: 0.72rem; }
  .actions { display: flex; gap: 7px; }

  .btn {
    background: var(--bg-dark);
    border: 1px solid var(--bg-light);
    border-radius: 5px;
    color: var(--fg);
    cursor: pointer;
    font-family: inherit;
    font-size: 0.8rem;
    padding: 6px 12px;
  }
  .btn:hover:not(:disabled) { border-color: var(--fg-dim); }
  .btn:focus-visible { outline: 2px solid var(--accent); outline-offset: 1px; }
  .btn.primary { background: var(--accent); border-color: var(--accent); color: var(--bg-dark); font-weight: 600; }
  .btn:disabled { cursor: default; opacity: 0.45; }
</style>
