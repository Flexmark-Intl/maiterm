<script lang="ts">
  /** Hive settings → Run queue (docs/hive.md §6.8): which commands a worker runs only through
   *  `hiveRun`, what each needs, and the Hive's own resource limits. Rust checks and saves it
   *  (`hive_set_run_queue`) and the window's Hive record follows. Same backdrop/panel vocabulary
   *  as ServiceModal, and the same explicit rAF focus (Svelte's `autofocus` is not focus). */
  import type { Hive } from '$lib/tauri/types';
  import { hiveSetRunQueue } from '$lib/tauri/commands';
  import { workspacesStore } from '$lib/stores/workspaces.svelte';
  import { fromRows, prefixError, resourcesText, toRows, type QueueRow } from '$lib/hive/runQueue';
  import { modLabel } from '$lib/utils/platform';
  import { error as logError } from '@tauri-apps/plugin-log';

  interface Props {
    workspaceId: string;
    hive: Hive;
    onclose: () => void;
  }

  let { workspaceId, hive, onclose }: Props = $props();

  // A snapshot taken when it opens: a Hive record changed underneath doesn't yank the fields.
  function seed() {
    const rq = hive.settings?.run_queue;
    return { gitFolders: hive.folders.filter((f) => f.kind === 'git').map((f) => f.name), rows: toRows(rq), resources: resourcesText(rq) };
  }
  const s0 = seed();
  const gitFolders = s0.gitFolders;
  let rows = $state<QueueRow[]>(s0.rows);
  let resources = $state(s0.resources);
  let saving = $state(false);
  let refusal = $state<string | null>(null);
  let panelEl = $state<HTMLDivElement | null>(null);

  $effect(() => {
    const id = requestAnimationFrame(() => panelEl?.querySelector<HTMLElement>('input, select, button')?.focus());
    return () => cancelAnimationFrame(id);
  });

  const errors = $derived(rows.map((r) => prefixError(r.prefix)));
  const canSave = $derived(!saving && errors.every((e) => e === null));

  function add() {
    rows.push({ folder: gitFolders[0] ?? '', prefix: '', needs: '', services: '', env: '', extraWrite: '' });
  }

  function remove(i: number) {
    rows.splice(i, 1);
  }

  async function save() {
    if (!canSave) return;
    saving = true;
    refusal = null;
    try {
      const saved = await hiveSetRunQueue(workspaceId, fromRows(rows, resources));
      workspacesStore.applyHiveRecord(workspaceId, saved);
      onclose();
    } catch (e) {
      saving = false;
      refusal = String(e).replace(/^Error: /, '');
      logError(`hive: run queue not saved: ${e}`);
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
<div class="backdrop" onclick={handleBackdropClick} onkeydown={handleKeydown} role="dialog" aria-modal="true" aria-label="Run queue" tabindex="-1">
  <div class="panel" bind:this={panelEl}>
    <div class="header">
      <div class="title">Run queue</div>
      <div class="subtitle">
        Heavy commands a worker runs only through <code>hiveRun</code>: maiTerm queues each behind the machine's other runs, runs it on the
        worker's branch under its sandbox, and its exit code becomes a receipt. Its shell refuses them. Anything not listed runs freely.
      </div>
    </div>

    <div class="body">
      {#if gitFolders.length === 0}
        <p class="note">This Hive has no git folder, so nothing can be queued.</p>
      {/if}
      {#each rows as row, i (i)}
        <div class="entry">
          <div class="row">
            <label class="field">
              <span class="label">Folder</span>
              <select class="text-input" bind:value={row.folder}>
                {#each gitFolders as f (f)}<option value={f}>{f}</option>{/each}
              </select>
            </label>
            <label class="field grow">
              <span class="label">Command starts with</span>
              <input class="text-input mono" bind:value={row.prefix} placeholder="cargo test" spellcheck="false" />
            </label>
            <button type="button" class="btn quiet" aria-label="Remove this command" onclick={() => remove(i)}>Remove</button>
          </div>
          {#if errors[i]}<span class="error">{errors[i]}</span>{/if}
          <div class="row">
            <label class="field grow">
              <span class="label">Needs <span class="opt">comma-separated: cargo, tauri-dev, e2e, gpu, migrate:{'{worker}'}</span></span>
              <input class="text-input mono" bind:value={row.needs} placeholder="cargo" spellcheck="false" />
            </label>
            <label class="field grow">
              <span class="label">Uses services <span class="opt">held while it runs</span></span>
              <input class="text-input mono" bind:value={row.services} placeholder="web" spellcheck="false" />
            </label>
          </div>
          <div class="row">
            <label class="field grow">
              <span class="label">Environment <span class="opt">KEY=value per line; {'{worker}'}, {'{instance}'} (the worker's name in 32 or fewer), {'{port}'} fill in</span></span>
              <textarea class="text-input mono" rows="2" bind:value={row.env} placeholder={'MAITERM_INSTANCE={instance}'} spellcheck="false"></textarea>
            </label>
            <label class="field grow">
              <span class="label">Also writes <span class="opt">absolute paths, one per line</span></span>
              <textarea class="text-input mono" rows="2" bind:value={row.extraWrite} placeholder={'~/Library/Caches/app.{worker}'} spellcheck="false"></textarea>
            </label>
          </div>
        </div>
      {/each}
      <button type="button" class="btn" disabled={gitFolders.length === 0} onclick={add}>Add a queued command</button>

      <label class="field">
        <span class="label">Limits <span class="opt">optional, resource=count per line: lowers this Hive's share of cargo (2), tauri-dev, e2e or gpu (1 each), or sizes its own</span></span>
        <textarea class="text-input mono" rows="2" bind:value={resources} placeholder="cargo=1" spellcheck="false"></textarea>
      </label>
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

  .body { display: flex; flex-direction: column; gap: 12px; overflow-y: auto; padding: 14px; }
  .entry { border: 1px solid var(--bg-light); border-radius: 6px; display: flex; flex-direction: column; gap: 8px; padding: 10px; }
  .row { align-items: flex-end; display: flex; flex-wrap: wrap; gap: 10px; }
  .grow { flex: 1 1 200px; }

  .field { display: flex; flex-direction: column; gap: 5px; min-width: 0; }
  .label { color: var(--fg-dim); font-size: 0.72rem; font-weight: 600; letter-spacing: 0.06em; text-transform: uppercase; }
  .opt { font-weight: 400; letter-spacing: 0; opacity: 0.7; overflow-wrap: anywhere; text-transform: none; }

  .text-input {
    background: var(--bg-dark);
    border: 1px solid var(--bg-light);
    border-radius: 5px;
    color: var(--fg);
    font-family: inherit;
    font-size: 0.85rem;
    padding: 7px 9px;
    width: 100%;
  }
  .text-input.mono { font-family: var(--font-mono, ui-monospace, monospace); font-size: 0.8rem; }
  .text-input:focus { border-color: var(--accent); outline: none; }
  .text-input::placeholder { color: var(--fg-dim); }
  textarea.text-input { resize: vertical; }

  .error { color: var(--red); font-size: 0.72rem; margin: 0; overflow-wrap: anywhere; }
  .note { background: var(--bg-dark); border-radius: 5px; color: var(--fg); font-size: 0.8rem; line-height: 1.4; margin: 0; padding: 8px 10px; }
  code { font-family: var(--font-mono, ui-monospace, monospace); font-size: 0.9em; }

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
  .btn.quiet { background: none; border-color: transparent; color: var(--fg-dim); }
  .btn.primary { background: var(--accent); border-color: var(--accent); color: var(--bg-dark); font-weight: 600; }
  .btn:disabled { cursor: default; opacity: 0.45; }
</style>
