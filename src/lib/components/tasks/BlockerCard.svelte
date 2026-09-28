<!--
  What a Blocked task is waiting for, and the controls to answer it (docs/tasks.md §3.1).
  One instance per task: the task panel, the loom's inspector and the decisions queue all
  render this, so the answer pinning lives in one place.
-->
<script lang="ts">
  import { overlordStore } from '$lib/stores/overlord.svelte';
  import { BLOCKER_LABEL } from '$lib/tasks/model';
  import { fmtAge } from '$lib/overlord/format';
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
   *  over to a question the human never answered. Only text that is on screen is ever sent. */
  let draft = $state<{ askedAt: string; text: string }>({ askedAt: '', text: '' });
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
    if (a && draft.askedAt && draft.askedAt !== a) draft = { askedAt: '', text: '' };
  });

  let sending = $state(false);

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
    try {
      const r = await overlordStore.answerBlocker(task.id, {
        // The asked_at the visible text was typed against; for a click, the question on screen.
        asked_at: draftText ? draft.askedAt : b.asked_at,
        option,
        text: draftText,
      });
      if (!r.answered) {
        onnote?.(r.detail ?? 'Could not answer.');
        return;
      }
      draft = { askedAt: '', text: '' };
      onnote?.(
        r.told === 'tab'
          ? 'Answer sent to the agent.'
          : r.told === 'agent'
            ? 'Answered. The tab was busy, so Overlord will pass it on.'
            : 'Answered and moved to Active, but the agent could not be told. The tab is not reachable.',
      );
    } finally {
      sending = false;
    }
  }
</script>

{#if b}
  <div class="blocker {variant}" data-kind={b.kind}>
    <span class="kind">{BLOCKER_LABEL[b.kind]} · {fmtAge(b.asked_at)}</span>
    <span class="q">{b.question}</span>
    {#if b.context}<span class="ctx">{b.context}</span>{/if}
    {#if b.options?.length}
      <div class="opts">
        {#each b.options as o, i (i)}
          <button class="opt" disabled={sending} onclick={(e) => { if (!secondClick(e)) answer(i); }}>
            <b>{o.label}{#if o.recommended}<em> recommended</em>{/if}</b>
            {#if o.detail}<span>{o.detail}</span>{/if}
          </button>
        {/each}
      </div>
    {/if}
    {#if b.command}<code class="cmd">{b.command}</code>{/if}
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
          draft = { askedAt: b.asked_at, text: e.currentTarget.value };
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
  </div>
{/if}

<style>
  .blocker {
    --kind: var(--orange, #ff9e64);
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
  .kind {
    color: var(--kind);
    font-size: 9.5px;
    text-transform: uppercase;
    letter-spacing: 0.06em;
  }
  .card .kind { font-size: 10.5px; }
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
