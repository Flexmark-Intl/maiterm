<!--
  The loom: agents on the left, the workstreams they are working in, and a string from each
  agent to each task it carries. Moving dots run along a string whose task is being worked
  right now; a slow amber dash runs backwards along one that is waiting; a faint dotted string
  joins a task to what it depends on. Selecting an agent fades everything it isn't part of.
-->
<script lang="ts">
  import { untrack } from 'svelte';
  import { loomStore } from '$lib/stores/loom.svelte';
  import { tasksStore } from '$lib/stores/tasks.svelte';
  import { claudeStateStore } from '$lib/stores/agentState.svelte';
  import { workspacesStore, tabDisplayName, navigateToTab } from '$lib/stores/workspaces.svelte';
  import { BLOCKER_LABEL, isRetired, laneName, resolveBlockers } from '$lib/tasks/model';
  import { isQuiet, laneOf, loomAgents, type AgentTabInput } from '$lib/loom/model';
  import { fmtAge } from '$lib/overlord/format';
  import BlockerCard from '$lib/components/tasks/BlockerCard.svelte';
  import type { Task, TaskStatus, Workspace } from '$lib/tauri/types';

  interface Props {
    workspaces: Workspace[];
    tasks: Task[];
    now: number;
    /** More than one workspace: label each workstream with its workspace. */
    multi: boolean;
  }
  let { workspaces, tasks, now, multi }: Props = $props();

  const parked = $derived(workspacesStore.parkedTaskIds);

  const agentTabs = $derived<AgentTabInput[]>(
    workspaces.flatMap((w) =>
      w.panes.flatMap((p) =>
        p.tabs
          .filter((t) => t.tab_type === 'terminal' && !t.service_id)
          .map((t) => {
            const s = claudeStateStore.getState(t.id);
            return { tabId: t.id, name: tabDisplayName(t.id), state: s?.state ?? null, toolDetail: s?.toolDetail ?? s?.toolName ?? null };
          }),
      ),
    ),
  );
  const agents = $derived(loomAgents(agentTabs, tasks));

  /** Open work only: retired rows are history and backlog is the parking lot. */
  const shown = (t: Task) => !isRetired(t.status) && t.status !== 'backlog';

  interface Stream { key: string; name: string; workspace: string; tasks: Task[]; done: number; total: number; }
  const streams = $derived.by<Stream[]>(() => {
    const out: Stream[] = [];
    for (const w of workspaces) {
      const list = tasksStore.forWorkspace(w.id);
      const groups = new Map<string, Task[]>();
      for (const t of list) {
        const k = t.workstream_id ?? '';
        if (!groups.has(k)) groups.set(k, []);
        groups.get(k)!.push(t);
      }
      for (const [k, all] of groups) {
        const open = all.filter(shown);
        if (!open.length) continue;
        out.push({
          key: `${w.id}:${k}`,
          name: k ? (tasksStore.workstream(w.id, k)?.name ?? 'Workstream') : 'Loose tasks',
          workspace: w.name,
          tasks: open,
          done: all.filter((t) => t.status === 'done').length,
          total: all.filter((t) => t.status !== 'dropped').length,
        });
      }
    }
    // Streams with something waiting on the human first, then the busiest.
    const weight = (s: Stream) =>
      (s.tasks.some((t) => t.status === 'blocked' && (t.blocker?.kind === 'decision' || t.blocker?.kind === 'action')) ? 0 : 1) * 1000 -
      s.tasks.filter((t) => t.status === 'active').length;
    return out.sort((a, b) => weight(a) - weight(b));
  });

  const byId = $derived(new Map(tasks.map((t) => [t.id, t])));
  const lane = (t: Task): TaskStatus => laneOf(t, tasks, parked);

  const KIND_COLOR = { decision: 'var(--orange, #ff9e64)', action: 'var(--red)', external: 'var(--cyan)' } as const;
  function color(t: Task): string {
    const l = lane(t);
    if (l === 'blocked' && t.status === 'blocked' && t.blocker) return KIND_COLOR[t.blocker.kind];
    return { active: 'var(--green)', review: 'var(--magenta)', blocked: 'var(--yellow)', todo: 'var(--fg-dim)', backlog: 'var(--fg-dim)', done: 'var(--fg-dim)', dropped: 'var(--fg-dim)' }[l];
  }
  function laneLabel(t: Task): string {
    const l = lane(t);
    if (l === 'blocked' && t.status === 'blocked' && t.blocker) return BLOCKER_LABEL[t.blocker.kind];
    if (l === 'blocked' && t.status !== 'blocked') return 'waiting on tasks';
    return laneName(l);
  }
  const unmet = (t: Task) => {
    const seen = new Set<string>();
    return resolveBlockers(t, tasks, parked).filter(
      (b) => (b.state === 'waiting' || b.state === 'parked') && !seen.has(b.id) && !!seen.add(b.id),
    );
  };

  function related(t: Task): boolean {
    const a = loomStore.focusAgentId;
    if (!a) return true;
    if (t.tab_id === a) return true;
    return (t.blocked_by ?? []).some((d) => byId.get(d)?.tab_id === a) || tasks.some((o) => o.tab_id === a && o.blocked_by?.includes(t.id));
  }

  const selected = $derived(loomStore.selectedTaskId ? byId.get(loomStore.selectedTaskId) ?? null : null);
  /** The last answer's outcome, for the selected task only. */
  let outcome = $state('');
  $effect(() => {
    void loomStore.selectedTaskId;
    outcome = '';
  });
  const agentName = (tabId: string | null | undefined) => (tabId ? tabDisplayName(tabId) : 'unassigned');
  const streamOf = (t: Task) => streams.find((s) => s.tasks.includes(t))?.name ?? '';

  // ── Strings ────────────────────────────────────────────────────────────────────────────
  let host = $state<HTMLElement | null>(null);
  let streamsEl = $state<HTMLElement | null>(null);
  let agentsEl = $state<HTMLElement | null>(null);
  interface Edge { d: string; color: string; cls: string; bead: boolean; key: string }
  let edges = $state<Edge[]>([]);
  let raf = 0;

  function draw() {
    cancelAnimationFrame(raf);
    raf = requestAnimationFrame(() => {
      if (!host) return;
      const H = host.getBoundingClientRect();
      const box = (sel: string) => {
        const n = host!.querySelector(sel);
        if (!n) return null;
        const r = n.getBoundingClientRect();
        return { l: r.left - H.left, r: r.right - H.left, t: r.top - H.top, b: r.bottom - H.top, cy: r.top - H.top + r.height / 2 };
      };
      // Both columns scroll; a string to a card scrolled out of its column is not drawn.
      const cols = [streamsEl, agentsEl].map((el) => el?.getBoundingClientRect());
      const within = (v: DOMRect | undefined) => (b: { t: number; b: number }) =>
        !v || (b.b > v.top - H.top && b.t < v.bottom - H.top);
      const inView = within(cols[0]);
      const agentInView = within(cols[1]);
      const out: Edge[] = [];
      const sel = loomStore.selectedTaskId, fa = loomStore.focusAgentId;
      for (const s of streams) {
        for (const t of s.tasks) {
          const tb = box(`[data-loom-task="${t.id}"]`);
          if (!tb || !inView(tb)) continue;
          const hot = sel === t.id || (!!fa && t.tab_id === fa);
          const cold = !!fa && !related(t);
          if (t.tab_id) {
            const ab = box(`[data-loom-agent="${t.tab_id}"]`);
            if (ab && agentInView(ab)) {
              const mx = (tb.l - ab.r) * 0.5;
              const l = lane(t);
              out.push({
                key: `a:${t.id}`,
                d: `M${ab.r},${ab.cy} C${ab.r + mx},${ab.cy} ${tb.l - mx},${tb.cy} ${tb.l},${tb.cy}`,
                color: color(t),
                cls: `${l === 'active' ? 'run' : l === 'blocked' ? 'wait' : ''}${hot ? ' hot' : ''}${cold ? ' cold' : ''}`,
                bead: l === 'active' && !cold,
              });
            }
          }
          // Deduped: createTasks stores blocked_by as sent, and a repeated id would give two
          // strings the same key.
          for (const dep of new Set(t.blocked_by ?? [])) {
            const db = box(`[data-loom-task="${dep}"]`);
            if (!db || !inView(db)) continue;
            const same = Math.abs(db.cy - tb.cy) < 24;
            const d = same
              ? `M${db.r},${db.cy} C${db.r + 16},${db.cy} ${tb.l - 16},${tb.cy} ${tb.l},${tb.cy}`
              : `M${db.l + 14},${db.b} C${db.l + 14},${db.b + 28} ${tb.l + 14},${tb.t - 28} ${tb.l + 14},${tb.t}`;
            const met = byId.get(dep)?.status === 'done';
            out.push({ key: `d:${dep}:${t.id}`, d, color: met ? 'var(--fg-dim)' : 'var(--yellow)', cls: `dep${sel === t.id || sel === dep ? ' hot' : ''}${cold ? ' cold' : ''}`, bead: false });
          }
        }
      }
      edges = out;
    });
  }

  // Redraw when anything that moves a card or changes a string changes.
  $effect(() => {
    void streams; void agents; void loomStore.selectedTaskId; void loomStore.focusAgentId;
    untrack(draw);
  });
  $effect(() => {
    if (!host) return;
    const ro = new ResizeObserver(() => draw());
    ro.observe(host);
    return () => { ro.disconnect(); cancelAnimationFrame(raf); };
  });
