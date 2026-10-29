<script lang="ts">
  /** Hive settings → Workers and access (docs/hive.md §3.4): workers at once, the coordinator's
   *  and the workers' model and effort, the hosts every builder may reach, push after land and
   *  who is a chat on the phone. Rust checks and saves it (`hive_set_settings`) and tells the
   *  coordinator. GallerySettingsModal's vocabulary and rAF focus. */
  import type { Hive } from '$lib/tauri/types';
  import { hiveSetSettings } from '$lib/tauri/commands';
  import { workspacesStore } from '$lib/stores/workspaces.svelte';
  import { addHost as addTo, EFFORTS, formFor, MAX_CAP, MODELS, toChange, validateForm, type ChangeForm, type PhoneChoice } from '$lib/hive/change';
  import { modLabel } from '$lib/utils/platform';
  import { error as logError } from '@tauri-apps/plugin-log';

  interface Props {
    workspaceId: string;
    hive: Hive;
    onclose: () => void;
  }

  let { workspaceId, hive, onclose }: Props = $props();

  // A snapshot taken when it opens: a record changed underneath doesn't yank the fields.
  const seed = (() => formFor(hive))();
  let cap = $state(seed.cap);
  let coordinatorModel = $state(seed.coordinatorModel);
  let coordinatorEffort = $state(seed.coordinatorEffort);
  let workerModel = $state(seed.workerModel);
  let workerEffort = $state(seed.workerEffort);
  let hosts = $state<string[]>(seed.hosts);
  let pushAfterLand = $state(seed.pushAfterLand);
  let phone = $state<PhoneChoice>(seed.phone);
  let hostDraft = $state('');
  let hostProblem = $state<string | null>(null);
  let saving = $state(false);
  let refusal = $state<string | null>(null);
  let panelEl = $state<HTMLDivElement | null>(null);

  $effect(() => {
    const id = requestAnimationFrame(() => panelEl?.querySelector<HTMLElement>('input:not(:disabled), button')?.focus());
    return () => cancelAnimationFrame(id);
  });

  const form = $derived<ChangeForm>({ cap: Number(cap), coordinatorModel, coordinatorEffort, workerModel, workerEffort, hosts, pushAfterLand, phone });
  const problem = $derived(validateForm(form));
  const change = $derived(problem ? null : toChange(hive, form));
  const canSave = $derived(!saving && !!change);
  const offPhone = $derived(hive.settings?.keep_off_phone ?? false);

  function addHost() {
    const r = addTo(hosts, hostDraft);
    if ('error' in r) {
      hostProblem = r.error;
      return;
    }
    hostProblem = null;
    hosts = r.hosts;
    hostDraft = '';
  }

  async function save() {
    if (!canSave || !change) return;
    saving = true;
    refusal = null;
    try {
      const saved = await hiveSetSettings(workspaceId, change);
      workspacesStore.applyHiveRecord(workspaceId, saved);
      onclose();
    } catch (e) {
      saving = false;
      refusal = String(e).replace(/^Error: /, '');
      logError(`hive: settings not saved: ${e}`);
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
<div class="backdrop" onclick={handleBackdropClick} onkeydown={handleKeydown} role="dialog" aria-modal="true" aria-label="Workers and access" tabindex="-1">
  <div class="panel" bind:this={panelEl}>
    <div class="header">
      <div class="title">Workers and access</div>
      <div class="subtitle">
        Each agent picks a change up at its next launch: a running worker keeps its model and network until it is relaunched. The
        coordinator is told what you changed.
      </div>
    </div>

    <div class="body">
      <label class="field">
        <span class="label">Workers at once</span>
        <input class="text-input num" type="number" min="1" max={MAX_CAP} bind:value={cap} />
        <span class="note">Lowering it stops nobody: no new worker starts while more than this are running.</span>
      </label>

      <div class="field">
        <span class="label">Models</span>
        <div class="grid">
          <span class="who">Queen</span>
          <select aria-label="Queen model" bind:value={coordinatorModel}>{#each MODELS as m (m)}<option value={m}>Claude {m}</option>{/each}</select>
          <select aria-label="Queen effort" bind:value={coordinatorEffort}>{#each EFFORTS as e (e)}<option value={e}>{e} effort</option>{/each}</select>
          <span class="who">Workers</span>
          <select aria-label="Worker model" bind:value={workerModel}>{#each MODELS as m (m)}<option value={m}>Claude {m}</option>{/each}</select>
          <select aria-label="Worker effort" bind:value={workerEffort}>{#each EFFORTS as e (e)}<option value={e}>{e} effort</option>{/each}</select>
        </div>
        <span class="note">A worker the queen started with its own model keeps it.</span>
      </div>

      <div class="field">
        <span class="label">Hosts workers may reach <span class="opt">besides the package registries; reviewers never get them</span></span>
        {#if hosts.length}
          <ul class="list">
            {#each hosts as h (h)}
              <li>
                <span class="item mono">{h}</span>
                <button type="button" class="link" onclick={() => (hosts = hosts.filter((x) => x !== h))}>Remove</button>
              </li>
            {/each}
          </ul>
        {:else}
          <span class="note">None: workers reach only the package registries.</span>
        {/if}
        <div class="add">
          <input
            class="text-input"
            placeholder="api.example.com"
            aria-label="Add a host workers may reach"
            bind:value={hostDraft}
            onkeydown={(e) => {
              if (e.key === 'Enter' && !e.metaKey && !e.ctrlKey) {
                e.preventDefault();
                addHost();
              }
            }}
          />
          <button type="button" class="btn" disabled={!hostDraft.trim()} onclick={addHost}>Add</button>
        </div>
        {#if hostProblem}<p class="error">{hostProblem}</p>{/if}
        <span class="note">A host a worker can reach can also receive what it sends. A git remote's host is never allowed.</span>
      </div>

      <label class="check">
        <input type="checkbox" bind:checked={pushAfterLand} />
        <span>Push after each land</span>
      </label>

      <div class="field">
        <span class="label">On the phone</span>
        {#if offPhone}
          <span class="note">Kept off when the Hive was made: no agent of this Hive is a chat on the phone.</span>
        {:else}
          <label class="check"><input type="radio" name="phone" value="coordinator" bind:group={phone} /><span>The queen only</span></label>
          <label class="check"><input type="radio" name="phone" value="all" bind:group={phone} /><span>The queen and its workers</span></label>
          <span class="note">Applies to workers started from now on.</span>
        {/if}
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
  .backdrop { position: fixed; inset: 0; background: rgba(0, 0, 0, 0.4); display: flex; justify-content: center; padding-top: 10vh; z-index: 1000; }
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
    width: 560px;
  }
  .header { border-bottom: 1px solid var(--bg-light); padding: 12px 14px 10px; }
  .title { color: var(--fg); font-size: 1rem; font-weight: 600; }
  .subtitle { color: var(--fg-dim); font-size: 0.8rem; line-height: 1.4; margin-top: 3px; }
  .body { display: flex; flex-direction: column; gap: 16px; overflow-y: auto; padding: 14px; }
  .field { display: flex; flex-direction: column; gap: 6px; min-width: 0; }
  .label { color: var(--fg-dim); font-size: 0.72rem; font-weight: 600; letter-spacing: 0.06em; text-transform: uppercase; }
  .opt { font-weight: 400; letter-spacing: 0; opacity: 0.7; overflow-wrap: anywhere; text-transform: none; }
  .note { color: var(--fg-dim); font-size: 0.75rem; line-height: 1.4; overflow-wrap: anywhere; }
  .grid { display: grid; grid-template-columns: max-content minmax(0, 1fr) minmax(0, 1fr); gap: 6px 8px; align-items: center; }
  .who { color: var(--fg); font-size: 0.8rem; }
  select, .text-input { background: var(--bg-dark); border: 1px solid var(--bg-light); border-radius: 5px; color: var(--fg); font-family: inherit; font-size: 0.85rem; padding: 6px 8px; min-width: 0; }
  .text-input.num { width: 90px; }
  select:focus, .text-input:focus { border-color: var(--accent); outline: none; }
  .list { display: flex; flex-direction: column; gap: 4px; list-style: none; margin: 0; padding: 0; }
  .list li { align-items: baseline; background: var(--bg-dark); border-radius: 5px; display: flex; gap: 10px; justify-content: space-between; padding: 5px 9px; }
  .item { color: var(--fg); font-size: 0.8rem; min-width: 0; overflow-wrap: anywhere; }
  .mono { font-family: var(--font-mono, ui-monospace, monospace); }
  .add { align-items: center; display: flex; flex-wrap: wrap; gap: 7px; }
  .add .text-input { flex: 1 1 200px; }
  .check { align-items: baseline; color: var(--fg); display: flex; font-size: 0.8rem; gap: 7px; }
  .link { background: none; border: none; color: var(--accent); cursor: pointer; font: inherit; font-size: 0.75rem; padding: 0; }
  .link:hover { text-decoration: underline; }
  .link:focus-visible { outline: 2px solid var(--accent); outline-offset: 1px; }
  .error { color: var(--red); font-size: 0.72rem; margin: 0; overflow-wrap: anywhere; }
  .footer { align-items: center; border-top: 1px solid var(--bg-light); display: flex; gap: 10px; justify-content: space-between; padding: 10px 14px; }
  .hint { color: var(--fg-dim); font-size: 0.72rem; }
  .actions { display: flex; gap: 7px; }
  .btn { background: var(--bg-dark); border: 1px solid var(--bg-light); border-radius: 5px; color: var(--fg); cursor: pointer; font-family: inherit; font-size: 0.8rem; padding: 6px 12px; }
  .btn:hover:not(:disabled) { border-color: var(--fg-dim); }
  .btn:focus-visible { outline: 2px solid var(--accent); outline-offset: 1px; }
  .btn.primary { background: var(--accent); border-color: var(--accent); color: var(--bg-dark); font-weight: 600; }
  .btn:disabled { cursor: default; opacity: 0.45; }
</style>
