<!--
  Needs you (docs/hive.md §4.8): everything any of the Hive's agents waits on the human for, in
  one queue, oldest first, so nobody has to find which worker's column holds a question. Each
  item is the card every other surface shows for it (BlockerCard, PromptCard, ScriptApprovalCard,
  AgentRequestCard), so its guards are the same here. What the coordinator holds is listed below,
  apart and not counted: its card reads "With coordinator" and offers Take over.
-->
<script lang="ts">
  import BlockerCard from '$lib/components/tasks/BlockerCard.svelte';
  import PromptCard from '$lib/components/loom/PromptCard.svelte';
  import ScriptApprovalCard from '$lib/components/followUps/ScriptApprovalCard.svelte';
  import AgentRequestCard from '$lib/components/agentRequests/AgentRequestCard.svelte';
  import { agentStateStore } from '$lib/stores/agentState.svelte';
  import { fmtAge } from '$lib/overlord/format';
  import type { HiveAsker, NeedsYouItem } from '$lib/hive/needsYou';

  interface Props {
    items: NeedsYouItem[];
    /** While shown: a prompt card polls its tab only then. */
    active: boolean;
    /** Opens the asker's column (a worker's on the board, or the coordinator's). */
    onopen: (asker: HiveAsker, e: MouseEvent) => void;
  }
  let { items, active, onopen }: Props = $props();

  const mine = $derived(items.filter((i) => !i.held));
  const held = $derived(items.filter((i) => i.held));

  /** When the list last changed, for the cards' click guard: anything in or out above a card
   *  can move it under the pointer. */
  let listChangedAt = $state(0);
  const signature = $derived(items.map((i) => i.key).join('|'));
  $effect(() => {
    void signature;
    listChangedAt = Date.now();
  });

  /** Outcome lines: an answered card leaves the queue, so what happened is kept here. */
  let answered = $state<{ key: string; title: string; text: string }[]>([]);
  const noted = (key: string, title: string) => (text: string) => {
    answered = [{ key, title, text }, ...answered.filter((a) => a.key !== key)].slice(0, 5);
  };

  const KIND: Record<NeedsYouItem['kind'], string> = {
    blocker: 'Question',
    prompt: 'On screen',
    script: 'Watch script',
    request: 'Approval',
  };
  const kindText = (i: NeedsYouItem) => (i.kind === 'blocker' && i.task.blocker?.kind === 'action' ? 'Only you can do it' : KIND[i.kind]);
  const askerName = (i: NeedsYouItem) => i.asker?.name ?? (i.kind === 'blocker' ? 'Unassigned task' : 'The Hive');
</script>

{#snippet item(i: NeedsYouItem)}
  <article class="card" class:held={i.held}>
    <header class="who">
      {#if i.asker?.badge != null}<span class="badge">#{i.asker.badge}</span>{/if}
      <b>{i.asker?.badge != null ? i.asker.name.replace(/^#\d+\s*/, '') : askerName(i)}</b>
      <span class="kind">{kindText(i)}</span>
      {#if i.at > 0}<span class="age">waiting {fmtAge(new Date(i.at).toISOString())}</span>{/if}
    </header>
    {#if i.kind === 'blocker'}
      {#if i.task.title}<h3>{i.task.title}</h3>{/if}
      {#key i.task.blocker?.asked_at}
        <BlockerCard task={i.task} variant="card" onnote={noted(i.key, i.task.title)} />
      {/key}
    {:else if i.kind === 'prompt'}
      <PromptCard tabId={i.asker.tabId} {active} pulse={agentStateStore.getState(i.asker.tabId)?.state} />
    {:else if i.kind === 'script'}
      <ScriptApprovalCard tabId={i.approval.tabId} followUp={i.approval.followUp} {listChangedAt} onnote={noted(i.key, i.approval.followUp.due.label ?? 'Watch script')} />
    {:else}
      <AgentRequestCard request={i.request} {listChangedAt} onnote={noted(i.key, i.request.summary)} />
    {/if}
    {#if i.asker}
      {@const a = i.asker}
      <div class="actions">
        <button type="button" onclick={(e) => onopen(a, e)}>{a.taskId ? 'Open worker' : 'Open queen'}</button>
      </div>
    {/if}
  </article>
{/snippet}

<section class="needs" aria-label="Needs you">
  {#if answered.length}
    <div class="answered" role="status">
      {#each answered as a (a.key)}
        <p><b>{a.title}</b> · {a.text}</p>
      {/each}
    </div>
  {/if}
  {#each mine as i (i.key)}
    {@render item(i)}
  {:else}
    <p class="empty">Nothing is waiting on you. When a worker or the queen stops on a question, a permission or an approval, it shows up here.</p>
  {/each}
  {#if held.length}
    <h4 class="held-h">With the queen</h4>
    <p class="held-sub">It answers these, and you're told if it takes too long. Take one over to answer it yourself.</p>
    {#each held as i (i.key)}
      {@render item(i)}
    {/each}
  {/if}
</section>

<style>
  .needs {
    flex: 1;
    min-width: 0;
    min-height: 0;
    overflow-y: auto;
    padding: 16px;
    display: grid;
    grid-template-columns: repeat(auto-fill, minmax(min(100%, 380px), 1fr));
    gap: 12px;
    align-content: start;
  }
  /* Every outcome lands here, refusals included, so it is not coloured as success. */
  .answered { grid-column: 1 / -1; display: flex; flex-direction: column; gap: 2px; font-size: 12px; color: var(--fg-dim); }
  .answered p { margin: 0; overflow-wrap: anywhere; }
  .answered b { color: var(--fg); font-weight: 600; }
  .card {
    display: flex;
    flex-direction: column;
    gap: 10px;
    -webkit-user-select: text;
    user-select: text;
    background: var(--bg-medium);
    border: 1px solid var(--bg-light);
    border-radius: 10px;
    padding: 14px;
    min-width: 0;
  }
  .card.held { background: transparent; }
  .who { display: flex; flex-wrap: wrap; align-items: baseline; gap: 4px 10px; font-size: 11px; color: var(--fg-dim); }
  .who b { color: var(--fg); font-size: 12.5px; font-weight: 600; overflow-wrap: anywhere; }
  .badge { font-variant-numeric: tabular-nums; color: var(--accent); font-weight: 600; font-size: 12px; }
  .kind { color: var(--orange, var(--yellow)); }
  .held .kind { color: var(--fg-dim); }
  .age { margin-left: auto; }
  h3 { margin: 0; font-size: 12px; font-weight: 500; color: var(--fg-dim); overflow-wrap: anywhere; }
  .actions { display: flex; flex-wrap: wrap; gap: 6px; }
  .actions button { background: var(--bg-dark); border: 1px solid var(--bg-light); color: var(--fg); border-radius: 6px; padding: 5px 10px; font: inherit; font-size: 12px; cursor: pointer; }
  .actions button:hover { border-color: var(--accent); }
  .empty { grid-column: 1 / -1; color: var(--fg-dim); text-align: center; padding: 40px 0; margin: 0; }
  .held-h { grid-column: 1 / -1; margin: 10px 0 0; padding-top: 12px; border-top: 1px solid var(--bg-light); font-size: 12px; font-weight: 600; color: var(--fg-dim); }
  .held-sub { grid-column: 1 / -1; margin: -6px 0 0; font-size: 11.5px; color: var(--fg-dim); }
</style>
