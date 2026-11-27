<script lang="ts">
  /** A workspace's stack on its sidebar row (docs/stack.md §7): one dot per row the stack panel
   *  would show (each copy of each service, a shared service by its source), in the panel's
   *  order. Seven or more become a fixed-width bar with a segment each, so a big stack never
   *  widens the row and a crashed service still shows as red. The tooltip names every row;
   *  a click shows this stack in the panel. */
  import type { Workspace } from '$lib/tauri/types';
  import { stackStore } from '$lib/stores/stack.svelte';
  import { copyTitle } from '$lib/stack/model';
  import Tooltip from '$lib/components/Tooltip.svelte';

  interface Props {
    workspace: Workspace;
    /** This stack is the one the panel shows. */
    shown: boolean;
    onclick: () => void;
  }

  let { workspace, shown, onclick }: Props = $props();

  type Mark = { name: string; port: number | null; tone: 'up' | 'starting' | 'crashed' | 'off' };

  function tone(status: string): Mark['tone'] {
    if (status === 'ready' || status === 'running') return 'up';
    if (status === 'starting') return 'starting';
    if (status === 'crashed') return 'crashed';
    return 'off';
  }

  const marks = $derived.by((): Mark[] => {
    const out: Mark[] = [];
    for (const s of stackStore.services(workspace.id)) {
      if (s.shared_from) {
        const src = stackStore.sharedStatus(s.id);
        out.push({ name: s.name, port: src?.port ?? null, tone: tone(src?.status ?? 'stopped') });
        continue;
      }
      for (const c of stackStore.copies(workspace.id, s.id)) {
        out.push({
          name: copyTitle(s.name, c.target, c.runtime.copy),
          port: stackStore.endpoint(workspace.id, s.id, c.target).port,
          tone: tone(c.runtime.status),
        });
      }
    }
    return out;
  });

  const BAR_FROM = 7;
  const SYMBOL: Record<Mark['tone'], string> = { up: '●', starting: '◐', crashed: '✕', off: '○' };

  const tip = $derived.by(() => {
    const up = marks.filter((m) => m.tone === 'up').length;
    const lines = marks.map((m) => `${SYMBOL[m.tone]} ${m.name}${m.port ? `  :${m.port}` : ''}${m.tone === 'crashed' ? '  crashed' : ''}`);
    return `${up} of ${marks.length} running\n${lines.join('\n')}\n\n${shown ? 'Shown in the stack panel' : 'Click to show in the stack panel'}`;
  });
</script>

{#if marks.length}
  <Tooltip text={tip}>
    <button
      type="button"
      class="stack-dots"
      class:shown
      aria-label={`${workspace.name} stack: show in the stack panel`}
      onclick={(e) => { e.stopPropagation(); onclick(); }}
    >
      {#if marks.length >= BAR_FROM}
        <span class="bar">{#each marks as m, i (i)}<i class={m.tone}></i>{/each}</span>
      {:else}
        {#each marks as m, i (i)}<span class="dot {m.tone}"></span>{/each}
      {/if}
    </button>
  </Tooltip>
{/if}

<style>
  .stack-dots {
    align-items: center;
    background: none;
    border: none;
    border-radius: 4px;
    cursor: pointer;
    display: flex;
    gap: 3px;
    height: 18px;
    justify-content: flex-end;
    padding: 0 3px;
  }
  .stack-dots:hover { background: color-mix(in srgb, var(--fg) 10%, transparent); }
  .stack-dots.shown { box-shadow: inset 0 -1.5px 0 var(--accent); border-radius: 0; }
  .stack-dots:focus-visible { outline: 2px solid var(--accent); outline-offset: 1px; }

  .dot {
    border-radius: 50%;
    box-sizing: border-box;
    flex-shrink: 0;
    height: 6px;
    width: 6px;
  }
  .dot.up { background: var(--green); }
  .dot.starting { background: var(--yellow); }
  .dot.crashed { background: var(--red); }
  /* Hollow in --fg-dim at full strength: a dim fill alone vanishes on the active row in light themes. */
  .dot.off { border: 1.2px solid var(--fg-dim); }

  .bar {
    border-radius: 2px;
    display: flex;
    gap: 1px;
    height: 6px;
    overflow: hidden;
    width: 44px;
  }
  .bar i { background: color-mix(in srgb, var(--fg-dim) 55%, transparent); flex: 1; }
  .bar i.up { background: var(--green); }
  .bar i.starting { background: var(--yellow); }
  .bar i.crashed { background: var(--red); }
</style>
