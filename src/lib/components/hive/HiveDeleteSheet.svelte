<script lang="ts">
  /** Delete Hive (docs/hive.md §11.2): before anything is touched, every branch not landed, every
   *  worktree that would stay, the scratch folders with work in them, the shared memory, the
   *  gallery's size, the agents that would stop and what goes. Clean worktrees go; dirty ones,
   *  every branch and (unless ticked) the gallery and the memory stay. The click is the human's authority; Rust re-checks (a land in flight refuses).
   *  Same backdrop/panel vocabulary as RunQueueModal, and the same explicit rAF focus. */
  import { hiveDelete, hiveDeletePreview } from '$lib/tauri/commands';
  import type { HiveDeletePreview } from '$lib/tauri/types';
  import { workspacesStore } from '$lib/stores/workspaces.svelte';
  import { toastStore } from '$lib/stores/toasts.svelte';
  import { deleteRefusal, deleteSummary, formatBytes, keptLines, removedLines, stoppingLines } from '$lib/hive/delete';
  import { error as logError } from '@tauri-apps/plugin-log';

  interface Props {
    workspaceId: string;
    name: string;
    onclose: () => void;
  }

  let { workspaceId, name, onclose }: Props = $props();

  let preview = $state<HiveDeletePreview | null>(null);
  let readError = $state<string | null>(null);
  let deleteGallery = $state(false);
  let deleteMemory = $state(false);
  let deleting = $state(false);
  let refusal = $state<string | null>(null);
  let panelEl = $state<HTMLDivElement | null>(null);

  $effect(() => {
    const id = workspaceId;
    hiveDeletePreview(id)
      .then((p) => { if (id === workspaceId) preview = p; })
      .catch((e) => { readError = String(e).replace(/^Error: /, ''); });
  });

  $effect(() => {
    const id = requestAnimationFrame(() => panelEl?.querySelector<HTMLElement>('button.cancel')?.focus());
    return () => cancelAnimationFrame(id);
  });

  const blocked = $derived(deleteRefusal(preview));
  const kept = $derived(preview ? keptLines(preview, deleteMemory, deleteGallery) : []);
  const removed = $derived(preview ? removedLines(preview, deleteGallery, deleteMemory) : []);
  const canDelete = $derived(!!preview && !blocked && !deleting);

  async function confirm() {
    if (!canDelete) return;
    deleting = true;
    refusal = null;
    try {
      const r = await hiveDelete(workspaceId, deleteGallery, deleteMemory);
      toastStore.addToast(`${name || 'Hive'} deleted`, deleteSummary(r), 'info');
      onclose();
      // Last: the Hive view this sheet sits in unmounts with the record.
      workspacesStore.applyHiveDeleted(workspaceId, r.active_tabs);
    } catch (e) {
      deleting = false;
      refusal = String(e).replace(/^Error: /, '');
      logError(`hive: delete of ${workspaceId.slice(0, 8)} refused: ${e}`);
    }
  }

  function handleKeydown(e: KeyboardEvent) {
    if (e.key === 'Escape' && !deleting) {
      e.stopPropagation();
      onclose();
    }
  }

  function handleBackdropClick(e: MouseEvent) {
    if (e.target === e.currentTarget && !deleting) onclose();
  }
</script>

