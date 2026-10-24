<!--
  A Hive workspace's main area (docs/hive.md §4.1): the header, the coordinator's column and
  the board. In place of the split tree, never over it: a Hive's terminals show only under their
  own column's chat, when the human asks (Show terminal).

  The coordinator's column is its transcript (ChatColumn), never its terminal: dragging the
  column's width touches no PTY, and a width change at a streaming Claude duplicates its
  scrollback (principle 3).

  The board is static cards in waves (§4.3, §4.4), with dependency strings between them. It reads
  no transcript: Rust's worker records are fetched when this window hears `hive-workers-changed`
  or its tasks change, diff stats and meta are pushed (hiveFeed), and the rest (lane, blocker,
  note, agent state and the activity line) comes live from the stores. Pause, Gallery and Memory
  arrive with later steps.

  Worker columns (§4.1): opening one turns the board into a rail; at most two show, and on a
  narrow window one gives way, then the coordinator goes to its strip (`hive/columns.ts`). Which
  are open and which has focus is `hiveView`'s, per workspace; the focused column's tab is what
  the Files column follows here. Every column is a transcript, so none of this touches a PTY.

  Show terminal (§4.5) adds a column's raw terminal under its chat (HiveTerminal). The panes are
  `fixedSize`, so that touches no PTY either; this view tells `+page` which ones are on screen
  (`hiveView.setTerminalsOnScreen`), which is all that makes Rust render them.
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
  import WorkerColumn from './WorkerColumn.svelte';
  import HiveTerminal from './HiveTerminal.svelte';
  import Icon from '$lib/components/Icon.svelte';
  import IconButton from '$lib/components/ui/IconButton.svelte';
  import Tooltip from '$lib/components/Tooltip.svelte';
  import { overlordStore } from '$lib/stores/overlord.svelte';
  import { hiveFeed } from '$lib/stores/hiveFeed.svelte';
  import { hiveView } from '$lib/stores/hiveView.svelte';
  import { toastStore } from '$lib/stores/toasts.svelte';
  import { clampCoord, clampRail, fitColumns, W } from '$lib/hive/columns';
  import { dispatchHiveOpenColumn, HIVE_OPEN_COLUMN, takePendingHiveOpen, type HiveOpenColumnDetail } from '$lib/hive/model';
  import { cardAge, cardState, latestNote, onScreenAgent, sortCards, STATE_LABEL, STATE_ORDER, waves, type CardStateName } from '$lib/hive/cards';
  import { activityLine, contextGauge, formatDiffStat } from '$lib/hive/activity';
  import { strings, type Box, type HiveString } from '$lib/hive/strings';

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

  // Ages tick on hiveFeed's 30 s render clock; a clock, not a poll.
  const now = $derived(hiveFeed.now);

  const cards = $derived.by(() => {
    const list = board?.workers ?? [];
    const warnAt = overlordStore.checkpointThreshold;
    return list.map((worker) => {
      const tab = worker.tab_id ? tabs.find((t) => t.id === worker.tab_id) ?? null : null;
      const task = tasks.find((t) => t.id === worker.task_id) ?? null;
      const agent = tab ? agentStateStore.getState(tab.id) ?? null : null;
      const tabFields = tab ? { suspended: !!tab.suspended_at || !!ws?.suspended } : null;
      const meta = hiveFeed.meta(tab?.id);
      const state = cardState(worker, task, agent, tabFields);
      return {
        worker,
        state,
        note: latestNote(task),
        runtime: tab?.runtime ?? 'claude',
        activity: activityLine(onScreenAgent(worker.phase, agent, tabFields) ?? undefined, now, hiveFeed.sticky(tab?.id)),
        diff: formatDiffStat(hiveFeed.diffStat(worker.task_id)),
        model: meta?.model || worker.model,
        ctx: contextGauge(meta, warnAt),
        node: {
          taskId: worker.task_id,
          dependsOn: worker.depends_on,
          active: state.state === 'working',
          waiting: worker.phase === 'waiting',
          done: task?.status === 'done',
        },
      };
    });
  });
  const columns = $derived(waves(sortCards(cards)));
  const cardByTask = $derived(new Map(cards.map((c) => [c.worker.task_id, c])));

  // ── The coordinator's width (380–560 px) and the rail's (200–240 px), kept per viewer (§4.1).
  // Worker columns share what's left (flex, no drag of their own). ──
  const WIDTH_KEY = 'hive.coordinator.width';
  const RAIL_KEY = 'hive.rail.width';
  function saved(key: string, clamp: (n: number) => number, fallback: number): number {
    try {
      const n = Number(localStorage.getItem(key));
      return n ? clamp(n) : fallback;
    } catch {
      return fallback;
    }
  }
  const keep = (key: string, n: number) => {
    try { localStorage.setItem(key, String(n)); } catch { /* per-viewer nicety only */ }
  };
  let width = $state(saved(WIDTH_KEY, clampCoord, W.COORD_DEFAULT));
  let railW = $state(saved(RAIL_KEY, clampRail, W.RAIL_DEFAULT));

  // ── The layout (§4.1): what's open is the view store's, what fits is measured here. ──
  const view = $derived(hiveView.view(workspaceId));
  let colsEl = $state<HTMLElement | undefined>();
  /** `.cols`' width: the window less the sidebar and an open Files column. Never a terminal's. */
  let mainW = $state(0);
  $effect(() => {
    const el = colsEl;
    if (!el) return;
    mainW = el.clientWidth;
    const ro = new ResizeObserver((entries) => { mainW = entries[0]?.contentRect.width ?? el.clientWidth; });
    ro.observe(el);
    return () => ro.disconnect();
  });
  // A column opened before the board arrived (a restart's saved view) waits for its card.
  const openCols = $derived(board ? view.open.filter((c) => cardByTask.has(c.taskId)) : []);
  const fit = $derived(fitColumns({
    main: mainW > 0 ? mainW : Number.POSITIVE_INFINITY,
    coordW: width,
    railW,
    coordCollapsed: view.coordCollapsed,
    open: openCols,
    focused: view.focused,
    coordFocusedAt: view.coordFocusedAt,
  }));
  /** 'rail' while a worker column is open (§4.1): the same cards, compact and sorted, no waves. */
  const boardMode = $derived(fit.board);
  const railCards = $derived(boardMode === 'rail' ? sortCards(cards) : []);
  const focusedKey = $derived(view.focused);

  // ── Raw terminals (§4.5): the ones whose column the layout shows are on screen. A column given
  // way keeps its slot mounted but hidden, and Rust stops rendering it. ──
  const coordTerminal = $derived(!!coordId && view.terminalShown.includes('coordinator'));
  const terminalsOnScreen = $derived.by(() => {
    const ids: string[] = [];
    if (coordTerminal && fit.coord === 'full') ids.push(coordId!);
    for (const taskId of fit.shown) {
      const tab = cardByTask.get(taskId)?.worker.tab_id;
      if (tab && view.terminalShown.includes(taskId)) ids.push(tab);
    }
    return ids;
  });
  $effect(() => {
    const id = workspaceId;
    const ids = terminalsOnScreen;
    untrack(() => hiveView.setTerminalsOnScreen(id, ids));
  });
  $effect(() => {
    const id = workspaceId;
    return () => hiveView.setTerminalsOnScreen(id, []);
  });

  // The Files column follows the focused column's tab (fileTree `followableTab`), and a worker's
  // tab is its card's. A column whose worker the board no longer has (a rolled-back spawn) closes.
  $effect(() => {
    const b = board;
    if (!b) return;
    const id = workspaceId;
    untrack(() => {
      hiveView.setTaskTabs(id, b.workers.filter((w) => w.tab_id).map((w) => [w.task_id, w.tab_id!] as const));
      hiveView.prune(id, b.workers.map((w) => w.task_id));
    });
  });

  // ── Dependency strings (§4.4): drawn from the cards' boxes, in a layer that scrolls with them,
  // so only a change of layout redraws them. ──
  let wavesEl = $state<HTMLElement | undefined>();
  let edges = $state<HiveString[]>([]);
  let stringsRaf = 0;
  function drawStrings() {
    cancelAnimationFrame(stringsRaf);
    stringsRaf = requestAnimationFrame(() => {
      const host = wavesEl;
      if (!host) {
        edges = [];
        return;
      }
      const H = host.getBoundingClientRect();
      const boxes = new Map<string, Box>();
      for (const el of host.querySelectorAll<HTMLElement>('[data-hive-task]')) {
        const r = el.getBoundingClientRect();
        boxes.set(el.dataset.hiveTask!, { l: r.left - H.left, r: r.right - H.left, t: r.top - H.top, b: r.bottom - H.top });
      }
      edges = strings(columns.map((w) => w.map((c) => c.node)), boxes);
    });
  }
  let stringsRo: ResizeObserver | null = null;
  $effect(() => {
    const host = wavesEl;
    if (!host) return;
    stringsRo = new ResizeObserver(() => drawStrings());
    return () => {
      stringsRo?.disconnect();
      stringsRo = null;
      cancelAnimationFrame(stringsRaf);
    };
  });
  // A card that grows (a longer activity line, a new note) moves the ones under it: every card is
  // observed, re-listed whenever the board changes.
  $effect(() => {
    void columns;
    const host = wavesEl;
    untrack(() => {
      if (!host || !stringsRo) return;
      stringsRo.disconnect();
      stringsRo.observe(host);
      for (const el of host.querySelectorAll('[data-hive-task]')) stringsRo.observe(el);
      drawStrings();
    });
  });
  const counts = $derived.by(() => {
    const n = new Map<CardStateName, number>();
    for (const c of cards) n.set(c.state.state, (n.get(c.state.state) ?? 0) + 1);
    return [...n].sort((a, b) => STATE_ORDER[a[0]] - STATE_ORDER[b[0]]).map(([s, k]) => `${k} ${STATE_LABEL[s].toLowerCase()}`);
  });
  const workers = $derived(board?.workers.length ?? 0);
  const room = $derived(board?.headroom ?? null);

  // ── Dragging the coordinator's and the rail's borders. ──
  let dragging = $state<'coord' | 'rail' | null>(null);
  function startResize(e: PointerEvent, which: 'coord' | 'rail') {
    e.preventDefault();
    const startX = e.clientX;
    const startW = which === 'coord' ? width : railW;
    const el = e.currentTarget as HTMLElement;
    el.setPointerCapture(e.pointerId);
    dragging = which;
    const move = (ev: PointerEvent) => {
      if (which === 'coord') width = clampCoord(startW + ev.clientX - startX);
      else railW = clampRail(startW + ev.clientX - startX);
    };
    const end = () => {
      dragging = null;
      el.removeEventListener('pointermove', move);
      el.removeEventListener('pointerup', end);
      el.removeEventListener('pointercancel', end);
      if (which === 'coord') keep(WIDTH_KEY, width);
      else keep(RAIL_KEY, railW);
    };
    el.addEventListener('pointermove', move);
    el.addEventListener('pointerup', end);
    el.addEventListener('pointercancel', end);
  }
  function onResizerKey(e: KeyboardEvent, which: 'coord' | 'rail') {
    const step = (e.shiftKey ? 40 : 10) * (e.key === 'ArrowLeft' ? -1 : e.key === 'ArrowRight' ? 1 : 0);
    if (!step) return;
    e.preventDefault();
    if (which === 'coord') keep(WIDTH_KEY, (width = clampCoord(width + step)));
    else keep(RAIL_KEY, (railW = clampRail(railW + step)));
  }

  // ── The coordinator's strip (§4.1): collapsed by the human, or given way to a narrow window.
  // Its chat stays mounted (hidden), so a draft in its composer survives. The dot: it finished a
  // turn, or wants a permission, since the strip appeared. ──
  let stripSince = $state(0);
  $effect(() => {
    if (fit.coord === 'strip') untrack(() => { if (!stripSince) stripSince = Date.now(); });
    else stripSince = 0;
  });
  const coordAgent = $derived(coordId ? agentStateStore.getState(coordId) : undefined);
  const unread = $derived(
    fit.coord === 'strip' && !!coordAgent
      && (coordAgent.state === 'permission' || (coordAgent.state === 'idle' && (coordAgent.idleSince ?? 0) > stripSince)),
  );
  const stripTip = $derived(
    view.coordCollapsed
      ? `Show the coordinator${unread ? ': it has news' : ''}`
      : `Show the coordinator: the window is too narrow for it beside a worker, which steps aside until you go back to it${unread ? '. It has news' : ''}`,
  );
  /** Focused, the coordinator never gives way (`fitColumns`): a worker steps aside if it must. Its
   *  composer takes the keyboard once the layout has shown it. */
  function expandCoordinator() {
    hiveView.setCoordCollapsed(workspaceId, false);
    hiveView.focus(workspaceId, 'coordinator');
    requestAnimationFrame(() => column?.focusComposer());
  }
  function collapseCoordinator() {
    hiveView.setCoordCollapsed(workspaceId, true);
    const to = hiveView.view(workspaceId).focused;
    requestAnimationFrame(() => workerCols[to]?.focusColumn());
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
  // coordinator's column, or a worker's column. ──
  let column = $state<ChatColumn | undefined>();
  const workerCols: Record<string, WorkerColumn | undefined> = $state({});
  let flash = $state(false);
  /** A worker asked for before its card was on the board (a fresh mount, a spawn the board hasn't
   *  refetched yet): applied when it arrives, if that's soon. */
  let pendingWorker: { detail: HiveOpenColumnDetail; at: number } | null = null;
  let flashTimer: ReturnType<typeof setTimeout> | null = null;
  function open(detail: HiveOpenColumnDetail) {
    if (detail.workspaceId !== workspaceId) return;
    if (detail.tabId && detail.tabId === coordId) {
      hiveView.setCoordCollapsed(workspaceId, false);
      hiveView.focus(workspaceId, 'coordinator');
      requestAnimationFrame(() => column?.focusComposer());
      flash = true;
      if (flashTimer) clearTimeout(flashTimer);
      flashTimer = setTimeout(() => (flash = false), 900);
      return;
    }
    const w = board?.workers.find((c) => (detail.taskId && c.task_id === detail.taskId) || (detail.tabId && c.tab_id === detail.tabId));
    if (!w) {
      if (pendingWorker?.detail !== detail) pendingWorker = { detail, at: Date.now() };
      return;
    }
    pendingWorker = null;
    if (hiveView.open(workspaceId, w.task_id).refused) {
      toastStore.addToast('Both worker columns are pinned', `Unpin one to open #${w.badge} ${w.title}.`, 'info');
      return;
    }
    // After the column mounts: its composer, or the column itself for a worker with no chat.
    requestAnimationFrame(() => workerCols[w.task_id]?.focusColumn());
  }
  function closeColumn(taskId: string) {
    hiveView.close(workspaceId, taskId);
  }
  /** Whichever column the keyboard or a click went to: the one the Files column follows. */
  const focusTo = (key: string) => () => hiveView.focus(workspaceId, key);
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
    const feedFor = workspaceId;
    hiveFeed.attach(feedFor);
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
    return () => {
      gone = true;
      unlisten?.();
      hiveFeed.detach(feedFor);
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

  <div class="cols" bind:this={colsEl}>
    {#if fit.coord === 'strip'}
      <Tooltip text={stripTip}>
        <button class="strip" class:focused={focusedKey === 'coordinator'} aria-label={stripTip} onclick={expandCoordinator}>
          <Icon name="chevron" size={12} />
          <span class="strip-label">Coordinator</span>
          {#if unread}<span class="dot" aria-hidden="true"></span>{/if}
        </button>
      </Tooltip>
    {/if}
    <!-- Kept mounted as a strip: a draft in its composer survives. -->
    <!-- svelte-ignore a11y_no_noninteractive_element_interactions -->
    <section
      class="coord"
      class:gone={fit.coord === 'strip'}
      class:flash
      class:focused={focusedKey === 'coordinator' && fit.board === 'rail'}
      class:dragging={dragging === 'coord'}
      style="width: {width}px"
      aria-label="Coordinator"
      onfocusin={focusTo('coordinator')}
      onpointerdown={focusTo('coordinator')}
    >
      <ChatColumn
        bind:this={column}
        {source}
        name="Coordinator"
        workspace={ws?.name ?? ''}
        {tasks}
        active={fit.coord === 'full'}
        actions={null}
        emptyText="This Hive has no coordinator tab."
      >
        {#snippet header()}
          <span class="role">coordinator</span>
          {#if coordId}
            <IconButton
              tooltip={coordTerminal ? 'Hide the terminal' : 'Show the terminal'}
              onclick={() => hiveView.setTerminalShown(workspaceId, 'coordinator', !coordTerminal)}
            ><Icon name="terminal" size={12} /></IconButton>
          {/if}
          <IconButton tooltip="Collapse to a strip" onclick={collapseCoordinator}><Icon name="collapse-all" size={12} /></IconButton>
        {/snippet}
      </ChatColumn>
      {#if coordTerminal}
        <HiveTerminal tabId={coordId!} onhide={() => hiveView.setTerminalShown(workspaceId, 'coordinator', false)} />
      {/if}
    </section>
    {#if fit.coord === 'full'}
      <!-- A focusable separator is a control (ARIA), which the a11y lint doesn't know. -->
      <!-- svelte-ignore a11y_no_noninteractive_tabindex, a11y_no_noninteractive_element_interactions -->
      <div
        class="resizer"
        class:dragging={dragging === 'coord'}
        role="separator"
        aria-orientation="vertical"
        aria-label="Resize the coordinator column"
        aria-valuemin={W.COORD_MIN}
        aria-valuemax={W.COORD_MAX}
        aria-valuenow={width}
        tabindex="0"
        onpointerdown={(e) => startResize(e, 'coord')}
        onkeydown={(e) => onResizerKey(e, 'coord')}
      ></div>
    {/if}
    <!-- svelte-ignore a11y_no_noninteractive_element_interactions -->
    <section
      class="board"
      class:rail={boardMode === 'rail'}
      style={boardMode === 'rail' ? `width: ${railW}px` : undefined}
      aria-label="Board"
      onfocusin={focusTo('board')}
      onpointerdown={focusTo('board')}
    >
      {#if boardMode === 'rail' && railCards.length}
        <!-- §4.4: in rail mode the waves flatten into the sorted list. -->
        <div class="rail-list" role="list">
          {#each railCards as c (c.worker.task_id)}
            <div role="listitem">
              <WorkerCard
                compact
                card={c.worker}
                state={c.state}
                note={c.note}
                runtime={c.runtime}
                age=""
                activity={c.activity}
                diff={c.diff}
                model={c.model}
                ctx={c.ctx}
                focused={fit.shown.includes(c.worker.task_id) && focusedKey === c.worker.task_id}
                onopen={() => openCard(c.worker.task_id, c.worker.tab_id)}
              />
            </div>
          {/each}
        </div>
      {:else if columns.length}
        <div class="waves" bind:this={wavesEl}>
          <svg class="strings" aria-hidden="true">
            {#each edges as e (e.key)}
              <path d={e.d} class="edge {e.kind}" class:held={e.held} />
            {/each}
          </svg>
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
                    activity={c.activity}
                    diff={c.diff}
                    model={c.model}
                    ctx={c.ctx}
                    focused={fit.shown.includes(c.worker.task_id) && focusedKey === c.worker.task_id}
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
    {#if boardMode === 'rail'}
      <!-- svelte-ignore a11y_no_noninteractive_tabindex, a11y_no_noninteractive_element_interactions -->
      <div
        class="resizer"
        class:dragging={dragging === 'rail'}
        role="separator"
        aria-orientation="vertical"
        aria-label="Resize the rail"
        aria-valuemin={W.RAIL_MIN}
        aria-valuemax={W.RAIL_MAX}
        aria-valuenow={railW}
        tabindex="0"
        onpointerdown={(e) => startResize(e, 'rail')}
        onkeydown={(e) => onResizerKey(e, 'rail')}
      ></div>
    {/if}
    <!-- Every open column stays mounted, a suppressed one hidden: its drafts survive the squeeze,
         and it comes back when the window widens. -->
    {#each openCols as col (col.taskId)}
      {@const c = cardByTask.get(col.taskId)}
      {#if c}
        <div
          class="wslot"
          class:gone={!fit.shown.includes(col.taskId)}
          role="presentation"
          onfocusin={focusTo(col.taskId)}
          onpointerdown={focusTo(col.taskId)}
        >
          <WorkerColumn
            bind:this={workerCols[col.taskId]}
            card={c.worker}
            cardState={c.state}
            diff={hiveFeed.diffStat(col.taskId)}
            workspace={ws?.name ?? ''}
            {tasks}
            active={fit.shown.includes(col.taskId)}
            pinned={col.pinned}
            focused={focusedKey === col.taskId}
            terminalShown={view.terminalShown.includes(col.taskId)}
            onpin={() => hiveView.togglePin(workspaceId, col.taskId)}
            onclose={() => closeColumn(col.taskId)}
            onterminal={(shown) => hiveView.setTerminalShown(workspaceId, col.taskId, shown)}
          />
        </div>
      {/if}
    {/each}
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
  .coord.focused { box-shadow: inset 0 2px 0 var(--accent); }
  .coord.dragging { user-select: none; }
  .coord.gone, .wslot.gone { display: none; }
  /* §4.1: the coordinator collapsed to 44 px, with a dot when it has news. */
  .cols > :global(.tooltip-wrapper) { flex: none; display: flex; }
  .strip { width: 44px; flex: none; display: flex; flex-direction: column; align-items: center; gap: 10px; padding: 12px 0; background: var(--bg-medium); border: none; border-right: 1px solid var(--bg-light); color: var(--fg-dim); font: inherit; cursor: pointer; }
  .strip:hover, .strip:focus-visible { color: var(--fg); outline: none; }
  .strip.focused { box-shadow: inset 0 2px 0 var(--accent); }
  .strip-label { writing-mode: vertical-rl; transform: rotate(180deg); font-size: 11px; letter-spacing: 0.06em; }
  .strip .dot { width: 8px; height: 8px; border-radius: 50%; background: var(--accent); }
  .wslot { flex: 1 1 420px; min-width: 0; min-height: 0; display: flex; }
  .resizer { flex: none; width: 4px; cursor: col-resize; background: var(--bg-light); opacity: 0.5; }
  .resizer:hover, .resizer.dragging, .resizer:focus-visible { opacity: 1; background: var(--accent); outline: none; }
  .board { flex: 1 1 260px; min-width: 260px; min-height: 0; overflow-y: auto; display: flex; }
  /* Waves sit side by side and wrap under each other when the board is narrow: the main area
     never scrolls sideways (§4.1). */
  .waves { position: relative; flex: 1; display: flex; flex-wrap: wrap; align-content: flex-start; gap: 16px 24px; padding: 14px 16px; min-width: 0; }
  .wave { position: relative; z-index: 1; flex: 1 1 220px; max-width: 340px; min-width: 0; display: flex; flex-direction: column; gap: 8px; }
  .board.rail { flex: none; min-width: 200px; max-width: 240px; }
  .rail-list { flex: 1; display: flex; flex-direction: column; gap: 6px; padding: 10px; min-width: 0; }
  /* Strings run under the cards (they're opaque), so one that skips a wave passes behind it. The
     Weave's vocabulary (docs/loom.md): moving dots = the upstream is active, a slow amber dash
     running backwards = this card waits on it, faint dots = a dependency that is neither. */
  .strings { position: absolute; inset: 0; width: 100%; height: 100%; pointer-events: none; z-index: 0; overflow: visible; }
  .edge { fill: none; stroke: var(--yellow); stroke-width: 1.2; stroke-dasharray: 1 4; opacity: 0.5; }
  .edge.met { stroke: var(--fg-dim); opacity: 0.35; }
  .edge.run { stroke: var(--green); stroke-width: 2.2; stroke-linecap: round; stroke-dasharray: 0.1 9.9; opacity: 0.85; animation: flow 1.1s linear infinite; }
  .edge.run.held { stroke: var(--yellow); }
  .edge.wait { stroke-dasharray: 6 4; opacity: 0.75; animation: flow 3.2s linear infinite reverse; }
  @keyframes flow { to { stroke-dashoffset: -20; } }
  .wave h3 { margin: 0; font-size: 10.5px; font-weight: 500; letter-spacing: 0.08em; text-transform: uppercase; color: var(--fg-dim); }
  .warn { color: var(--yellow); }
  .empty { margin: auto; display: flex; flex-direction: column; align-items: center; gap: 4px; padding: 24px; text-align: center; max-width: 320px; }
  .empty b { font-size: 13px; color: var(--fg); }
  .empty span { font-size: 12px; color: var(--fg-dim); overflow-wrap: anywhere; }
  .role { font-size: 10px; text-transform: uppercase; letter-spacing: 0.05em; color: var(--accent); border: 1px solid color-mix(in srgb, var(--accent) 45%, transparent); border-radius: 3px; padding: 0 4px; }
  @media (prefers-reduced-motion: reduce) {
    .coord { transition: none; }
    .edge.run, .edge.wait { animation: none; }
  }
</style>
