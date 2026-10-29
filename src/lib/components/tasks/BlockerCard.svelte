<!--
  What a Blocked task is waiting for, and the controls to answer it (docs/tasks.md §3.1).
  One instance per task: the task panel, the loom's inspector and the decisions queue all
  render this, so the answer pinning lives in one place.

  A Hive worker's question its coordinator holds (docs/hive.md §7.4) reads "With coordinator" on
  every one of those surfaces: options read-only, no answer field, no Dismiss, and Take over under
  the same two guards. `answerBlocker` refuses it meanwhile anyway, the phone's answer included.
-->
<script lang="ts">
  import { overlordStore } from '$lib/stores/overlord.svelte';
  import { hivePrompts } from '$lib/stores/hivePrompts.svelte';
  import { coordinatorHolds, withCoordinatorText } from '$lib/hive/model';
  import { blockerDrafts } from '$lib/stores/waiting.svelte';
  import { BLOCKER_LABEL } from '$lib/tasks/model';
  import { fmtAge } from '$lib/overlord/format';
  import Tooltip from '$lib/components/Tooltip.svelte';
  import TaskText from './TaskText.svelte';
  import type { Task } from '$lib/tauri/types';

  interface Props {
    task: Task;
    /** Receives a one-line outcome to show the human ("Answer sent to the agent."). */
    onnote?: (text: string) => void;
    /** 'inline' sits under a task row; 'card' is the larger decisions-queue layout. */
    variant?: 'inline' | 'card';
  }

  let { task, onnote, variant = 'inline' }: Props = $props();

  const b = $derived(task.status === 'blocked' ? task.blocker : null);

  /** Free text typed under the question. It belongs to the question it was typed against:
   *  when the agent re-asks, the draft is dropped (and the field empties) rather than carried
   *  over to a question the human never answered. Only text that is on screen is ever sent.
   *  Held in `blockerDrafts`, not here, so it outlives this card (a corner card closes when its
   *  tab is left). */
  const draft = $derived(blockerDrafts.get(task.id) ?? { askedAt: '', text: '' });
  const setDraft = (d: { askedAt: string; text: string }) => blockerDrafts.set(task.id, d);
  const draftText = $derived(draft.askedAt === b?.asked_at ? draft.text : '');

  /** Nothing answers a question in its first moments on screen. Measured from the question's own
   *  `asked_at` (stamped locally when it was recorded), not from when this card saw it: a card
   *  mounts when a question first appears, and may appear right under the pointer, while a card
   *  opened on a question asked minutes ago is answerable at once. */
  const FRESH_MS = 1500;
  /** A future asked_at (a task shared from a machine whose clock runs ahead) is not fresh: it
   *  would otherwise stay unanswerable until this clock caught up. */
  const isFresh = () => {
    if (!b) return false;
    const age = Date.now() - Date.parse(b.asked_at);
    return age >= 0 && age < FRESH_MS;
  };

  /** The second click of a double-click is never an answer. When a card is answered it leaves
   *  its list at once, and the next card slides into its place, under the pointer, with a
   *  question that is not fresh; the double-click's second click would answer it. `detail` is
   *  the click count, which the browser keeps by position and time, not by element; keyboard
   *  activation reports 0. */
  const secondClick = (e: MouseEvent) => e.detail > 1;

  $effect.pre(() => {
    const a = b?.asked_at;
    if (a && draft.askedAt && draft.askedAt !== a) setDraft({ askedAt: '', text: '' });
  });

  let sending = $state(false);

  /** The Hive route of a worker's question, and whether its coordinator holds it alone. */
  const route = $derived(b ? hivePrompts.blockerOwner(task.id, b.asked_at) : null);
  const held = $derived(coordinatorHolds(route));
  /** "told in N min" moves on while it's held. */
  let clock = $state(Date.now());
  $effect(() => {
    if (!held) return;
    clock = Date.now();
    const t = setInterval(() => (clock = Date.now()), 30_000);
    return () => clearInterval(t);
  });

  async function takeOver(e: MouseEvent) {
    if (!route || sending || secondClick(e)) return;
    if (isFresh()) {
      onnote?.('The agent just changed its question. Read it again before taking it over.');
      return;
    }
    sending = true;
    try {
      await hivePrompts.takeOver(route.tab_id, route.key);
      onnote?.("Yours now: the queen was told to leave it.");
    } catch (err) {
      onnote?.(`Not taken over: ${err instanceof Error ? err.message : err}`);
    } finally {
      sending = false;
    }
  }

  /** Anything that sends without text of the human's own on screen (an option, "I've done it",
   *  Enter in an empty field) answers whatever is showing now, so it is refused while the
   *  question is fresh. Visible typed text was typed under the current question: a re-ask
   *  drops it. */
  async function answer(option: number | undefined) {
    if (!b || sending) return;
    if (!draftText.trim() && isFresh()) {
      onnote?.('The agent just changed its question. Read it again before answering.');
      return;
    }
    sending = true;
    // The asked_at the visible text was typed against; for a click, the question on screen.
    const askedAt = draftText ? draft.askedAt : b.asked_at;
    // Read now: an answer usually unblocks the task, which unmounts this card during the await,
    // and its props are gone after it.
    const taskId = task.id;
    try {
      const r = await overlordStore.answerBlocker(taskId, { asked_at: askedAt, option, text: draftText });
      if (!r.answered) {
        onnote?.(r.detail ?? 'Could not answer.');
        return;
      }
      // Only the draft that was sent. Delivery can wait on the agent long enough for it to ask
      // again and the human to start typing under the new question, in the same shared slot.
      if (blockerDrafts.get(taskId)?.askedAt === askedAt) blockerDrafts.set(taskId, { askedAt: '', text: '' });
      onnote?.(
        r.told === 'tab'
          ? 'Answer sent to the agent.'
          : r.told === 'agent'
            ? 'Answered. No agent in the tab could take it, so Overlord will pass it on once one is running.'
            : 'Answered and moved to Active, but the agent could not be told. The tab is not reachable.',
      );
    } finally {
      sending = false;
    }
  }

  /** Close the question without answering: the human dealt with it in the agent's tab. It
   *  answers nothing, but it does close whatever question is showing, so it takes the same two
   *  guards as a click answer. */
  function dismiss(e: MouseEvent) {
    if (!b || sending || secondClick(e)) return;
    if (isFresh()) {
      onnote?.('The agent just changed its question. Read it again before dismissing it.');
      return;
    }
    const r = overlordStore.dismissBlocker(task.id, b.asked_at);
    onnote?.(r.dismissed ? 'Dismissed and moved to Active. Nothing was sent to the agent.' : (r.detail ?? 'Could not dismiss.'));
  }
