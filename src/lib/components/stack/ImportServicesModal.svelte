<script lang="ts">
  /** Import services from what the project already declares (docs/stack.md §8): a checklist
   *  of package.json scripts, Procfile entries, compose services, justfile/Makefile recipes,
   *  pre-ticked for the dev/start-shaped ones. This is what replaces Solo's yaml as the
   *  10-second first run. */
  import { suggestStack, type StackSuggestion } from '$lib/tauri/commands';
  import { error as logError } from '@tauri-apps/plugin-log';
  import { untrack } from 'svelte';

  interface Props {
    cwd: string;
    /** Names already in the stack — shown ticked-and-disabled so a re-import is a no-op. */
    existingNames: string[];
    onsubmit: (rows: StackSuggestion[]) => void;
    oncancel: () => void;
  }

  let { cwd, existingNames, onsubmit, oncancel }: Props = $props();

  /* Snapshot of the prop on open — the field is the user's to edit after that. */
  function initialDir() { return cwd; }
  let dir = $state(initialDir());
  let rows = $state<StackSuggestion[]>([]);
  let picked = $state<Set<number>>(new Set());
  let loading = $state(false);
  let scannedDir = $state<string | null>(null);

  const existing = $derived(new Set(existingNames.map((n) => n.trim().toLowerCase())));

  async function scan() {
    loading = true;
    try {
      rows = await suggestStack(dir);
      picked = new Set(rows.map((r, i) => (r.recommended && !existing.has(r.name.toLowerCase()) ? i : -1)).filter((i) => i >= 0));
      scannedDir = dir;
    } catch (e) {
      logError(`stack: suggest: ${e}`);
      rows = [];
      picked = new Set();
    } finally {
      loading = false;
    }
  }

  // Scan once on open. `scan` reads `dir`, and subscribing this effect to it would rescan
  // on every keystroke in the directory field — Enter and the Scan button rescan instead.
  $effect(() => { untrack(() => { void scan(); }); });

  function toggle(i: number) {
    const next = new Set(picked);
    if (next.has(i)) next.delete(i); else next.add(i);
    picked = next;
  }

  function submit() {
    onsubmit(rows.filter((_, i) => picked.has(i) && !existing.has(rows[i].name.toLowerCase())));
  }

  function handleKeydown(e: KeyboardEvent) {
    if (e.key === 'Escape') { e.stopPropagation(); oncancel(); }
    if (e.key === 'Enter' && (e.metaKey || e.ctrlKey)) { e.preventDefault(); submit(); }
  }
</script>

<!-- svelte-ignore a11y_no_noninteractive_tabindex -->
<div class="backdrop" onclick={(e) => { if (e.target === e.currentTarget) oncancel(); }} onkeydown={handleKeydown} role="dialog" aria-modal="true" aria-label="Import services" tabindex="-1">
  <div class="panel">
    <div class="header">
      <div class="title">Import from project</div>
      <div class="subtitle">What this directory already says it runs. Tick what should become part of the stack.</div>
    </div>

    <div class="body">
      <label class="field">
        <span class="label">Directory</span>
        <div class="row">
          <input class="text-input mono" bind:value={dir} spellcheck="false" onkeydown={(e) => { if (e.key === 'Enter') { e.preventDefault(); void scan(); } }} />
          <button type="button" class="btn" onclick={() => void scan()} disabled={loading}>Scan</button>
        </div>
      </label>

      {#if loading}
        <div class="empty">Scanning…</div>
      {:else if rows.length === 0}
        <div class="empty">
          Nothing found{#if scannedDir}{' '}in <span class="mono">{scannedDir}</span>{/if}. Looked for package.json scripts, a Procfile, a compose file, a justfile and a Makefile.
        </div>
      {:else}
        <div class="list">
          {#each rows as row, i (i)}
            {@const dup = existing.has(row.name.toLowerCase())}
            <label class="item" class:dup>
              <input type="checkbox" checked={dup || picked.has(i)} disabled={dup} onchange={() => toggle(i)} />
              <span class="name">{row.name}</span>
              <span class="cmd mono">{row.command}</span>
              <span class="src">{dup ? 'already in stack' : row.source}</span>
            </label>
          {/each}
        </div>
      {/if}
    </div>

    <div class="footer">
      <span class="hint">{picked.size} selected</span>
      <div class="actions">
        <button type="button" class="btn" onclick={oncancel}>Cancel</button>
        <button type="button" class="btn primary" disabled={picked.size === 0} onclick={submit}>Add {picked.size === 1 ? 'service' : 'services'}</button>
      </div>
    </div>
  </div>
</div>

<style>
  .backdrop { position: fixed; inset: 0; background: rgba(0, 0, 0, 0.4); display: flex; justify-content: center; padding-top: 12vh; z-index: 1000; }
  .panel { align-self: flex-start; background: var(--bg-medium); border: 1px solid var(--bg-light); border-radius: 8px; box-shadow: 0 8px 32px rgba(0, 0, 0, 0.5); display: flex; flex-direction: column; max-height: 76vh; width: 560px; }
  .header { border-bottom: 1px solid var(--bg-light); padding: 12px 14px 10px; }
  .title { color: var(--fg); font-size: 1rem; font-weight: 600; }
  .subtitle { color: var(--fg-dim); font-size: 0.8rem; line-height: 1.4; margin-top: 3px; }
  .body { display: flex; flex-direction: column; gap: 12px; overflow-y: auto; padding: 14px; }
  .field { display: flex; flex-direction: column; gap: 5px; }
  .row { display: flex; gap: 6px; }
  .label { color: var(--fg-dim); font-size: 0.72rem; font-weight: 600; letter-spacing: 0.06em; text-transform: uppercase; }
  .text-input { background: var(--bg-dark); border: 1px solid var(--bg-light); border-radius: 5px; color: var(--fg); font-family: inherit; font-size: 0.85rem; padding: 7px 9px; width: 100%; }
  .text-input:focus { border-color: var(--accent); outline: none; }
  .mono { font-family: var(--font-mono, ui-monospace, monospace); font-size: 0.78rem; }
  .empty { color: var(--fg-dim); font-size: 0.85rem; line-height: 1.5; }
  .list { display: flex; flex-direction: column; gap: 2px; }
  .item { align-items: center; border-radius: 5px; cursor: pointer; display: grid; gap: 8px; grid-template-columns: auto minmax(60px, 0.6fr) minmax(0, 1.4fr) auto; padding: 5px 6px; }
  .item:hover { background: var(--bg-light); }
  .item.dup { opacity: 0.55; cursor: default; }
  .item input { accent-color: var(--accent); }
  .name { color: var(--fg); font-size: 0.85rem; overflow-wrap: anywhere; }
  .cmd { color: var(--fg-dim); overflow-wrap: anywhere; }
  .src { color: var(--fg-dim); font-size: 0.68rem; white-space: nowrap; }
  .footer { align-items: center; border-top: 1px solid var(--bg-light); display: flex; gap: 10px; justify-content: space-between; padding: 10px 14px; }
  .hint { color: var(--fg-dim); font-size: 0.72rem; }
  .actions { display: flex; gap: 7px; }
  .btn { background: var(--bg-dark); border: 1px solid var(--bg-light); border-radius: 5px; color: var(--fg); cursor: pointer; font-family: inherit; font-size: 0.8rem; padding: 6px 12px; }
  .btn:hover:not(:disabled) { border-color: var(--fg-dim); }
  .btn.primary { background: var(--accent); border-color: var(--accent); color: var(--bg-dark); font-weight: 600; }
  .btn:disabled { cursor: default; opacity: 0.45; }
</style>
