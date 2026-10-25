<script lang="ts">
  /** Hive header → Budget (docs/hive.md §10.3, §7.8, §14.16): the budget for paid generation, the
   *  output folders `galleryAdd` files from, and the generator hosts a worker's sandbox reaches.
   *  Rust checks and saves it (`hive_set_gallery_settings`); a raise past the spend closes the
   *  budget row and tells the coordinator. HiveAutonomyModal's vocabulary and rAF focus. */
  import type { Hive, HiveSpend } from '$lib/tauri/types';
  import { hiveDetectGeneratorHosts, hiveSetGallerySettings } from '$lib/tauri/commands';
  import { workspacesStore } from '$lib/stores/workspaces.svelte';
  import { formFor, newHosts, normalizeHost, toSave, usd, validateForm, type GalleryForm } from '$lib/hive/budget';
  import Tooltip from '$lib/components/Tooltip.svelte';
  import { modLabel } from '$lib/utils/platform';
  import { open as dialogOpen } from '@tauri-apps/plugin-dialog';
  import { error as logError } from '@tauri-apps/plugin-log';

  interface Props {
    workspaceId: string;
    hive: Hive;
    spend: HiveSpend | null;
    onclose: () => void;
  }

  let { workspaceId, hive, spend, onclose }: Props = $props();

  // A snapshot taken when it opens: a Hive record changed underneath doesn't yank the fields.
  const seed = (() => formFor(hive))();
  let dollars = $state(seed.dollars);
  let outputFolders = $state<string[]>(seed.outputFolders);
  let hosts = $state<string[]>(seed.hosts);
  let hostDraft = $state('');
  let hostProblem = $state<string | null>(null);
  /** Re-detect's offer: found now, not listed yet, each ticked in by the human. */
  let offered = $state<{ host: string; take: boolean }[] | null>(null);
  let detecting = $state(false);
  let saving = $state(false);
  let refusal = $state<string | null>(null);
  let panelEl = $state<HTMLDivElement | null>(null);

  $effect(() => {
    const id = requestAnimationFrame(() => panelEl?.querySelector<HTMLElement>('input:not(:disabled), button')?.focus());
    return () => cancelAnimationFrame(id);
  });

  const form = $derived<GalleryForm>({ dollars, outputFolders, hosts });
  const problem = $derived(validateForm(form));
  const canSave = $derived(!saving && problem === null);

  function addHost() {
    const r = normalizeHost(hostDraft);
    if ('error' in r) {
      hostProblem = r.error;
      return;
    }
    hostProblem = null;
    if (!hosts.includes(r.host)) hosts = [...hosts, r.host];
    hostDraft = '';
  }

  async function addFolder() {
    const picked = await dialogOpen({ directory: true, multiple: false, title: 'Add an output folder' }).catch(() => null);
    if (typeof picked === 'string' && !outputFolders.includes(picked)) outputFolders = [...outputFolders, picked];
  }

  async function redetect() {
    detecting = true;
    try {
      const found = await hiveDetectGeneratorHosts(workspaceId);
      offered = newHosts(hosts, found).map((host) => ({ host, take: true }));
    } catch (e) {
      refusal = String(e).replace(/^Error: /, '');
      logError(`hive: generator hosts not detected: ${e}`);
    } finally {
      detecting = false;
    }
  }

  function takeOffered() {
    const add = (offered ?? []).filter((o) => o.take).map((o) => o.host);
    hosts = [...hosts, ...add.filter((h) => !hosts.includes(h))];
    offered = null;
  }

  async function save() {
    if (!canSave) return;
    saving = true;
    refusal = null;
    try {
      const saved = await hiveSetGallerySettings(workspaceId, toSave(form));
      workspacesStore.applyHiveRecord(workspaceId, saved);
      onclose();
    } catch (e) {
      saving = false;
      refusal = String(e).replace(/^Error: /, '');
      logError(`hive: gallery settings not saved: ${e}`);
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
<div class="backdrop" onclick={handleBackdropClick} onkeydown={handleKeydown} role="dialog" aria-modal="true" aria-label="Budget and generators" tabindex="-1">
  <div class="panel" bind:this={panelEl}>
    <div class="header">
      <div class="title">Budget and generators</div>
      <div class="subtitle">
        What this Hive may spend on paid generation, where its tools write, and which generator hosts its workers' sandboxes reach.
      </div>
    </div>

    <div class="body">
      <label class="field">
        <span class="label">Budget <span class="opt">dollars, 0–100,000: a rule agents follow and a total you watch; the provider account's credit is the only hard limit</span></span>
        <div class="money">
          <span class="dollar" aria-hidden="true">$</span>
          <input class="text-input num" inputmode="decimal" bind:value={dollars} aria-label="Budget in dollars" />
          {#if spend}
            <span class="spent">{usd(spend.spent_micros)} spent so far{#if spend.over} · reached{/if}</span>
          {/if}
        </div>
        <span class="note">
          Agents record each paid render's cost, kept or not, and ask you before a spend that would cross the budget. Renders already
          under way can carry the total past it.
        </span>
      </label>

      <div class="field">
        <span class="label">Output folders <span class="opt">for a tool that writes only to its own place (ComfyUI's output): agents may file from these</span></span>
        {#if outputFolders.length}
          <ul class="list">
            {#each outputFolders as f (f)}
              <li>
                <span class="item">{f}</span>
                <button type="button" class="link" onclick={() => (outputFolders = outputFolders.filter((x) => x !== f))}>Remove</button>
              </li>
            {/each}
          </ul>
        {/if}
        <div><button type="button" class="btn" onclick={() => void addFolder()}>Add folder…</button></div>
      </div>

      <div class="field">
        <span class="label">Generator hosts <span class="opt">what a worker's sandbox may reach besides the package registries</span></span>
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
          <span class="note">None: workers reach no generator host.</span>
        {/if}
        <div class="add">
          <input
            class="text-input"
            placeholder="api.example.com"
            aria-label="Add a generator host"
            bind:value={hostDraft}
            onkeydown={(e) => {
              if (e.key === 'Enter' && !e.metaKey && !e.ctrlKey) {
                e.preventDefault();
                addHost();
              }
            }}
          />
          <button type="button" class="btn" disabled={!hostDraft.trim()} onclick={addHost}>Add</button>
          <Tooltip text="Find the hosts this Hive's chosen generators need now, to tick in">
            <button type="button" class="btn" disabled={detecting} onclick={() => void redetect()}>{detecting ? 'Detecting…' : 'Re-detect'}</button>
          </Tooltip>
        </div>
        {#if hostProblem}<p class="error">{hostProblem}</p>{/if}
        {#if offered}
          <div class="offer">
            {#if offered.length === 0}
              <span class="note">Nothing new: every host the chosen generators need is listed.</span>
              <div><button type="button" class="link" onclick={() => (offered = null)}>OK</button></div>
            {:else}
              <span class="note">Found now and not listed:</span>
              {#each offered as o (o.host)}
                <label class="check"><input type="checkbox" bind:checked={o.take} /><span class="mono">{o.host}</span></label>
              {/each}
              <div class="offer-actions">
                <button type="button" class="btn" onclick={takeOffered}>Add ticked</button>
                <button type="button" class="link" onclick={() => (offered = null)}>Dismiss</button>
              </div>
            {/if}
          </div>
        {/if}
        <span class="note">
          Each host is also a channel a file can leave by. A git remote's host is never allowed. A running worker gets new hosts at its
          next launch.
        </span>
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
    width: 600px;
  }

  .header { border-bottom: 1px solid var(--bg-light); padding: 12px 14px 10px; }
  .title { color: var(--fg); font-size: 1rem; font-weight: 600; }
  .subtitle { color: var(--fg-dim); font-size: 0.8rem; line-height: 1.4; margin-top: 3px; }

  .body { display: flex; flex-direction: column; gap: 16px; overflow-y: auto; padding: 14px; }
  .field { display: flex; flex-direction: column; gap: 6px; min-width: 0; }
  .label { color: var(--fg-dim); font-size: 0.72rem; font-weight: 600; letter-spacing: 0.06em; text-transform: uppercase; }
  .opt { font-weight: 400; letter-spacing: 0; opacity: 0.7; overflow-wrap: anywhere; text-transform: none; }
  .note { color: var(--fg-dim); font-size: 0.75rem; line-height: 1.4; overflow-wrap: anywhere; }

  .money { align-items: center; display: flex; flex-wrap: wrap; gap: 6px 8px; }
  .dollar { color: var(--fg-dim); font-size: 0.9rem; }
  .spent { color: var(--fg-dim); font-size: 0.78rem; }

  .list { display: flex; flex-direction: column; gap: 4px; list-style: none; margin: 0; padding: 0; }
  .list li { align-items: baseline; background: var(--bg-dark); border-radius: 5px; display: flex; gap: 10px; justify-content: space-between; padding: 5px 9px; }
  .item { color: var(--fg); font-size: 0.8rem; min-width: 0; overflow-wrap: anywhere; }
  .mono { font-family: var(--font-mono, ui-monospace, monospace); }

  .add { align-items: center; display: flex; flex-wrap: wrap; gap: 7px; }
  .add .text-input { flex: 1 1 200px; min-width: 0; }
  .offer { background: var(--bg-dark); border-radius: 5px; display: flex; flex-direction: column; gap: 5px; padding: 8px 10px; }
  .offer-actions { align-items: center; display: flex; gap: 10px; margin-top: 3px; }
  .check { align-items: baseline; color: var(--fg); display: flex; font-size: 0.8rem; gap: 7px; }

  .text-input {
    background: var(--bg-dark);
    border: 1px solid var(--bg-light);
    border-radius: 5px;
    color: var(--fg);
    font-family: inherit;
    font-size: 0.85rem;
    padding: 7px 9px;
  }
  .text-input.num { width: 110px; }
  .text-input:focus { border-color: var(--accent); outline: none; }

  .link { background: none; border: none; color: var(--accent); cursor: pointer; font: inherit; font-size: 0.75rem; padding: 0; }
  .link:hover { text-decoration: underline; }
  .link:focus-visible { outline: 2px solid var(--accent); outline-offset: 1px; }

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
