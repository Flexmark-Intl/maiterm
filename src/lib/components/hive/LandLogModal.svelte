<script lang="ts">
  /** The land log (docs/hive.md §8.6): every land, undo and post-land check of the Hive, newest
   *  first, with what's undone and what's suspect. Undo lives here and in the worker column's
   *  menu, never on a card (§4.3). Its confirmation opens in place under the line and names the
   *  repo, the branch it writes, the commits it reverts and any config files before anything is
   *  written; the click is the human's approval (plan D15), and Rust re-checks everything.
   *  Same backdrop/panel vocabulary as HiveDeleteSheet, and the same explicit rAF focus. */
  import { hiveLandLog, hiveUndoLand, hiveUndoPreview } from '$lib/tauri/commands';
  import type { HiveLandLog, HiveLandLogEntry } from '$lib/tauri/types';
  import { flags, kindLabel, moveLine, offersUndo, shownEntries, suspectLines, undoBlocked, undoConfirm, type UndoConfirm } from '$lib/hive/landLog';
  import Tooltip from '$lib/components/Tooltip.svelte';
  import { error as logError } from '@tauri-apps/plugin-log';

  interface Props {
    workspaceId: string;
    /** Only this worker's lines (opened from its column's menu). */
    taskId?: string | null;
    /** Changes whenever the board refetches: the log is read again. */
    refreshKey?: unknown;
    onclose: () => void;
  }

  let { workspaceId, taskId = null, refreshKey, onclose }: Props = $props();

  let log = $state<HiveLandLog | null>(null);
  let readError = $state<string | null>(null);
  let panelEl = $state<HTMLDivElement | null>(null);
  /** The line whose confirmation is open, its preview, and what came of an undo. */
  let open = $state<{ id: string; confirm: UndoConfirm | null; fp: string | null; error: string | null } | null>(null);
  let undoing = $state(false);
  let result = $state<{ id: string; text: string; failed: boolean } | null>(null);

  function load() {
    const id = workspaceId;
    hiveLandLog(id)
      .then((l) => { if (id === workspaceId) { log = l; readError = null; } })
      .catch((e) => { readError = String(e).replace(/^Error: /, ''); });
  }

  $effect(() => {
    void refreshKey;
    void workspaceId;
    load();
  });

  $effect(() => {
    const id = requestAnimationFrame(() => panelEl?.querySelector<HTMLElement>('button.close')?.focus());
    return () => cancelAnimationFrame(id);
  });

  const entries = $derived(log ? shownEntries(log, taskId) : []);
  const warnings = $derived(log ? suspectLines(log) : []);
  const held = $derived(log?.undos.filter((u) => u.state === 'held') ?? []);

  async function startUndo(e: Pick<HiveLandLogEntry, 'id'>) {
    if (undoing) return;
    result = null;
    open = { id: e.id, confirm: null, fp: null, error: null };
    try {
      const p = await hiveUndoPreview(workspaceId, e.id);
      if (open?.id === e.id) open = { id: e.id, confirm: undoConfirm(p), fp: p.config_fp ?? null, error: null };
    } catch (err) {
      if (open?.id === e.id) open = { id: e.id, confirm: null, fp: null, error: String(err).replace(/^Error: /, '') };
    }
  }

  async function confirmUndo() {
    if (!open?.confirm || open.confirm.refusal || undoing) return;
    const { id, fp } = open;
    undoing = true;
    try {
      const r = await hiveUndoLand(workspaceId, id, fp);
      result = { id, text: r.result ?? (r.state === 'checking' ? 'Its land check runs now; the queen hears the result.' : r.state), failed: false };
      open = null;
    } catch (err) {
      result = { id, text: String(err).replace(/^Error: /, ''), failed: true };
      logError(`hive: undo of ${id} refused: ${err}`);
    } finally {
      undoing = false;
      load();
    }
  }

  function handleKeydown(e: KeyboardEvent) {
    if (e.key !== 'Escape' || undoing) return;
    e.stopPropagation();
    if (open) open = null;
    else onclose();
  }

  function handleBackdropClick(e: MouseEvent) {
    if (e.target === e.currentTarget && !undoing) onclose();
  }

  const when = (at: string) => {
    const d = new Date(at);
    return Number.isNaN(d.getTime()) ? at : d.toLocaleString();
  };
</script>

