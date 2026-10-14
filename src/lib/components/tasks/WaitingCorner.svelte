<!--
  What a tab's agent is waiting on the human for, in the lower right of its terminal: a pill
  while nobody is looking at it, the cards themselves once the human opens it (docs/loom.md,
  "On the terminal").

  The terminal under it is live and usually focused, so the card never opens by itself and
  never takes the keyboard unasked: only a click opens it, which moves focus into it, and Hide
  or Escape gives focus back to where it was. Escape is stopped here — in the terminal it would
  reach the agent and interrupt its turn. Hide is not Dismiss: Hide puts the card back in the
  corner and changes nothing, Dismiss (on a question) closes the question.
-->
<script lang="ts">
  import { onDestroy, untrack } from 'svelte';
  import { waitingStore, waitingTone, type WaitingItem } from '$lib/stores/waiting.svelte';
  import { tasksStore } from '$lib/stores/tasks.svelte';
  import { terminalsStore } from '$lib/stores/terminals.svelte';
  import { BLOCKER_LABEL } from '$lib/tasks/model';
  import BlockerCard from './BlockerCard.svelte';
  import ScriptApprovalCard from '$lib/components/followUps/ScriptApprovalCard.svelte';
  import AgentRequestCard from '$lib/components/agentRequests/AgentRequestCard.svelte';
  import Tooltip from '$lib/components/Tooltip.svelte';

  interface Props {
    tabId: string;
  }
  let { tabId }: Props = $props();

  const waiting = $derived(waitingStore.forTab(tabId));
  const opened = $derived(waitingStore.opened(tabId));
  /** A task opened from the task panel that isn't in this tab's queue (see `OpenCorner`). */
  const extra = $derived.by((): WaitingItem | null => {
    const id = opened?.extraTaskId;
    if (!id || waiting.some((i) => i.key === id)) return null;
    const task = tasksStore.findAnywhere(id)?.task;
    if (!task || task.status !== 'blocked' || !task.blocker) return null;
    return { kind: 'task', key: task.id, at: Date.parse(task.blocker.asked_at), task };
  });
  const items = $derived(extra ? [extra, ...waiting] : waiting);
  const tone = $derived(waitingTone(items));

  function label(i: WaitingItem): string {
    if (i.kind === 'task') return BLOCKER_LABEL[i.task.blocker!.kind];
    return i.kind === 'script' ? 'A watch script to allow' : 'An agent request';
  }

  /** When the list last changed: anything above a card moving can put it under the pointer
   *  (the script and request cards' click guard; BlockerCard guards by its own asked_at). */
  let listChangedAt = $state(0);
  const signature = $derived(items.map((i) => i.key).join('|'));
  $effect(() => {
    void signature;
    listChangedAt = Date.now();
  });

  /** The last outcome ("Answer sent to the agent."). An answered card leaves the list, so the
   *  receipt is kept here. */
  let note = $state<string | null>(null);
  let noteTimer: ReturnType<typeof setTimeout> | undefined;
  function showNote(text: string) {
    note = text;
    clearTimeout(noteTimer);
    noteTimer = setTimeout(() => (note = null), 6000);
  }

  let panelEl = $state<HTMLElement | null>(null);
  /** Where focus was when the human opened the card: almost always the terminal. */
  let before: HTMLElement | null = null;

  // On every open (including a second open that names another card): bring the card into view
  // and focus its answer field, or the panel. On a frame, never `autofocus` (root CLAUDE.md).
  $effect(() => {
    const o = opened;
    if (!o) return;
    void o.seq;
    // The body is nowhere to go back to (`hide` then gives the tab's terminal the keyboard).
    const cur = document.activeElement;
    if (!before && cur instanceof HTMLElement && cur !== document.body && !panelEl?.contains(cur)) before = cur;
    const raf = requestAnimationFrame(() => {
      if (!panelEl) return;
      const card = o.focus ? panelEl.querySelector<HTMLElement>(`[data-key="${CSS.escape(o.focus)}"]`) : null;
      card?.scrollIntoView({ block: 'nearest' });
      const field = (card ?? panelEl).querySelector<HTMLElement>('input, textarea');
      (field ?? panelEl).focus({ preventScroll: true });
    });
    return () => cancelAnimationFrame(raf);
  });

  function hide() {
    const back = before;
    before = null;
    // Only if focus is still ours: a human who clicked into the terminal meanwhile keeps it.
    // Nobody's counts as ours: answering the last card removes the field that held focus,
    // which leaves it on the body.
    const cur = document.activeElement;
    const ours = !cur || cur === document.body || !!panelEl?.contains(cur);
    waitingStore.close(tabId);
    if (!ours) return;
    if (back?.isConnected) back.focus({ preventScroll: true });
    else terminalsStore.focusTerminal(tabId);
  }

  // Leaving the tab hides its card. An open card that survived would be focused again by the
  // effect above when the tab is shown again, with no click: the human's next keystrokes, meant
  // for the terminal, would answer the agent. The same goes for a card closing itself below
  // when the tab is left inside its delay.
  //
  // The id as mounted, not the prop: props are live, and by the time a `{#key}` swap destroys
  // this instance its `tabId` already reads the NEW tab, which would close that one's card.
  const mountedTabId = untrack(() => tabId);
  onDestroy(() => waitingStore.close(mountedTabId));

  // Everything answered: show the last receipt for a moment, then go back to nothing.
  $effect(() => {
    if (!opened || items.length) return;
    const t = setTimeout(hide, 2500);
    return () => clearTimeout(t);
  });

  function onkeydown(e: KeyboardEvent) {
    if (e.key !== 'Escape' || e.defaultPrevented) return;
    e.preventDefault();
    e.stopPropagation();
    hide();
  }
