<script lang="ts">
  import { workspacesStore, navigateToTab, tabDisplayName } from '$lib/stores/workspaces.svelte';
  import { overlordStore } from '$lib/stores/overlord.svelte';
  import { tasksStore } from '$lib/stores/tasks.svelte';
  import { effectiveStatus, hasUnmetDeps, isParked, TASK_STATUSES, type TaskRow } from '$lib/tasks/model';
  import type { TaskStatus } from '$lib/tauri/types';
  import { fmtAge } from '$lib/overlord/format';
  import Tooltip from '$lib/components/Tooltip.svelte';

  /**
   * The board, indexed by WORKSTREAM (docs/tasks.md §4).
   *
   * The previous shape nested workspace → workstream → six lanes, which meant the DOM held
   * every board in the window at once and the human had to walk projects to reach a task.
   * At fleet scale (100+ tabs) that is the same navigation tax the task system exists to
   * remove, in kanban clothing.
   *
   * So: a workstream INDEX on the left, exactly ONE board on the right. The workstream is
   * the unit of attention — the named job someone is actually pushing forward — and the tab
   * an item happens to be assigned to drops to a chip on the card, because which terminal
   * is doing the work is not how anyone decides what to do next.
   *
   * Consequences worth keeping: only the selected stream's cards are rendered, the index is
   * one O(n) pass rather than lanes×streams filters, and moving work between jobs is a drag
   * onto its row in the index.
   */

  interface Props {
    /** Ticking clock from the deck — staleness is relative, so it has to re-evaluate. */
    now: number;
  }

  let { now }: Props = $props();

  const LANES = TASK_STATUSES;
  const STALE_DAYS = 3;
  /** Cards rendered per lane IN THE EVERYTHING VIEW ONLY, which at fleet scale would
   *  otherwise put thousands of nodes on screen that nobody reads. A single workstream is
   *  never capped: it is where the overflow message sends you, so it has to actually show
   *  the rest — a cap there makes those rows unreachable and the message a lie. */
  const LANE_CAP = 40;
  const EVERYTHING = '*';

  /** The Overlord workspace is excluded, so nothing here may ever WRITE to it — a task
   *  filed there is invisible to this board forever (see `createStream`). */
  const boardWorkspaces = $derived(workspacesStore.workspaces.filter((w) => !w.overlord));

  /** Whether "Send" has anywhere to send to — see the button's own note. */
  const hasAgent = $derived(overlordStore.hasAgentTab);

  /** Tasks bucketed by workspace — the one grouping both the index and the dependency
   *  scan below read, so a fleet-sized board walks the list once rather than per lane. */
  const grouped = $derived.by<Map<string, TaskRow[]>>(() => {
    const byWorkspace = new Map<string, TaskRow[]>();
    for (const t of overlordStore.tasks) {
      const list = byWorkspace.get(t.workspace_id);
      if (list) list.push(t);
      else byWorkspace.set(t.workspace_id, [t]);
    }
    return byWorkspace;
  });

  /** Ids whose lane is DERIVED from an unfinished prerequisite rather than stored.
   *  Those cards can't be stepped by hand — see the ‹ › controls. */
  const depBlocked = $derived.by<Set<string>>(() => {
    const out = new Set<string>();
    // Same parked set the lane assignment uses. Disagreeing with it re-enabled the ‹ ›
    // steppers on a card the board was showing as Blocked: four clicks walked the STORED
    // status to `done`, `effectiveStatus` short-circuits on done, and the card jumped to
    // Done — work marked finished that never started, with its prerequisite still parked.
    const parked = workspacesStore.parkedTaskIds;
    for (const list of grouped.values()) {
      for (const t of list) if (hasUnmetDeps(t, list, parked)) out.add(t.id);
    }
    return out;
  });

  // ── Index ───────────────────────────────────────────────────────────────────

  type Lanes = Record<TaskStatus, TaskRow[]>;

  const emptyLanes = (): Lanes => ({
    backlog: [], todo: [], active: [], blocked: [], review: [], done: [],
  });

  interface StreamEntry {
    key: string;
    wsId: string;
    wsName: string;
    suspended: boolean;
    streamId: string | null;
    /** null means the workspace's unfiled tasks — real work, just not named yet. */
    name: string | null;
    lanes: Lanes;
    open: number;
    parked: number;
    done: number;
    blocked: number;
    active: number;
    stale: number;
    total: number;
  }

  /**
   * One pass over every task in the window, bucketed by workspace + workstream.
   *
   * Lane membership uses the EFFECTIVE status so a task waiting on an unfinished
   * prerequisite appears under `blocked` without anyone restating it there. The tallies,
   * though, follow the STORED status — `open` has to mean the same thing here as it does
   * to the staleness sweep and the scan, which read `isInFlight`.
   */
  const index = $derived.by<StreamEntry[]>(() => {
    void now;
    const staleBefore = now - STALE_DAYS * 86_400_000;
    const out: StreamEntry[] = [];
    for (const ws of boardWorkspaces) {
      const all = grouped.get(ws.id);
      if (!all?.length) continue;
      const buckets = new Map<string, StreamEntry>();

      for (const t of all) {
        const streamId = t.workstream_id ?? null;
        const bucketKey = streamId ?? '';
        let e = buckets.get(bucketKey);
        if (!e) {
          e = {
            key: `${ws.id}|${bucketKey}`,
            wsId: ws.id,
            wsName: ws.name,
            suspended: !!ws.suspended,
            streamId,
            name: streamId ? (tasksStore.workstream(ws.id, streamId)?.name ?? 'Unnamed') : null,
            lanes: emptyLanes(),
            open: 0, parked: 0, done: 0, blocked: 0, active: 0, stale: 0, total: 0,
          };
          buckets.set(bucketKey, e);
        }
        const lane = effectiveStatus(t, all, workspacesStore.parkedTaskIds);
        e.lanes[lane].push(t);
        e.total++;
        if (t.status === 'done') {
          e.done++;
        } else if (isParked(t.status)) {
          e.parked++;
        } else {
          e.open++;
          if (lane === 'blocked') e.blocked++;
          if (lane === 'active') e.active++;
          if (Date.parse(t.updated_at) < staleBefore) e.stale++;
        }
      }

      // Named jobs lead — someone deliberately organized those. Unfiled trails.
      out.push(
        ...[...buckets.values()]
          .filter((e) => e.streamId)
          .sort((a, b) => (a.name ?? '').localeCompare(b.name ?? '')),
      );
      const loose = buckets.get('');
      if (loose) out.push(loose);
    }
    return out;
  });

  /** The cross-cutting view: every stream's lanes merged. One flat grid, not N nested ones. */
  const everything = $derived.by<StreamEntry>(() => {
    const e: StreamEntry = {
      key: EVERYTHING,
      wsId: '', wsName: '', suspended: false, streamId: null, name: null,
      lanes: emptyLanes(),
      open: 0, parked: 0, done: 0, blocked: 0, active: 0, stale: 0, total: 0,
    };
    for (const s of index) {
      for (const lane of LANES) e.lanes[lane].push(...s.lanes[lane]);
      e.open += s.open;
      e.parked += s.parked;
      e.done += s.done;
      e.blocked += s.blocked;
      e.active += s.active;
      e.stale += s.stale;
      e.total += s.total;
    }
    return e;
  });

  // ── Selection ───────────────────────────────────────────────────────────────

  let selected = $state<string>(EVERYTHING);
  try {
    selected = localStorage.getItem('overlord-board-stream') ?? EVERYTHING;
  } catch { /* first run / storage blocked */ }

  $effect(() => {
    try { localStorage.setItem('overlord-board-stream', selected); } catch { /* ignore */ }
  });

  /** Falls back to the everything view rather than a blank board when the remembered
   *  stream is gone — a workstream disappears the moment its last task does. */
  const current = $derived(
    selected === EVERYTHING ? everything : (index.find((e) => e.key === selected) ?? everything),
  );
  const isEverything = $derived(current.key === EVERYTHING);

  let query = $state('');
  const rail = $derived.by(() => {
    const q = query.trim().toLowerCase();
    if (!q) return index;
    return index.filter(
      (e) => (e.name ?? 'unfiled').toLowerCase().includes(q) || e.wsName.toLowerCase().includes(q),
    );
  });

  /** Arrow keys walk the index — the whole point is reaching a job without a mouse hunt. */
  function railKeydown(e: KeyboardEvent) {
    if (e.key !== 'ArrowDown' && e.key !== 'ArrowUp') return;
    const order = [EVERYTHING, ...rail.map((r) => r.key)];
    const at = order.indexOf(selected);
    const next = order[Math.min(order.length - 1, Math.max(0, at + (e.key === 'ArrowDown' ? 1 : -1)))];
    if (next) {
      selected = next;
      e.preventDefault();
    }
  }

  // ── Drag and drop ───────────────────────────────────────────────────────────
  //
  // Two drop surfaces, one gesture each: a LANE changes status, an index ROW changes which
  // job the task belongs to. The ‹ › buttons remain the pointer-free path.

  let dragging = $state<string | null>(null);
  let dragLane = $state<TaskStatus | null>(null);
  let dragRow = $state<string | null>(null);

  const draggingWs = $derived(
    dragging ? (overlordStore.tasks.find((t) => t.id === dragging)?.workspace_id ?? null) : null,
  );

  function endDrag() {
    dragging = null;
    dragLane = null;
    dragRow = null;
  }

  function onDragStart(e: DragEvent, taskId: string) {
    dragging = taskId;
    e.dataTransfer?.setData('text/plain', taskId);
    if (e.dataTransfer) e.dataTransfer.effectAllowed = 'move';
  }

  function dropped(e: DragEvent): TaskRow | null {
    e.preventDefault();
    const id = dragging ?? e.dataTransfer?.getData('text/plain');
    endDrag();
    return id ? (overlordStore.tasks.find((t) => t.id === id) ?? null) : null;
  }

  function dropOnLane(e: DragEvent, lane: TaskStatus) {
    const t = dropped(e);
    if (!t) return;
    // Compare against the EFFECTIVE status — the lane the card is actually rendered in.
    // Comparing the stored one meant dropping a dependency-blocked card back on Blocked
    // rewrote its stored status to 'blocked', so it never returned to Active when the
    // prerequisite finished: stuck in Blocked with nothing blocking it.
    const all = overlordStore.tasks.filter((x) => x.workspace_id === t.workspace_id);
    if (effectiveStatus(t, all, workspacesStore.parkedTaskIds) !== lane) tasksStore.setStatus(t.workspace_id, t.id, lane);
  }

  function dropOnStream(e: DragEvent, entry: StreamEntry) {
    const t = dropped(e);
    // Refuse a cross-workspace move rather than silently relocating a task between
    // projects: the lists persist per workspace, and the tab assignment travels with it.
    if (!t || t.workspace_id !== entry.wsId) return;
    if ((t.workstream_id ?? null) !== entry.streamId) {
      tasksStore.update(t.workspace_id, t.id, { workstream_id: entry.streamId });
    }
  }

  // ── Mutations ───────────────────────────────────────────────────────────────

  let openCard = $state<string | null>(null);
  let newTitle = $state('');

  function addTask() {
    const title = newTitle.trim();
    if (!title || isEverything) return;
    tasksStore.add(current.wsId, { title, origin: 'human', workstream_id: current.streamId });
    newTitle = '';
  }

  /** Step a card one lane.
   *
   *  Refused for a dependency-blocked card, whose lane is DERIVED rather than stored: it
   *  renders in Blocked no matter what, so stepping it wrote a new stored status and moved
   *  nothing on screen — the card sat still and the button read as broken, while the task
   *  quietly skipped a lane the moment its prerequisite finished. The buttons are disabled
   *  and say why instead; a drag still works, because that names a destination explicitly. */
  function moveTask(t: TaskRow, dir: 1 | -1) {
    if (depBlocked.has(t.id)) return;
    const at = LANES.indexOf(t.status);
    const next = LANES[Math.min(LANES.length - 1, Math.max(0, at + dir))];
    if (next !== t.status) tasksStore.setStatus(t.workspace_id, t.id, next);
  }

  /** Delete a card THROUGH the engine, not `tasksStore.remove`.
   *
   *  A task the human deletes has to reach whoever was carrying it, or the tab keeps
   *  believing in the work and puts the row back the next time it re-sends its list. The
   *  engine tells the tab directly when it can, and hands it to the Overlord agent to
   *  relay when it can't (`deleteTask`). */
  function dropTask(t: TaskRow) {
    void overlordStore.deleteTask(t.id);
  }

  /** Start a named job, seeded with its first task — a workstream with no tasks is dropped
   *  on persist, so the two have to be created together. */
  let creating = $state(false);
  let newName = $state('');
  let newFirst = $state('');
  let newWs = $state('');

  /** Which workspace a new job lands in.
   *
   *  MUST resolve to a board workspace. `activeWorkspaceId` is not one: reaching this board
   *  goes through the sidebar accessor, which activates the OVERLORD workspace — and that
   *  one is filtered out of `boardWorkspaces`, so a stream created there persists into a
   *  list this board can never render. It counted toward the Board badge, vanished from the
   *  rail, and resurfaced days later as a stale Triage card for a task with no visible home.
   */
  function defaultWorkspace(): string {
    if (current.wsId) return current.wsId;
    const active = workspacesStore.activeWorkspaceId;
    if (active && boardWorkspaces.some((w) => w.id === active)) return active;
    return boardWorkspaces[0]?.id ?? '';
  }

  function openCreate() {
    creating = true;
    newName = '';
    newFirst = '';
    newWs = defaultWorkspace();
  }

  function createStream() {
    const name = newName.trim();
    const title = newFirst.trim();
    // Never write to a workspace this board cannot show.
    if (!name || !title || !boardWorkspaces.some((w) => w.id === newWs)) return;
    const stream = tasksStore.ensureWorkstream(newWs, name);
    tasksStore.add(newWs, { title, origin: 'human', workstream_id: stream?.id ?? null });
    selected = `${newWs}|${stream?.id ?? ''}`;
    creating = false;
  }

  let renaming = $state(false);
  let renameText = $state('');

  function startRename() {
    if (isEverything || !current.streamId) return;
    renameText = current.name ?? '';
    renaming = true;
  }

  function commitRename() {
    const name = renameText.trim();
    if (name && current.streamId) tasksStore.renameWorkstream(current.wsId, current.streamId, name);
    renaming = false;
  }

  // ── Presentation ────────────────────────────────────────────────────────────

  const laneLabel = (l: TaskStatus) => (l === 'todo' ? 'to-do' : l);

  const PINNED_WHY =
    'Held here by an unfinished prerequisite — finish that task, or drag this one to choose where it lands.';

  function laneTone(l: TaskStatus): string {
    switch (l) {
      case 'backlog': return 'var(--ov-ink-dim)';
      case 'todo': return 'var(--ov-ink-mid)';
      case 'active': return 'var(--ov-live)';
      case 'blocked': return 'var(--ov-critical)';
      case 'review': return 'var(--ov-cool)';
      case 'done': return 'var(--ov-ok)';
    }
  }

  /** The index pip: worst-first, so a row's colour is the reason to look at it. */
  function pipTone(e: StreamEntry): string {
    if (e.stale) return 'var(--ov-critical)';
    if (e.blocked) return 'var(--ov-warn)';
    if (e.active) return 'var(--ov-live)';
    if (e.open) return 'var(--ov-ink-mid)';
    return 'var(--ov-ink-dim)';
  }

  /** Lane spread as proportional segments — the shape of a job at a glance: all-to-do
   *  reads differently from all-review, and neither needs a number to say so. */
  function spread(e: StreamEntry): { lane: TaskStatus; n: number }[] {
    return LANES.map((lane) => ({ lane, n: e.lanes[lane].length })).filter((s) => s.n > 0);
  }

  function streamOf(t: TaskRow): string | null {
    return tasksStore.workstream(t.workspace_id, t.workstream_id)?.name ?? null;
  }

  /** Everything the row can't fit, on one bubble — the name is clipped, and the staleness
   *  flag is a bare count that means nothing without its unit. */
  function rowTip(e: StreamEntry): string {
    const lead = e.name ? `${e.name} · ${e.wsName}` : `Unfiled tasks in ${e.wsName}`;
    return e.stale ? `${lead}\n${e.stale} untouched for days` : lead;
  }

  /** The meter is 3px tall — per-segment hovering is a fiction, so the whole bar carries
   *  the breakdown instead of six unhittable strips each carrying one number. */
  function meterTip(e: StreamEntry): string {
    return spread(e).map((s) => `${s.n} ${laneLabel(s.lane)}`).join(' · ');
  }