<!-- svelte-ignore a11y_no_noninteractive_tabindex -->
<div class="backdrop" onclick={handleBackdropClick} onkeydown={handleKeydown} role="dialog" aria-modal="true" aria-label="Land log" tabindex="-1">
  <div class="panel" bind:this={panelEl}>
    <div class="header">
      <div class="title">Land log{taskId && entries[0] ? `: ${entries[0].title}` : ''}</div>
      <div class="subtitle">
        Every land, undo and post-land check, newest first. Undo writes a revert commit and lands it like a land; it never resets
        history.{#if log?.integration} In this Hive's approval mode, Undo is yours alone.{/if}
      </div>
    </div>

    <div class="body">
      {#if readError}
        <p class="error" role="alert">Couldn't read the land log: {readError}</p>
      {:else if !log}
        <p class="note">Reading the land log…</p>
      {:else}
        {#each warnings as w (w)}<p class="warn" role="status">{w}</p>{/each}
        {#each held as u (u.id)}
          <p class="warn" role="status">The undo of {u.undoes} holds: {u.held_reason ?? 'held'}. Undo it again once that's gone.</p>
        {/each}
        {#if !entries.length}
          <p class="quiet">Nothing has landed yet.</p>
        {/if}
        <ul class="lines">
          {#each entries as e (e.id)}
            {@const blocked = offersUndo(e) ? undoBlocked(e, log) : null}
            <li class="line" class:undone={!!e.undone_by} data-kind={e.kind}>
              <div class="row">
                <span class="kind" data-check={e.check}>{kindLabel(e)}</span>
                <b class="t">{e.title || e.worker}</b>
                {#if offersUndo(e)}
                  {#if blocked}
                    <Tooltip text={blocked}><span class="muted">Undo</span></Tooltip>
                  {:else}
                    <button type="button" class="link" disabled={undoing} onclick={() => void startUndo(e)}>Undo…</button>
                  {/if}
                {/if}
              </div>
              <div class="meta">
                <code>{e.id}</code>
                <span>{e.folder}: {moveLine(e)}</span>
                {#if e.worker}<span>{e.worker}</span>{/if}
                <span>by {e.by}</span>
                {#if e.kind === 'undo' && e.undoes}<span>undoes {e.undoes}</span>{/if}
                <span>{when(e.at)}</span>
                {#each flags(e) as f (f)}<span class="flag" data-flag={f.split(' ')[0]}>{f}</span>{/each}
              </div>
              {#if e.summary && e.kind !== 'base_check'}<p class="summary">{e.summary}</p>{/if}
              {#if open?.id === e.id}
                <div class="confirm" role="group" aria-label="Confirm undo">
                  {#if open.error}
                    <p class="error" role="alert">{open.error}</p>
                  {:else if !open.confirm}
                    <p class="note">Working out what the undo would write…</p>
                  {:else}
                    <b>{open.confirm.title}</b>
                    {#each open.confirm.head as l (l)}<p>{l}</p>{/each}
                    <ul class="commits">
                      {#each open.confirm.commits as c (c.sha)}<li><code>{c.sha.slice(0, 7)}</code> {c.subject}</li>{/each}
                      {#if open.confirm.more}<li class="quiet">and {open.confirm.more} more</li>{/if}
                    </ul>
                    {#each open.confirm.tail as l (l)}<p>{l}</p>{/each}
                    {#if open.confirm.refusal}<p class="error" role="alert">Can't undo it: {open.confirm.refusal}</p>{/if}
                  {/if}
                  <div class="actions">
                    <button type="button" class="btn" disabled={undoing} onclick={() => (open = null)}>Cancel</button>
                    <button type="button" class="btn danger" disabled={undoing || !open.confirm || !!open.confirm.refusal} onclick={() => void confirmUndo()}>
                      {undoing ? 'Undoing…' : 'Undo land'}
                    </button>
                  </div>
                </div>
              {/if}
              {#if result?.id === e.id}
                <p class={result.failed ? 'error' : 'note'} role="status">{result.text}</p>
              {/if}
            </li>
          {/each}
        </ul>
      {/if}
    </div>

    <div class="footer">
      <button type="button" class="btn close" disabled={undoing} onclick={onclose}>Close</button>
    </div>
  </div>
</div>

<style>
  .backdrop { position: fixed; inset: 0; background: rgba(0, 0, 0, 0.4); display: flex; justify-content: center; padding-top: 8vh; z-index: 1000; }
  .panel {
    align-self: flex-start;
    background: var(--bg-medium);
    border: 1px solid var(--bg-light);
    border-radius: 8px;
    box-shadow: 0 8px 32px rgba(0, 0, 0, 0.5);
    display: flex;
    flex-direction: column;
    max-height: 84vh;
    max-width: calc(100vw - 32px);
    width: 680px;
  }
  .header { border-bottom: 1px solid var(--bg-light); padding: 12px 14px 10px; }
  .title { color: var(--fg); font-size: 1rem; font-weight: 600; overflow-wrap: anywhere; }
  .subtitle { color: var(--fg-dim); font-size: 0.8rem; line-height: 1.4; margin-top: 3px; }
  .body { display: flex; flex-direction: column; gap: 10px; overflow-y: auto; padding: 14px; }
  .lines { list-style: none; margin: 0; padding: 0; display: flex; flex-direction: column; gap: 8px; }
  .line { border: 1px solid var(--bg-light); border-radius: 6px; padding: 8px 10px; display: flex; flex-direction: column; gap: 3px; }
  .line.undone .t { text-decoration: line-through; color: var(--fg-dim); }
  .row { display: flex; align-items: baseline; gap: 8px; min-width: 0; }
  .kind { flex: none; font-size: 0.72rem; color: var(--fg-dim); text-transform: uppercase; letter-spacing: 0.05em; }
  .kind[data-check='failed'] { color: var(--red); }
  .t { flex: 1; min-width: 0; font-size: 0.85rem; color: var(--fg); overflow-wrap: anywhere; }
  .meta { display: flex; flex-wrap: wrap; gap: 2px 10px; font-size: 0.75rem; color: var(--fg-dim); min-width: 0; }
  .meta > * { overflow-wrap: anywhere; min-width: 0; }
  .flag { color: var(--yellow); }
  .flag[data-flag='suspect'] { color: var(--orange, #ff9e64); }
  .summary { margin: 2px 0 0; font-size: 0.78rem; color: var(--fg); overflow-wrap: anywhere; }
  .confirm { margin-top: 6px; padding: 8px 10px; border-left: 2px solid var(--red); background: var(--bg-dark); display: flex; flex-direction: column; gap: 4px; font-size: 0.8rem; color: var(--fg); }
  .confirm p { margin: 0; line-height: 1.4; overflow-wrap: anywhere; }
  .commits { margin: 0; padding-left: 1.2em; display: flex; flex-direction: column; gap: 1px; }
  .commits li { overflow-wrap: anywhere; }
  .actions { display: flex; justify-content: flex-end; gap: 7px; margin-top: 4px; }
  code { font-family: var(--font-mono, ui-monospace, monospace); font-size: 0.9em; }
  .link { background: none; border: none; padding: 0; color: var(--accent); font: inherit; font-size: 0.78rem; cursor: pointer; }
  .link:hover:not(:disabled) { text-decoration: underline; }
  .link:focus-visible { outline: 2px solid var(--accent); outline-offset: 1px; }
  .link:disabled { opacity: 0.45; cursor: default; }
  .muted { font-size: 0.78rem; color: var(--fg-dim); opacity: 0.6; }
  .warn { margin: 0; color: var(--orange, #ff9e64); font-size: 0.8rem; overflow-wrap: anywhere; }
  .quiet { color: var(--fg-dim); font-size: 0.8rem; margin: 0; }
  .error { color: var(--red); font-size: 0.78rem; margin: 0; overflow-wrap: anywhere; white-space: pre-wrap; }
  .note { color: var(--fg-dim); font-size: 0.8rem; margin: 0; overflow-wrap: anywhere; }
  .footer { align-items: center; border-top: 1px solid var(--bg-light); display: flex; gap: 7px; justify-content: flex-end; padding: 10px 14px; }
  .btn { background: var(--bg-dark); border: 1px solid var(--bg-light); border-radius: 5px; color: var(--fg); cursor: pointer; font-family: inherit; font-size: 0.8rem; padding: 6px 12px; }
  .btn:hover:not(:disabled) { border-color: var(--fg-dim); }
  .btn:focus-visible { outline: 2px solid var(--accent); outline-offset: 1px; }
  .btn.danger { background: var(--red); border-color: var(--red); color: var(--bg-dark); font-weight: 600; }
  .btn:disabled { cursor: default; opacity: 0.45; }
</style>
