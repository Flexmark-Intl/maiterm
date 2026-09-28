<!--
  Every question waiting on the human, oldest first: decisions to make and things only the
  human can do. Each card answers in place through BlockerCard, which types the answer back
  into the agent's tab.
-->
<script lang="ts">
  import { loomStore } from '$lib/stores/loom.svelte';
  import { tasksStore } from '$lib/stores/tasks.svelte';
  import { tabDisplayName, navigateToTab } from '$lib/stores/workspaces.svelte';
  import { decisionsQueue } from '$lib/loom/model';
  import { fmtAge } from '$lib/overlord/format';
  import BlockerCard from '$lib/components/tasks/BlockerCard.svelte';
  import type { Task, Workspace } from '$lib/tauri/types';

  interface Props {
    workspaces: Workspace[];
    tasks: Task[];
  }
  let { workspaces, tasks }: Props = $props();

  const queue = $derived(decisionsQueue(tasks));
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
  {#each queue as t (t.id)}
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
        <button onclick={() => { loomStore.select(t.id); loomStore.setView('loom'); }}>Show in the loom</button>
        {#if t.tab_id}
          <button onclick={() => { loomStore.close(); void navigateToTab(t.tab_id!); }}>Open the tab</button>
        {/if}
      </div>
    </article>
  {:else}
    <p class="empty">Nothing is waiting on you. When an agent stops on a question, it shows up here.</p>
  {/each}
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
</style>
