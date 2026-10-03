<!--
  Every question waiting on the human, oldest first: decisions to make and things only the
  human can do. Two kinds, in one queue: a task's blocker, answered through BlockerCard, which
  types the answer back into the agent's tab; and a watch script waiting to be allowed
  (docs/follow-ups.md §5.1), answered through ScriptApprovalCard, which maiTerm acts on itself.
-->
<script lang="ts">
  import { loomStore } from '$lib/stores/loom.svelte';
  import { tasksStore } from '$lib/stores/tasks.svelte';
  import { workspacesStore, tabDisplayName, navigateToTab } from '$lib/stores/workspaces.svelte';
  import { decisionsQueue, unexplainedBlocked } from '$lib/loom/model';
  import { overlordStore } from '$lib/stores/overlord.svelte';
  import { fmtAge } from '$lib/overlord/format';
  import BlockerCard from '$lib/components/tasks/BlockerCard.svelte';
  import ScriptApprovalCard from '$lib/components/followUps/ScriptApprovalCard.svelte';
  import type { PendingApproval } from '$lib/stores/followUps.svelte';
  import type { Task, Workspace } from '$lib/tauri/types';

  interface Props {
    workspaces: Workspace[];
    tasks: Task[];
    /** Watch scripts waiting to be allowed, already scoped by LoomHome (see there: not limited
     *  to awake workspaces). */
    approvals: PendingApproval[];
  }
  let { workspaces, tasks, approvals }: Props = $props();

  type Item = { kind: 'task'; at: number; task: Task } | { kind: 'script'; at: number; approval: PendingApproval };
  /** Blockers and scripts, oldest first: the order they should be answered in. */
  const queue = $derived.by((): Item[] =>
    [
      ...decisionsQueue(tasks).map((task): Item => ({ kind: 'task', at: Date.parse(task.blocker!.asked_at), task })),
      ...approvals.map((approval): Item => ({ kind: 'script', at: Date.parse(approval.followUp.created_at), approval })),
    ].sort((a, b) => a.at - b.at),
  );
  /** When the queue last changed — an item in, out or reordered — for the script cards' click
   *  guard: any change can move a card under the pointer. Written, never read, here. */
  let listChangedAt = $state(0);
  const signature = $derived(queue.map((i) => (i.kind === 'task' ? i.task.id : i.approval.followUp.id)).join('|'));
  $effect(() => {
    void signature;
    listChangedAt = Date.now();
  });
  const unexplained = $derived(unexplainedBlocked(tasks, workspacesStore.parkedTaskIds));
  /** Receipts for "Ask for the reason", by task id. */
  let asked = $state<Record<string, string>>({});
  /** Asks in flight. The button stays up across the liveness round trip, and a second ask
   *  would either type the question twice or, failing the output-quiet check, raise a second
   *  Overlord handoff and overwrite this one's receipt with "could not be reached". */
  let asking = $state<Record<string, true>>({});
  async function askReason(t: Task, e: MouseEvent) {
    if (e.detail > 1 || asking[t.id] || asked[t.id]) return;
    asking = { ...asking, [t.id]: true };
    const r = await overlordStore.askForBlockerReason(t.id);
    asked = {
      ...asked,
      [t.id]: !r.asked
        ? 'Nobody carries this task, so there is nobody to ask.'
        : r.told === 'tab'
          ? 'Asked. The question will show up here when the agent records it.'
          : r.told === 'agent'
            ? 'The tab was busy, so Overlord will pass it on.'
            : 'The tab could not be reached, and there is no supervisor to relay it.',
    };
    const { [t.id]: _, ...rest } = asking;
    asking = rest;
  }
  /** Outcome lines by task id. An answered card leaves the queue, so the receipt is kept here
   *  and shown in a short list of what was just answered. */
  let answered = $state<{ id: string; title: string; text: string }[]>([]);

  function streamName(t: Task): string {
    for (const w of workspaces) {
      if (tasksStore.find(w.id, t.id)) {
        const s = tasksStore.workstream(w.id, t.workstream_id)?.name;
        return workspaces.length > 1 ? `${w.name}${s ? ` · ${s}` : ''}` : (s ?? 'Loose tasks');
      }
    }
    return '';
  }
</script>

