<!--
  A Hive workspace's main area (docs/hive.md §4.1): the header, the coordinator's column and
  the board. In place of the split tree, never over it: a Hive has no visible terminal.

  The coordinator's column is its transcript (ChatColumn), never its terminal: dragging the
  column's width touches no PTY, and a width change at a streaming Claude duplicates its
  scrollback (principle 3).

  The board is static cards in waves (§4.3, §4.4). It reads no transcript: Rust's worker records
  are fetched when this window hears `hive-workers-changed` or its tasks change, and the rest
  (lane, blocker, note, agent state) comes live from the stores. Worker columns, the activity
  line, Pause, Gallery and Memory arrive with later steps.
-->
<script lang="ts">
  import { onMount, untrack } from 'svelte';
  import { getCurrentWindow } from '@tauri-apps/api/window';
  import { hiveLiveWorkerTabs, hiveWorkers } from '$lib/tauri/commands';
  import type { HiveBoard } from '$lib/tauri/types';
  import { error as logError } from '@tauri-apps/plugin-log';
  import { countedListen } from '$lib/utils/listenCounter';
  import { workspacesStore } from '$lib/stores/workspaces.svelte';
  import { tasksStore } from '$lib/stores/tasks.svelte';
  import { terminalsStore } from '$lib/stores/terminals.svelte';
  import { agentStateStore } from '$lib/stores/agentState.svelte';
  import ChatColumn from '$lib/components/loom/ChatColumn.svelte';
  import WorkerCard from './WorkerCard.svelte';
  import { dispatchHiveOpenColumn, HIVE_OPEN_COLUMN, takePendingHiveOpen, type HiveOpenColumnDetail } from '$lib/hive/model';
  import { cardAge, cardState, latestNote, sortCards, STATE_LABEL, STATE_ORDER, waves, type CardStateName } from '$lib/hive/cards';

  interface Props {
    workspaceId: string;
  }
  let { workspaceId }: Props = $props();

  const ws = $derived(workspacesStore.workspaces.find((w) => w.id === workspaceId));
  const hive = $derived(ws?.hive ?? null);
  const tabs = $derived(ws?.panes.flatMap((p) => p.tabs) ?? []);
  const coordinator = $derived(tabs.find((t) => t.hive_role === 'coordinator') ?? null);
  const coordId = $derived(coordinator?.id ?? null);
  const source = $derived(coordId ? { kind: 'tab' as const, tabId: coordId } : null);
  const tasks = $derived(tasksStore.forWorkspace(workspaceId));

  // ── The board: Rust's worker records, refetched on its event and on task changes (§4.4). ──
  let board = $state<HiveBoard | null>(null);
  let fetchSeq = 0;
  let fetchTimer: ReturnType<typeof setTimeout> | null = null;
  /** Coalesced: a spawn writes the task list and emits in one breath. */
  function refetch() {
    if (fetchTimer) clearTimeout(fetchTimer);
    fetchTimer = setTimeout(() => {
      fetchTimer = null;
      const id = workspaceId;
      const mine = ++fetchSeq;
      hiveWorkers(id)
        .then((b) => { if (mine === fetchSeq && id === workspaceId) board = b; })
        .catch((e) => logError(`hive: board of ${id.slice(0, 8)} not fetched: ${e}`));
    }, 120);
  }
  // A task's lane moving can launch a waiter (Rust polls them after every task write).
  const taskSig = $derived(tasks.map((t) => `${t.id}:${t.status}:${t.updated_at}`).join('|'));
  // A reload gives a worker a new tab id and changes neither its lane nor the Hive: Rust tells us,
  // and this catches the event a webview reload or a lost emit would miss.
  const workerTabSig = $derived(tabs.filter((t) => t.hive_role === 'worker').map((t) => t.id).join('|'));
  $effect(() => {
    void workspaceId;
    void taskSig;
    void workerTabSig;
    void hive?.status;
    untrack(refetch);
  });
  // The headroom changes with no event when the breaker ends or a spawn ages out of the hour or
  // day: Rust says when, and the board refetches then. A clock, not a poll (§4.4).
  $effect(() => {
    const at = board?.refresh_at;
    if (!at) return;
    // setTimeout's delay is a signed 32-bit count of ms; past it, it fires at once.
    const t = setTimeout(() => untrack(refetch), Math.min(Math.max(0, at - Date.now()) + 500, 2 ** 31 - 1));
    return () => clearTimeout(t);
  });

  // Ages tick by the minute; a clock, not a poll.
  let now = $state(Date.now());

  const cards = $derived.by(() => {
    const list = board?.workers ?? [];
    return list.map((worker) => {
      const tab = worker.tab_id ? tabs.find((t) => t.id === worker.tab_id) ?? null : null;
      const task = tasks.find((t) => t.id === worker.task_id) ?? null;
      const agent = tab ? agentStateStore.getState(tab.id) ?? null : null;
      const suspended = !!tab && (!!tab.suspended_at || !!ws?.suspended);
      return {
        worker,
        state: cardState(worker, task, agent, tab ? { suspended } : null),
        note: latestNote(task),
        runtime: tab?.runtime ?? 'claude',
      };
    });
  });
  const columns = $derived(waves(sortCards(cards)));
  const counts = $derived.by(() => {
    const n = new Map<CardStateName, number>();
    for (const c of cards) n.set(c.state.state, (n.get(c.state.state) ?? 0) + 1);
    return [...n].sort((a, b) => STATE_ORDER[a[0]] - STATE_ORDER[b[0]]).map(([s, k]) => `${k} ${STATE_LABEL[s].toLowerCase()}`);
  });
  const workers = $derived(board?.workers.length ?? 0);
  const room = $derived(board?.headroom ?? null);

  // ── The coordinator column's width: 380–560 px, kept per viewer (§4.1). ──
  const WIDTH_KEY = 'hive.coordinator.width';
  const MIN_W = 380;
  const MAX_W = 560;
  const clampW = (n: number) => Math.min(MAX_W, Math.max(MIN_W, Math.round(n)));
  function savedWidth(): number {
    try {
      const n = Number(localStorage.getItem(WIDTH_KEY));
      return n ? clampW(n) : 440;
    } catch {
      return 440;
    }
  }
  let width = $state(savedWidth());
  let dragging = $state(false);
  function startResize(e: PointerEvent) {
    e.preventDefault();
    const startX = e.clientX;
    const startW = width;
    const el = e.currentTarget as HTMLElement;
    el.setPointerCapture(e.pointerId);
    dragging = true;
    const move = (ev: PointerEvent) => { width = clampW(startW + ev.clientX - startX); };
    const end = () => {
      dragging = false;
      el.removeEventListener('pointermove', move);
      el.removeEventListener('pointerup', end);
      el.removeEventListener('pointercancel', end);
      try { localStorage.setItem(WIDTH_KEY, String(width)); } catch { /* per-viewer nicety only */ }
    };
    el.addEventListener('pointermove', move);
    el.addEventListener('pointerup', end);
    el.addEventListener('pointercancel', end);
  }
  function onResizerKey(e: KeyboardEvent) {
    const step = e.shiftKey ? 40 : 10;
    if (e.key === 'ArrowLeft') width = clampW(width - step);
    else if (e.key === 'ArrowRight') width = clampW(width + step);
    else return;
    e.preventDefault();
    try { localStorage.setItem(WIDTH_KEY, String(width)); } catch { /* per-viewer nicety only */ }
  }

  // The coordinator runs while its Hive is on screen: a hidden tab is never a pane's active tab,
  // so nothing else mounts it after a restart. Its mount types the launch builder's line.
  $effect(() => {
    const id = coordId;
    if (id && !terminalsStore.get(id)) window.dispatchEvent(new CustomEvent('activate-tab', { detail: id }));
  });

  // So do its live workers (after a restart, or a reopen): Rust names the tabs whose worker is in
  // a live phase. A stopped or failed one stays down until its coordinator restarts it.
  $effect(() => {
    const id = workspaceId;
    untrack(() => {
      hiveLiveWorkerTabs(id)
        .then((tabs) => {
          for (const t of tabs) if (!terminalsStore.get(t)) window.dispatchEvent(new CustomEvent('activate-tab', { detail: t }));
        })
        .catch((e) => logError(`hive: live workers of ${id.slice(0, 8)} not mounted: ${e}`));
    });
  });

  // ── "Open this Hive agent" (navigateToTab, the waiting cards, a card's click): the
  // coordinator's column, or a worker's card. Worker columns arrive with build step 5. ──
  let column = $state<ChatColumn | undefined>();
  let boardEl = $state<HTMLElement | undefined>();
  let flash = $state(false);
  let focusedTask = $state<string | null>(null);
  /** A worker asked for before its card was on the board (a fresh mount, a spawn the board hasn't
   *  refetched yet): applied when it arrives, if that's soon. */
  let pendingWorker: { detail: HiveOpenColumnDetail; at: number } | null = null;
  let flashTimer: ReturnType<typeof setTimeout> | null = null;
  function open(detail: HiveOpenColumnDetail) {
    if (detail.workspaceId !== workspaceId) return;
    if (detail.tabId && detail.tabId === coordId) {
      column?.focusComposer();
      flash = true;
      setTimeout(() => (flash = false), 900);
      return;
    }
    const w = board?.workers.find((c) => (detail.taskId && c.task_id === detail.taskId) || (detail.tabId && c.tab_id === detail.tabId));
    if (!w) {
      if (pendingWorker?.detail !== detail) pendingWorker = { detail, at: Date.now() };
      return;
    }
    pendingWorker = null;
    focusedTask = w.task_id;
    if (flashTimer) clearTimeout(flashTimer);
    flashTimer = setTimeout(() => (focusedTask = null), 1500);
    requestAnimationFrame(() => {
      const el = boardEl?.querySelector<HTMLElement>(`[data-hive-task="${CSS.escape(w.task_id)}"]`);
      el?.scrollIntoView({ block: 'nearest', inline: 'nearest' });
      el?.focus({ preventScroll: true });
    });
  }
  $effect(() => {
    if (!board || !pendingWorker) return;
    const p = pendingWorker;
    if (Date.now() - p.at > 5000) pendingWorker = null;
    else untrack(() => open(p.detail));
  });
  function openCard(taskId: string, tabId: string | null | undefined) {
    dispatchHiveOpenColumn({ workspaceId, tabId: tabId ?? '', taskId });
  }
  onMount(() => {
    const pending = takePendingHiveOpen(workspaceId);
    if (pending) requestAnimationFrame(() => open(pending));
    const onOpen = (e: Event) => {
      takePendingHiveOpen(workspaceId);
      open((e as CustomEvent<HiveOpenColumnDetail>).detail);
    };
    window.addEventListener(HIVE_OPEN_COLUMN, onOpen);
    // Sent to this window only (`emit_to`), so another window's Hive never refetches here.
    let unlisten: (() => void) | undefined;
    let gone = false;
    countedListen<{ workspace_id: string }>('hive-workers-changed', (e) => {
      if (e.payload.workspace_id === workspaceId) refetch();
    }, { target: getCurrentWindow().label })
      .then((u) => { if (gone) u(); else unlisten = u; })
      .catch((e) => logError(`hive: workers-changed listener: ${e}`));
    const clock = setInterval(() => (now = Date.now()), 60_000);
    return () => {
      gone = true;
      unlisten?.();
      clearInterval(clock);
      if (fetchTimer) clearTimeout(fetchTimer);
      if (flashTimer) clearTimeout(flashTimer);
      window.removeEventListener(HIVE_OPEN_COLUMN, onOpen);
    };
  });