<!-- svelte-ignore a11y_no_noninteractive_tabindex -->
<div class="backdrop" onclick={handleBackdropClick} onkeydown={handleKeydown} role="dialog" aria-modal="true" aria-label="Delete Hive" tabindex="-1">
  <div class="panel" bind:this={panelEl}>
    <div class="header">
      <div class="title">Delete {name || 'this Hive'}?</div>
      <div class="subtitle">
        Its agents stop and its tabs close. The workspace stays, with its tasks as history. Clean worktrees are removed;
        anything holding work, the shared memory included, stays on disk and is listed below, and so is everything that
        goes. No branch is ever deleted.
      </div>
    </div>

    <div class="body">
      {#if readError}
        <p class="error" role="alert">Couldn't read what the Hive holds: {readError}</p>
      {:else if !preview}
        <p class="note">Reading what the Hive holds…</p>
      {:else}
        <section>
          <h4>Agents that stop</h4>
          {#if preview.live_agents.length}
            <ul>{#each stoppingLines(preview) as a (a)}<li>{a}</li>{/each}</ul>
          {:else}
            <p class="quiet">{stoppingLines(preview)[0]}.</p>
          {/if}
        </section>
        <section>
          <h4>Branches not landed</h4>
          {#if preview.unlanded.length}
            <ul>
              {#each preview.unlanded as u (u.folder + u.branch)}
                <li><code>{u.branch}</code> in {u.folder}: {u.ahead} commit{u.ahead === 1 ? '' : 's'} {u.target ? `not on ${u.target}` : 'ahead'} ({u.worker})</li>
              {/each}
            </ul>
          {:else}
            <p class="quiet">None: every worker's commits are on its landing target.</p>
          {/if}
          {#if preview.branches.length}
            <p class="quiet">All {preview.branches.length} of the Hive's branches are kept.</p>
          {/if}
        </section>
        <section>
          <h4>Kept on disk</h4>
          {#if kept.length}
            <ul>{#each kept as k (k)}<li>{k}</li>{/each}</ul>
          {:else}
            <p class="quiet">Nothing: no worktree holds uncommitted or untracked work.</p>
          {/if}
        </section>
        <label class="check">
          <input type="checkbox" bind:checked={deleteGallery} disabled={deleting} />
          <span>Also delete the gallery ({preview.gallery_files} asset{preview.gallery_files === 1 ? '' : 's'}, {formatBytes(preview.gallery_bytes)})</span>
        </label>
        {#if preview.memory}
          <label class="check">
            <input type="checkbox" bind:checked={deleteMemory} disabled={deleting} />
            <span>Also delete the shared memory ({preview.memory.files} file{preview.memory.files === 1 ? '' : 's'}, files you wrote there included)</span>
          </label>
        {/if}
        {#if removed.length}
          <section>
            <h4>Removed</h4>
            <ul>{#each removed as r (r)}<li>{r}</li>{/each}</ul>
          </section>
        {/if}
        {#if blocked}<p class="error" role="alert">{blocked}</p>{/if}
      {/if}
      {#if refusal}<p class="error" role="alert">{refusal}</p>{/if}
    </div>

    <div class="footer">
      <button type="button" class="btn cancel" disabled={deleting} onclick={onclose}>Cancel</button>
      <button type="button" class="btn danger" disabled={!canDelete} onclick={() => void confirm()}>{deleting ? 'Deleting…' : 'Delete Hive'}</button>
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
    width: 560px;
  }

  .header { border-bottom: 1px solid var(--bg-light); padding: 12px 14px 10px; }
  .title { color: var(--fg); font-size: 1rem; font-weight: 600; overflow-wrap: anywhere; }
  .subtitle { color: var(--fg-dim); font-size: 0.8rem; line-height: 1.4; margin-top: 3px; }

  .body { display: flex; flex-direction: column; gap: 12px; overflow-y: auto; padding: 14px; }
  section { display: flex; flex-direction: column; gap: 4px; }
  h4 { margin: 0; color: var(--fg-dim); font-size: 0.72rem; font-weight: 600; letter-spacing: 0.06em; text-transform: uppercase; }
  ul { margin: 0; padding-left: 1.2em; display: flex; flex-direction: column; gap: 2px; }
  li { color: var(--fg); font-size: 0.8rem; line-height: 1.4; overflow-wrap: anywhere; }
  .quiet { color: var(--fg-dim); font-size: 0.8rem; margin: 0; }
  code { font-family: var(--font-mono, ui-monospace, monospace); font-size: 0.9em; }
  .check { display: flex; align-items: flex-start; gap: 8px; color: var(--fg); font-size: 0.8rem; cursor: pointer; }
  .check input { margin-top: 2px; }

  .error { color: var(--red); font-size: 0.78rem; margin: 0; overflow-wrap: anywhere; }
  .note { color: var(--fg-dim); font-size: 0.8rem; margin: 0; }

  .footer { align-items: center; border-top: 1px solid var(--bg-light); display: flex; gap: 7px; justify-content: flex-end; padding: 10px 14px; }

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
  .btn.danger { background: var(--red); border-color: var(--red); color: var(--bg-dark); font-weight: 600; }
  .btn:disabled { cursor: default; opacity: 0.45; }
</style>
