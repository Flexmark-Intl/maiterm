<!--
  Workstream Loom (docs/loom.md): the workspace's work as a picture, the questions waiting on
  the human, and a condensed chat. It covers the terminal area the way ServiceConsole does:
  absolutely positioned inside .main-content, so the terminal underneath keeps its size (a
  width change makes Claude Code re-render its transcript into scrollback).
-->
<script lang="ts">
  import { untrack } from 'svelte';
  import { loomStore, type LoomView as View } from '$lib/stores/loom.svelte';
  import { tasksStore } from '$lib/stores/tasks.svelte';
  import { workspacesStore } from '$lib/stores/workspaces.svelte';
  import { terminalsStore } from '$lib/stores/terminals.svelte';
  import { decisionsQueue, summarize } from '$lib/loom/model';
  import LoomView from './LoomView.svelte';
  import DecisionsView from './DecisionsView.svelte';
  import FocusView from './FocusView.svelte';

  interface Props {
    workspaceId: string;
  }
  let { workspaceId }: Props = $props();

  const workspace = $derived(workspacesStore.workspaces.find((w) => w.id === workspaceId));
  /** The Overlord workspace supervises the window, so it always sees every workspace. */
  const windowWide = $derived(!!workspace?.overlord || loomStore.scope === 'window');
  const scoped = $derived(
    windowWide ? workspacesStore.workspaces : workspace ? [workspace] : [],
  );
  const tasks = $derived(scoped.flatMap((w) => tasksStore.forWorkspace(w.id)));

  /** Ages and the quiet rule move with the clock, not only with the data. */
  let now = $state(Date.now());
  $effect(() => {
    if (!loomStore.open) return;
    now = Date.now();
    const t = setInterval(() => (now = Date.now()), 60_000);
    return () => clearInterval(t);
  });

  const summary = $derived(summarize(tasks, now, workspacesStore.parkedTaskIds));
  const decisions = $derived(decisionsQueue(tasks).length);

  /** The drawer covers the terminal, so it must hold the keyboard for as long as it is open.
   *  Unlike ServiceConsole, whose focused terminal is the visible one inside it, a terminal
   *  focused here is HIDDEN: its xterm eats Escape (sending ESC interrupts a working agent) and
   *  every keystroke lands in a terminal nobody can see.
   *
   *  Taking focus once on open was not enough: switching tabs while the drawer is open focuses
   *  the new tab's terminal underneath (Cmd+1-9, a toast, an Overlord chip). So any focus that
   *  lands in the terminal area outside the drawer is sent back to the drawer. On close, focus
   *  goes to the tab on screen NOW, never to whatever had it when the drawer opened: a tab
   *  switched away from keeps its slot in the DOM, so refocusing it would type into a hidden
   *  tab. */
  let drawerEl = $state<HTMLElement | null>(null);
  $effect(() => {
    if (!loomStore.open || !drawerEl) return;
    // What had the keyboard, and on which tab. Restored on close only while that tab is still
    // the one on screen: an editor keeps its caret, and a tab switched away from is not
    // refocused (its slot stays in the DOM, hidden).
    const before = document.activeElement as HTMLElement | null;
    const beforeTab = untrack(() => workspacesStore.activeTab?.id ?? null);
    // Root CLAUDE.md: focus explicitly on a frame, never rely on autofocus.
    const raf = requestAnimationFrame(() => drawerEl?.focus({ preventScroll: true }));
    const onFocusIn = (e: FocusEvent) => {
      const target = e.target as Element | null;
      if (!target || target.closest('.loom-drawer') || !target.closest('.main-content')) return;
      drawerEl?.focus({ preventScroll: true });
    };
    document.addEventListener('focusin', onFocusIn, true);
    return () => {
      cancelAnimationFrame(raf);
      document.removeEventListener('focusin', onFocusIn, true);
      // Only when the keyboard is still with the drawer (or nowhere): if the human clicked
      // something else to close it, that has focus now and keeps it.
      const active = document.activeElement;
      if (active && active !== document.body && !active.closest('.loom-drawer')) return;
      const tab = workspacesStore.activeTab;
      if (tab && tab.id === beforeTab && before?.isConnected && !before.closest('.loom-drawer')) {
        before.focus({ preventScroll: true });
      } else if (tab?.tab_type === 'terminal') {
        terminalsStore.focusTerminal(tab.id);
      }
    };
  });

  // Escape closes, except from a field inside the drawer (an answer being typed), and except
  // an Escape another overlay already handled (QuickOpen and the pickers preventDefault it).
  $effect(() => {
    if (!loomStore.open) return;
    const onKey = (e: KeyboardEvent) => {
      if (e.key !== 'Escape' || e.defaultPrevented) return;
      const target = e.target as Element | null;
      if (target?.closest?.('.loom-drawer') && target.matches('input, textarea')) {
        (target as HTMLElement).blur();
        drawerEl?.focus({ preventScroll: true });
        return;
      }
      e.preventDefault();
      loomStore.close();
    };
    window.addEventListener('keydown', onKey);
    return () => window.removeEventListener('keydown', onKey);
  });

  const VIEWS: { id: View; label: string }[] = [
    { id: 'loom', label: 'Loom' },
    { id: 'decisions', label: 'Decisions' },
    { id: 'focus', label: 'Focus' },
  ];
