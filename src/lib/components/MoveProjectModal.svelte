<script lang="ts">
  /**
   * Move project (docs/relocate.md): move a project folder — or point maiTerm at one that was
   * moved outside it — and carry every tab, service and agent session along. Shows exactly what
   * will change and which running tabs will restart before anything happens.
   */
  import { error as logError } from '@tauri-apps/plugin-log';
  import * as commands from '$lib/tauri/commands';
  import type { RelocateOutcome, RelocatePreview } from '$lib/tauri/types';
  import { relocateStore, liveTabsUnder } from '$lib/stores/relocate.svelte';
  import { dispatch } from '$lib/stores/notificationDispatch';
  import Button from '$lib/components/ui/Button.svelte';
  import IconButton from '$lib/components/ui/IconButton.svelte';

  const req = $derived(relocateStore.request);
  const repoint = $derived(req?.mode === 'repoint');

  let target = $state('');
  let preview = $state<RelocatePreview | null>(null);
  let previewError = $state<string | null>(null);
  let restarts = $state<{ name: string; working: boolean }[]>([]);
  let running = $state(false);
  let outcome = $state<RelocateOutcome | null>(null);
  let runError = $state<string | null>(null);
  let input = $state<HTMLInputElement | null>(null);

  // A fresh request resets the dialog.
  let seenFor: object | null = null;
  $effect(() => {
    if (!req || req === seenFor) return;
    seenFor = req;
    target = req.suggested ?? (req.mode === 'move' ? req.old : '');
    preview = null;
    previewError = null;
    outcome = null;
    runError = null;
    running = false;
    void liveTabsUnder([req.old]).then((t) => {
      restarts = t.map((x) => ({ name: x.tab.name || 'Terminal', working: x.working }));
    });
    // Explicit focus: Svelte's autofocus doesn't move focus off a keyboard opener (CLAUDE.md).
    requestAnimationFrame(() => {
      input?.focus();
      if (req.mode === 'move') input?.select();
    });
  });

  // Preview whenever the target settles.
  let previewSeq = 0;
  $effect(() => {
    const r = req;
    const t = target.trim();
    if (!r || !t || outcome) return;
    // Moving: the field starts as the folder itself, to be edited — not an error yet.
    if (r.mode === 'move' && t.replace(/\/+$/, '') === r.old.replace(/\/+$/, '')) { preview = null; previewError = null; return; }
    const seq = ++previewSeq;
    const timer = setTimeout(async () => {
      try {
        const p = await commands.previewRelocation(r.old, t, r.mode === 'move');
        if (seq === previewSeq) { preview = p; previewError = null; }
      } catch (e) {
        if (seq === previewSeq) { preview = null; previewError = String(e); }
      }
    }, 250);
    return () => clearTimeout(timer);
  });

  async function browse() {
    if (!req) return;
    const start = repoint ? (req.old.split('/').slice(0, -1).join('/') || null) : (target || req.old);
    const picked = await commands.pickFolder(start);
    if (!picked) return;
    // Moving: the picked folder is where it goes INTO, keeping its name.
    target = repoint ? picked : `${picked.replace(/\/+$/, '')}/${req.old.split('/').filter(Boolean).pop()}`;
  }

  async function run() {
    if (!req || !preview || running) return;
    running = true;
    runError = null;
    relocateStore.markRan();
    try {
      outcome = await commands.relocateProject(req.old, target.trim(), req.mode === 'move');
      const n = outcome.state.tabs.length;
      dispatch(
        repoint ? 'Project located' : 'Project moved',
        `${n} tab${n === 1 ? '' : 's'} now in ${preview.new}`,
        outcome.agents.warnings.length ? 'error' : 'success',
      );
      if (repoint) await relocateStore.scan();
    } catch (e) {
      logError(`relocate: ${e}`);
      runError = String(e);
    } finally {
      running = false;
    }
  }

  function close() {
    if (running) return;
    relocateStore.close();
  }

  function onkeydown(e: KeyboardEvent) {
    if (e.key === 'Escape') close();
    if (e.key === 'Enter' && !outcome && preview && !running) run();
  }

  const working = $derived(restarts.filter((r) => r.working));
  const agentLines = $derived.by(() => {
    const a = preview?.agents;
    if (!a) return [];
    const out: string[] = [];
    if (a.claude_sessions || a.claude_memory) out.push(`Claude: ${a.claude_sessions} session${a.claude_sessions === 1 ? '' : 's'}${a.claude_memory ? ' and project memory' : ''}`);
    if (a.claude_trust) out.push('Claude: folder trust and project settings');
    if (a.codex_sessions) out.push(`Codex: ${a.codex_sessions} session${a.codex_sessions === 1 ? '' : 's'}`);
    if (a.codex_trust) out.push('Codex: folder trust');
    if (a.gemini_projects) out.push(`Gemini: ${a.gemini_projects} project${a.gemini_projects === 1 ? '' : 's'}`);
    return out;
  });
