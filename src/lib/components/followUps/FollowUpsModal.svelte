<script lang="ts">
  /** A tab's follow-ups (docs/follow-ups.md §8): what is pending, why one that is due hasn't
   *  gone, and the human's three levers — deliver now, cancel, add one by hand. Opened from the
   *  tab's context menu (the `open-follow-ups` event, owned by +layout). */
  import { followUpsStore } from '$lib/stores/followUps.svelte';
  import { preferencesStore } from '$lib/stores/preferences.svelte';
  import { workspacesStore } from '$lib/stores/workspaces.svelte';
  import { whenText } from '$lib/followUps/model';

  interface Props {
    tabId: string | null;
    onclose: () => void;
  }

  let { tabId, onclose }: Props = $props();

  let dialogEl = $state<HTMLDivElement | null>(null);
  /** The list reads relative times ("in 12m"); tick while open so they don't go stale. */
  let now = $state(Date.now());
  let text = $state('');
  let minutes = $state(30);
  let busy = $state(false);
  let addError = $state<string | null>(null);
  /** Per follow-up: why "Deliver now" didn't deliver, until the next attempt. */
  let heldReasons = $state<Record<string, string>>({});

  const open = $derived(tabId !== null);
  const tab = $derived(tabId ? workspacesStore._locateTab(tabId)?.tab ?? null : null);
  const live = $derived(preferencesStore.followUpsLive);
  const rows = $derived.by(() => {
    void now; // re-derive as time passes: statuses move from pending to due to expired
    const all = tab?.follow_ups ?? [];
    return tabId ? followUpsStore.list(tabId).map(v => ({ v, f: all.find(f => f.id === v.id)! })).filter(r => r.f) : [];
  });

  $effect(() => {
    if (!open) return;
    now = Date.now();
    text = '';
    addError = null;
    heldReasons = {};
    const t = setInterval(() => { now = Date.now(); }, 15_000);
    // Explicit focus, not `autofocus`: opened from a context menu, the backdrop must hold focus
    // or Escape never reaches it (root CLAUDE.md, "Svelte's autofocus is not focus").
    requestAnimationFrame(() => dialogEl?.focus());
    return () => clearInterval(t);
  });

  async function deliverNow(id: string) {
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

  async function cancel(id: string) {
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
      if (r.ok) text = '';
      else addError = r.detail;
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
      <div class="header">
        <div class="title">Follow-ups</div>
        <div class="subtitle">
          Prompts scheduled back into {#if tab}<strong>{tab.name}</strong>{:else}this tab{/if}'s agent.
          Each goes between turns, when nothing is typed in its input box, and only once.
        </div>
        {#if !live}
          <div class="notice">
            Follow-ups are off, so these are held, not delivered. Turn them on in
            Preferences → Overlord.
          </div>
        {/if}
      </div>

      <div class="body">
        {#if rows.length === 0}
          <p class="status">Nothing scheduled.</p>
        {:else}
          {#each rows as { v, f } (v.id)}
            <div class="row" class:expired={v.status === 'expired'}>
              <div class="row-text">{v.text}</div>
              <div class="row-meta">
                <span class="when" class:due={v.status === 'due'}>{whenText(f, now)}</span>
                <span class="dot">·</span>
                <span>{v.author === 'human' ? 'added by you' : v.author === 'maiterm' ? 'added by maiTerm' : 'scheduled by the agent'}</span>
                {#if v.status === 'due'}
                  <span class="dot">·</span><span>waiting for the agent to be free</span>
                {:else if v.status === 'expired'}
                  <span class="dot">·</span><span>won't be delivered</span>
                {/if}
              </div>
              {#if heldReasons[v.id]}
                <div class="held">Not delivered: {heldReasons[v.id]}</div>
              {/if}
              <div class="row-actions">
                {#if v.status !== 'expired'}
                  <button class="btn btn-small" onclick={() => deliverNow(v.id)} disabled={busy || !live}>Deliver now</button>
                {/if}
                <button class="btn btn-small" onclick={() => cancel(v.id)} disabled={busy}>
                  {v.status === 'expired' ? 'Clear' : 'Cancel'}
                </button>
              </div>
            </div>
          {/each}
        {/if}
      </div>

      <div class="add">
        <textarea
          bind:value={text}
          rows="2"
          placeholder="A prompt to deliver back to the agent later…"
          disabled={busy}
        ></textarea>
        <div class="add-row">
          <label class="minutes">
            in
            <input type="number" min="1" max="10080" bind:value={minutes} disabled={busy} />
            minutes
          </label>
          <div class="spacer"></div>
          <button class="btn btn-primary" onclick={add} disabled={busy || !text.trim()}>Add follow-up</button>
        </div>
        {#if addError}<div class="error">{addError}</div>{/if}
      </div>

      <div class="footer">
        <div class="spacer"></div>
        <button class="btn" onclick={onclose}>Close</button>
      </div>
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
    padding-top: 12vh;
    z-index: 1000;
    outline: none;
  }

  .palette {
    background: var(--bg-medium);
    border: 1px solid var(--bg-light);
    border-radius: 8px;
    width: min(520px, calc(100vw - 32px));
    max-height: 70vh;
    display: flex;
    flex-direction: column;
    box-shadow: 0 8px 32px rgba(0, 0, 0, 0.5);
    align-self: flex-start;
  }

  .header {
    padding: 12px 14px 10px;
    border-bottom: 1px solid var(--bg-light);
  }

  .title {
    font-size: 1rem;
    font-weight: 600;
    color: var(--fg);
  }

  .subtitle {
    margin-top: 3px;
    font-size: 0.8rem;
    color: var(--fg-dim);
    line-height: 1.4;
  }

  .subtitle strong {
    color: var(--accent);
    font-weight: 600;
  }

  .notice {
    margin-top: 8px;
    padding: 6px 8px;
    border-radius: 5px;
    background: var(--bg-dark);
    font-size: 0.8rem;
    color: var(--yellow, #e0af68);
  }

  .body {
    flex: 1;
    overflow-y: auto;
    padding: 6px;
  }

  .status {
    padding: 12px;
    font-size: 0.85rem;
    color: var(--fg-dim);
  }

  .row {
    padding: 8px 10px;
    border-radius: 6px;
  }

  .row + .row {
    border-top: 1px solid var(--bg-light);
  }

  .row.expired .row-text {
    color: var(--fg-dim);
  }

  .row-text {
    font-size: 0.88rem;
    color: var(--fg);
    white-space: pre-wrap;
    overflow-wrap: anywhere;
  }

  .row-meta {
    margin-top: 3px;
    display: flex;
    flex-wrap: wrap;
    gap: 4px;
    font-size: 0.75rem;
    color: var(--fg-dim);
  }

  .when.due {
    color: var(--accent);
  }

  .held {
    margin-top: 4px;
    font-size: 0.78rem;
    color: var(--yellow, #e0af68);
  }

  .row-actions {
    margin-top: 6px;
    display: flex;
    gap: 6px;
  }

  .add {
    padding: 10px 14px;
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

  .add-row {
    display: flex;
    align-items: center;
    gap: 8px;
    flex-wrap: wrap;
  }

  .minutes {
    display: flex;
    align-items: center;
    gap: 6px;
    font-size: 0.8rem;
    color: var(--fg-dim);
  }

  .minutes input {
    width: 70px;
    padding: 3px 6px;
    border-radius: 5px;
    border: 1px solid var(--bg-light);
    background: var(--bg-dark);
    color: var(--fg);
    font: inherit;
  }

  .error {
    font-size: 0.8rem;
    color: var(--error, #f7768e);
  }

  .footer {
    display: flex;
    gap: 8px;
    padding: 10px 14px;
    border-top: 1px solid var(--bg-light);
  }

  .spacer {
    flex: 1;
  }

  .btn {
    padding: 5px 14px;
    border-radius: 6px;
    border: 1px solid var(--bg-light);
    background: var(--bg-dark);
    color: var(--fg);
    font-size: 0.85rem;
    cursor: pointer;
  }

  .btn-small {
    padding: 3px 10px;
    font-size: 0.78rem;
  }

  .btn:hover:not(:disabled) {
    background: var(--bg-light);
  }

  .btn:disabled {
    opacity: 0.5;
    cursor: default;
  }

  .btn-primary {
    background: var(--accent);
    border-color: var(--accent);
    color: var(--bg-dark);
  }

  .btn-primary:hover:not(:disabled) {
    background: var(--accent);
    filter: brightness(1.1);
  }
</style>