</script>

{#if b}
  <div class="blocker {variant}" data-kind={b.kind} class:held>
    <div class="head">
      <span class="kind">{BLOCKER_LABEL[b.kind]} · {fmtAge(b.asked_at)}</span>
      {#if held}
        <Tooltip text="Answer it yourself: the queen is told to leave it">
          <button class="dismiss take" disabled={sending} onclick={takeOver}>Take over</button>
        </Tooltip>
      {:else}
        <Tooltip text="Already handled in the tab: close this without sending the agent anything">
          <button class="dismiss" disabled={sending} onclick={dismiss}>Dismiss</button>
        </Tooltip>
      {/if}
    </div>
    {#if held}<span class="with">{withCoordinatorText(route?.due_ms, clock)}</span>{/if}
    <span class="q"><TaskText text={b.question} of={task.id} /></span>
    {#if b.context}<span class="ctx"><TaskText text={b.context} of={task.id} /></span>{/if}
    {#if b.options?.length}
      <div class="opts">
        {#each b.options as o, i (i)}
          <button class="opt" disabled={sending || held} onclick={(e) => { if (!secondClick(e)) answer(i); }}>
            <b><TaskText text={o.label} of={task.id} />{#if o.recommended}<em> recommended</em>{/if}</b>
            {#if o.detail}<span><TaskText text={o.detail} of={task.id} /></span>{/if}
          </button>
        {/each}
      </div>
    {/if}
    {#if b.command}<code class="cmd">{b.command}</code>{/if}
    {#if !held}
    <div class="answer">
      <input
        id="blocker-answer-{task.id}"
        placeholder={b.kind === 'decision'
          ? b.options?.length ? 'Or write an answer…' : 'Your answer…'
          : 'Add a comment (optional)…'}
        value={draftText}
        oninput={(e) => {
          // Typing into a question that just appeared: it may have replaced the one being
          // answered, and the field just emptied under the human's hands.
          if (isFresh()) {
            e.currentTarget.value = '';
            onnote?.('The agent just changed its question. Read it again before answering.');
            return;
          }
          setDraft({ askedAt: b.asked_at, text: e.currentTarget.value });
        }}
        onkeydown={(e) => {
          if (e.key === 'Enter' && (b.kind !== 'decision' || draftText.trim())) answer(undefined);
        }}
      />
      {#if b.kind === 'decision'}
        <button class="send" disabled={sending || !draftText.trim()} onclick={(e) => { if (!secondClick(e)) answer(undefined); }}>Send</button>
      {:else}
        <button class="send" disabled={sending} onclick={(e) => { if (!secondClick(e)) answer(undefined); }}>{b.kind === 'action' ? "I've done it" : 'It arrived'}</button>
      {/if}
    </div>
    {/if}
  </div>
{/if}

<style>
  .blocker {
    --kind: var(--orange, #ff9e64);
    -webkit-user-select: text;
    user-select: text;
    display: flex;
    flex-direction: column;
    gap: 3px;
    padding: 5px 7px;
    border-left: 2px solid var(--kind);
    background: color-mix(in srgb, var(--kind) 8%, transparent);
    border-radius: 0 4px 4px 0;
    font-size: 11px;
    overflow-wrap: anywhere;
  }
  .blocker.card {
    gap: 7px;
    padding: 12px 14px;
    border-left-width: 3px;
    border-radius: 0 8px 8px 0;
    font-size: 12.5px;
  }
  .blocker[data-kind='action'] { --kind: var(--red, #f7768e); }
  .blocker[data-kind='external'] { --kind: var(--cyan, #7dcfff); }
  .blocker.held { --kind: var(--fg-dim); }
  .with { color: var(--fg-dim); }
  .dismiss.take { color: var(--accent); }
  .kind {
    color: var(--kind);
    font-size: 9.5px;
    text-transform: uppercase;
    letter-spacing: 0.06em;
  }
  .card .kind { font-size: 10.5px; }
  .head { display: flex; align-items: baseline; justify-content: space-between; gap: 8px; }
  .dismiss {
    background: none;
    border: 0;
    padding: 0;
    color: var(--fg-dim);
    font-size: 10px;
    cursor: pointer;
  }
  .card .dismiss { font-size: 11px; }
  .dismiss:hover:not(:disabled), .dismiss:focus-visible { color: var(--fg); text-decoration: underline; }
  .dismiss:disabled { opacity: 0.4; cursor: default; }
  .q { color: var(--fg); font-weight: 600; }
  .card .q { font-size: 15px; line-height: 1.3; }
  .ctx { color: var(--fg-dim); }
  .opts { display: flex; flex-direction: column; gap: 3px; }
  .card .opts { gap: 5px; }
  .opt {
    display: flex;
    flex-direction: column;
    gap: 1px;
    text-align: left;
    background: var(--bg-dark);
    border: 1px solid var(--bg-light);
    border-radius: 4px;
    padding: 4px 6px;
    color: var(--fg);
    font-size: inherit;
    cursor: pointer;
  }
  .card .opt { padding: 7px 10px; border-radius: 6px; }
  .opt:hover:not(:disabled), .opt:focus-visible { border-color: var(--kind); }
  .opt em { color: var(--kind); font-style: normal; font-size: 9.5px; font-weight: 400; }
  .opt span { color: var(--fg-dim); }
  .cmd {
    font-family: var(--font-mono, ui-monospace, monospace);
    font-size: 10.5px;
    background: var(--bg-dark);
    padding: 3px 5px;
    border-radius: 3px;
  }
  .answer { display: flex; gap: 4px; }
  .answer input {
    flex: 1;
    min-width: 0;
    background: var(--bg-dark);
    border: 1px solid var(--bg-light);
    border-radius: 4px;
    color: var(--fg);
    font-size: inherit;
    padding: 3px 6px;
  }
  .send {
    background: var(--kind);
    color: var(--bg-dark);
    border: 0;
    border-radius: 4px;
    font-size: 10.5px;
    font-weight: 600;
    padding: 3px 8px;
    cursor: pointer;
  }
  .send:disabled, .opt:disabled { opacity: 0.4; cursor: default; }
</style>