<div class="decisions">
  {#if answered.length}
    <div class="answered">
      {#each answered as a (a.id)}
        <p><b>{a.title}</b> · {a.text}</p>
      {/each}
    </div>
  {/if}
  {#each queue as item (item.kind === 'task' ? item.task.id : item.approval.followUp.id)}
    {#if item.kind === 'task'}
      {@const t = item.task}
      <article class="card">
        <div class="who">
          <span>{t.tab_id ? tabDisplayName(t.tab_id) : 'unassigned'}</span>
          <span>{streamName(t)}</span>
          <span>waiting {fmtAge(t.blocker!.asked_at)}</span>
        </div>
        <h3>{t.title}</h3>
        <BlockerCard
          task={t}
          variant="card"
          onnote={(text) => (answered = [{ id: t.id, title: t.title, text }, ...answered.filter((a) => a.id !== t.id)].slice(0, 5))}
        />
        <div class="actions">
          <button onclick={() => loomStore.show('weave', t.id)}>Show in the weave</button>
          {#if t.tab_id}
            <button onclick={() => { loomStore.openChat(t.tab_id!); loomStore.show('focus'); }}>Talk to the agent</button>
            <button onclick={() => void navigateToTab(t.tab_id!)}>Open the tab</button>
          {/if}
        </div>
      </article>
    {:else}
      {@const a = item.approval}
      <article class="card">
        <div class="who">
          <span>{a.tabName}{a.archived ? ' (archived)' : ''}</span>
          <span>watch script</span>
          <span>waiting {fmtAge(a.followUp.created_at)}</span>
        </div>
        <ScriptApprovalCard
          tabId={a.tabId}
          followUp={a.followUp}
          {listChangedAt}
          onnote={(text) => (answered = [{ id: a.followUp.id, title: a.followUp.due.label ?? 'Watch script', text }, ...answered.filter((x) => x.id !== a.followUp.id)].slice(0, 5))}
        />
        {#if !a.archived}
          <!-- An archived tab can't be opened or talked to until it is restored. -->
          <div class="actions">
            <button onclick={() => { loomStore.openChat(a.tabId); loomStore.show('focus'); }}>Talk to the agent</button>
            <button onclick={() => void navigateToTab(a.tabId)}>Open the tab</button>
          </div>
        {/if}
      </article>
    {/if}
  {:else}
    <p class="empty">Nothing is waiting on you. When an agent stops on a question, it shows up here.</p>
  {/each}

  {#if unexplained.length}
    <section class="unexplained">
      <h4>Blocked with no reason recorded</h4>
      <p class="sub">Whatever these stopped on is only in the agent's chat. Ask, and the question comes back here.</p>
      {#each unexplained as t (t.id)}
        <div class="row">
          <span class="t">{t.title}</span>
          <span class="m">{t.tab_id ? tabDisplayName(t.tab_id) : 'unassigned'} · {streamName(t)} · {fmtAge(t.updated_at)}</span>
          {#if asked[t.id]}
            <span class="receipt">{asked[t.id]}</span>
          {:else if t.tab_id}
            <button disabled={!!asking[t.id]} onclick={(e) => askReason(t, e)}>Ask for the reason</button>
          {/if}
        </div>
      {/each}
    </section>
  {/if}
</div>

<style>
  .decisions {
    position: absolute;
    inset: 0;
    overflow-y: auto;
    padding: 18px;
    display: grid;
    grid-template-columns: repeat(auto-fill, minmax(min(100%, 380px), 1fr));
    gap: 14px;
    align-content: start;
  }
  /* Every outcome lands here, refusals included ("the agent just changed its question"), so
     it is not coloured as success. */
  .answered { grid-column: 1 / -1; display: flex; flex-direction: column; gap: 2px; font-size: 12px; color: var(--fg-dim); }
  .answered p { margin: 0; overflow-wrap: anywhere; }
  .answered b { color: var(--fg); font-weight: 600; }
  .card {
    display: flex;
    flex-direction: column;
    gap: 10px;
    background: var(--bg-medium);
    border: 1px solid var(--bg-light);
    border-radius: 10px;
    padding: 14px;
    min-width: 0;
  }
  .who { display: flex; flex-wrap: wrap; gap: 4px 12px; font-size: 11px; color: var(--fg-dim); }
  h3 { margin: 0; font-size: 13px; font-weight: 600; color: var(--fg-dim); overflow-wrap: anywhere; }
  .actions { display: flex; flex-wrap: wrap; gap: 6px; }
  .actions button { background: var(--bg-dark); border: 1px solid var(--bg-light); color: var(--fg); border-radius: 6px; padding: 5px 10px; font: inherit; font-size: 12px; cursor: pointer; }
  .empty { grid-column: 1 / -1; color: var(--fg-dim); text-align: center; padding: 40px 0; margin: 0; }
  .unexplained { grid-column: 1 / -1; display: flex; flex-direction: column; gap: 6px; border-top: 1px solid var(--bg-light); padding-top: 14px; }
  .unexplained h4 { margin: 0; font-size: 10.5px; font-weight: 500; letter-spacing: 0.08em; text-transform: uppercase; color: var(--yellow); }
  .sub { margin: 0 0 4px; font-size: 12px; color: var(--fg-dim); }
  .row { display: flex; flex-wrap: wrap; align-items: baseline; gap: 4px 12px; padding: 6px 0; border-bottom: 1px solid var(--bg-medium); }
  .row .t { font-size: 12.5px; overflow-wrap: anywhere; }
  .row .m { font-size: 11px; color: var(--fg-dim); flex: 1; min-width: 12em; }
  .row button { background: var(--bg-medium); border: 1px solid var(--bg-light); color: var(--fg); border-radius: 6px; padding: 4px 10px; font: inherit; font-size: 12px; cursor: pointer; }
  .receipt { font-size: 11.5px; color: var(--fg-dim); }
</style>
