<script lang="ts">
  /** A tab's follow-ups (docs/follow-ups.md §8): what this tab's agent will be sent back later.
   *  Opened from the tab's clock badge or its context menu (the `open-follow-ups` event, owned by
   *  +layout).
   *
   *  It is a LIST first. A watch script waiting to be allowed is a decision, so it sits on top as
   *  the same ScriptApprovalCard the Decisions queue shows; everything else is one row per
   *  follow-up, led by what it waits for. Scheduling one by hand is folded away until asked for:
   *  open by default, that form read as the next step after approving a script. */
  import { followUpsStore } from '$lib/stores/followUps.svelte';
  import { preferencesStore } from '$lib/stores/preferences.svelte';
  import { whenText, durationText } from '$lib/followUps/model';
  import * as commands from '$lib/tauri/commands';
  import type { WatchStatus } from '$lib/tauri/types';
  import ScriptApprovalCard from './ScriptApprovalCard.svelte';

  interface Props {
    tabId: string | null;
    onclose: () => void;
  }

  let { tabId, onclose }: Props = $props();

  let dialogEl = $state<HTMLDivElement | null>(null);
  /** The list reads relative times ("in 12m"); tick while open so they don't go stale. */
  let now = $state(Date.now());
  let adding = $state(false);
  let text = $state('');
  let minutes = $state(30);
  let busy = $state(false);
  let addError = $state<string | null>(null);
  /** Per follow-up: why "Send now" didn't send, until the next attempt. */
  let heldReasons = $state<Record<string, string>>({});
  /** The outcome of the last approval answered here; its card leaves once answered. */
  let approvalNote = $state<string | null>(null);
  /** How each watch script's runs have gone (Rust, since this launch), refreshed while open. */
  let watch = $state<Record<string, WatchStatus>>({});

  async function refreshWatch() {
    try {
      watch = await commands.followUpWatchStatus();
    } catch {
      watch = {};
    }
  }

  /** Seconds as a human reads them, never rounded: `durationText` rounds to whole minutes, which
   *  turned a 15-second schedule into "every 1m" and a 90-second one into "every 2m". */
  function secsText(secs: number): string {
    const s = Math.max(0, Math.round(secs));
    return s < 60 || s % 60 !== 0 ? `${s}s` : durationText(s * 1000);
  }

  /** "checked 40s ago: not yet", for a script's row. */
  function runText(id: string): string {
    const s = watch[id];
    if (!s?.last_run_at) return s?.running ? 'first check running' : 'not checked yet';
    const ago = secsText((now - Date.parse(s.last_run_at)) / 1000);
    // A host that can't be reached is waited on, not counted: say so, and why.
    if (s.last_result === 'unreachable') return `waiting for the connection (${s.detail ?? 'host unreachable'})`;
    const result = s.last_result === 'not_yet' ? 'not yet' : s.last_result === 'broken' ? `broken (${s.detail ?? 'unknown'})` : 'passed';
    const streak = s.broken_runs > 1 ? `, ${s.broken_runs} runs in a row` : '';
    return `checked ${ago} ago: ${result}${streak}`;
  }

  const open = $derived(tabId !== null);
  // The store's own search (archived tabs included), so this can't say "nothing" for a tab the
  // store can see. `found` null: the tab is gone — closed, or reloaded under a new id.
  const found = $derived(tabId ? followUpsStore.findTab(tabId) : null);
  const tab = $derived(found?.tab ?? null);
  const live = $derived(preferencesStore.followUpsLive);
  const rows = $derived.by(() => {
    void now; // re-derive as time passes: statuses move from pending to due to expired
    const all = tab?.follow_ups ?? [];
    return tabId ? followUpsStore.list(tabId).map(v => ({ v, f: all.find(f => f.id === v.id)! })).filter(r => r.f) : [];
  });
  const asking = $derived(rows.filter(r => r.v.awaiting_approval && r.v.status !== 'expired'));
  const listed = $derived(rows.filter(r => !(r.v.awaiting_approval && r.v.status !== 'expired')));
  /** When the approval cards last changed, for their click guard (ScriptApprovalCard). */
  let askingChangedAt = $state(0);
  const askingIds = $derived(asking.map(r => r.f.id).join('|'));
  $effect(() => {
    void askingIds;
    askingChangedAt = Date.now();
  });

  $effect(() => {
    if (!open) return;
    now = Date.now();
    adding = false;
    text = '';
    addError = null;
    heldReasons = {};
    approvalNote = null;
    void refreshWatch();
    // 5 s, not the list's usual 15: "checked 10s ago" is read in seconds.
    const t = setInterval(() => { now = Date.now(); void refreshWatch(); }, 5_000);
    // Explicit focus, not `autofocus`: opened from a context menu, the backdrop must hold focus
    // or Escape never reaches it (root CLAUDE.md, "Svelte's autofocus is not focus").
    requestAnimationFrame(() => dialogEl?.focus());
    return () => clearInterval(t);
  });

  async function sendNow(id: string) {
    if (!tabId) return;
    busy = true;
    try {
      const reason = await followUpsStore.deliverNow(tabId, id);
      heldReasons = { ...heldReasons, [id]: reason ?? '' };
    } finally {
      busy = false;
      now = Date.now();
    }
  }

  async function remove(id: string) {
    if (!tabId) return;
    busy = true;
    try {
      await followUpsStore.cancel(tabId, id);
    } finally {
      busy = false;
    }
  }

  async function add() {
    if (!tabId) return;
    busy = true;
    addError = null;
    try {
      const r = await followUpsStore.create(tabId, { text, in_minutes: minutes }, 'human');
      if (r.ok) {
        text = '';
        adding = false;
      } else addError = r.detail;
    } catch (e) {
      addError = String(e);
    } finally {
      busy = false;
      now = Date.now();
    }
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

{#if open}
  <!-- svelte-ignore a11y_no_noninteractive_tabindex -->
  <div
    class="backdrop"
    bind:this={dialogEl}
    onclick={handleBackdropClick}
    onkeydown={handleKeydown}
    role="dialog"
    aria-modal="true"
    aria-label="Follow-ups"
    tabindex="-1"
  >
    <div class="palette">
      <header>
        <h2>Follow-ups{#if tab}<span class="tab-name">{tab.name}</span>{/if}</h2>
        <p>What this tab's agent will be sent later, between its turns.</p>
        {#if !live}
          <p class="notice">Follow-ups are off, so nothing here is sent or checked. Turn them on in Preferences → Overlord.</p>
        {/if}
        {#if found?.archived}
          <p class="notice">This tab is archived. Its follow-ups are kept and go once it's restored.</p>
        {/if}
      </header>

      <div class="body">
        {#if !found}
          <p class="status">This tab is gone — closed, or reloaded under a new id. Open Follow-ups from its tab again.</p>
        {:else}
          {#each asking as { f } (f.id)}
            <ScriptApprovalCard tabId={tabId!} followUp={f} listChangedAt={askingChangedAt} onnote={(t) => (approvalNote = t)} />
          {/each}
          {#if approvalNote}<p class="outcome">{approvalNote}</p>{/if}

          {#if listed.length === 0 && asking.length === 0}
            <p class="status">Nothing scheduled.</p>
          {/if}

          {#if listed.length}
            <ul class="list">
              {#each listed as { v, f } (v.id)}
                <li class="row" class:expired={v.status === 'expired'}>
                  <div class="when" class:due={v.status === 'due'}>{whenText(f, now)}</div>
                  <p class="text">{v.text}</p>
                  {#if f.due.kind === 'script'}
                    <details class="script">
                      <summary>
                        {f.due.met_at ? 'Watch script' : `Watch script, every ${secsText(f.due.every_secs ?? 60)} · ${runText(v.id)}`}
                      </summary>
                      <pre>{f.due.script}</pre>
                      <div class="where">{#if f.due.host}on {f.due.host}, {/if}in {f.due.cwd} · up to {f.due.timeout_secs ?? 10}s a run</div>
                    </details>
                  {/if}
                  <div class="meta">
                    {v.author === 'human' ? 'Added by you' : v.author === 'maiterm' ? 'Added by maiTerm' : 'Scheduled by the agent'}{#if v.status === 'due'}{v.waiting ? `. Waiting: ${v.waiting}` : '. Goes at the next check'}{:else if v.status === 'expired'}. Expired, won't be sent{/if}
                  </div>
                  {#if heldReasons[v.id]}
                    <div class="held">Not sent: {heldReasons[v.id]}</div>
                  {/if}
                  <div class="actions">
                    {#if v.status !== 'expired'}
                      <button class="btn" onclick={() => sendNow(v.id)} disabled={busy || !live}>Send now</button>
                    {/if}
                    <button class="btn" onclick={() => remove(v.id)} disabled={busy}>Remove</button>
                  </div>
                </li>
              {/each}
            </ul>
          {/if}
        {/if}
      </div>

      {#if adding}
        <div class="add">
          <textarea
            bind:value={text}
            rows="2"
            placeholder="What should the agent be sent?"
            disabled={busy}
          ></textarea>
          <div class="add-row">
            <label class="minutes">
              in
              <input type="number" min="1" max="10080" bind:value={minutes} disabled={busy} />
              minutes
            </label>
            <div class="spacer"></div>
            <button class="btn" onclick={() => { adding = false; addError = null; }} disabled={busy}>Cancel</button>
            <button class="btn btn-primary" onclick={add} disabled={busy || !text.trim() || !found}>Schedule</button>
          </div>
          {#if addError}<div class="error">{addError}</div>{/if}
        </div>
      {/if}

      <footer>
        {#if !adding && found}
          <button class="link" onclick={() => (adding = true)}>Schedule one yourself</button>
        {/if}
        <div class="spacer"></div>
        <button class="btn" onclick={onclose}>Close</button>
      </footer>
    </div>
  </div>
{/if}

<style>
  .backdrop {
    position: fixed;
    inset: 0;
    background: rgba(0, 0, 0, 0.4);
    display: flex;
    justify-content: center;
    padding-top: 10vh;
    z-index: 1000;
    outline: none;
  }

  .palette {
    background: var(--bg-medium);
    border: 1px solid var(--bg-light);
    border-radius: 8px;
    width: min(540px, calc(100vw - 32px));
    max-height: 78vh;
    display: flex;
    flex-direction: column;
    box-shadow: 0 8px 32px rgba(0, 0, 0, 0.5);
    align-self: flex-start;
  }

  header {
    padding: 12px 16px 10px;
    border-bottom: 1px solid var(--bg-light);
  }
  h2 {
    margin: 0;
    display: flex;
    flex-wrap: wrap;
    align-items: baseline;
    gap: 2px 10px;
    font-size: 1rem;
    font-weight: 600;
    color: var(--fg);
  }
  .tab-name { font-size: 0.85rem; font-weight: 500; color: var(--accent); overflow-wrap: anywhere; }
  header p { margin: 3px 0 0; font-size: 0.8rem; color: var(--fg-dim); line-height: 1.4; }
  header .notice {
    margin-top: 8px;
    padding: 6px 8px;
    border-radius: 5px;
    background: var(--bg-dark);
    color: var(--yellow, #e0af68);
  }

  .body {
    flex: 1;
    overflow-y: auto;
    padding: 12px 16px;
    display: flex;
    flex-direction: column;
    gap: 12px;
  }
  .status { margin: 4px 0; font-size: 0.85rem; color: var(--fg-dim); }
  .outcome { margin: 0; font-size: 0.8rem; color: var(--fg-dim); }

  .list { list-style: none; margin: 0; padding: 0; display: flex; flex-direction: column; }
  .row { padding: 10px 0; display: flex; flex-direction: column; gap: 4px; min-width: 0; }
  .row + .row { border-top: 1px solid var(--bg-light); }
  .row:first-child { padding-top: 0; }

  /* What it waits for leads: that is what tells two follow-ups apart at a glance. */
  .when { font-size: 0.82rem; font-weight: 600; color: var(--fg); overflow-wrap: anywhere; }
  .when.due { color: var(--accent); }
  .expired .when, .expired .text { color: var(--fg-dim); }
  .text {
    margin: 0;
    font-size: 0.82rem;
    color: var(--fg-dim);
    line-height: 1.45;
    white-space: pre-wrap;
    overflow-wrap: anywhere;
    display: -webkit-box;
    -webkit-line-clamp: 3;
    line-clamp: 3;
    -webkit-box-orient: vertical;
    overflow: hidden;
  }

  .script summary { cursor: pointer; font-size: 0.76rem; color: var(--fg-dim); overflow-wrap: anywhere; }
  .script summary:hover, .script summary:focus-visible { color: var(--fg); }
  .script pre {
    margin: 6px 0 0;
    padding: 6px 8px;
    border-radius: 5px;
    border: 1px solid var(--bg-light);
    background: var(--bg-dark);
    color: var(--fg);
    font-family: var(--font-mono, ui-monospace, monospace);
    font-size: 0.76rem;
    white-space: pre-wrap;
    overflow-wrap: anywhere;
  }
  .where { margin-top: 3px; font-size: 0.72rem; color: var(--fg-dim); overflow-wrap: anywhere; }

  .meta { font-size: 0.74rem; color: var(--fg-dim); overflow-wrap: anywhere; }
  .held { font-size: 0.78rem; color: var(--yellow, #e0af68); }
  .actions { display: flex; flex-wrap: wrap; gap: 6px; margin-top: 2px; }

  .add {
    padding: 10px 16px;
    border-top: 1px solid var(--bg-light);
    display: flex;
    flex-direction: column;
    gap: 6px;
  }
  textarea {
    resize: vertical;
    min-height: 44px;
    padding: 6px 8px;
    border-radius: 6px;
    border: 1px solid var(--bg-light);
    background: var(--bg-dark);
    color: var(--fg);
    font: inherit;
    font-size: 0.85rem;
  }
  .add-row { display: flex; align-items: center; gap: 8px; flex-wrap: wrap; }
  .minutes { display: flex; align-items: center; gap: 6px; font-size: 0.8rem; color: var(--fg-dim); }
  .minutes input {
    width: 70px;
    padding: 3px 6px;
    border-radius: 5px;
    border: 1px solid var(--bg-light);
    background: var(--bg-dark);
    color: var(--fg);
    font: inherit;
  }
  .error { font-size: 0.8rem; color: var(--error, #f7768e); }

  footer {
    display: flex;
    align-items: center;
    gap: 8px;
    padding: 10px 16px;
    border-top: 1px solid var(--bg-light);
  }
  .spacer { flex: 1; }

  .link {
    background: none;
    border: 0;
    padding: 0;
    font: inherit;
    font-size: 0.8rem;
    color: var(--fg-dim);
    cursor: pointer;
  }
  .link:hover, .link:focus-visible { color: var(--fg); text-decoration: underline; }

  .btn {
    padding: 4px 12px;
    border-radius: 6px;
    border: 1px solid var(--bg-light);
    background: var(--bg-dark);
    color: var(--fg);
    font-size: 0.8rem;
    cursor: pointer;
  }
  .btn:hover:not(:disabled) { background: var(--bg-light); }
  .btn:disabled { opacity: 0.5; cursor: default; }
  .btn:focus-visible, .link:focus-visible { outline: 2px solid var(--accent); outline-offset: 2px; }
  .btn-primary { background: var(--accent); border-color: var(--accent); color: var(--bg-dark); }
  .btn-primary:hover:not(:disabled) { background: var(--accent); filter: brightness(1.1); }
</style>