</script>

<div class="loom" bind:this={host}>
  <svg class="strings" aria-hidden="true">
    {#each edges as e (e.key)}
      <path d={e.d} stroke={e.color} class="edge {e.cls}" />
      {#if e.bead}
        <circle r="2.6" fill={e.color} class="bead"><animateMotion dur="2.4s" repeatCount="indefinite" path={e.d} /></circle>
      {/if}
    {/each}
  </svg>

  <aside class="agents" bind:this={agentsEl} onscroll={draw}>
    <h3>Agents</h3>
    {#each agents as a (a.tabId)}
      <button
        class="agent"
        data-loom-agent={a.tabId}
        aria-pressed={loomStore.focusAgentId === a.tabId}
        onclick={() => loomStore.toggleFocusAgent(a.tabId)}
      >
        <span class="nm">
          <i class="dot" class:pulse={a.state === 'active' || a.state === 'permission'} data-state={a.state ?? 'none'}></i>
          {a.name}
        </span>
        <span class="st">{a.state === 'permission' ? 'waiting on a permission' : (a.state ?? 'no session')} · {a.taskIds.length} open</span>
        {#if a.doing}<span class="doing">{a.doing}</span>{/if}
      </button>
    {:else}
      <p class="empty">No agents in this workspace.</p>
    {/each}
  </aside>

  <main class="streams" bind:this={streamsEl} onscroll={draw}>
    {#each streams as s (s.key)}
      <section class="ws">
        <div class="ws-h">
          <h4>{s.name}</h4>
          {#if multi}<span class="meta">{s.workspace}</span>{/if}
          <span class="meter" aria-hidden="true"><i style="width: {s.total ? (s.done / s.total) * 100 : 0}%"></i></span>
          <span class="meta">{s.done}/{s.total} done</span>
        </div>
        <div class="chips">
          {#each s.tasks as t (t.id)}
            {@const u = lane(t) === 'blocked' && t.status !== 'blocked' ? unmet(t).length : 0}
            <button
              class="chip"
              class:quiet={isQuiet(t, now)}
              class:dim={!related(t)}
              data-loom-task={t.id}
              data-lane={lane(t)}
              style="--c: {color(t)}"
              aria-pressed={loomStore.selectedTaskId === t.id}
              onclick={() => loomStore.select(t.id)}
            >
              <span class="t">{t.title}</span>
              <span class="m"><span>{laneLabel(t)}</span><span>{isQuiet(t, now) ? 'quiet ' : ''}{fmtAge(t.updated_at)}</span></span>
              {#if t.status === 'blocked' && t.blocker}
                <span class="flag">?</span>
              {:else if u}
                <span class="flag">{u} open</span>
              {/if}
            </button>
          {/each}
        </div>
      </section>
    {:else}
      <p class="empty">No open tasks. Agents add them with createTasks, and you can add them from the task panel.</p>
    {/each}
  </main>

  <aside class="inspector">
    {#if selected}
      <span class="pill" style="--c: {color(selected)}">{laneLabel(selected)}</span>
      <h3 class="title">{selected.title}</h3>
      <dl class="kv">
        <dt>workstream</dt><dd>{streamOf(selected) || '—'}</dd>
        <dt>agent</dt><dd>{agentName(selected.tab_id)}</dd>
        <dt>last change</dt><dd>{fmtAge(selected.updated_at)} ago{isQuiet(selected, now) ? ' · quiet' : ''}</dd>
      </dl>
      {#if selected.status === 'blocked' && selected.blocker}
        {#key selected.id}
          {@const forId = selected.id}
          <!-- The note can arrive after the human has moved on (it waits for the paste), so it
               only lands if this task is still the one selected. -->
          <BlockerCard task={selected} variant="card" onnote={(text) => { if (loomStore.selectedTaskId === forId) outcome = text; }} />
        {/key}
      {:else if lane(selected) === 'blocked'}
        {@const deps = unmet(selected)}
        {#if deps.length}
          <div class="deps">
            <b>Waiting on {deps.length} task{deps.length === 1 ? '' : 's'}</b>
            <ul>
              {#each deps as d (d.id)}
                <li><button onclick={() => loomStore.select(d.id)}>{d.title ?? 'a task parked with an archived tab'}</button> · {d.status ? laneName(d.status) : 'parked'}</li>
              {/each}
            </ul>
            <span class="hint">It starts on its own when the last one finishes.</span>
          </div>
        {:else}
          <p class="hint">Blocked with no reason recorded. Ask the agent to set a blocker with its question.</p>
        {/if}
      {/if}
      {#if outcome}<p class="outcome">{outcome}</p>{/if}
      {#if selected.detail}<p class="detail">{selected.detail}</p>{/if}
      {#if selected.notes?.length}
        <h4>Log</h4>
        <ol class="log">
          {#each [...selected.notes].reverse() as n, i (i)}
            <li class:human={n.by === 'human'}><time>{fmtAge(n.at)} ago · {n.by}</time>{n.text}</li>
          {/each}
        </ol>
      {/if}
      {#if selected.tab_id}
        <div class="actions">
          <button onclick={() => { loomStore.openChat(selected.tab_id!); loomStore.show('focus'); }}>Talk to the agent</button>
          <button onclick={() => void navigateToTab(selected.tab_id!)}>Open the tab</button>
        </div>
      {/if}
    {:else}
      <p class="hint">Select a task to see what it is waiting for, its log, and who carries it.</p>
    {/if}
  </aside>
</div>

<style>
  .loom {
    position: absolute;
    inset: 0;
    display: grid;
    grid-template-columns: 210px minmax(0, 1fr) 330px;
    overflow: hidden;
  }
  .strings { position: absolute; inset: 0; width: 100%; height: 100%; pointer-events: none; z-index: 1; overflow: visible; }
  .edge { fill: none; stroke-width: 1.2; opacity: 0.45; transition: opacity 0.2s; }
  .edge.run { stroke-dasharray: 3 7; animation: flow 1.1s linear infinite; opacity: 0.75; }
  .edge.wait { stroke-dasharray: 6 5; animation: flow 3.2s linear infinite reverse; }
  .edge.dep { stroke-dasharray: 1 4; opacity: 0.5; }
  .edge.hot { opacity: 1; stroke-width: 2; }
  .edge.cold { opacity: 0.08; }
  @keyframes flow { to { stroke-dashoffset: -20; } }
  @media (prefers-reduced-motion: reduce) {
    .edge.run, .edge.wait { animation: none; }
    .bead { display: none; }
  }

  h3, h4 { margin: 0; }
  .agents, .streams, .inspector { overflow-y: auto; padding: 14px; min-width: 0; }
  .agents { border-right: 1px solid var(--bg-light); display: flex; flex-direction: column; gap: 8px; position: relative; z-index: 2; }
  .agents h3, .inspector h4 { font-size: 10.5px; font-weight: 500; letter-spacing: 0.08em; text-transform: uppercase; color: var(--fg-dim); }
  .agent {
    display: flex;
    flex-direction: column;
    gap: 2px;
    text-align: left;
    background: var(--bg-medium);
    border: 1px solid var(--bg-light);
    border-radius: 8px;
    padding: 8px 10px;
    color: var(--fg);
    font: inherit;
    cursor: pointer;
  }
  .agent[aria-pressed='true'] { border-color: var(--accent); }
  .nm { display: flex; align-items: center; gap: 7px; font-weight: 600; font-size: 12.5px; overflow-wrap: anywhere; }
  .st { font-size: 11px; color: var(--fg-dim); }
  .doing { font-size: 11px; color: var(--fg); opacity: 0.8; overflow-wrap: anywhere; }
  .dot { width: 7px; height: 7px; border-radius: 50%; flex: none; background: var(--fg-dim); }
  .dot[data-state='active'] { background: var(--green); }
  .dot[data-state='permission'] { background: var(--red); }
  .pulse { animation: pulse 1.6s ease-in-out infinite; }
  @keyframes pulse { 50% { opacity: 0.35; } }

  .streams { display: flex; flex-direction: column; gap: 16px; padding: 14px 18px; }
  .ws { display: flex; flex-direction: column; gap: 8px; position: relative; z-index: 2; }
  .ws-h { display: flex; flex-wrap: wrap; align-items: center; gap: 4px 10px; }
  .ws-h h4 { font-size: 13.5px; font-weight: 600; overflow-wrap: anywhere; }
  .meta { font-size: 11px; color: var(--fg-dim); font-variant-numeric: tabular-nums; }
  .meter { display: block; width: 90px; height: 3px; border-radius: 2px; background: var(--bg-light); overflow: hidden; }
  .meter i { display: block; height: 100%; background: var(--green); }
  .chips { display: flex; flex-wrap: wrap; gap: 8px 22px; }
  .chip {
    position: relative;
    display: flex;
    flex-direction: column;
    gap: 2px;
    max-width: 250px;
    text-align: left;
    background: var(--bg-medium);
    border: 1px solid var(--bg-light);
    border-radius: 6px;
    padding: 6px 9px 6px 11px;
    color: var(--fg);
    font: inherit;
    cursor: pointer;
    transition: opacity 0.25s, border-color 0.15s;
  }
  .chip::before { content: ''; position: absolute; left: -1px; top: 6px; bottom: 6px; width: 2px; border-radius: 2px; background: var(--c); }
  .chip[aria-pressed='true'] { border-color: var(--c); box-shadow: 0 0 0 1px var(--c); }
  .chip.quiet { opacity: 0.6; }
  .chip.dim { opacity: 0.25; }
  .chip .t { font-size: 12px; line-height: 1.3; overflow-wrap: anywhere; }
  .chip[data-lane='todo'] .t { color: var(--fg-dim); }
  .chip .m { display: flex; gap: 8px; font-size: 10.5px; color: var(--fg-dim); }
  .flag { position: absolute; top: -8px; right: -8px; font-size: 10px; font-weight: 600; padding: 1px 5px; border-radius: 8px; background: var(--c); color: var(--bg-dark); }

  .inspector { border-left: 1px solid var(--bg-light); display: flex; flex-direction: column; gap: 12px; position: relative; z-index: 2; background: var(--bg-dark); }
  .pill { align-self: flex-start; font-size: 10.5px; text-transform: uppercase; letter-spacing: 0.06em; padding: 2px 7px; border-radius: 9px; border: 1px solid var(--c); color: var(--c); }
  .title { font-size: 15px; line-height: 1.3; }
  .kv { display: grid; grid-template-columns: auto 1fr; gap: 3px 12px; margin: 0; font-size: 11.5px; }
  .kv dt { color: var(--fg-dim); }
  .kv dd { margin: 0; overflow-wrap: anywhere; }
  .deps { display: flex; flex-direction: column; gap: 6px; border-left: 2px solid var(--yellow); padding: 8px 10px; font-size: 12px; background: color-mix(in srgb, var(--yellow) 7%, transparent); border-radius: 0 6px 6px 0; }
  .deps ul { margin: 0; padding-left: 16px; }
  .deps button { background: none; border: 0; padding: 0; color: var(--accent); font: inherit; cursor: pointer; text-align: left; }
  .hint { margin: 0; font-size: 11.5px; color: var(--fg-dim); }
  .outcome { margin: 0; font-size: 11.5px; color: var(--fg-dim); }
  .detail { margin: 0; font-size: 12px; white-space: pre-wrap; overflow-wrap: anywhere; color: var(--fg); opacity: 0.85; }
  .log { margin: 0; padding: 0 0 0 12px; list-style: none; border-left: 1px solid var(--bg-light); display: flex; flex-direction: column; gap: 7px; font-size: 12px; }
  .log time { display: block; font-size: 10.5px; color: var(--fg-dim); }
  .log li.human { color: var(--fg); }
  .actions { display: flex; flex-wrap: wrap; gap: 6px; }
  .actions button { background: var(--bg-medium); border: 1px solid var(--bg-light); color: var(--fg); border-radius: 6px; padding: 5px 10px; font: inherit; font-size: 12px; cursor: pointer; }
  .empty { color: var(--fg-dim); font-size: 12px; margin: 0; }

  @media (max-width: 1000px) {
    .loom { grid-template-columns: 180px minmax(0, 1fr); grid-template-rows: minmax(0, 1fr) auto; }
    .inspector { grid-column: 1 / -1; border-left: 0; border-top: 1px solid var(--bg-light); max-height: 45%; }
  }
</style>
