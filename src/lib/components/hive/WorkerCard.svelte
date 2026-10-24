<!--
  One worker on the Hive board (docs/hive.md §4.3): state, badge, title, age, the live activity
  line, its task's latest note, its folders and diff stat, runtime · model and the context gauge.
  No buttons: cards are small and re-laid-out often, so a button on one is where a stray click
  lands. The whole card opens the worker (§4.6); every answer happens in its column.

  `compact` is the rail's row (§4.1): badge, state dot, title (two lines, the rest on the
  tooltip) and the activity line.

  Asset thumbnails arrive with the gallery.
-->
<script lang="ts">
  import Tooltip from '$lib/components/Tooltip.svelte';
  import { stateLine, type CardState } from '$lib/hive/cards';
  import type { Activity, contextGauge } from '$lib/hive/activity';
  import type { HiveWorkerCard } from '$lib/tauri/types';

  interface Props {
    card: HiveWorkerCard;
    state: CardState;
    note: string | null;
    runtime: string;
    age: string;
    activity: Activity;
    diff: { text: string; tip: string };
    /** The agent's own model name once it has replied, else the one the Hive routed it to. */
    model: string;
    ctx: ReturnType<typeof contextGauge>;
    compact?: boolean;
    focused?: boolean;
    onopen: () => void;
  }
  let { card, state, note, runtime, age, activity, diff, model, ctx, compact = false, focused = false, onopen }: Props = $props();

  const pulse = $derived(state.state === 'working' || state.state === 'starting' || state.state === 'landing');
  const folders = $derived(card.folders.map((f) => f.name).join(' · '));
  // An idle agent's line is the state line with its age, and a prompt's is the sub-label already
  // shown: the card says each thing once.
  const idleLine = $derived(state.state === 'idle' && activity.tone === 'idle');
  const label = $derived(idleLine ? activity.text : stateLine(state));
  const showActivity = $derived(activity.tone !== 'none' && !idleLine && activity.text !== state.sub);
  // A rail row has one line under its title: what the agent is doing, else its state.
  const railLine = $derived(activity.tone === 'none' || activity.tone === 'idle' ? label : activity.text);
</script>

<button
  class="card"
  class:compact
  class:needs={state.state === 'needs_you'}
  class:urgent={state.urgent}
  class:focused
  data-state={state.state}
  data-hive-task={card.task_id}
  aria-label="#{card.badge} {card.title}: {stateLine(state)}{activity.tone !== 'none' ? `, ${activity.text}` : ''}"
  onclick={onopen}
>
  <span class="top">
    {#if state.state === 'reported' || state.state === 'landed'}
      <span class="check" aria-hidden="true">✓</span>
    {:else if state.state === 'retired'}
      <!-- §4.3: Retired is dimmed, with no dot. -->
      <span class="nodot" aria-hidden="true"></span>
    {:else}
      <span class="dot" class:pulse aria-hidden="true"></span>
    {/if}
    <span class="badge">{card.badge}</span>
    <Tooltip text={card.title} block>
      <span class="t">{card.title}</span>
    </Tooltip>
    {#if age && !compact}<span class="age">{age}</span>{/if}
  </span>
  {#if compact}
    <span class="act" data-tone={activity.tone === 'none' ? 'state' : activity.tone}>{railLine}</span>
  {:else}
    <span class="st">
      <span class="lbl">{label}</span>
      {#if state.sub}<span class="sub">{state.sub}</span>{/if}
    </span>
    {#if showActivity}<span class="act" data-tone={activity.tone}>{activity.text}</span>{/if}
    <!-- A note can run to paragraphs: three lines here, the whole of it on hover. -->
    {#if note}<Tooltip text={note} block><span class="note">“{note}”</span></Tooltip>{/if}
    {#if folders || diff.text}
      <span class="row">
        <span class="m">{folders}</span>
        {#if diff.text}<Tooltip text={diff.tip}><span class="diff">{diff.text}</span></Tooltip>{/if}
      </span>
    {/if}
    <span class="row">
      <span class="m">{runtime} · {model}</span>
      {#if ctx}
        <Tooltip text={ctx.tip}>
          <span class="ctx" data-tone={ctx.tone}>ctx {ctx.pct}%<i><s style="width: {ctx.pct}%"></s></i></span>
        </Tooltip>
      {/if}
    </span>
  {/if}
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
  .card[data-state='landed'], .card[data-state='retired'] { opacity: 0.6; }
  [data-state='landed'] .check { color: var(--fg-dim); }
  .nodot { flex: none; width: 8px; }
  [data-state='landing'] .dot { background: var(--accent); }
  .card.compact { gap: 2px; padding: 6px 8px 6px 10px; border-radius: 6px; }

  .top { display: flex; align-items: flex-start; gap: 7px; min-width: 0; }
  .top > :global(.tooltip-wrapper) { flex: 1; min-width: 0; }
  .t { font-weight: 600; font-size: 12.5px; line-height: 1.3; overflow-wrap: anywhere; }
  /* §4.1: a rail title wraps to two lines rather than clipping; the whole of it is on hover. */
  .compact .t { font-size: 12px; display: -webkit-box; -webkit-line-clamp: 2; line-clamp: 2; -webkit-box-orient: vertical; overflow: hidden; }
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
  /* The activity line: what the agent is doing right now, from its hooks (§4.4). */
  .act { font-size: 11.5px; line-height: 1.35; color: var(--fg); opacity: 0.9; overflow-wrap: anywhere; }
  .act[data-tone='wait'] { color: var(--needs); opacity: 1; }
  .act[data-tone='idle'], .act[data-tone='state'] { color: var(--fg-dim); opacity: 1; }
  .compact .act { font-size: 11px; padding-left: 15px; }
  .note { font-size: 11.5px; line-height: 1.35; color: var(--fg); opacity: 0.8; overflow-wrap: anywhere; display: -webkit-box; -webkit-line-clamp: 3; line-clamp: 3; -webkit-box-orient: vertical; overflow: hidden; }
  .row { display: flex; flex-wrap: wrap; align-items: baseline; justify-content: space-between; gap: 2px 10px; min-width: 0; }
  .m { font-size: 10.5px; color: var(--fg-dim); overflow-wrap: anywhere; min-width: 0; }
  .diff { font-size: 10.5px; color: var(--fg-dim); font-variant-numeric: tabular-nums; white-space: nowrap; }
  .ctx { --t: var(--green); display: inline-flex; align-items: center; gap: 5px; font-size: 10.5px; color: var(--fg-dim); font-variant-numeric: tabular-nums; white-space: nowrap; }
  .ctx[data-tone='warn'] { --t: var(--orange, #ff9e64); }
  .ctx[data-tone='high'] { --t: var(--red); color: var(--red); }
  .ctx i { display: block; width: 28px; height: 3px; border-radius: 2px; background: var(--bg-light); overflow: hidden; }
  .ctx s { display: block; height: 100%; background: var(--t); }

  @media (prefers-reduced-motion: reduce) {
    .pulse { animation: none; }
    .card { transition: none; }
  }
</style>
