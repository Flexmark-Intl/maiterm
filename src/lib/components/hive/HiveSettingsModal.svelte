<script lang="ts">
  /** Hive settings from the header (docs/hive.md §4.1, §3.2): what can change opens its own sheet
   *  (workers and access, autonomy, run queue, budget and generation), and what was fixed at Create is shown
   *  read-only, with why where it isn't obvious. Same backdrop/panel vocabulary as RunQueueModal,
   *  and the same explicit rAF focus (Svelte's `autofocus` is not focus). */
  import type { Hive } from '$lib/tauri/types';
  import { fixedRows, folderLines } from '$lib/hive/settings';

  interface Props {
    hive: Hive;
    name: string;
    onclose: () => void;
    onworkers: () => void;
    onautonomy: () => void;
    onqueue: () => void;
    onbudget: () => void;
  }

  let { hive, name, onclose, onworkers, onautonomy, onqueue, onbudget }: Props = $props();
  let panelEl = $state<HTMLDivElement | null>(null);

  $effect(() => {
    const id = requestAnimationFrame(() => panelEl?.querySelector<HTMLElement>('button')?.focus());
    return () => cancelAnimationFrame(id);
  });

  const rows = $derived(fixedRows(hive, name));
  const folders = $derived(folderLines(hive));

  /** Each editable section replaces this sheet. */
  function go(open: () => void) {
    onclose();
    open();
  }

  function handleKeydown(e: KeyboardEvent) {
    if (e.key === 'Escape') {
      e.stopPropagation();
      onclose();
    }
  }

  function handleBackdropClick(e: MouseEvent) {
    if (e.target === e.currentTarget) onclose();
  }
</script>

<!-- svelte-ignore a11y_no_noninteractive_tabindex -->
<div class="backdrop" onclick={handleBackdropClick} onkeydown={handleKeydown} role="dialog" aria-modal="true" aria-label="Hive settings" tabindex="-1">
  <div class="panel" bind:this={panelEl}>
    <div class="header">
      <div class="title">Hive settings</div>
      <div class="subtitle">What can change opens its own sheet. The rest was fixed when the Hive was made.</div>
    </div>

    <div class="body">
      <section>
        <h4>Can change</h4>
        <div class="links">
          <button type="button" class="btn" onclick={() => go(onworkers)}>Workers and access…</button>
          <button type="button" class="btn" onclick={() => go(onautonomy)}>Autonomy…</button>
          <button type="button" class="btn" onclick={() => go(onqueue)}>Run queue…</button>
          <button type="button" class="btn" onclick={() => go(onbudget)}>Budget and generation…</button>
        </div>
      </section>

      <section>
        <h4>Fixed at creation</h4>
        <dl>
          {#each rows as r (r.label)}
            <dt>{r.label}</dt>
            <dd>{r.value}{#if r.note}<span class="why">{r.note}</span>{/if}</dd>
          {/each}
        </dl>
      </section>

      <section>
        <h4>Folders</h4>
        <ul>
          {#each folders as f (f.path)}
            <li>
              <span>{f.head}</span>
              <code>{f.path}</code>
              {#if f.detail}<span class="why">{f.detail}</span>{/if}
            </li>
          {/each}
        </ul>
      </section>

      {#if hive.brief?.trim() || hive.brief_files?.length}
        <section>
          <h4>Project brief <span class="who">the queen's only</span></h4>
          {#if hive.brief?.trim()}<pre class="instr">{hive.brief}</pre>{/if}
          {#if hive.brief_files?.length}
            <ul>
              {#each hive.brief_files as f (f)}<li><code>{f}</code></li>{/each}
            </ul>
          {/if}
        </section>
      {/if}

      {#if hive.instructions.trim()}
        <section>
          <h4>Rules for every agent</h4>
          <pre class="instr">{hive.instructions}</pre>
        </section>
      {/if}
    </div>

    <div class="footer">
      <button type="button" class="btn" onclick={onclose}>Close</button>
    </div>
  </div>
</div>

<style>
  .backdrop { position: fixed; inset: 0; background: rgba(0, 0, 0, 0.4); display: flex; justify-content: center; padding: 10vh 16px 0; z-index: 1000; }
  .panel {
    align-self: flex-start;
    background: var(--bg-medium);
    border: 1px solid var(--bg-light);
    border-radius: 8px;
    box-shadow: 0 8px 32px rgba(0, 0, 0, 0.5);
    display: flex;
    flex-direction: column;
    max-height: 80vh;
    max-width: 100%;
    width: 560px;
  }
  .header { border-bottom: 1px solid var(--bg-light); padding: 12px 14px 10px; }
  .title { color: var(--fg); font-size: 1rem; font-weight: 600; }
  .subtitle { color: var(--fg-dim); font-size: 0.8rem; line-height: 1.4; margin-top: 3px; }
  .body { display: flex; flex-direction: column; gap: 14px; overflow-y: auto; padding: 14px; }
  section { display: flex; flex-direction: column; gap: 6px; }
  h4 { margin: 0; color: var(--fg-dim); font-size: 0.72rem; font-weight: 600; letter-spacing: 0.06em; text-transform: uppercase; }
  .links { display: flex; flex-wrap: wrap; gap: 6px; }
  dl { display: grid; grid-template-columns: minmax(0, max-content) minmax(0, 1fr); gap: 4px 12px; margin: 0; font-size: 0.8rem; }
  dt { color: var(--fg-dim); }
  dd { margin: 0; color: var(--fg); overflow-wrap: anywhere; display: flex; flex-direction: column; gap: 1px; }
  .why { color: var(--fg-dim); font-size: 0.75rem; overflow-wrap: anywhere; }
  ul { margin: 0; padding-left: 1.2em; display: flex; flex-direction: column; gap: 4px; }
  li { color: var(--fg); font-size: 0.8rem; display: flex; flex-direction: column; gap: 1px; overflow-wrap: anywhere; }
  code { font-family: var(--font-mono, ui-monospace, monospace); font-size: 0.75rem; color: var(--fg-dim); overflow-wrap: anywhere; }
  .who { text-transform: none; letter-spacing: 0; font-weight: 400; margin-left: 6px; }
  .instr { margin: 0; font-family: inherit; font-size: 0.8rem; color: var(--fg); white-space: pre-wrap; overflow-wrap: anywhere; max-height: 12em; overflow-y: auto; }
  .footer { align-items: center; border-top: 1px solid var(--bg-light); display: flex; justify-content: flex-end; padding: 10px 14px; }
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
  .btn:hover { border-color: var(--fg-dim); }
  .btn:focus-visible { outline: 2px solid var(--accent); outline-offset: 1px; }
</style>
