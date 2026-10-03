<!--
  Workstream Loom (docs/loom.md): the Overlord deck's home view. It spans every workspace in the
  window, or one picked from the filter, in three modes: Focus (work with the agents), Weave (the
  work as a picture), Decisions (the questions waiting on the human).

  It lives in the Overlord workspace rather than over the terminal area: nothing here is
  rendered on top of a live terminal, and while it is on screen no other workspace's terminal
  is visible, so none of them paints.
-->
<script lang="ts">
  import { loomStore, type LoomMode } from '$lib/stores/loom.svelte';
  import { tasksStore } from '$lib/stores/tasks.svelte';
  import { workspacesStore } from '$lib/stores/workspaces.svelte';
  import { claudeStateStore } from '$lib/stores/agentState.svelte';
  import { decisionsQueue, summarize } from '$lib/loom/model';
  import { followUpsStore } from '$lib/stores/followUps.svelte';
  import { workspaceIsLive } from '$lib/workspace/liveness';
  import LoomView from './LoomView.svelte';
  import DecisionsView from './DecisionsView.svelte';
  import FocusView from './FocusView.svelte';

  interface Props {
    /** The deck is on screen. */
    active: boolean;
  }
  let { active }: Props = $props();

  /** Every awake workspace but the Overlord's own: it is the supervisor, not the work. "Awake"
   *  is the sidebar's rule (`workspaceIsLive`, a live PTY), not the `suspended` flag, which
   *  stays clear on a workspace whose tabs were parked one by one. Waking one brings it back. */
  const all = $derived(workspacesStore.workspaces.filter((w) => !w.overlord && workspaceIsLive(w)));
  const scoped = $derived(
    loomStore.workspaceFilter ? all.filter((w) => w.id === loomStore.workspaceFilter) : all,
  );
  // A filtered workspace that was deleted falls back to all of them.
  $effect(() => {
    const f = loomStore.workspaceFilter;
    if (f && !all.some((w) => w.id === f)) loomStore.setWorkspaceFilter(null);
  });
  const tasks = $derived(scoped.flatMap((w) => tasksStore.forWorkspace(w.id)));

  /** Ages and the quiet rule move with the clock, not only with the data. */
  let now = $state(Date.now());
  $effect(() => {
    if (!active) return;
    now = Date.now();
    const t = setInterval(() => (now = Date.now()), 30_000);
    return () => clearInterval(t);
  });

  const summary = $derived(summarize(tasks, now, workspacesStore.parkedTaskIds));
  /** Watch scripts waiting to be allowed, in scope: the other half of the Decisions queue. */
  const scripts = $derived.by(() => {
    const ids = new Set(scoped.map((w) => w.id));
    return followUpsStore.pendingApprovals.filter((a) => ids.has(a.workspaceId)).length;
  });
  const decisions = $derived(decisionsQueue(tasks).length + scripts);
  /** Agents stopped at a permission prompt, in scope. */
  const permissions = $derived(
    scoped.reduce(
      (n, w) => n + w.panes.reduce((m, p) => m + p.tabs.filter((t) => claudeStateStore.getState(t.id)?.state === 'permission').length, 0),
      0,
    ),
  );

  const MODES: { id: LoomMode; label: string }[] = [
    { id: 'focus', label: 'Focus' },
    { id: 'weave', label: 'Weave' },
    { id: 'decisions', label: 'Decisions' },
  ];
</script>

<section class="loom-home" aria-label="Workstream Loom">
  <header>
    <nav class="modes" aria-label="Mode">
      {#each MODES as m (m.id)}
        <button aria-pressed={loomStore.mode === m.id} onclick={() => loomStore.setMode(m.id)}>
          {m.label}
          {#if m.id === 'focus' && permissions}<span class="count perm">{permissions}</span>{/if}
          {#if m.id === 'decisions' && decisions}<span class="count">{decisions}</span>{/if}
        </button>
      {/each}
    </nav>
    <span class="summary">
      <span><i style="background: var(--green)"></i>{summary.active} active</span>
      <span><i style="background: var(--orange, #ff9e64)"></i>{summary.needsYou + scripts + permissions} need you</span>
      <span><i style="background: var(--yellow)"></i>{summary.waiting} waiting</span>
      <span><i style="background: var(--fg-dim)"></i>{summary.quiet} quiet 14d+</span>
    </span>
    {#if all.length > 1}
      <div class="filter" role="group" aria-label="Workspaces">
        <button aria-pressed={!loomStore.workspaceFilter} onclick={() => loomStore.setWorkspaceFilter(null)}>All</button>
        {#each all as w (w.id)}
          <button aria-pressed={loomStore.workspaceFilter === w.id} onclick={() => loomStore.setWorkspaceFilter(w.id)}>{w.name}</button>
        {/each}
      </div>
    {/if}
  </header>

  <div class="body">
    {#if loomStore.mode === 'focus'}
      <FocusView workspaces={scoped} {tasks} {now} {active} />
    {:else if loomStore.mode === 'weave'}
      <LoomView workspaces={scoped} {tasks} {now} multi={scoped.length > 1} />
    {:else}
      <DecisionsView workspaces={scoped} {tasks} />
    {/if}
  </div>
</section>

<style>
  .loom-home { display: flex; flex-direction: column; flex: 1; min-height: 0; color: var(--fg); background: var(--bg-dark); }
  header {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: 8px 18px;
    padding: 8px 14px;
    border-bottom: 1px solid var(--bg-light);
    font-size: 12px;
  }
  .modes, .filter { display: flex; gap: 2px; background: var(--bg-medium); padding: 3px; border-radius: 7px; }
  .filter { flex-wrap: wrap; background: none; padding: 0; }
  .modes button, .filter button {
    display: flex;
    align-items: center;
    gap: 6px;
    background: none;
    border: 0;
    color: var(--fg-dim);
    padding: 4px 12px;
    border-radius: 5px;
    font: inherit;
    cursor: pointer;
    overflow-wrap: anywhere;
  }
  .modes button { font-weight: 600; }
  .modes button[aria-pressed='true'], .filter button[aria-pressed='true'] { background: var(--bg-light); color: var(--fg); }
  .filter button { padding: 3px 9px; font-size: 11.5px; }
  .summary { display: flex; flex-wrap: wrap; gap: 4px 14px; color: var(--fg-dim); font-variant-numeric: tabular-nums; }
  .summary span { display: inline-flex; align-items: center; gap: 6px; }
  .summary i { width: 7px; height: 7px; border-radius: 50%; display: inline-block; }
  .count { background: var(--orange, #ff9e64); color: var(--bg-dark); border-radius: 8px; padding: 0 5px; font-size: 10.5px; }
  .count.perm { background: var(--red); }
  .filter { margin-left: auto; }
  .body { flex: 1; min-height: 0; position: relative; }
</style>