</script>

<!-- The query container must be an ANCESTOR: an element never matches its own
     container query, so `.board` carrying container-type could never restyle itself. -->
<div class="board-frame">
<div class="board">

  <!-- ══ Index ═════════════════════════════════════════════════════════════ -->
  <!-- svelte-ignore a11y_no_noninteractive_element_interactions -->
  <nav class="rail" onkeydown={railKeydown}>
    <div class="rail-search">
      <input
        class="ov-input"
        type="text"
        placeholder="Filter workstreams…"
        bind:value={query}
      />
    </div>

    <div class="rail-list">
      <button
        class="row row-all"
        class:on={selected === EVERYTHING}
        onclick={() => (selected = EVERYTHING)}
      >
        <span class="ov-dot" style:--tone={pipTone(everything)}></span>
        <span class="row-name">Everything</span>
        <span class="ov-mono row-count">{everything.open || '—'}</span>
        <div class="spread">
          {#each spread(everything) as s (s.lane)}
            <span style:flex={s.n} style:background={laneTone(s.lane)}></span>
          {/each}
        </div>
      </button>

      {#each rail as e, i (e.key)}
        {#if i === 0 || rail[i - 1].wsId !== e.wsId}
          <div class="rail-ws" class:dimmed={e.suspended}>
            <span class="ov-label">{e.wsName}</span>
            {#if e.suspended}<span class="ov-chip">suspended</span>{/if}
            <span class="rail-ws-rule"></span>
          </div>
        {/if}
        <!-- The staleness flag's own tooltip is folded into the row's, because two nested
             wrappers both fire on enter and neither hides the other — mouseenter doesn't
             bubble, so the outer bubble is already up when the inner one appears. One
             trigger per element; the bubble renders newlines. -->
        <!-- svelte-ignore a11y_no_static_element_interactions -->
        <Tooltip text={rowTip(e)} block>
          <button
            class="row"
            class:on={selected === e.key}
            class:dimmed={e.suspended}
            class:drop={dragRow === e.key && draggingWs === e.wsId}
            class:deny={dragRow === e.key && draggingWs !== null && draggingWs !== e.wsId}
            onclick={() => (selected = e.key)}
            ondragover={(ev) => { ev.preventDefault(); dragRow = e.key; }}
            ondragleave={() => { if (dragRow === e.key) dragRow = null; }}
            ondrop={(ev) => dropOnStream(ev, e)}
          >
            <span class="ov-dot" style:--tone={pipTone(e)}></span>
            <span class="row-name" class:loose={!e.name}>{e.name ?? 'Unfiled'}</span>
            {#if e.stale}<span class="row-flag">{e.stale}◆</span>{/if}
            <span class="ov-mono row-count">{e.open || '—'}</span>
            <div class="spread">
              {#each spread(e) as s (s.lane)}
                <span style:flex={s.n} style:background={laneTone(s.lane)}></span>
              {/each}
            </div>
          </button>
        </Tooltip>
      {/each}

      {#if index.length && !rail.length}
        <p class="rail-none">No workstream matches “{query}”.</p>
      {/if}
    </div>

    <div class="rail-foot">
      {#if creating}
        <div class="create">
          {#if boardWorkspaces.length > 1}
            <select class="ov-select" bind:value={newWs}>
              {#each boardWorkspaces as ws (ws.id)}
                <option value={ws.id}>{ws.name}</option>
              {/each}
            </select>
          {/if}
          <input class="ov-input" type="text" placeholder="Workstream name" bind:value={newName} />
          <input
            class="ov-input"
            type="text"
            placeholder="Its first task…"
            bind:value={newFirst}
            onkeydown={(e) => e.key === 'Enter' && createStream()}
          />
          {#if !newWs}
            <p class="create-none">No ordinary workspace to file this under — Overlord's own workspace can't hold board work.</p>
          {/if}
          <div class="create-actions">
            <button class="ov-btn ov-btn-primary" disabled={!newName.trim() || !newFirst.trim() || !newWs} onclick={createStream}>Create</button>
            <button class="ov-btn" onclick={() => (creating = false)}>Cancel</button>
          </div>
        </div>
      {:else}
        <button class="ov-btn rail-new" onclick={openCreate}>+ Workstream</button>
      {/if}
    </div>
  </nav>

  <!-- ══ The one board ═════════════════════════════════════════════════════ -->
  <section class="stage">
    {#if index.length === 0}
      <div class="empty ov-panel ov-bracket ov-in">
        <p class="ov-label-lead">No work on the board</p>
        <p class="empty-copy">
          Scan your running agent tabs to populate it — their tasks come across where they
          exist, and every other running tab gets a row you can fill in. Nothing is typed
          into any tab, and it's safe to run again any time.
        </p>
        <button class="ov-btn ov-btn-primary" onclick={() => overlordStore.scanWorkspaces()} disabled={overlordStore.scanning}>
          {overlordStore.scanning ? 'Scanning…' : 'Scan running tabs'}
        </button>
      </div>
    {:else}
      <header class="stage-head">
        <div class="stage-title">
          {#if renaming}
            <!-- svelte-ignore a11y_autofocus -->
            <input
              class="ov-input rename"
              type="text"
              autofocus
              bind:value={renameText}
              onblur={commitRename}
              onkeydown={(e) => {
                if (e.key === 'Enter') commitRename();
                if (e.key === 'Escape') renaming = false;
              }}
            />
          {:else}
            <Tooltip text={current.streamId ? 'Click to rename this workstream' : ''}>
              <button
                class="ov-label-lead stage-name"
                class:renameable={!isEverything && !!current.streamId}
                onclick={startRename}
              >
                {isEverything ? 'Everything' : (current.name ?? 'Unfiled')}
              </button>
            </Tooltip>
          {/if}
          {#if !isEverything}
            <span class="ov-chip">{current.wsName}</span>
          {:else}
            <span class="ov-chip">{index.length} workstream{index.length === 1 ? '' : 's'}</span>
          {/if}
        </div>

        <div class="tallies">
          <span class="tally"><b class="ov-mono">{current.open}</b><span class="ov-label">open</span></span>
          {#if current.blocked}<span class="tally"><b class="ov-mono" style:color="var(--ov-critical)">{current.blocked}</b><span class="ov-label">blocked</span></span>{/if}
          {#if current.stale}<span class="tally"><b class="ov-mono" style:color="var(--ov-warn)">{current.stale}</b><span class="ov-label">stale</span></span>{/if}
          {#if current.parked}<span class="tally"><b class="ov-mono">{current.parked}</b><span class="ov-label">parked</span></span>{/if}
          {#if current.done}<span class="tally"><b class="ov-mono" style:color="var(--ov-ok)">{current.done}</b><span class="ov-label">done</span></span>{/if}
        </div>
      </header>

      <Tooltip text={meterTip(current)} block>
        <div class="meter" aria-hidden="true">
          {#each spread(current) as s (s.lane)}
            <span style:flex={s.n} style:background={laneTone(s.lane)}></span>
          {/each}
        </div>
      </Tooltip>

      <div class="lanes">
        {#each LANES as lane, li (lane)}
          {@const cards = current.lanes[lane]}
          {@const shown = isEverything ? cards.slice(0, LANE_CAP) : cards}
          <!-- svelte-ignore a11y_no_static_element_interactions -->
          <div
            class="lane ov-in"
            class:parked={lane === 'backlog'}
            class:drop={dragLane === lane}
            style:--i={li}
            style:--lane={laneTone(lane)}
            ondragover={(e) => { e.preventDefault(); dragLane = lane; }}
            ondragleave={() => { if (dragLane === lane) dragLane = null; }}
            ondrop={(e) => dropOnLane(e, lane)}
          >
            <div class="lane-head">
              <span class="ov-label lane-name">{laneLabel(lane)}</span>
              <span class="ov-mono lane-count">{cards.length}</span>
            </div>

            <div class="lane-body">
              {#each shown as t (t.id)}
                {@const sentAt = overlordStore.taskHandoffAt(t.id)}
                <!-- The accent marks "an agent put this here", covering both a task created
                     over MCP ('agent') and one imported from a runtime's own list
                     ('imported'). Testing for 'agent' alone would miss every importer row. -->
                <div
                  class="card"
                  class:from-agent={t.origin === 'agent' || t.origin === 'imported'}
                  class:from-overlord={t.origin === 'overlord'}
                  class:dragging={dragging === t.id}
                  draggable="true"
                  ondragstart={(e) => onDragStart(e, t.id)}
                  ondragend={endDrag}
                >
                  <!-- Whose tab this is headlines the card as plain text, one line, clipped.
                       It was a chip button, which wrapped on long tab names and pushed the
                       controls around — and the whole point of the board is not having to
                       walk to the tab, so its name is context, not a destination. -->
                  <div class="card-head">
                    <Tooltip text={t.tab_id ? tabDisplayName(t.tab_id) : 'Not assigned to a tab'} block>
                      <span class="card-tab">
                        {t.tab_id ? tabDisplayName(t.tab_id) : 'unassigned'}
                      </span>
                    </Tooltip>
                    <span class="ov-mono card-age">{fmtAge(t.updated_at)}</span>
                    <Tooltip text="Delete">
                      <button class="tick tick-del" onclick={() => dropTask(t)}>×</button>
                    </Tooltip>
                  </div>

                  <Tooltip text={t.detail ? 'Click to read the description' : 'No description'} block>
                    <button
                      class="card-title"
                      onclick={() => (openCard = openCard === t.id ? null : t.id)}
                    >
                      {t.title}
                      {#if t.detail}<span class="has-detail" class:open={openCard === t.id}>▾</span>{/if}
                    </button>
                  </Tooltip>

                  {#if openCard === t.id}
                    <p class="card-detail">{t.detail || 'No description was recorded for this task.'}</p>
                  {/if}

                  {#if (isEverything && streamOf(t)) || depBlocked.has(t.id)}
                    <div class="card-meta">
                      {#if isEverything && streamOf(t)}
                        <button class="ov-chip card-stream" onclick={() => (selected = `${t.workspace_id}|${t.workstream_id ?? ''}`)}>
                          {streamOf(t)}
                        </button>
                      {/if}
                      {#if depBlocked.has(t.id)}
                        <Tooltip text="Waiting on an unfinished prerequisite. It moves on its own once that task is done.">
                          <span class="ov-chip card-dep">waiting</span>
                        </Tooltip>
                      {/if}
                    </div>
                  {/if}

                  <!-- Steppers pin to the edges they move toward; the two acts that leave the
                       board sit centred between them. -->
                  <div class="card-foot">
                    <Tooltip text={depBlocked.has(t.id) ? PINNED_WHY : 'Back'}>
                      <button class="tick"
                              disabled={depBlocked.has(t.id) || t.status === 'backlog'}
                              onclick={() => moveTask(t, -1)}>‹</button>
                    </Tooltip>
                    <span class="card-acts">
                      <Tooltip text={t.tab_id ? 'View Tab' : 'Not assigned to a tab'}>
                        <button class="act" disabled={!t.tab_id}
                                onclick={() => navigateToTab(t.tab_id!)}>View</button>
                      </Tooltip>
                      <!-- Disabled with no agent tab, rather than accepting a handoff
                           nothing will collect: a handoff is hidden from the deck and is
                           swept undelivered after 30 minutes, so a "Sent" receipt would
                           have been the only trace, and a false one. -->
                      <Tooltip text={!hasAgent
                                ? 'No Overlord agent tab in this window — start one in the Overlord workspace to hand work to it'
                                : sentAt === null
                                  ? 'Send to Overlord'
                                  : `Sent to Overlord ${fmtAge(new Date(sentAt).toISOString())} — click to send again`}>
                        <button class="act act-send" class:sent={sentAt !== null}
                                disabled={!hasAgent}
                                onclick={() => overlordStore.sendTaskToOverlord(t.id)}>
                          {sentAt === null ? 'Send' : 'Sent'}
                        </button>
                      </Tooltip>
                    </span>
                    <Tooltip text={depBlocked.has(t.id) ? PINNED_WHY : 'Forward'}>
                      <button class="tick"
                              disabled={depBlocked.has(t.id) || t.status === 'done'}
                              onclick={() => moveTask(t, 1)}>›</button>
                    </Tooltip>
                  </div>
                </div>
              {/each}

              {#if cards.length > shown.length}
                <!-- Only reachable in the everything view, where the named workstream is a
                     real destination that shows all of them. -->
                <p class="lane-more">
                  +{cards.length - shown.length} more · open the workstream to see
                  {cards.length - shown.length === 1 ? 'it' : 'them'}
                </p>
              {/if}
            </div>
          </div>
        {/each}
      </div>

      <footer class="stage-foot">
        {#if isEverything}
          <p class="foot-hint">
            Showing every workstream at once. Pick one on the left to add work to it —
            or drag any card onto a row there to move it between jobs.
          </p>
        {:else}
          <input
            class="ov-input add"
            type="text"
            placeholder="Add to {current.name ?? 'unfiled'}…"
            bind:value={newTitle}
            onkeydown={(e) => e.key === 'Enter' && addTask()}
          />
        {/if}
      </footer>
    {/if}
  </section>
</div>
</div>

<style>
  /* Container queries, not media queries: this board lives in a pane that can be half a
     window wide while the window itself is huge. The container has to be an ANCESTOR of
     everything it restyles — an element never matches its own container query, so putting
     `container-type` on `.board` left `.board`'s own rule permanently dead while its
     children reflowed around it. */
  .board-frame {
    container-type: inline-size;
    display: flex;
    flex-direction: column;
    flex: 1;
    min-height: 0;
    min-width: 0;
  }

  .board {
    display: flex;
    gap: 14px;
    align-items: stretch;
    min-height: 0;
    height: 100%;
  }

  /* ── Index rail ───────────────────────────────────────────────────────── */
  .rail {
    display: flex;
    flex-direction: column;
    gap: 8px;
    width: 236px;
    flex-shrink: 0;
    min-height: 0;
    border-right: 1px solid var(--ov-hair);
    padding-right: 12px;
  }

  .rail-search :global(.ov-input) { width: 100%; }

  .rail-list {
    display: flex;
    flex-direction: column;
    gap: 1px;
    overflow-y: auto;
    min-height: 0;
    flex: 1;
    margin-right: -4px;
    padding-right: 4px;
  }

  .rail-ws {
    display: flex;
    align-items: center;
    gap: 7px;
    padding: 13px 2px 5px;
  }
  .rail-ws.dimmed { opacity: 0.55; }
  .rail-ws-rule { flex: 1; height: 1px; background: var(--ov-hair); }
  .rail-ws:first-child { padding-top: 4px; }

  .row {
    display: grid;
    grid-template-columns: auto 1fr auto auto;
    grid-template-rows: auto auto;
    align-items: center;
    gap: 2px 7px;
    width: 100%;
    padding: 6px 8px 7px;
    border: 1px solid transparent;
    border-radius: 2px;
    text-align: left;
    cursor: pointer;
    transition: background 0.13s ease, border-color 0.13s ease;
  }
  .row:hover { background: color-mix(in srgb, var(--fg) 5%, transparent); }
  .row.on {
    background: color-mix(in srgb, var(--ov-live) 13%, transparent);
    border-color: color-mix(in srgb, var(--ov-live) 40%, transparent);
  }
  .row.dimmed { opacity: 0.55; }
  /* A drop that would cross projects is refused, so it must never look accepted. */
  .row.drop {
    background: color-mix(in srgb, var(--ov-live) 20%, transparent);
    border-color: var(--ov-live);
  }
  .row.deny {
    background: color-mix(in srgb, var(--ov-critical) 12%, transparent);
    border-color: color-mix(in srgb, var(--ov-critical) 45%, transparent);
    cursor: not-allowed;
  }

  .row-name {
    font-size: 0.87rem;
    font-weight: 500;
    color: var(--ov-ink);
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  /* Unfiled work is real work, just unorganized — dimmed, never hidden. */
  .row-name.loose { color: var(--ov-ink-dim); font-style: italic; }
  .row-all .row-name { letter-spacing: 0.04em; text-transform: uppercase; font-size: 0.8rem; font-weight: 600; }

  .row-flag {
    font-family: var(--ov-mono);
    font-size: 0.66rem;
    color: var(--ov-critical);
    letter-spacing: -0.03em;
  }
  .row-count { font-size: 0.78rem; color: var(--ov-ink-dim); }

  /* The signature detail: every row carries the shape of its own job. */
  .spread {
    grid-column: 1 / -1;
    display: flex;
    gap: 1px;
    height: 2px;
    margin-top: 3px;
    border-radius: 1px;
    overflow: hidden;
  }
  .spread span { display: block; opacity: 0.8; min-width: 2px; }
  .row.on .spread span { opacity: 1; }

  .rail-none {
    color: var(--ov-ink-dim);
    font-size: 0.82rem;
    line-height: 1.5;
    padding: 14px 2px;
  }

  .rail-foot { flex-shrink: 0; padding-top: 8px; border-top: 1px solid var(--ov-hair); }
  .rail-new { width: 100%; }
  .create { display: flex; flex-direction: column; gap: 5px; }
  .create :global(.ov-input),
  .create :global(.ov-select) { width: 100%; }
  .create-none { color: var(--ov-critical); font-size: 0.76rem; line-height: 1.45; }
  .create-actions { display: flex; gap: 5px; }
  .create-actions :global(.ov-btn) { flex: 1; }

  /* ── Stage ────────────────────────────────────────────────────────────── */
  .stage {
    flex: 1;
    min-width: 0;
    min-height: 0;
    display: flex;
    flex-direction: column;
    gap: 9px;
  }

  .stage-head {
    display: flex;
    align-items: baseline;
    gap: 14px;
    flex-wrap: wrap;
    flex-shrink: 0;
  }
  .stage-title { display: flex; align-items: baseline; gap: 9px; min-width: 0; }
  .stage-name {
    background: none;
    border: none;
    padding: 0;
    cursor: default;
    color: inherit;
    font: inherit;
    letter-spacing: inherit;
    text-transform: inherit;
    text-align: left;
  }
  .stage-name.renameable { cursor: text; }
  .stage-name.renameable:hover { color: var(--ov-live); }
  .rename { font-size: 1rem; min-width: 220px; }

  .tallies { display: flex; gap: 16px; margin-left: auto; }
  .tally { display: flex; align-items: baseline; gap: 5px; }
  .tally b { font-size: 1rem; font-weight: 500; color: var(--ov-ink); }

  /* Anything given a tooltip hands its layout to the wrapper — it, not the trigger, is
     the flex item the stage measures. */
  .stage > :global(.tooltip-wrapper) { flex-shrink: 0; }

  .meter {
    display: flex;
    flex: 1;
    gap: 1px;
    height: 3px;
    border-radius: 2px;
    overflow: hidden;
    flex-shrink: 0;
    background: var(--ov-hair);
  }
  .meter span { display: block; opacity: 0.85; transition: flex-grow 0.4s cubic-bezier(0.2, 0.7, 0.3, 1); }

  .empty { text-align: center; padding: 30px 24px; }
  .empty-copy {
    color: var(--ov-ink-dim);
    font-size: 0.92rem;
    line-height: 1.6;
    max-width: 54ch;
    margin: 9px auto 14px;
  }

  /* ── Lanes ────────────────────────────────────────────────────────────── */
  .lanes {
    flex: 1;
    min-height: 0;
    display: grid;
    /* One board on screen means the lanes finally get real width. A floor keeps them
       readable in a split pane; the grid scrolls sideways rather than crushing cards. */
    grid-template-columns: repeat(6, minmax(168px, 1fr));
    gap: 8px;
    overflow-x: auto;
  }

  .lane {
    display: flex;
    flex-direction: column;
    min-width: 0;
    min-height: 0;
    border-radius: 3px;
    border: 1px solid transparent;
    transition: background 0.12s ease, border-color 0.12s ease;
  }
  /* The parking lot reads as set-aside rather than as the first step of the flow. */
  .lane.parked { opacity: 0.7; }
  .lane.parked:hover { opacity: 1; }
  .lane.drop {
    background: color-mix(in srgb, var(--ov-live) 8%, transparent);
    border-color: color-mix(in srgb, var(--ov-live) 55%, transparent);
    opacity: 1;
  }

  .lane-head {
    display: flex;
    align-items: baseline;
    justify-content: space-between;
    gap: 6px;
    padding: 0 3px 5px;
    margin-bottom: 6px;
    border-bottom: 1px solid var(--ov-hair);
    flex-shrink: 0;
    /* Each lane owns a colour, and it is the same colour the index spread uses. */
    box-shadow: inset 0 -1px 0 var(--lane);
  }
  .lane-name { color: var(--lane); }
  .lane-count { font-size: 0.75rem; color: var(--ov-ink-dim); }

  .lane-body {
    flex: 1;
    min-height: 0;
    overflow-y: auto;
    padding: 0 2px 4px;
    margin: 0 -2px;
  }

  .lane-more {
    color: var(--ov-ink-dim);
    font-size: 0.74rem;
    line-height: 1.4;
    padding: 5px 3px 2px;
  }

  /* ── Cards ────────────────────────────────────────────────────────────── */
  .card {
    background: var(--ov-panel);
    border: 1px solid var(--ov-hair);
    border-left: 2px solid var(--ov-hair-strong);
    border-radius: 2px;
    padding: 7px 9px;
    margin-bottom: 6px;
    cursor: grab;
    transition: border-color 0.14s ease, background 0.14s ease;
  }
  .card:hover { background: var(--ov-panel-lift); }
  .card:active { cursor: grabbing; }
  .card.dragging { opacity: 0.4; }
  .card.from-agent { border-left-color: var(--ov-live); }
  .card.from-overlord { border-left-color: var(--ov-note); }

  /* Headline: who owns it, how old, and the one control that removes it. */
  .card-head {
    display: flex;
    align-items: center;
    gap: 6px;
    margin-bottom: 3px;
  }
  .card-tab {
    flex: 1;
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    font-family: var(--ov-face);
    font-size: 0.68rem;
    font-weight: 600;
    letter-spacing: 0.07em;
    text-transform: uppercase;
    color: var(--ov-ink-mid);
  }

  .card-title {
    background: none;
    border: none;
    color: inherit;
    display: block;
    font: inherit;
    font-size: 0.86rem;
    line-height: 1.4;
    padding: 0;
    text-align: left;
    width: 100%;
    word-break: break-word;
    cursor: pointer;
  }
  .has-detail {
    color: var(--ov-ink-dim);
    display: inline-block;
    font-size: 0.7rem;
    margin-left: 3px;
    transition: transform 0.14s ease;
  }
  .has-detail.open { transform: rotate(180deg); }

  .card-detail {
    border-top: 1px solid var(--ov-hair);
    color: var(--ov-ink-dim);
    font-size: 0.78rem;
    line-height: 1.5;
    margin: 6px 0 0;
    padding-top: 6px;
    white-space: pre-wrap;
    word-break: break-word;
  }

  .card-meta {
    display: flex;
    align-items: center;
    gap: 5px;
    margin-top: 6px;
    flex-wrap: wrap;
  }

  /* Steppers hug the edges they move toward; the acts that take the task off this board
     sit centred between them, so neither can be hit by accident reaching for the other. */
  .card-foot {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 5px;
    margin-top: 6px;
  }
  .card-acts { display: flex; gap: 4px; min-width: 0; }
  .act {
    font-family: var(--ov-face);
    font-size: 0.66rem;
    font-weight: 600;
    letter-spacing: 0.07em;
    text-transform: uppercase;
    height: 18px;
    padding: 0 8px;
    border-radius: 2px;
    border: 1px solid var(--ov-hair);
    background: transparent;
    color: var(--ov-ink-mid);
    cursor: pointer;
    white-space: nowrap;
    transition: color 0.12s ease, border-color 0.12s ease, background 0.12s ease;
  }
  .act:hover {
    color: var(--ov-ink);
    border-color: color-mix(in srgb, var(--ov-live) 60%, transparent);
    background: color-mix(in srgb, var(--ov-live) 10%, transparent);
  }
  .act:disabled { opacity: 0.3; cursor: default; }
  .act:disabled:hover { color: var(--ov-ink-mid); border-color: var(--ov-hair); background: transparent; }
  .act-send.sent {
    border-color: color-mix(in srgb, var(--ov-note) 45%, transparent);
    color: color-mix(in srgb, var(--ov-note) 85%, var(--fg));
  }
  .card-stream {
    cursor: pointer;
    max-width: 100%;
    overflow: hidden;
    text-overflow: ellipsis;
    display: inline-block;
    line-height: 17px;
  }
  .card-stream:hover { color: var(--ov-ink); border-color: var(--ov-hair-strong); }
  .card-age { font-size: 0.7rem; color: var(--ov-ink-dim); flex-shrink: 0; }
  /* The lane this card sits in is derived, not chosen — say so, since its steppers are off. */
  .card-dep {
    border-color: color-mix(in srgb, var(--ov-critical) 35%, transparent);
    color: color-mix(in srgb, var(--ov-critical) 80%, var(--fg));
  }
  .tick {
    color: var(--ov-ink-dim);
    font-size: 0.95rem;
    line-height: 1;
    padding: 0 3px;
    border-radius: 2px;
    transition: color 0.12s ease;
  }
  .tick:hover { color: var(--ov-live); }
  .tick:disabled { opacity: 0.2; cursor: default; }
  .tick:disabled:hover { color: var(--ov-ink-dim); }
  .tick-del:hover { color: var(--ov-critical); }

  /* ── Footer ───────────────────────────────────────────────────────────── */
  .stage-foot { flex-shrink: 0; }
  .add { width: 100%; }
  .foot-hint {
    color: var(--ov-ink-dim);
    font-size: 0.8rem;
    line-height: 1.5;
  }

  /* ── Narrow pane: the index becomes a strip, not a column ─────────────── */
  @container (max-width: 720px) {
    .board { flex-direction: column; gap: 10px; }
    .rail {
      width: auto;
      border-right: none;
      border-bottom: 1px solid var(--ov-hair);
      padding-right: 0;
      padding-bottom: 9px;
      flex-direction: row;
      align-items: center;
      gap: 8px;
    }
    .rail-search { flex-shrink: 0; width: 150px; }
    .rail-list {
      flex-direction: row;
      align-items: center;
      gap: 5px;
      overflow-x: auto;
      overflow-y: hidden;
      padding-right: 0;
      margin-right: 0;
    }
    .rail-ws { display: none; }
    /* Rows are wrapped for their tooltip, so the strip's sizing belongs on the wrapper. */
    .rail-list :global(.tooltip-wrapper) { width: auto; flex-shrink: 0; }
    .row {
      grid-template-columns: auto auto auto;
      grid-template-rows: auto auto;
      width: auto;
      flex-shrink: 0;
      border-color: var(--ov-hair);
      padding: 5px 9px 6px;
    }
    .row-name { max-width: 150px; }
    .rail-foot { border-top: none; padding-top: 0; flex-shrink: 0; }
    .create { flex-direction: row; align-items: center; }
    .create :global(.ov-input) { width: 140px; }
    .stage-foot .add { width: 100%; }
  }
</style>
