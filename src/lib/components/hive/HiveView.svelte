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
  note, agent state and the activity line) comes live from the stores. A card's newest assets come
  with its record, refetched when the gallery changes (§4.3). The header's top row is the name and goal,
  the crew strip (one segment per worker in its card's colour), the status (Running, Paused, or
  while done its Done time and Report, §11.4), Pause or Resume, Settings, and a ⋯ menu with the
  land log and Delete. Notices (throttled, usage limit, a land undone since Done, a suspect repo)
  get a line of their own. The bar under them holds the views and the readouts: slots, budget,
  the completion criteria (read-only: the coordinator sets them), where lands go, autonomy and
  the run queue.

  Gallery (§10.4, step 8d) and Memory (§9, step 7a) are the header's other views (Board, Gallery,
  Memory, Cmd+Shift+Y): GalleryView or HiveMemoryView takes the board's place and the worker
  columns', which stay mounted, hidden, so their drafts survive (plan D10). The coordinator column
  stays put. An inline asset in a chat (`HIVE_OPEN_ASSET`) opens the gallery at it.

  Worker columns (§4.1): opening one turns the board into a rail; at most two show, and on a
  narrow window one gives way, then the coordinator goes to its strip (`hive/columns.ts`). A
  click on a card switches to its worker (in place of the unpinned columns), Shift+click opens it
  beside them, and a click on the rail's empty space closes the unpinned ones. Which
  are open and which has focus is `hiveView`'s, per workspace; the focused column's tab is what
  the Files column follows here. Every column is a transcript, so none of this touches a PTY.

  Show terminal (§4.5) adds a column's raw terminal under its chat (HiveTerminal). The panes are
  `fixedSize`, so that touches no PTY either; this view tells `+page` which ones are on screen
  (`hiveView.setTerminalsOnScreen`), which is all that makes Rust render them.

  The keyboard (§4.6, `hive/keys.ts`): Cmd+1 the coordinator, Cmd+2…9 switches to a worker by badge,
  Cmd+Shift+[ ] between columns, Cmd+W (twice) closes the focused worker column, Escape blurs a
  composer or closes a worker column, and inside a raw terminal stays the agent's.