</script>

<div class="hive">
  <header class="hive-h">
    <div class="title">
      <b>{ws?.name ?? ''}</b>
      {#if hive?.goal}<span class="goal">{hive.goal}</span>{/if}
    </div>
    <div class="facts">
      <span>
        {workers === 0 ? '0 workers' : `${workers} worker${workers === 1 ? '' : 's'}`}{#if counts.length}: {counts.join(' · ')}{/if}
      </span>
      <span>
        {room?.live ?? 0} of {room?.cap ?? hive?.cap ?? 0} slots in use{#if room?.starting} · {room.starting} starting{/if}{#if room}
          · {room.spawns_left_this_hour} starts left this hour, {room.spawns_left_today} today{/if}
      </span>
      {#if room?.usage_limit_until}
        <span class="warn">No new workers until {room.usage_limit_until}: usage limit</span>
      {:else if board && board.hive_status !== 'active'}
        <span class="warn">{board.hive_status === 'paused' ? 'Paused: no new workers start' : `Hive is ${board.hive_status}`}</span>
      {/if}
      <span>{hive?.landing_mode === 'integration' ? `Lands to hive/${hive.slug}, each with your approval` : 'Lands to Base'}</span>
    </div>
  </header>

  <div class="cols">
    <section class="coord" class:flash class:dragging style="width: {width}px" aria-label="Coordinator">
      <ChatColumn
        bind:this={column}
        {source}
        name="Coordinator"
        workspace={ws?.name ?? ''}
        {tasks}
        active={true}
        actions={null}
        emptyText="This Hive has no coordinator tab."
      >
        {#snippet header()}
          <span class="role">coordinator</span>
        {/snippet}
      </ChatColumn>
    </section>
    <!-- A focusable separator is a control (ARIA), which the a11y lint doesn't know. -->
    <!-- svelte-ignore a11y_no_noninteractive_tabindex, a11y_no_noninteractive_element_interactions -->
    <div
      class="resizer"
      class:dragging
      role="separator"
      aria-orientation="vertical"
      aria-label="Resize the coordinator column"
      aria-valuemin={MIN_W}
      aria-valuemax={MAX_W}
      aria-valuenow={width}
      tabindex="0"
      onpointerdown={startResize}
      onkeydown={onResizerKey}
    ></div>
    <section class="board" aria-label="Board" bind:this={boardEl}>
      {#if columns.length}
        <div class="waves">
          {#each columns as wave, i (i)}
            <div class="wave" role="list" aria-label="Wave {i + 1}">
              {#if columns.length > 1}<h3>Wave {i + 1}</h3>{/if}
              {#each wave as c (c.worker.task_id)}
                <div role="listitem">
                  <WorkerCard
                    card={c.worker}
                    state={c.state}
                    note={c.note}
                    runtime={c.runtime}
                    age={cardAge(c.worker, now)}
                    focused={focusedTask === c.worker.task_id}
                    onopen={() => openCard(c.worker.task_id, c.worker.tab_id)}
                  />
                </div>
              {/each}
            </div>
          {/each}
        </div>
      {:else}
        <div class="empty">
          <b>No workers yet</b>
          <span>Workers the coordinator starts show up here as cards, one per task.</span>
        </div>
      {/if}
    </section>
  </div>
</div>

<style>
  .hive { flex: 1; display: flex; flex-direction: column; min-width: 0; min-height: 0; background: var(--bg-dark); }
  .hive-h { display: flex; flex-wrap: wrap; align-items: baseline; gap: 4px 18px; padding: 10px 18px; border-bottom: 1px solid var(--bg-light); }
  .title { display: flex; flex-direction: column; gap: 2px; min-width: 0; flex: 1 1 260px; }
  .title b { font-size: 14px; overflow-wrap: anywhere; }
  .goal { font-size: 12px; color: var(--fg-dim); overflow-wrap: anywhere; }
  .facts { display: flex; flex-direction: column; gap: 2px; font-size: 11px; color: var(--fg-dim); align-items: flex-end; text-align: right; }
  .cols { flex: 1; min-height: 0; display: flex; min-width: 0; overflow: hidden; }
  .coord { flex: none; display: flex; flex-direction: column; min-height: 0; min-width: 0; max-width: calc(100% - 264px); transition: box-shadow 0.3s; }
  .coord > :global(.convo) { flex: 1; }
  .coord.flash { box-shadow: inset 0 0 0 1px var(--accent); }
  .coord.dragging { user-select: none; }
  .resizer { flex: none; width: 4px; cursor: col-resize; background: var(--bg-light); opacity: 0.5; }
  .resizer:hover, .resizer.dragging, .resizer:focus-visible { opacity: 1; background: var(--accent); outline: none; }
  .board { flex: 1 1 260px; min-width: 260px; min-height: 0; overflow-y: auto; display: flex; }
  /* Waves sit side by side and wrap under each other when the board is narrow: the main area
     never scrolls sideways (§4.1). */
  .waves { flex: 1; display: flex; flex-wrap: wrap; align-content: flex-start; gap: 16px; padding: 14px 16px; min-width: 0; }
  .wave { flex: 1 1 220px; max-width: 340px; min-width: 0; display: flex; flex-direction: column; gap: 8px; }
  .wave h3 { margin: 0; font-size: 10.5px; font-weight: 500; letter-spacing: 0.08em; text-transform: uppercase; color: var(--fg-dim); }
  .warn { color: var(--yellow); }
  .empty { margin: auto; display: flex; flex-direction: column; align-items: center; gap: 4px; padding: 24px; text-align: center; max-width: 320px; }
  .empty b { font-size: 13px; color: var(--fg); }
  .empty span { font-size: 12px; color: var(--fg-dim); overflow-wrap: anywhere; }
  .role { font-size: 10px; text-transform: uppercase; letter-spacing: 0.05em; color: var(--accent); border: 1px solid color-mix(in srgb, var(--accent) 45%, transparent); border-radius: 3px; padding: 0 4px; }
  @media (prefers-reduced-motion: reduce) { .coord { transition: none; } }
</style>
