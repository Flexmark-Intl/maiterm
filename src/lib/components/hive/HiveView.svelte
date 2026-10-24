<!--
  A Hive workspace's main area (docs/hive.md §4.1): the header, the coordinator's column and
  the board. In place of the split tree, never over it: a Hive has no visible terminal.

  The coordinator's column is its transcript (ChatColumn), never its terminal: dragging the
  column's width touches no PTY, and a width change at a streaming Claude duplicates its
  scrollback (principle 3). Workers, Pause, Gallery and Memory arrive with later steps.
-->
<script lang="ts">
  import { onMount } from 'svelte';
  import { workspacesStore } from '$lib/stores/workspaces.svelte';
  import { tasksStore } from '$lib/stores/tasks.svelte';
  import { terminalsStore } from '$lib/stores/terminals.svelte';
  import ChatColumn from '$lib/components/loom/ChatColumn.svelte';
  import { HIVE_OPEN_COLUMN, takePendingHiveOpen, type HiveOpenColumnDetail } from '$lib/hive/model';

  interface Props {
    workspaceId: string;
  }
  let { workspaceId }: Props = $props();

  const ws = $derived(workspacesStore.workspaces.find((w) => w.id === workspaceId));
  const hive = $derived(ws?.hive ?? null);
  const coordinator = $derived(ws?.panes.flatMap((p) => p.tabs).find((t) => t.hive_role === 'coordinator') ?? null);
  const coordId = $derived(coordinator?.id ?? null);
  const source = $derived(coordId ? { kind: 'tab' as const, tabId: coordId } : null);
  const tasks = $derived(tasksStore.forWorkspace(workspaceId));
  const workers = $derived(ws?.panes.flatMap((p) => p.tabs).filter((t) => t.hive_role === 'worker').length ?? 0);

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

  // ── "Open this Hive agent" (navigateToTab, the waiting cards): focus its column. ──
  let column = $state<ChatColumn | undefined>();
  let flash = $state(false);
  function open(detail: HiveOpenColumnDetail) {
    if (detail.workspaceId !== workspaceId) return;
    // Worker columns arrive with workers (build step 5); until then only the coordinator has one.
    if (detail.tabId !== coordId) return;
    column?.focusComposer();
    flash = true;
    setTimeout(() => (flash = false), 900);
  }
  onMount(() => {
    const pending = takePendingHiveOpen(workspaceId);
    if (pending) requestAnimationFrame(() => open(pending));
    const onOpen = (e: Event) => {
      takePendingHiveOpen(workspaceId);
      open((e as CustomEvent<HiveOpenColumnDetail>).detail);
    };
    window.addEventListener(HIVE_OPEN_COLUMN, onOpen);
    return () => window.removeEventListener(HIVE_OPEN_COLUMN, onOpen);
  });
</script>

<div class="hive">
  <header class="hive-h">
    <div class="title">
      <b>{ws?.name ?? ''}</b>
      {#if hive?.goal}<span class="goal">{hive.goal}</span>{/if}
    </div>
    <div class="facts">
      <span>{workers === 0 ? '0 workers' : `${workers} worker${workers === 1 ? '' : 's'}`} · up to {hive?.cap ?? 0} at once</span>
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
    <section class="board" aria-label="Board">
      <div class="empty">
        <b>No workers yet</b>
        <span>Workers the coordinator starts show up here as cards, one per task.</span>
      </div>
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
  .empty { margin: auto; display: flex; flex-direction: column; align-items: center; gap: 4px; padding: 24px; text-align: center; max-width: 320px; }
  .empty b { font-size: 13px; color: var(--fg); }
  .empty span { font-size: 12px; color: var(--fg-dim); overflow-wrap: anywhere; }
  .role { font-size: 10px; text-transform: uppercase; letter-spacing: 0.05em; color: var(--accent); border: 1px solid color-mix(in srgb, var(--accent) 45%, transparent); border-radius: 3px; padding: 0 4px; }
  @media (prefers-reduced-motion: reduce) { .coord { transition: none; } }
</style>
