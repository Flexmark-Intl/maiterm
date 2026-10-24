<!--
  One worker on the Hive board (docs/hive.md §4.3): state, badge, title, age, its task's latest
  note, its folders and runtime · model. No buttons: cards are small and re-laid-out often, so a
  button on one is where a stray click lands. The whole card opens the worker (§4.6); every
  answer happens in its column.

  The activity line, diff stat, context gauge and asset thumbnails arrive with build step 5.
-->
<script lang="ts">
  import Tooltip from '$lib/components/Tooltip.svelte';
  import { stateLine, type CardState } from '$lib/hive/cards';
  import type { HiveWorkerCard } from '$lib/tauri/types';

  interface Props {
    card: HiveWorkerCard;
    state: CardState;
    note: string | null;
    runtime: string;
    age: string;
    focused?: boolean;
    onopen: () => void;
  }
  let { card, state, note, runtime, age, focused = false, onopen }: Props = $props();

  const pulse = $derived(state.state === 'working' || state.state === 'starting');
  const folders = $derived(card.folders.map((f) => f.name).join(' · '));
</script>

<button
  class="card"
  class:needs={state.state === 'needs_you'}
  class:urgent={state.urgent}
  class:focused
  data-state={state.state}
  data-hive-task={card.task_id}
  aria-label="#{card.badge} {card.title}: {stateLine(state)}"
  onclick={onopen}
>
  <span class="top">
    {#if state.state === 'reported'}
      <span class="check" aria-hidden="true">✓</span>
    {:else}
      <span class="dot" class:pulse aria-hidden="true"></span>
    {/if}
    <span class="badge">{card.badge}</span>
    <Tooltip text={card.title} block>
      <span class="t">{card.title}</span>
    </Tooltip>
    {#if age}<span class="age">{age}</span>{/if}
  </span>
  <span class="st">
    <span class="lbl">{stateLine(state)}</span>
    {#if state.sub}<span class="sub">{state.sub}</span>{/if}
  </span>
  <!-- A note can run to paragraphs: three lines here, the whole of it on hover. -->
  {#if note}<Tooltip text={note} block><span class="note">“{note}”</span></Tooltip>{/if}
  {#if folders}<span class="m">{folders}</span>{/if}
  <span class="m">{runtime} · {card.model}</span>
</button>

<style>
  .card {
    --needs: var(--orange, #ff9e64);
    position: relative;
    display: flex;
    flex-direction: column;
    gap: 3px;
    width: 100%;
    text-align: left;
    background: var(--bg-medium);
    border: 1px solid var(--bg-light);
    border-radius: 8px;
    padding: 8px 10px 8px 12px;
    color: var(--fg);
    font: inherit;
    cursor: pointer;
    transition: border-color 0.15s, box-shadow 0.3s;
  }
  .card:hover { border-color: color-mix(in srgb, var(--accent) 50%, var(--bg-light)); }
  .card:focus-visible { outline: none; border-color: var(--accent); }
  .card.focused { box-shadow: 0 0 0 1px var(--accent); border-color: var(--accent); }
  /* §4.3: a Needs-you card carries an orange edge (red for an action). */
  .card.needs::before { content: ''; position: absolute; left: -1px; top: 7px; bottom: 7px; width: 3px; border-radius: 2px; background: var(--needs); }
  .card.needs.urgent::before { background: var(--red); }
  .card[data-state='reported'], .card[data-state='stopped'], .card[data-state='suspended'] { opacity: 0.8; }

  .top { display: flex; align-items: flex-start; gap: 7px; min-width: 0; }
  .top > :global(.tooltip-wrapper) { flex: 1; min-width: 0; }
  .t { font-weight: 600; font-size: 12.5px; line-height: 1.3; overflow-wrap: anywhere; }
  .badge { flex: none; font-size: 10px; line-height: 15px; min-width: 15px; padding: 0 4px; border-radius: 8px; text-align: center; background: var(--bg-light); color: var(--fg); font-variant-numeric: tabular-nums; }
  .age { flex: none; font-size: 10.5px; color: var(--fg-dim); font-variant-numeric: tabular-nums; line-height: 16px; }

  .dot { flex: none; width: 8px; height: 8px; margin-top: 4px; border-radius: 50%; box-sizing: border-box; background: var(--fg-dim); }
  .check { flex: none; width: 10px; font-size: 11px; line-height: 16px; color: var(--accent); }
  [data-state='needs_you'] .dot { background: var(--needs); }
  .urgent .dot { background: var(--red); }
  [data-state='blocked'] .dot { background: var(--yellow); }
  [data-state='working'] .dot { background: var(--green); }
  [data-state='idle'] .dot, [data-state='suspended'] .dot { background: transparent; border: 1.5px solid var(--fg-dim); }
  [data-state='failed'] .dot { background: var(--red); }
  .pulse { animation: pulse 1.6s ease-in-out infinite; }
  @keyframes pulse { 50% { opacity: 0.35; } }

  .st { display: flex; flex-direction: column; gap: 1px; font-size: 11px; color: var(--fg-dim); }
  .lbl { color: var(--fg-dim); }
  [data-state='needs_you'] .lbl { color: var(--needs); }
  .urgent .lbl { color: var(--red); }
  [data-state='blocked'] .lbl { color: var(--yellow); }
  [data-state='failed'] .lbl { color: var(--red); }
  .sub { color: var(--fg); opacity: 0.85; overflow-wrap: anywhere; }
  .note { font-size: 11.5px; line-height: 1.35; color: var(--fg); opacity: 0.8; overflow-wrap: anywhere; display: -webkit-box; -webkit-line-clamp: 3; line-clamp: 3; -webkit-box-orient: vertical; overflow: hidden; }
  .m { font-size: 10.5px; color: var(--fg-dim); overflow-wrap: anywhere; }

  @media (prefers-reduced-motion: reduce) {
    .pulse { animation: none; }
    .card { transition: none; }
  }
</style>
