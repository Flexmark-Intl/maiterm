<script lang="ts">
  /** A Done Hive's report (docs/hive.md §11.4, plan D7), read from Rust's `logs/finishes.jsonl`:
   *  each criterion with how it is met and the receipts maiTerm verified, what landed, what was
   *  abandoned, the final assets, the spend and what the coordinator decided alone. The
   *  coordinator's own words (summary, how, reasons) are quoted as its; the rest is maiTerm's.
   *  Read-only. HiveAutonomyModal's vocabulary, and the same explicit rAF focus. */
  import type { HiveFinishRecord } from '$lib/tauri/types';
  import { hiveFinishReport } from '$lib/tauri/commands';
  import { doneLabel, landedLine, receiptLine } from '$lib/hive/finish';
  import { usd } from '$lib/hive/budget';
  import { error as logError } from '@tauri-apps/plugin-log';

  interface Props {
    workspaceId: string;
    onclose: () => void;
  }

  let { workspaceId, onclose }: Props = $props();

  let report = $state<HiveFinishRecord | null>(null);
  let loading = $state(true);
  let failed = $state<string | null>(null);
  let panelEl = $state<HTMLDivElement | null>(null);

  $effect(() => {
    const id = workspaceId;
    loading = true;
    hiveFinishReport(id)
      .then((r) => { if (id === workspaceId) report = r; })
      .catch((e) => {
        failed = String(e).replace(/^Error: /, '');
        logError(`hive: finish report of ${id.slice(0, 8)} not read: ${e}`);
      })
      .finally(() => { if (id === workspaceId) loading = false; });
  });

  $effect(() => {
    const id = requestAnimationFrame(() => panelEl?.querySelector<HTMLElement>('button')?.focus());
    return () => cancelAnimationFrame(id);
  });

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
<div class="backdrop" onclick={handleBackdropClick} onkeydown={handleKeydown} role="dialog" aria-modal="true" aria-label="Done report" tabindex="-1">
  <div class="panel" bind:this={panelEl}>
    <div class="header">
      <div class="title">{report ? doneLabel(report.at_ms) : 'Done'}</div>
      <div class="subtitle">What the queen finished, with the receipts maiTerm checked. Its own words are quoted.</div>
    </div>

    <div class="body">
      {#if loading}
        <p class="dim">Reading the report…</p>
      {:else if failed}
        <p class="error" role="alert">{failed}</p>
      {:else if !report}
        <p class="dim">This Hive has no report yet.</p>
      {:else}
        <section>
          <span class="label">The queen’s summary</span>
          <blockquote>{report.summary}</blockquote>
        </section>

        <section>
          <span class="label">Completion criteria</span>
          <ol class="criteria">
            {#each report.criteria as c (c.n)}
              <li value={c.n}>
                <div class="crit">{c.text}</div>
                <blockquote class="how">{c.how}</blockquote>
                <ul class="receipts">
                  {#each c.receipts as r, i (i)}
                    {@const line = receiptLine(r)}
                    <li><span class="kind">{line.kind}</span> <span class="rtext">{line.text}</span></li>
                  {/each}
                </ul>
              </li>
            {/each}
          </ol>
        </section>

        <section>
          <span class="label">Landed</span>
          {#if report.landed.length}
            <ul class="plain">
              {#each report.landed as l (l.land)}<li class:muted={l.undone}>{landedLine(l)}</li>{/each}
            </ul>
          {:else}
            <p class="dim">Nothing landed.</p>
          {/if}
        </section>

        {#if report.abandoned.length}
          <section>
            <span class="label">Abandoned (branches kept)</span>
            <ul class="plain">
              {#each report.abandoned as a (a.worker)}
                <li>
                  <b>{a.worker}</b>: {a.branches.join('; ')}
                  <blockquote class="how">{a.reason}</blockquote>
                </li>
              {/each}
            </ul>
          </section>
        {/if}

        <section>
          <span class="label">Final assets</span>
          {#if report.assets_final.length}
            <ul class="plain">
              {#each report.assets_final as a (a.id)}<li>{a.title || a.id} <span class="dim">({a.kind})</span></li>{/each}
            </ul>
          {:else}
            <p class="dim">None marked final.</p>
          {/if}
        </section>

        <section>
          <span class="label">Spent</span>
          <p class="fact">{usd(report.spent_micros)} on paid generation, as agents recorded it</p>
        </section>

        <section>
          <span class="label">Decided alone</span>
          {#if report.decided_alone.length}
            <ul class="plain">
              {#each report.decided_alone as d, i (i)}<li>{d}</li>{/each}
            </ul>
          {:else}
            <p class="dim">The queen decided nothing alone.</p>
          {/if}
        </section>
      {/if}
    </div>

    <div class="footer">
      <span class="hint">{report ? `Report ${report.id}` : ''}</span>
      <button type="button" class="btn" onclick={onclose}>Close</button>
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
    width: 680px;
  }

  .header { border-bottom: 1px solid var(--bg-light); padding: 12px 14px 10px; }
  .title { color: var(--fg); font-size: 1rem; font-weight: 600; }
  .subtitle { color: var(--fg-dim); font-size: 0.8rem; line-height: 1.4; margin-top: 3px; }

  .body { display: flex; flex-direction: column; gap: 14px; overflow-y: auto; padding: 14px; }
  section { display: flex; flex-direction: column; gap: 5px; min-width: 0; }
  .label { color: var(--fg-dim); font-size: 0.72rem; font-weight: 600; letter-spacing: 0.06em; text-transform: uppercase; }
  blockquote {
    border-left: 2px solid var(--bg-light);
    color: var(--fg);
    font-size: 0.82rem;
    line-height: 1.45;
    margin: 0;
    overflow-wrap: anywhere;
    padding: 2px 0 2px 9px;
    white-space: pre-wrap;
  }
  .how { color: var(--fg-dim); font-size: 0.78rem; }
  .criteria { display: flex; flex-direction: column; gap: 10px; margin: 0; padding-left: 20px; }
  .criteria > li { color: var(--fg); font-size: 0.85rem; }
  .crit { font-weight: 600; margin-bottom: 4px; overflow-wrap: anywhere; }
  .receipts { list-style: none; margin: 5px 0 0; padding: 0; }
  .receipts li { font-size: 0.76rem; line-height: 1.45; overflow-wrap: anywhere; }
  .kind {
    background: var(--bg-dark);
    border-radius: 3px;
    color: var(--accent);
    font-size: 0.68rem;
    padding: 0 5px;
    text-transform: uppercase;
  }
  .rtext { color: var(--fg); }
  .plain { color: var(--fg); font-size: 0.8rem; line-height: 1.45; margin: 0; padding-left: 18px; }
  .plain li { overflow-wrap: anywhere; }
  .muted { opacity: 0.55; }
  .fact { color: var(--fg); font-size: 0.82rem; margin: 0; }
  .dim { color: var(--fg-dim); font-size: 0.8rem; margin: 0; }
  .error { color: var(--red); font-size: 0.75rem; margin: 0; overflow-wrap: anywhere; }

  .footer { align-items: center; border-top: 1px solid var(--bg-light); display: flex; gap: 10px; justify-content: space-between; padding: 10px 14px; }
  .hint { color: var(--fg-dim); font-size: 0.72rem; }
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
