<!--
  A Hive agent's raw terminal, under its column's chat (docs/hive.md §4.5): the escape hatch for
  what only the terminal shows. The tab's TerminalPane portals into the slot here, the way the
  stack's console drawer takes a service's (`data-terminal-slot`). SplitPane renders no slot for a
  Hive tab, so this one is the portal's only match.

  The pane is `fixedSize`: its PTY keeps the size it spawned at, so dragging this split or the
  column's width never resizes it (a width change at a streaming Claude duplicates its
  scrollback). The slot scrolls both ways instead.
-->
<script lang="ts">
  import { tick } from 'svelte';
  import Icon from '$lib/components/Icon.svelte';
  import IconButton from '$lib/components/ui/IconButton.svelte';
  import { terminalsStore } from '$lib/stores/terminals.svelte';
  import { clampTerminalHeight, TERMINAL_HEIGHT_DEFAULT, TERMINAL_HEIGHT_KEY } from '$lib/hive/fixedSize';

  interface Props {
    tabId: string;
    onhide: () => void;
  }
  let { tabId, onhide }: Props = $props();

  function savedHeight(): number {
    try {
      const raw = localStorage.getItem(TERMINAL_HEIGHT_KEY);
      return raw ? clampTerminalHeight(Number(raw)) : TERMINAL_HEIGHT_DEFAULT;
    } catch {
      return TERMINAL_HEIGHT_DEFAULT;
    }
  }
  function keep(pct: number) {
    try { localStorage.setItem(TERMINAL_HEIGHT_KEY, String(pct)); } catch { /* per-viewer nicety only */ }
  }
  let pct = $state(savedHeight());
  let rootEl = $state<HTMLElement | undefined>();
  let dragging = $state(false);

  // The pane looks for its slot when told (same contract as ServiceConsole and MeshStageView).
  $effect(() => {
    const id = tabId;
    tick().then(() => {
      window.dispatchEvent(new CustomEvent('terminal-slot-ready', { detail: { tabId: id } }));
    });
  });

  /** The split's border: a share of the column, measured from its bottom. Touches no PTY. */
  function startDrag(e: PointerEvent) {
    const col = rootEl?.parentElement;
    if (!col) return;
    e.preventDefault();
    const el = e.currentTarget as HTMLElement;
    el.setPointerCapture(e.pointerId);
    dragging = true;
    const move = (ev: PointerEvent) => {
      const r = col.getBoundingClientRect();
      if (r.height > 0) pct = clampTerminalHeight(((r.bottom - ev.clientY) / r.height) * 100);
    };
    const end = () => {
      dragging = false;
      el.removeEventListener('pointermove', move);
      el.removeEventListener('pointerup', end);
      el.removeEventListener('pointercancel', end);
      keep(pct);
    };
    el.addEventListener('pointermove', move);
    el.addEventListener('pointerup', end);
    el.addEventListener('pointercancel', end);
  }
  function onKey(e: KeyboardEvent) {
    const step = (e.shiftKey ? 10 : 3) * (e.key === 'ArrowUp' ? 1 : e.key === 'ArrowDown' ? -1 : 0);
    if (!step) return;
    e.preventDefault();
    keep((pct = clampTerminalHeight(pct + step)));
  }
</script>

<div class="ht" class:dragging bind:this={rootEl} style="height: {pct}%">
  <!-- A focusable separator is a control (ARIA), which the a11y lint doesn't know. -->
  <!-- svelte-ignore a11y_no_noninteractive_tabindex, a11y_no_noninteractive_element_interactions -->
  <div
    class="split"
    role="separator"
    aria-orientation="horizontal"
    aria-label="Resize the terminal"
    aria-valuemin={15}
    aria-valuemax={85}
    aria-valuenow={pct}
    tabindex="0"
    onpointerdown={startDrag}
    onkeydown={onKey}
  ></div>
  <div class="bar">
    <Icon name="terminal" size={12} />
    <span class="label">Terminal</span>
    <IconButton tooltip="Hide the terminal" onclick={onhide}><Icon name="close" size={11} /></IconButton>
  </div>
  <div class="slot" data-terminal-slot={tabId} data-hive-terminal>
    {#if !terminalsStore.get(tabId)}
      <p class="none">This agent's terminal isn't running.</p>
    {/if}
  </div>
</div>

<style>
  .ht { flex: none; display: flex; flex-direction: column; min-height: 0; border-top: 1px solid var(--bg-light); background: var(--bg-dark); }
  .ht.dragging { user-select: none; }
  .split { flex: none; height: 4px; cursor: row-resize; background: var(--bg-light); opacity: 0.5; }
  .split:hover, .ht.dragging .split, .split:focus-visible { opacity: 1; background: var(--accent); outline: none; }
  .bar { flex: none; display: flex; align-items: center; gap: 6px; padding: 2px 8px 2px 12px; font-size: 11px; color: var(--fg-dim); background: var(--bg-medium); }
  .label { flex: 1; }
  /* Block, not flex: the fixed-size pane is as big as its grid, and this scrolls around it. */
  .slot { flex: 1; min-height: 0; position: relative; overflow: auto; }
  .none { margin: 12px; font-size: 12px; color: var(--fg-dim); }
</style>