</script>

{#if loomStore.open && workspace}
  <section class="loom-drawer" aria-label="Workstream Loom" tabindex="-1" bind:this={drawerEl}>
    <header>
      <span class="where">
        {#if workspace.overlord}
          <b>All workspaces</b>
        {:else}
          <button class="scope" class:on={!windowWide} onclick={() => loomStore.setScope('workspace')}>{workspace.name}</button>
          <button class="scope" class:on={windowWide} onclick={() => loomStore.setScope('window')}>All workspaces</button>
        {/if}
      </span>
      <span class="summary">
        <span><i style="background: var(--green)"></i>{summary.active} active</span>
        <span><i style="background: var(--orange, #ff9e64)"></i>{summary.needsYou} need you</span>
        <span><i style="background: var(--yellow)"></i>{summary.waiting} waiting</span>
        <span><i style="background: var(--fg-dim)"></i>{summary.quiet} quiet 14d+</span>
      </span>
      <nav class="views" aria-label="View">
        {#each VIEWS as v (v.id)}
          <button aria-pressed={loomStore.view === v.id} onclick={() => loomStore.setView(v.id)}>
            {v.label}
            {#if v.id === 'decisions' && decisions}<span class="count">{decisions}</span>{/if}
          </button>
        {/each}
      </nav>
      <button class="close" aria-label="Close" onclick={() => loomStore.close()}>×</button>
    </header>

    <div class="body">
      {#if loomStore.view === 'loom'}
        <LoomView workspaces={scoped} {tasks} {now} multi={windowWide} />
      {:else if loomStore.view === 'decisions'}
        <DecisionsView workspaces={scoped} {tasks} />
      {:else}
        <FocusView workspaces={scoped} {tasks} {now} />
      {/if}
    </div>
  </section>
{/if}

<style>
  .loom-drawer {
    position: absolute;
    inset: 0;
    z-index: 45;
    display: flex;
    flex-direction: column;
    background: var(--bg-dark);
    color: var(--fg);
    animation: loom-in 160ms ease-out;
    outline: none;
  }
  @keyframes loom-in {
    from { opacity: 0; transform: translateY(6px); }
  }
  @media (prefers-reduced-motion: reduce) {
    .loom-drawer { animation: none; }
  }
  header {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: 8px 18px;
    padding: 8px 14px;
    background: var(--bg-medium);
    border-bottom: 1px solid var(--bg-light);
    font-size: 12px;
  }
  .where { display: flex; gap: 2px; align-items: center; }
  .where b { font-weight: 600; }
  .scope {
    background: none;
    border: 0;
    color: var(--fg-dim);
    padding: 3px 8px;
    border-radius: 5px;
    font: inherit;
    cursor: pointer;
    overflow-wrap: anywhere;
  }
  .scope.on { background: var(--bg-light); color: var(--fg); }
  .summary { display: flex; flex-wrap: wrap; gap: 4px 14px; color: var(--fg-dim); font-variant-numeric: tabular-nums; }
  .summary span { display: inline-flex; align-items: center; gap: 6px; }
  .summary i { width: 7px; height: 7px; border-radius: 50%; display: inline-block; }
  .views {
    display: flex;
    gap: 2px;
    margin-left: auto;
    background: var(--bg-dark);
    padding: 3px;
    border-radius: 7px;
  }
  .views button {
    display: flex;
    align-items: center;
    gap: 6px;
    background: none;
    border: 0;
    color: var(--fg-dim);
    padding: 4px 12px;
    border-radius: 5px;
    font: inherit;
    font-weight: 600;
    cursor: pointer;
  }
  .views button[aria-pressed='true'] { background: var(--bg-light); color: var(--fg); }
  .count {
    background: var(--orange, #ff9e64);
    color: var(--bg-dark);
    border-radius: 8px;
    padding: 0 5px;
    font-size: 10.5px;
  }
  .close {
    background: none;
    border: 0;
    color: var(--fg-dim);
    font-size: 18px;
    line-height: 1;
    padding: 2px 6px;
    cursor: pointer;
  }
  .close:hover { color: var(--fg); }
  .body { flex: 1; min-height: 0; position: relative; }
</style>
