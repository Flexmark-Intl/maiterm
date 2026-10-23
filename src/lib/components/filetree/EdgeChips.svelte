<script lang="ts">
  /**
   * Edge chips (docs/file-tree.md §3.2 item 7): when rows an agent is working on (or just
   * touched) are scrolled out of view, a chip on the top or bottom edge says how many
   * (`↑ 3 active`, `↓ 1 editing`); a click scrolls to the nearest. They replace the mod's lit
   * rows pinned above the list, which moved content under the cursor.
   *
   * A lit path inside a collapsed folder counts as that folder's row: folders never open by
   * themselves, so the folder is what there is to scroll to.
   */
  import { fileTreeStore } from '$lib/stores/fileTree.svelte';
  import { ancestorsOf, type Row } from '$lib/filetree/model';

  interface Props {
    rows: Row[];
    scrollTop: number;
    viewportH: number;
    rowH: number;
    /** Rows the sticky header covers at the top. */
    headerRows: number;
    onjump: (index: number) => void;
  }

  let { rows, scrollTop, viewportH, rowH, headerRows, onjump }: Props = $props();

  /** key → index, built only while something is lit, once per `rows`. */
  let indexCache: { rows: Row[] | null; map: Map<string, number> } = { rows: null, map: new Map() };
  function indexOf(key: string): number {
    if (indexCache.rows !== rows) {
      const map = new Map<string, number>();
      rows.forEach((r, i) => { if (r.kind !== 'm') map.set(r.key, i); });
      indexCache = { rows, map };
    }
    return indexCache.map.get(key) ?? -1;
  }

  const lit = $derived.by(() => {
    const rels = fileTreeStore.litRels();
    if (!rels.length) return [] as { i: number; editing: boolean }[];
    const out = new Map<number, boolean>();
    for (const { rel, editing } of rels) {
      let i = indexOf(rel);
      if (i < 0) {
        const up = ancestorsOf(rel);
        for (let k = up.length - 1; k >= 0 && i < 0; k--) i = indexOf(up[k]);
      }
      if (i >= 0) out.set(i, (out.get(i) ?? false) || editing);
    }
    return [...out].map(([i, editing]) => ({ i, editing }));
  });

  const edges = $derived.by(() => {
    const first = Math.floor(scrollTop / rowH) + headerRows;
    const last = Math.floor((scrollTop + viewportH - 1) / rowH);
    const above = lit.filter((l) => l.i < first);
    const below = lit.filter((l) => l.i > last);
    return {
      up: above.length ? { label: label(above), to: Math.max(...above.map((l) => l.i)) } : null,
      down: below.length ? { label: label(below), to: Math.min(...below.map((l) => l.i)) } : null,
    };
  });

  function label(ls: { editing: boolean }[]): string {
    const editing = ls.filter((l) => l.editing).length;
    const active = ls.length - editing;
    return [editing && `${editing} editing`, active && `${active} active`].filter(Boolean).join(' · ');
  }
</script>

{#if edges.up}
  <button class="chip top" style="top: {headerRows * rowH + 4}px" onclick={() => { fileTreeStore.humanTouched(); onjump(edges.up!.to); }}>
    ↑ {edges.up.label}
  </button>
{/if}
{#if edges.down}
  <button class="chip bottom" onclick={() => { fileTreeStore.humanTouched(); onjump(edges.down!.to); }}>
    ↓ {edges.down.label}
  </button>
{/if}

<style>
  .chip {
    position: absolute;
    left: 50%;
    transform: translateX(-50%);
    z-index: 4;
    padding: 1px 8px;
    border-radius: 9px;
    font-size: 0.82em;
    color: var(--fg);
    background: color-mix(in srgb, var(--bg-light) 85%, var(--ft-write));
    box-shadow: 0 1px 4px color-mix(in srgb, var(--bg-dark) 60%, transparent);
    max-width: calc(100% - 16px);
    overflow-wrap: anywhere;
  }

  .chip:hover {
    background: color-mix(in srgb, var(--bg-light) 70%, var(--ft-write));
  }

  .bottom {
    bottom: 6px;
  }
</style>
