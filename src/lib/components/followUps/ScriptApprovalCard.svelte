<!--
  A watch script waiting for the human to allow it (docs/follow-ups.md §5.1) — the script half of
  the Decisions queue. One component for every surface that asks (the Loom's Decisions, the tab's
  task panel, the tab's follow-ups list), so the question reads the same everywhere and the click
  guards live in one place.

  Everything shown comes from the follow-up itself, never from text an agent could edit after
  asking: the approval is for exactly the script, folder and schedule drawn here.
-->
<script lang="ts">
  import { followUpsStore } from '$lib/stores/followUps.svelte';
  import { durationText } from '$lib/followUps/model';
  import type { FollowUp } from '$lib/tauri/types';

  interface Props {
    tabId: string;
    followUp: FollowUp;
    /** Receives one line saying what happened, for the surface to show. */
    onnote?: (text: string) => void;
    /** 'card' for the Decisions queue and the follow-ups list; 'inline' for the narrow task panel. */
    variant?: 'card' | 'inline';
    /** When the list holding this card last changed (ms). Anything inserted or removed ABOVE a
     *  card moves it, so a card already on screen can be pushed under the pointer. */
    listChangedAt?: number;
  }
  let { tabId, followUp: f, onnote, variant = 'card', listChangedAt = 0 }: Props = $props();

  /** Nothing is decided in a card's first moments, counted from the latest of: the agent asking
   *  (BlockerCard's rule), this card appearing, and its list last changing. The list matters
   *  because, unlike task blockers, scripts don't only arrive at the end of the queue: an older
   *  one inserted above, or a card above answered or removed, moves this card — and a card that
   *  has been on screen for minutes can land under the pointer (reviews of d56ac45, 30b4844). */
  const FRESH_MS = 1500;
  const shownAt = Date.now();
  const isFresh = () => {
    const age = Date.now() - Date.parse(f.created_at);
    const settled = Date.now() - Math.max(shownAt, listChangedAt);
    return (age >= 0 && age < FRESH_MS) || settled < FRESH_MS;
  };
  /** The second click of a double-click is never a decision. */
  const secondClick = (e: MouseEvent) => e.detail > 1;

  let busy = $state(false);

  /** Exact seconds unless it is whole minutes — the human allows the schedule stated here. */
  function every(secs: number): string {
    return secs < 60 || secs % 60 !== 0 ? `${secs} seconds` : secs === 60 ? 'minute' : durationText(secs * 1000);
  }

  /** A trailing newline ends the last line; it doesn't add one. */
  function linesText(script: string): string {
    const n = script.replace(/\n$/, '').split('\n').length;
    return `${n} line${n === 1 ? '' : 's'}`;
  }

  async function decide(allow: boolean, e: MouseEvent) {
    if (busy || secondClick(e)) return;
    if (isFresh()) {
      onnote?.('This just appeared or moved. Read it, then choose.');
      return;
    }
    busy = true;
    try {
      const ok = allow ? await followUpsStore.approve(tabId, f.id) : await followUpsStore.reject(tabId, f.id);
      onnote?.(
        !ok
          ? 'It was already answered or removed.'
          : allow
            ? 'Allowed. The agent will be woken when it passes.'
            : 'Not allowed. The script was removed and will not run.',
      );
    } catch (err) {
      onnote?.(`Could not save that: ${err}`);
    } finally {
      busy = false;
    }
  }
</script>

<div class="approval {variant}">
  <p class="q">Allow this watch script?</p>
  <p class="why">
    The agent wants maiTerm to run this check for it, and to be woken only when it passes.
    It runs as you, outside the agent's own permission checks.
  </p>

  <figure class="pane">
    <figcaption>
      <span class="label">{f.due.label ?? 'watch script'}</span>
      <span class="sched">every {every(f.due.every_secs ?? 60)}</span>
    </figcaption>
    <!-- Wrapped and never clipped: the approval is for every character of it. -->
    <pre>{f.due.script}</pre>
    <!-- Everything the approval covers: the folder, the run limit, and how much script there is
         (so a script padded past the screen can't pass for a short one). -->
    <div class="where">in {f.due.cwd} · up to {f.due.timeout_secs ?? 10}s a run · {linesText(f.due.script ?? '')}, {(f.due.script ?? '').length} characters</div>
  </figure>

  <details class="then">
    <summary>What the agent is told when it passes</summary>
    <p>{f.text}</p>
  </details>

  <div class="choices">
    <button class="allow" disabled={busy} onclick={(e) => decide(true, e)}>Allow</button>
    <button class="deny" disabled={busy} onclick={(e) => decide(false, e)}>Don't allow</button>
  </div>
</div>

<style>
  .approval {
    display: flex;
    flex-direction: column;
    gap: 8px;
    padding: 12px 14px;
    border-left: 3px solid var(--yellow, #e0af68);
    background: color-mix(in srgb, var(--yellow, #e0af68) 7%, transparent);
    border-radius: 0 8px 8px 0;
    font-size: 12.5px;
    color: var(--fg);
    min-width: 0;
  }
  .approval.inline { gap: 6px; padding: 7px 9px; border-left-width: 2px; border-radius: 0 5px 5px 0; font-size: 11.5px; }

  .q { margin: 0; font-size: 15px; font-weight: 600; line-height: 1.3; }
  .inline .q { font-size: 12.5px; }
  .why { margin: 0; color: var(--fg-dim); line-height: 1.45; }

  /* The script as a small terminal pane: its name and schedule in the title bar, the folder it
     runs in underneath — what runs, how often, where, in one place. */
  .pane {
    margin: 2px 0 0;
    border: 1px solid var(--bg-light);
    border-radius: 7px;
    background: var(--bg-dark);
    overflow: hidden;
  }
  figcaption {
    display: flex;
    flex-wrap: wrap;
    justify-content: space-between;
    gap: 2px 12px;
    padding: 5px 10px;
    background: var(--bg-medium);
    border-bottom: 1px solid var(--bg-light);
    font-size: 11.5px;
  }
  .label { font-weight: 600; overflow-wrap: anywhere; }
  .sched { color: var(--fg-dim); white-space: nowrap; }
  pre {
    margin: 0;
    padding: 8px 10px;
    font-family: var(--font-mono, ui-monospace, monospace);
    font-size: 11.5px;
    line-height: 1.5;
    white-space: pre-wrap;
    overflow-wrap: anywhere;
    color: var(--fg);
  }
  .where {
    padding: 4px 10px 6px;
    font-family: var(--font-mono, ui-monospace, monospace);
    font-size: 10.5px;
    color: var(--fg-dim);
    overflow-wrap: anywhere;
  }

  .then summary { cursor: pointer; color: var(--fg-dim); font-size: 11.5px; }
  .then summary:hover, .then summary:focus-visible { color: var(--fg); }
  .then p { margin: 6px 0 0; white-space: pre-wrap; overflow-wrap: anywhere; line-height: 1.45; }

  .choices { display: flex; flex-wrap: wrap; gap: 6px; }
  .choices button {
    font: inherit;
    font-weight: 600;
    padding: 5px 14px;
    border-radius: 6px;
    cursor: pointer;
  }
  .inline .choices button { padding: 3px 10px; }
  .allow { background: var(--accent); border: 1px solid var(--accent); color: var(--bg-dark); }
  .allow:hover:not(:disabled) { filter: brightness(1.1); }
  .deny { background: none; border: 1px solid var(--bg-light); color: var(--fg); }
  .deny:hover:not(:disabled) { background: var(--bg-light); }
  .choices button:focus-visible { outline: 2px solid var(--accent); outline-offset: 2px; }
  .choices button:disabled { opacity: 0.5; cursor: default; }
</style>