-->
<script lang="ts">
  import { onMount, untrack } from 'svelte';
  import { getCurrentWindow } from '@tauri-apps/api/window';
  import { hiveLiveWorkerTabs, hivePause, hiveResume, hiveWorkers } from '$lib/tauri/commands';
  import HiveSettingsModal from './HiveSettingsModal.svelte';
  import { hiveFailureNotice } from '$lib/agents/turnFailure';
  import { boardGone, pausedLabel, throttleTip } from '$lib/hive/model';
  import { doneLabel, doneStaleTip } from '$lib/hive/finish';
  import FinishReportModal from './FinishReportModal.svelte';
  import type { HiveBoard } from '$lib/tauri/types';
  import { error as logError, info as logInfo } from '@tauri-apps/plugin-log';
  import { countedListen } from '$lib/utils/listenCounter';
  import { workspacesStore } from '$lib/stores/workspaces.svelte';
  import { tasksStore } from '$lib/stores/tasks.svelte';
  import { terminalsStore } from '$lib/stores/terminals.svelte';
  import { agentStateStore } from '$lib/stores/agentState.svelte';
  import ChatColumn from '$lib/components/loom/ChatColumn.svelte';
  import WorkerCard from './WorkerCard.svelte';
  import WorkerColumn from './WorkerColumn.svelte';
  import HiveTerminal from './HiveTerminal.svelte';
  import RunQueueModal from './RunQueueModal.svelte';
  import HiveDeleteSheet from './HiveDeleteSheet.svelte';
  import LandLogModal from './LandLogModal.svelte';
  import HiveAutonomyModal from './HiveAutonomyModal.svelte';
  import GallerySettingsModal from './GallerySettingsModal.svelte';
  import { autonomyLabel } from '$lib/hive/autonomy';
  import { allHivesLabel, budgetFill, budgetLabel, budgetLevel } from '$lib/hive/budget';
  import ContextMenu, { type MenuItem } from '$lib/components/ContextMenu.svelte';
  import { hivePrompts } from '$lib/stores/hivePrompts.svelte';
  import HiveMemoryView from './HiveMemoryView.svelte';
  import GalleryView from './gallery/GalleryView.svelte';
  import { hiveGallery } from '$lib/stores/hiveGallery.svelte';
  import { HIVE_OPEN_ASSET, type HiveOpenAssetDetail } from '$lib/hive/gallery';
  import Icon from '$lib/components/Icon.svelte';
  import IconButton from '$lib/components/ui/IconButton.svelte';
  import Tooltip from '$lib/components/Tooltip.svelte';
  import { overlordStore } from '$lib/stores/overlord.svelte';
  import { hiveFeed } from '$lib/stores/hiveFeed.svelte';
  import { hiveView } from '$lib/stores/hiveView.svelte';
  import { toastStore } from '$lib/stores/toasts.svelte';
  import { clampCoord, clampRail, cycleOrder, fitColumns, MAX_WORKER_COLUMNS, nextFocus, tabsOnScreen, W, type OpenMode } from '$lib/hive/columns';
  import { escapeAction, HIVE_KEY, type HiveKeyAction, type HiveKeyDetail } from '$lib/hive/keys';
  import { modSymbol } from '$lib/utils/platform';
  import { HIVE_OPEN_COLUMN, takePendingHiveOpen, type HiveOpenColumnDetail } from '$lib/hive/model';
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
  /** Hive settings → Run queue (docs/hive.md §6.8), from the header. */
  let queueOpen = $state(false);
  /** Delete Hive (docs/hive.md §11.2): the sheet lists what stays before anything is touched. */
  let deleteOpen = $state(false);
  /** Hive settings → Autonomy (docs/hive.md §7.4), from the header. */
  let autonomyOpen = $state(false);
  /** Hive header → Budget (docs/hive.md §10.3): the budget, output folders, generator hosts. */
  let budgetOpen = $state(false);
  /** The land log (docs/hive.md §8.6), whole from the header or one worker's from its column. */
  let landLog = $state<{ taskId: string | null } | null>(null);
  /** Done (docs/hive.md §11.4): the report, from the header while done; the criteria, read-only. */
  let reportOpen = $state(false);
  let criteriaOpen = $state(false);
  let resuming = $state(false);
  /** Hive settings (§4.1): the editable sheets, and what was fixed at Create. */
  let settingsOpen = $state(false);
  /** The header's Pause (§11.2): no worker starts or relaunches; running agents finish. */
  async function pause() {
    if (resuming) return;
    resuming = true;
    try {
      workspacesStore.applyHiveRecord(workspaceId, await hivePause(workspaceId));
    } catch (e) {
      logError(`hive: ${workspaceId.slice(0, 8)} not paused: ${e}`);
      toastStore.addToast("The Hive didn't pause", String(e).replace(/^Error: /, ''), 'error');
    } finally {
      resuming = false;
    }
  }
  /** A paused Hive starts workers again (`hive_resume`, which polls the waiters). */
  async function resume() {
    if (resuming) return;
    resuming = true;
    try {
      workspacesStore.applyHiveRecord(workspaceId, await hiveResume(workspaceId));
    } catch (e) {
      logError(`hive: ${workspaceId.slice(0, 8)} not resumed: ${e}`);
      toastStore.addToast("The Hive didn't resume", String(e).replace(/^Error: /, ''), 'error');
    } finally {
      resuming = false;
    }
  }
  const queuedCount = $derived(hive?.settings?.run_queue?.commands.length ?? 0);

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
        .catch((e) => {
          // A fetch racing the Hive's delete (or its turning back into a workspace) is no error.
          const gone = boardGone(String(e), !!workspacesStore.workspaces.find((w) => w.id === id)?.hive);
          (gone ? logInfo : logError)(`hive: board of ${id.slice(0, 8)} not fetched${gone ? ' (no longer a Hive)' : ''}: ${e}`);
        });
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
    // The budget line is the board's (`spend`): a raise moves it with no gallery event.
    void hive?.budget_cents;
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

  // The gallery's feed for this Hive, attached before the gallery view mounts: its event moves
  // `version` whichever view shows, and a card's newest assets come with the board (§4.3).
  const galleryToken = hiveGallery.attach(untrack(() => workspaceId));
  $effect(() => {
    if (hiveGallery.version) untrack(refetch);
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
  /** Memory (§9) and the gallery (§10.4) take the board's place and the worker columns' (kept
   *  mounted, hidden, plan D10), so the layout fits as if none were open: no rail, no worker
   *  terminal on screen. Opening a worker column goes back to the board (`open`). */
  const showMemory = $derived(view.view === 'memory');
  const showGallery = $derived(view.view === 'gallery');
  const VIEW_TABS = [{ key: 'board', label: 'Board' }, { key: 'gallery', label: 'Gallery' }, { key: 'memory', label: 'Memory' }] as const;
  const layoutCols = $derived(showMemory || showGallery ? [] : openCols);
  const fit = $derived(fitColumns({
    main: mainW > 0 ? mainW : Number.POSITIVE_INFINITY,
    coordW: width,
    railW,
    coordCollapsed: view.coordCollapsed,
    open: layoutCols,
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
    return () => {
      hiveView.setTerminalsOnScreen(id, []);
      hiveView.setColumnsOnScreen(id, []);
    };
  });
  // The tabs whose column the layout shows, terminal or chat: what counts as visible for a Hive
  // tab's notices (§4.7). A column given way, or a coordinator squeezed to its strip, is not.
  const columnsOnScreen = $derived(tabsOnScreen(fit, coordId, (t) => cardByTask.get(t)?.worker.tab_id));
  $effect(() => {
    const id = workspaceId;
    const ids = columnsOnScreen;
    untrack(() => hiveView.setColumnsOnScreen(id, ids));
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
  /** The header's crew strip: one segment per worker, in the board's state order. */
  const crew = $derived(cards.map((c) => c.state.state).sort((a, b) => STATE_ORDER[a] - STATE_ORDER[b]));
  const room = $derived(board?.headroom ?? null);
  /** Slot pips only while they fit at a glance; past that the numbers say it. */
  const PIP_MAX = 10;
  /** The header's ⋯ menu: the land log, and Delete kept off the always-visible row. */
  let moreBtn = $state<HTMLElement | null>(null);
  let moreMenu = $state<{ x: number; y: number } | null>(null);
  function openMore() {
    if (moreMenu) {
      moreMenu = null;
      return;
    }
    const r = moreBtn?.getBoundingClientRect();
    if (r) moreMenu = { x: r.right - 180, y: r.bottom + 4 };
  }
  const moreItems: MenuItem[] = [
    { label: 'Land log', action: () => (landLog = { taskId: null }) },
    { label: '', action: () => {}, separator: true },
    { label: 'Delete Hive…', action: () => (deleteOpen = true) },
  ];
  /** A failed turn's notice (hive/turn_failure.rs), by tab: until a turn of that agent ends well. */
  const failureOf = (tabId: string | null | undefined) => (tabId ? board?.turn_failures?.[tabId] ?? null : null);
  const coordFailure = $derived(failureOf(coordId));

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
  let pendingWorker: { detail: HiveOpenColumnDetail; mode: OpenMode; at: number } | null = null;
  let flashTimer: ReturnType<typeof setTimeout> | null = null;
  /** `mode` (§4.1): a worker's column shows in place of the unpinned ones (`switch`: a click, a
   *  badge key, a notification, the gallery) or beside them (`add`: a Shift+click on a card). */
  function open(detail: HiveOpenColumnDetail, mode: OpenMode = 'switch') {
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
      if (pendingWorker?.detail !== detail) pendingWorker = { detail, mode, at: Date.now() };
      return;
    }
    pendingWorker = null;
    // A worker's column lives in the board view.
    hiveView.setView(workspaceId, 'board');
    if (hiveView.open(workspaceId, w.task_id, mode).refused) {
      toastStore.addToast('Both worker columns are pinned', `Unpin one to open #${w.badge} ${w.title}.`, 'info');
      return;
    }
    // A switch, or a third column, closes one: an arm on it goes too (`closeColumn`).
    if (closeArm && !hiveView.view(workspaceId).open.some((c) => c.taskId === closeArm?.taskId)) disarmClose();
    // After the column mounts: its composer, or the column itself for a worker with no chat.
    requestAnimationFrame(() => workerCols[w.task_id]?.focusColumn());
  }
  /** Every way a column closes (Cmd+W, Escape, its close button, the rail's empty space) takes back a Cmd+W armed on it, so a
   *  reopened column never closes on one press. */
  function closeColumn(taskId: string) {
    if (closeArm?.taskId === taskId) disarmClose();
    hiveView.close(workspaceId, taskId);
  }
  /** Whichever column the keyboard or a click went to: the one the Files column follows. */
  const focusTo = (key: string) => () => hiveView.focus(workspaceId, key);
  $effect(() => {
    if (!board || !pendingWorker) return;
    const p = pendingWorker;
    if (Date.now() - p.at > 5000) pendingWorker = null;
    else untrack(() => open(p.detail, p.mode));
  });
  /** A card's click: Shift opens it beside the open column, any other click switches to it. */
  function openCard(taskId: string, tabId: string | null | undefined, e?: MouseEvent) {
    open({ workspaceId, tabId: tabId ?? '', taskId }, e?.shiftKey ? 'add' : 'switch');
  }

  /** A click on the rail's empty space (§4.1): the unpinned worker columns close and the board
   *  comes back to full width. Only a plain click that starts and ends on the rail's own
   *  background, never a card or a control, the end of a drag, or a text selection. */
  let railDown: { x: number; y: number } | null = null;
  const isRailBackground = (t: EventTarget | null) =>
    t instanceof HTMLElement && (t === boardEl || t.classList.contains('rail-list') || t.classList.contains('rail-hint') || t.getAttribute('role') === 'listitem');
  function onRailPointerDown(e: PointerEvent) {
    railDown = boardMode === 'rail' && e.button === 0 && isRailBackground(e.target) ? { x: e.clientX, y: e.clientY } : null;
  }
  function onRailClick(e: MouseEvent) {
    const down = railDown;
    railDown = null;
    if (!down || boardMode !== 'rail' || dragging || e.button !== 0 || !isRailBackground(e.target)) return;
    if (Math.abs(e.clientX - down.x) > 4 || Math.abs(e.clientY - down.y) > 4) return;
    if (window.getSelection()?.isCollapsed === false) return;
    hiveView.closeUnpinned(workspaceId);
    if (closeArm && !hiveView.view(workspaceId).open.some((c) => c.taskId === closeArm?.taskId)) disarmClose();
  }

  // ── The gallery's Iterate and Send to worker… (§10.4): files onto a composer ──
  /** Iterate switches nothing: the coordinator's column is beside the gallery already (shown
   *  again if it was a strip). */
  function iterateOnCoordinator(paths: string[], text: string) {
    if (!coordId || !column) return;
    if (view.coordCollapsed) hiveView.setCoordCollapsed(workspaceId, false);
    hiveView.focus(workspaceId, 'coordinator');
    column.attachFiles(paths, text);
  }
  /** Send to worker…: Board, that worker's column, then its composer once the column mounts. */
  function sendToWorker(taskId: string, paths: string[]) {
    const c = cardByTask.get(taskId);
    if (!c?.worker.tab_id) return;
    open({ workspaceId, tabId: c.worker.tab_id, taskId });
    if (!hiveView.view(workspaceId).open.some((o) => o.taskId === taskId)) return;
    let tries = 0;
    const attach = () => {
      if (workerCols[taskId]?.attachFiles(paths)) return;
      if (++tries < 10) requestAnimationFrame(attach);
      else toastStore.addToast(`Not attached to #${c.worker.badge}`, `${c.worker.title} has no chat to attach to right now.`, 'info');
    };
    requestAnimationFrame(attach);
  }

  // ── The keyboard (§4.6). The Cmd keys come from `+layout` as `hive-key`, ahead of its tab
  // handlers; Escape is read here. ──
  let boardEl = $state<HTMLElement | undefined>();
  /** The keyboard to a focus stop, once the layout has shown it (a column coming back from a
   *  give-way is display:none until then). */
  function focusDom(to: string) {
    requestAnimationFrame(() => {
      if (to === 'coordinator') column?.focusComposer();
      else if (to === 'board') boardEl?.focus({ preventScroll: true });
      else workerCols[to]?.focusColumn();
    });
  }
  /** Cmd+Shift+[ / ], over every open column: one squeezed out by the window comes back when
   *  focused (`fitColumns`' keeper). */
  function cycle(dir: -1 | 1) {
    const order = cycleOrder(layoutCols, view.coordCollapsed);
    const to = nextFocus(order, view.focused, dir);
    hiveView.focus(workspaceId, to);
    focusDom(to);
  }

  /** Cmd+W closes the focused worker's column (never its agent, never the coordinator's) on a
   *  second press within 2 s, as tabs do; the first says so in a toast, taken back when the
   *  window lapses or the column closes. */
  let closeArm: { taskId: string; toast: string; timer: ReturnType<typeof setTimeout> } | null = null;
  function disarmClose() {
    if (!closeArm) return;
    clearTimeout(closeArm.timer);
    toastStore.removeToast(closeArm.toast);
    closeArm = null;
  }
  function closeFocused() {
    const f = view.focused;
    const c = cardByTask.get(f);
    if (!c || !view.open.some((o) => o.taskId === f)) return;
    if (closeArm?.taskId === f) {
      disarmClose();
      closeAndRefocus(f);
      return;
    }
    disarmClose();
    const toast = toastStore.addToast(
      `Press ${modSymbol}W again to close #${c.worker.badge}`,
      `${c.worker.title}: the column closes, the worker keeps running.`,
      'info',
    );
    closeArm = { taskId: f, toast, timer: setTimeout(disarmClose, 2000) };
  }
  function closeAndRefocus(taskId: string) {
    closeColumn(taskId);
    focusDom(hiveView.view(workspaceId).focused);
  }

  function onHiveKey(a: HiveKeyAction) {
    switch (a.kind) {
      case 'focus-coordinator':
        if (coordId) expandCoordinator();
        return;
      case 'open-badge': {
        // Badges are spawn order, kept for the task's life (§4.6); past 8, a click.
        const w = board?.workers.find((c) => c.badge === a.badge);
        if (w) open({ workspaceId, tabId: w.tab_id ?? '', taskId: w.task_id });
        return;
      }
      case 'cycle':
        cycle(a.dir);
        return;
      case 'cycle-view':
        // Board → Gallery → Memory (plan D24).
        hiveView.cycleView(workspaceId);
        return;
      case 'close-focused':
        closeFocused();
        return;
    }
  }

  /** Escape (§4.6), after anything inside handled it (a menu, a card's own field). In a raw
   *  terminal it's the agent's (it interrupts), so it passes. A composer blurs, leaving the
   *  keyboard on its worker's column, where a second Escape closes it. */
  function onKeydown(e: KeyboardEvent) {
    if (e.key !== 'Escape' || e.defaultPrevented || e.isComposing) return;
    const t = e.target as HTMLElement | null;
    if (!t?.closest) return;
    const colEl = t.closest<HTMLElement>('[data-hive-column]');
    const act = escapeAction({
      terminal: !!t.closest('[data-hive-terminal], .xterm'),
      editable: t.isContentEditable || !!t.closest('input, textarea, select'),
      workerColumn: colEl?.dataset.hiveColumn ?? null,
    });
    if (!act) return;
    e.preventDefault();
    if (act.kind === 'blur') {
      // ChatColumn's composer has already blurred itself; any other field hasn't.
      t.blur();
      colEl?.focus({ preventScroll: true });
    } else {
      closeAndRefocus(act.taskId);
    }
  }

  onMount(() => {
    const feedFor = workspaceId;
    hiveFeed.attach(feedFor);
    // Who holds each worker prompt; `hive-prompts-changed` keeps it after this (hivePrompts).
    void hivePrompts.refresh(workspaceId);
    const pending = takePendingHiveOpen(workspaceId);
    if (pending) requestAnimationFrame(() => open(pending));
    const onOpen = (e: Event) => {
      takePendingHiveOpen(workspaceId);
      open((e as CustomEvent<HiveOpenColumnDetail>).detail);
    };
    window.addEventListener(HIVE_OPEN_COLUMN, onOpen);
    const onKey = (e: Event) => {
      const d = (e as CustomEvent<HiveKeyDetail>).detail;
      if (d?.workspaceId === workspaceId) onHiveKey(d.action);
    };
    window.addEventListener(HIVE_KEY, onKey);
    // A chat's inline asset (§4.2): the gallery, at that asset.
    const onAsset = (e: Event) => {
      const d = (e as CustomEvent<HiveOpenAssetDetail>).detail;
      if (d?.workspaceId !== workspaceId) return;
      hiveView.setView(workspaceId, 'gallery');
      hiveGallery.openDetail(workspaceId, d.assetId);
    };
    window.addEventListener(HIVE_OPEN_ASSET, onAsset);
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
      window.removeEventListener(HIVE_KEY, onKey);
      window.removeEventListener(HIVE_OPEN_ASSET, onAsset);
      hiveGallery.detach(feedFor, galleryToken);
      disarmClose();
    };
  });
</script>

<!-- Escape bubbling out of the columns (§4.6); every control inside keeps its own keys. -->
<!-- svelte-ignore a11y_no_static_element_interactions -->
<div class="hive" onkeydown={onKeydown}>
  <header class="hive-h">
    <div class="top">
      <div class="title">
        <h2>{ws?.name ?? ''}</h2>
        {#if hive?.goal}<p class="goal">{hive.goal}</p>{/if}
      </div>
      <div class="controls">
        {#if workers}
          <Tooltip text={counts.join(', ')}>
            <div class="crew" role="img" aria-label="{workers} worker{workers === 1 ? '' : 's'}: {counts.join(', ')}">
              <span class="segs" aria-hidden="true">
                {#each crew as s, i (i)}<span class="seg" data-state={s}></span>{/each}
              </span>
              <span class="crew-n">{workers} worker{workers === 1 ? '' : 's'}</span>
            </div>
          </Tooltip>
        {:else if board}
          <span class="crew-n">No workers yet</span>
        {/if}
        {#if hive}
          {#if hive.status === 'done'}
            <span class="status done"><Icon name="check" size={11} />{doneLabel(hive.done_at)}</span>
            <Tooltip text="The coordinator's report: each criterion's receipts, what landed, final assets, spend, what it decided alone">
              <button type="button" class="btn primary" onclick={() => (reportOpen = true)}>Report</button>
            </Tooltip>
          {:else if hive.status === 'paused'}
            <Tooltip text={pausedLabel(hive.paused_by)}>
              <span class="status paused"><span class="pulse" aria-hidden="true"></span>{hive.paused_by === 'human' ? 'Paused by you' : 'Paused'}</span>
            </Tooltip>
            <Tooltip text={hive.paused_by === 'human' ? 'Start workers again' : "Start workers again: you've dealt with what paused it"}>
              <button type="button" class="btn primary" disabled={resuming} onclick={() => void resume()}><Icon name="resume" size={11} />Resume</button>
            </Tooltip>
          {:else}
            <span class="status running"><span class="pulse" aria-hidden="true"></span>Running</span>
            <Tooltip text="No worker starts or relaunches until you resume it; running agents finish their turns">
              <button type="button" class="btn" disabled={resuming} onclick={() => void pause()}><Icon name="pause" size={11} />Pause</button>
            </Tooltip>
          {/if}
          <IconButton tooltip="Hive settings: autonomy, run queue, budget, and what was fixed when it was made" onclick={() => (settingsOpen = true)}>
            <Icon name="settings" size={13} />
          </IconButton>
          <span bind:this={moreBtn}>
            <IconButton tooltip="Land log, Delete Hive" aria-haspopup="menu" onclick={openMore}><Icon name="more" size={13} /></IconButton>
          </span>
        {/if}
      </div>
    </div>
    {#if hive?.done_stale || room?.throttled || room?.usage_limit_until || (hive?.status === 'paused' && hive.paused_by !== 'human') || (board && board.hive_status !== 'active' && board.hive_status !== 'done' && board.hive_status !== hive?.status) || board?.suspect_repos?.length}
      <div class="notices" role="status">
        {#if hive?.done_stale}
          <Tooltip text={doneStaleTip(hive.done_stale)}><span class="notice"><Icon name="warning" size={11} />A land it relied on was undone</span></Tooltip>
        {/if}
        {#if room?.throttled}
          <Tooltip text={throttleTip(room.throttle_critical ?? false)}><span class="notice"><Icon name="warning" size={11} />Throttled: {room.throttled}</span></Tooltip>
        {/if}
        {#if room?.usage_limit_until}
          <span class="notice"><Icon name="clock" size={11} />No new workers until {room.usage_limit_until}: usage limit</span>
        {:else if hive?.status === 'paused' && hive.paused_by !== 'human'}
          <span class="notice"><Icon name="warning" size={11} />{pausedLabel(hive.paused_by)}</span>
        {:else if board && board.hive_status !== 'active' && board.hive_status !== 'done' && board.hive_status !== hive?.status}
          <span class="notice"><Icon name="warning" size={11} />Hive is {board.hive_status}</span>
        {/if}
        {#if board?.suspect_repos?.length}
          <span class="notice"><Icon name="warning" size={11} />{board.suspect_repos.join(', ')} fails its land check alone</span>
        {/if}
      </div>
    {/if}
    <div class="bar">
      <div class="views" role="tablist" aria-label="Hive view">
        {#each VIEW_TABS as { key, label } (key)}
          <Tooltip text="{label} ({modSymbol}⇧Y switches)">
            <button
              type="button"
              role="tab"
              class="view-btn"
              class:active={view.view === key}
              aria-selected={view.view === key}
              onclick={() => hiveView.setView(workspaceId, key)}
            >{label}</button>
          </Tooltip>
        {/each}
      </div>
      {#if hive}
        <div class="readouts" aria-label="Hive facts">
          <div class="ro">
            <span class="k">Slots</span>
            <Tooltip text={room ? `${room.spawns_left_this_hour} starts left this hour, ${room.spawns_left_today} today` : "Workers running at once, of the Hive's cap"}>
              <span class="v">
                {#if (room?.cap ?? hive.cap) <= PIP_MAX}
                  <span class="pips" aria-hidden="true">
                    {#each { length: room?.cap ?? hive.cap } as _, i (i)}<span class="pip" class:on={i < (room?.live ?? 0)} class:starting={i >= (room?.live ?? 0) && i < (room?.live ?? 0) + (room?.starting ?? 0)}></span>{/each}
                  </span>
                {/if}
                {room?.live ?? 0} of {room?.cap ?? hive.cap}{#if room?.starting}<span class="sub">+{room.starting} starting</span>{/if}
              </span>
            </Tooltip>
          </div>
          {#if board?.spend}
            {@const spend = board.spend}
            {@const level = budgetLevel(spend)}
            {@const all = allHivesLabel(spend)}
            <div class="ro">
              <span class="k">Budget</span>
              <Tooltip text="{level === 'over' ? 'Reached: no paid render until you raise it.' : 'Paid generation, as agents record it.'}{all ? ` Across every Hive: ${all.replace(/^all Hives /, '')}.` : ''} Opens the budget, output folders and generator hosts">
                <span class="v">
                  <button type="button" class="ro-btn budget" data-level={level} onclick={() => (budgetOpen = true)}>
                    <span class="meter" aria-hidden="true"><span style="width: {budgetFill(spend) * 100}%"></span></span>
                    {budgetLabel(spend)}
                  </button>
                </span>
              </Tooltip>
            </div>
          {/if}
          <div class="ro">
            <span class="k">Criteria</span>
            <Tooltip text={hive.criteria.length ? 'What must be true for the goal to be met: hiveFinish proves each one' : 'The coordinator sets these with hiveSetCriteria'}>
              <span class="v">
                <button type="button" class="ro-btn" aria-expanded={criteriaOpen} onclick={() => (criteriaOpen = !criteriaOpen)}>
                  {hive.criteria.length ? hive.criteria.length : 'None yet'}<span class="chev" class:open={criteriaOpen}><Icon name="chevron" size={9} /></span>
                </button>
              </span>
            </Tooltip>
          </div>
          <div class="ro">
            <span class="k">Lands to</span>
            <Tooltip text={hive.landing_mode === 'integration' ? `Each land goes to hive/${hive.slug}, with your approval` : 'Each land goes straight to the base branch'}>
              <span class="v">{hive.landing_mode === 'integration' ? `hive/${hive.slug}` : 'Base'}</span>
            </Tooltip>
          </div>
          <div class="ro">
            <span class="k">Autonomy</span>
            <Tooltip text="Which decisions the coordinator makes and which come to you, the prompt timeout and the daily digest">
              <span class="v"><button type="button" class="ro-btn" onclick={() => (autonomyOpen = true)}>{autonomyLabel(hive)}</button></span>
            </Tooltip>
          </div>
          <div class="ro">
            <span class="k">Run queue</span>
            <Tooltip text="The heavy commands workers run only through the run queue (hiveRun), and what each needs">
              <span class="v">
                <button type="button" class="ro-btn" onclick={() => (queueOpen = true)}>
                  {queuedCount === 0 ? 'No commands' : `${queuedCount} command${queuedCount === 1 ? '' : 's'}`}
                </button>
              </span>
            </Tooltip>
          </div>
        </div>
      {/if}
    </div>
  </header>
  {#if moreMenu}
    <ContextMenu items={moreItems} x={moreMenu.x} y={moreMenu.y} anchor={moreBtn} onclose={() => (moreMenu = null)} />
  {/if}
  {#if criteriaOpen && hive}
    <div class="criteria-panel" aria-label="Completion criteria">
      {#if hive.criteria.length}
        <ol>
          {#each hive.criteria as c, i (i)}<li>{c}</li>{/each}
        </ol>
      {:else}
        <span class="dim">No completion criteria yet: the coordinator sets them.</span>
      {/if}
    </div>
  {/if}
  {#if settingsOpen && hive}
    <HiveSettingsModal
      {hive}
      name={ws?.name ?? ''}
      onclose={() => (settingsOpen = false)}
      onautonomy={() => (autonomyOpen = true)}
      onqueue={() => (queueOpen = true)}
      onbudget={() => (budgetOpen = true)}
    />
  {/if}
  {#if reportOpen && hive}
    <FinishReportModal {workspaceId} onclose={() => (reportOpen = false)} />
  {/if}
  {#if queueOpen && hive}
    <RunQueueModal {workspaceId} {hive} onclose={() => (queueOpen = false)} />
  {/if}
  {#if autonomyOpen && hive}
    <HiveAutonomyModal {workspaceId} {hive} onclose={() => (autonomyOpen = false)} />
  {/if}
  {#if budgetOpen && hive}
    <GallerySettingsModal {workspaceId} {hive} spend={board?.spend ?? null} onclose={() => (budgetOpen = false)} />
  {/if}
  {#if deleteOpen && hive}
    <HiveDeleteSheet {workspaceId} name={ws?.name ?? ''} onclose={() => (deleteOpen = false)} />
  {/if}
  {#if landLog && hive}
    <LandLogModal {workspaceId} taskId={landLog.taskId} refreshKey={board} onclose={() => (landLog = null)} />
  {/if}

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
      {#if coordFailure}
        <p class="coord-failed" role="status">{hiveFailureNotice(coordFailure)}</p>
      {/if}
      <ChatColumn
        bind:this={column}
        {source}
        name="Coordinator"
        workspace={ws?.name ?? ''}
        {tasks}
        active={fit.coord === 'full'}
        actions={null}
        emptyText="This Hive has no coordinator tab."
        gallery={hive ? { hiveId: hive.id, workspaceId } : null}
        composerPlaceholder={hive?.status === 'done' ? 'This Hive is done. Send a message to wake it.' : undefined}
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
    <!-- A focus stop of its own for Cmd+Shift+[ ] (§4.6); Tab goes on into its cards. A click on
         its empty space closes the unpinned worker columns (§4.1): Escape and Cmd+W are its keys. -->
    <!-- svelte-ignore a11y_no_noninteractive_element_interactions, a11y_click_events_have_key_events -->
    <section
      bind:this={boardEl}
      class="board"
      class:gone={showMemory || showGallery}
      class:rail={boardMode === 'rail'}
      class:focused={focusedKey === 'board' && boardMode === 'rail'}
      style={boardMode === 'rail' ? `width: ${railW}px` : undefined}
      tabindex="-1"
      aria-label="Board"
      onfocusin={focusTo('board')}
      onpointerdown={(e) => {
        focusTo('board')();
        onRailPointerDown(e);
      }}
      onclick={onRailClick}
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
                failure={failureOf(c.worker.tab_id)}
                focused={fit.shown.includes(c.worker.task_id) && focusedKey === c.worker.task_id}
                onopen={(e) => openCard(c.worker.task_id, c.worker.tab_id, e)}
              />
            </div>
          {/each}
          {#if openCols.length < MAX_WORKER_COLUMNS}
            <p class="rail-hint">Shift-click a card to open another worker beside this one.</p>
          {/if}
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
                    hiveId={hive?.id ?? ''}
                    activity={c.activity}
                    diff={c.diff}
                    model={c.model}
                    ctx={c.ctx}
                    failure={failureOf(c.worker.tab_id)}
                    focused={fit.shown.includes(c.worker.task_id) && focusedKey === c.worker.task_id}
                    onopen={(e) => openCard(c.worker.task_id, c.worker.tab_id, e)}
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
    {#if showMemory}
      <HiveMemoryView {workspaceId} />
    {/if}
    {#if showGallery && hive}
      <GalleryView
        {workspaceId}
        hiveId={hive.id}
        workers={board?.workers ?? []}
        anyLive={cards.some((c) => c.state.state === 'working' || c.state.state === 'starting')}
        active={showGallery}
        onopenworker={(taskId) => openCard(taskId, cardByTask.get(taskId)?.worker.tab_id)}
        onopencoordinator={() => { if (coordId) open({ workspaceId, tabId: coordId }); }}
        oniterate={iterateOnCoordinator}
        onsend={sendToWorker}
      />
    {/if}
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
            hiveId={hive?.id ?? ''}
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
            {workspaceId}
            onlandlog={() => (landLog = { taskId: col.taskId })}
            failure={failureOf(c.worker.tab_id)}
          />
        </div>
      {/if}
    {/each}
  </div>
</div>

<style>
  .hive { flex: 1; display: flex; flex-direction: column; min-width: 0; min-height: 0; background: var(--bg-dark); }
  /* The header (§4.1): who it is and what it's doing on top, the views and the Hive's standing
     facts on a bar whose tabs sit on the bottom rule. Wraps, never widens. */
  .hive-h { container-type: inline-size; flex: none; display: flex; flex-direction: column; gap: 10px; padding: 12px 18px 0; border-bottom: 1px solid var(--bg-light); background: color-mix(in srgb, var(--bg-medium) 45%, var(--bg-dark)); }
  .top { display: flex; flex-wrap: wrap; align-items: flex-start; justify-content: space-between; gap: 8px 24px; }
  .title { min-width: 0; flex: 1 1 260px; }
  .title h2 { margin: 0; font-size: 15px; font-weight: 600; line-height: 1.25; letter-spacing: -0.005em; color: var(--fg); overflow-wrap: anywhere; }
  .goal { margin: 3px 0 0; font-size: 12.5px; line-height: 1.4; color: var(--fg-dim); max-width: 72ch; overflow-wrap: anywhere; }
  .controls { display: flex; flex-wrap: wrap; align-items: center; justify-content: flex-end; gap: 6px 10px; min-width: 0; }

  /* The crew strip: one segment per worker in its card's colour, the Hive at a glance. */
  .crew { display: inline-flex; align-items: center; gap: 8px; padding-right: 4px; cursor: default; }
  .segs { display: inline-flex; gap: 2px; max-width: 220px; flex-wrap: wrap; }
  .seg { width: 9px; height: 14px; border-radius: 2px; background: color-mix(in srgb, var(--fg-dim) 45%, transparent); }
  .seg[data-state='working'] { background: var(--green); }
  .seg[data-state='starting'], .seg[data-state='landing'] { background: color-mix(in srgb, var(--accent) 75%, transparent); }
  .seg[data-state='needs_you'] { background: var(--orange, #ff9e64); }
  .seg[data-state='blocked'] { background: var(--yellow); }
  .seg[data-state='failed'] { background: var(--red); }
  .seg[data-state='reported'] { background: color-mix(in srgb, var(--accent) 35%, transparent); box-shadow: inset 0 0 0 1px var(--accent); }
  .seg[data-state='idle'], .seg[data-state='suspended'], .seg[data-state='stopped'] { background: transparent; box-shadow: inset 0 0 0 1px var(--fg-dim); }
  .seg[data-state='landed'], .seg[data-state='retired'] { background: color-mix(in srgb, var(--fg-dim) 25%, transparent); }
  .crew-n { font-size: 12px; color: var(--fg-dim); white-space: nowrap; }

  .status { display: inline-flex; align-items: center; gap: 6px; height: 22px; padding: 0 9px; border-radius: 11px; font-size: 11.5px; font-weight: 500; white-space: nowrap; }
  .status .pulse { width: 6px; height: 6px; border-radius: 50%; background: currentColor; }
  .status.running { color: var(--green); background: color-mix(in srgb, var(--green) 12%, transparent); }
  .status.running .pulse { animation: breathe 2.4s ease-in-out infinite; }
  .status.paused { color: var(--yellow); background: color-mix(in srgb, var(--yellow) 13%, transparent); }
  .status.done { color: var(--green); background: color-mix(in srgb, var(--green) 12%, transparent); }
  @keyframes breathe { 50% { opacity: 0.35; } }

  .btn { display: inline-flex; align-items: center; gap: 5px; height: 24px; padding: 0 10px; border: 1px solid var(--bg-light); border-radius: 5px; background: var(--bg-medium); color: var(--fg); font: inherit; font-size: 12px; cursor: pointer; }
  .btn:hover:not(:disabled) { border-color: var(--fg-dim); }
  .btn.primary { background: var(--accent); border-color: var(--accent); color: var(--bg-dark); font-weight: 500; }
  .btn.primary:hover:not(:disabled) { filter: brightness(1.08); border-color: var(--accent); }
  .btn:focus-visible { outline: 2px solid var(--accent); outline-offset: 1px; }
  .btn:disabled { cursor: default; opacity: 0.5; }

  .notices { display: flex; flex-wrap: wrap; gap: 4px 16px; margin-top: -4px; }
  .notice { display: inline-flex; align-items: center; gap: 5px; font-size: 11.5px; color: var(--yellow); overflow-wrap: anywhere; }

  /* The bar: tabs left on the bottom rule, readouts right. */
  .bar { display: flex; flex-wrap: wrap; align-items: flex-end; justify-content: space-between; gap: 4px 24px; }
  .views { display: flex; gap: 18px; font-size: 12.5px; }
  .view-btn { background: none; border: none; padding: 4px 0 8px; margin-bottom: -1px; color: var(--fg-dim); font: inherit; cursor: pointer; border-bottom: 2px solid transparent; }
  .view-btn:hover { color: var(--fg); }
  .view-btn.active { color: var(--fg); font-weight: 500; border-bottom-color: var(--accent); }
  .view-btn:focus-visible { outline: 2px solid var(--accent); outline-offset: -2px; border-radius: 2px; }

  .readouts { display: flex; flex-wrap: wrap; align-items: stretch; row-gap: 6px; padding-bottom: 8px; min-width: 0; }
  .ro { display: flex; flex-direction: column; gap: 2px; padding: 0 14px; border-left: 1px solid color-mix(in srgb, var(--bg-light) 70%, transparent); min-width: 0; }
  .ro:first-child { border-left: none; }
  .ro:last-child { padding-right: 0; }
  .ro .k { font-size: 10.5px; color: var(--fg-dim); }
  .ro .v { display: inline-flex; align-items: center; gap: 6px; font-size: 12px; color: var(--fg); font-variant-numeric: tabular-nums; white-space: nowrap; }
  .ro .sub { margin-left: 4px; color: var(--accent); font-size: 11px; }
  .ro-btn { display: inline-flex; align-items: center; gap: 6px; background: none; border: none; padding: 0; color: var(--fg); font: inherit; cursor: pointer; border-radius: 2px; }
  .ro-btn:hover { color: var(--accent); }
  .ro-btn:focus-visible { outline: 2px solid var(--accent); outline-offset: 2px; }
  .ro-btn.budget[data-level='amber'] { color: var(--yellow); }
  .ro-btn.budget[data-level='over'] { color: var(--red); font-weight: 600; }
  .chev { display: inline-flex; color: var(--fg-dim); transform: rotate(90deg); transition: transform 0.15s; }
  .chev.open { transform: rotate(-90deg); }
  .pips { display: inline-flex; gap: 3px; }
  .pip { width: 6px; height: 6px; border-radius: 50%; box-shadow: inset 0 0 0 1px var(--fg-dim); }
  .pip.on { background: var(--green); box-shadow: none; }
  .pip.starting { box-shadow: inset 0 0 0 1px var(--accent); background: color-mix(in srgb, var(--accent) 35%, transparent); }
  .meter { width: 40px; height: 4px; border-radius: 2px; background: color-mix(in srgb, var(--fg-dim) 30%, transparent); overflow: hidden; }
  .meter > span { display: block; height: 100%; background: var(--green); }
  .budget[data-level='amber'] .meter > span { background: var(--yellow); }
  .budget[data-level='over'] .meter > span { background: var(--red); }
  /* Narrow: the readouts leave the tabs' line for a grid of their own, with no dividers to strand
     at a line's start. */
  @container (max-width: 820px) {
    .bar { flex-direction: column-reverse; flex-wrap: nowrap; align-items: stretch; gap: 10px; }
    .readouts { display: grid; align-content: start; grid-template-columns: repeat(auto-fill, minmax(130px, 1fr)); gap: 8px 16px; padding-bottom: 0; }
    .ro, .ro:last-child { padding: 0; border-left: none; }
  }
  .criteria-panel { border-bottom: 1px solid var(--bg-light); color: var(--fg); font-size: 12px; padding: 6px 18px 8px; }
  .criteria-panel ol { margin: 0; padding-left: 20px; }
  .criteria-panel li { overflow-wrap: anywhere; }
  .criteria-panel .dim { color: var(--fg-dim); }
  .cols { flex: 1; min-height: 0; display: flex; min-width: 0; overflow: hidden; }
  .coord { flex: none; display: flex; flex-direction: column; min-height: 0; min-width: 0; max-width: calc(100% - 264px); transition: box-shadow 0.3s; }
  .coord > :global(.convo) { flex: 1; }
  .coord.flash { box-shadow: inset 0 0 0 1px var(--accent); }
  .coord.focused { box-shadow: inset 0 2px 0 var(--accent); }
  .coord.dragging { user-select: none; }
  .coord.gone, .wslot.gone, .board.gone { display: none; }
  .coord-failed { margin: 0; padding: 6px 14px; border-bottom: 1px solid var(--bg-light); font-size: 11.5px; color: var(--yellow); overflow-wrap: anywhere; }
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
  .board:focus { outline: none; }
  .board.focused { box-shadow: inset 0 2px 0 var(--accent); }
  .rail-list { flex: 1; display: flex; flex-direction: column; gap: 6px; padding: 10px; min-width: 0; }
  .rail-hint { margin: auto 0 0; padding: 10px 2px 0; font-size: 11px; line-height: 1.4; color: var(--fg-dim); overflow-wrap: anywhere; }
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
    .edge.run, .edge.wait, .status.running .pulse { animation: none; }
    .chev { transition: none; }
  }
</style>