</script>

{#if items.length || opened}
  <div class="corner">
    {#if opened}
      <div class="card" data-tone={tone} bind:this={panelEl} tabindex="-1" role="dialog" aria-label="Waiting on you" {onkeydown}>
        <header>
          <span class="title">Waiting on you{items.length > 1 ? ` · ${items.length}` : ''}</span>
          <Tooltip text="Back to the corner. Nothing is answered or closed (Esc)">
            <button class="hide" onclick={hide}>Hide</button>
          </Tooltip>
        </header>
        <div class="list">
          {#each items as item (item.key)}
            <article data-key={item.key} class:focused={item.key === opened.focus}>
              {#if item.kind === 'task'}
                <h4>{item.task.title}</h4>
                <BlockerCard task={item.task} variant="card" onnote={showNote} />
              {:else if item.kind === 'script'}
                <ScriptApprovalCard tabId={item.approval.tabId} followUp={item.approval.followUp} {listChangedAt} onnote={showNote} />
              {:else}
                <AgentRequestCard request={item.request} {listChangedAt} onnote={showNote} />
              {/if}
            </article>
          {:else}
            <p class="empty">Nothing else is waiting on you here.</p>
          {/each}
        </div>
        <!-- Below the list, never above it: a line appearing above would push the cards down
             under the pointer, and no card guard sees that move. -->
        {#if note}<p class="note">{note}</p>{/if}
      </div>
    {:else}
      <button class="pill" data-tone={tone} onclick={() => waitingStore.open(tabId)}>
        <span class="dot"></span>
        {items.length === 1 ? label(items[0]) : `${items.length} waiting on you`}
      </button>
    {/if}
  </div>
{/if}

<style>
  /* Fills the terminal area with a margin and lets every click through, so only the pill and
     the card themselves take the pointer; the card's height is bounded by the area's. */
  .corner {
    position: absolute;
    inset: 10px 14px;
    z-index: 6;
    display: flex;
    flex-direction: column;
    justify-content: flex-end;
    align-items: flex-end;
    pointer-events: none;
  }
  .pill,
  .card {
    pointer-events: auto;
    --tone: var(--orange, #ff9e64);
  }
  [data-tone='action'] { --tone: var(--red, #f7768e); }

  .pill {
    display: flex;
    align-items: center;
    gap: 7px;
    max-width: 100%;
    padding: 5px 12px;
    border: 1px solid var(--tone);
    border-radius: 999px;
    background: color-mix(in srgb, var(--tone) 16%, var(--bg-medium));
    color: var(--fg);
    font: inherit;
    font-size: 12px;
    font-weight: 600;
    box-shadow: 0 4px 14px rgba(0, 0, 0, 0.35);
    cursor: pointer;
    overflow-wrap: anywhere;
    text-align: left;
  }
  .pill:hover,
  .pill:focus-visible { background: color-mix(in srgb, var(--tone) 28%, var(--bg-medium)); }
  .dot {
    flex-shrink: 0;
    width: 7px;
    height: 7px;
    border-radius: 50%;
    background: var(--tone);
  }

  .card {
    display: flex;
    flex-direction: column;
    width: min(400px, 100%);
    max-height: 100%;
    background: var(--bg-medium);
    border: 1px solid var(--bg-light);
    border-top: 2px solid var(--tone);
    border-radius: 10px;
    box-shadow: 0 10px 30px rgba(0, 0, 0, 0.45);
    overflow: hidden;
    outline: none;
  }
  header {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 8px;
    padding: 8px 12px;
    border-bottom: 1px solid var(--bg-light);
  }
  .title { font-size: 12px; font-weight: 600; color: var(--fg); }
  .hide {
    background: none;
    border: 1px solid var(--bg-light);
    border-radius: 5px;
    padding: 2px 9px;
    color: var(--fg-dim);
    font: inherit;
    font-size: 11.5px;
    cursor: pointer;
  }
  .hide:hover,
  .hide:focus-visible { color: var(--fg); border-color: var(--fg-dim); }
  .note {
    margin: 0;
    padding: 6px 12px;
    font-size: 12px;
    color: var(--fg-dim);
    border-top: 1px solid var(--bg-light);
    overflow-wrap: anywhere;
  }
  .list {
    display: flex;
    flex-direction: column;
    gap: 12px;
    padding: 10px 12px 12px;
    overflow-y: auto;
    min-height: 0;
  }
  article { display: flex; flex-direction: column; gap: 6px; }
  article.focused h4 { color: var(--accent); }
  h4 {
    margin: 0;
    font-size: 12px;
    font-weight: 600;
    color: var(--fg-dim);
    overflow-wrap: anywhere;
  }
  .empty { margin: 0; font-size: 12px; color: var(--fg-dim); }
</style>