</script>

{#if req}
  <div
    class="backdrop"
    onclick={(e) => { if (e.target === e.currentTarget) close(); }}
    {onkeydown}
    role="dialog"
    aria-modal="true"
    tabindex="-1"
  >
    <div class="modal">
      <div class="header">
        <h2>{repoint ? 'Locate moved project' : 'Move project'}</h2>
        <IconButton tooltip="Close" style="font-size: 1.538rem;padding:4px 8px;width:auto;height:auto" onclick={close}>&times;</IconButton>
      </div>

      <div class="content">
        {#if repoint}
          <p class="lead">
            <code>{req.old}</code> no longer exists.
            {#if req.missing}{req.missing.tab_ids.length} tab{req.missing.tab_ids.length === 1 ? '' : 's'}{#if req.missing.service_ids.length} and {req.missing.service_ids.length} service{req.missing.service_ids.length === 1 ? '' : 's'}{/if} would open in your home folder instead.{/if}
            Where is it now?
          </p>
          {#if req.missing?.candidates.length}
            <div class="candidates">
              {#each req.missing.candidates as c (c)}
                <label class="candidate">
                  <input type="radio" name="candidate" value={c} bind:group={target} />
                  <code>{c}</code>
                </label>
              {/each}
            </div>
          {/if}
        {:else}
          <div class="row"><span class="label">From</span><code class="path">{req.old}</code></div>
        {/if}

        <div class="row">
          <span class="label">{repoint ? 'Folder' : 'To'}</span>
          <input bind:this={input} bind:value={target} spellcheck="false" autocomplete="off" disabled={running || !!outcome} />
          <Button variant="secondary" onclick={browse} disabled={running || !!outcome}>Browse…</Button>
        </div>

        {#if previewError && !outcome}
          <p class="error">{previewError}</p>
        {/if}

        {#if preview && !outcome}
          <div class="summary">
            <div class="summary-title">This will</div>
            <ul>
              {#if !repoint}<li>Rename the folder on disk</li>{/if}
              <li>Point {preview.tabs} tab{preview.tabs === 1 ? '' : 's'}{#if preview.services}, {preview.services} service{preview.services === 1 ? '' : 's'}{/if} at the new location</li>
              {#each agentLines as l (l)}<li>Carry {l}</li>{/each}
              {#if restarts.length}
                <li>Restart {restarts.length} running tab{restarts.length === 1 ? '' : 's'} in this window, each resuming its agent</li>
              {/if}
            </ul>
            {#if working.length}
              <p class="warn">
                Still working — interrupted mid-turn: {working.map((w) => w.name).join(', ')}
              </p>
            {/if}
            <p class="hint">Tabs in other windows are restarted by their own window. If one doesn't answer, nothing is moved.</p>
          </div>
        {/if}

        {#if runError}
          <p class="error">{runError}</p>
        {/if}

        {#if outcome}
          <div class="summary">
            <div class="summary-title">Done</div>
            <ul>
              <li>{outcome.state.tabs.length} tab{outcome.state.tabs.length === 1 ? '' : 's'}{#if outcome.state.services}, {outcome.state.services} service{outcome.state.services === 1 ? '' : 's'}{/if} moved{#if outcome.state.approvals_carried}; {outcome.state.approvals_carried} watch-script approval{outcome.state.approvals_carried === 1 ? '' : 's'} kept{/if}</li>
              {#each outcome.agents.done as d (d)}<li>{d}</li>{/each}
            </ul>
            {#each outcome.agents.warnings as w (w)}<p class="warn">{w}</p>{/each}
            {#if outcome.unconfirmed_windows.length}
              <p class="warn">{outcome.unconfirmed_windows.length} window{outcome.unconfirmed_windows.length === 1 ? '' : 's'} didn't confirm — reload {outcome.unconfirmed_windows.length === 1 ? 'it' : 'them'} (Cmd+R) before using {outcome.unconfirmed_windows.length === 1 ? 'it' : 'them'}.</p>
            {/if}
          </div>
        {/if}
      </div>

      <div class="footer">
        {#if outcome}
          <Button onclick={close}>Close</Button>
        {:else}
          <Button variant="secondary" onclick={close} disabled={running}>Cancel</Button>
          <Button onclick={run} disabled={!preview || running}>
            {#if running}Working…{:else if repoint}Use this folder{:else}Move{/if}
          </Button>
        {/if}
      </div>
    </div>
  </div>
{/if}

<style>
  .backdrop {
    position: fixed;
    inset: 0;
    background: rgba(0, 0, 0, 0.6);
    display: flex;
    align-items: center;
    justify-content: center;
    z-index: 1000;
  }
  .modal {
    background: var(--bg-medium);
    border: 1px solid var(--bg-light);
    border-radius: 10px;
    width: 560px;
    max-width: calc(100vw - 32px);
    max-height: 80vh;
    display: flex;
    flex-direction: column;
    box-shadow: 0 8px 32px rgba(0, 0, 0, 0.4);
  }
  .header {
    display: flex;
    align-items: center;
    justify-content: space-between;
    padding: 16px 20px 12px;
    border-bottom: 1px solid var(--bg-light);
  }
  .header h2 {
    font-size: 1.154rem;
    font-weight: 600;
    color: var(--fg);
    margin: 0;
  }
  .content {
    padding: 16px 20px;
    overflow-y: auto;
    flex: 1;
    min-height: 0;
    display: flex;
    flex-direction: column;
    gap: 12px;
  }
  .lead, .hint, .error, .warn {
    margin: 0;
    font-size: 0.923rem;
    overflow-wrap: anywhere;
  }
  .lead { color: var(--fg); }
  .hint { color: var(--fg-dim); font-size: 0.846rem; margin-top: 6px; }
  .error { color: var(--red, #f7768e); }
  .warn { color: var(--yellow, #e0af68); margin-top: 6px; }
  code {
    font-family: monospace;
    font-size: 0.885rem;
    overflow-wrap: anywhere;
  }
  .row {
    display: flex;
    align-items: center;
    gap: 8px;
  }
  .label {
    width: 52px;
    flex-shrink: 0;
    font-size: 0.923rem;
    color: var(--fg-dim);
  }
  .path { color: var(--fg); }
  .row input {
    flex: 1;
    min-width: 0;
    font-family: monospace;
    font-size: 0.885rem;
    padding: 5px 8px;
    border-radius: 4px;
    border: 1px solid var(--bg-light);
    background: var(--bg-dark);
    color: var(--fg);
  }
  .candidates {
    display: flex;
    flex-direction: column;
    gap: 4px;
  }
  .candidate {
    display: flex;
    align-items: center;
    gap: 8px;
    padding: 4px 6px;
    border-radius: 4px;
    cursor: pointer;
    color: var(--fg);
  }
  .candidate:hover { background: color-mix(in srgb, var(--bg-light) 50%, transparent); }
  .candidate input { accent-color: var(--accent); }
  .summary {
    background: var(--bg-dark);
    border-radius: 6px;
    padding: 10px 12px;
  }
  .summary-title {
    font-size: 0.846rem;
    font-weight: 600;
    color: var(--fg-dim);
    text-transform: uppercase;
    letter-spacing: 0.5px;
    margin-bottom: 4px;
  }
  .summary ul {
    margin: 0;
    padding-left: 18px;
    font-size: 0.923rem;
    color: var(--fg);
    display: flex;
    flex-direction: column;
    gap: 2px;
  }
  .footer {
    display: flex;
    justify-content: flex-end;
    gap: 8px;
    padding: 12px 20px;
    border-top: 1px solid var(--bg-light);
  }
</style>
